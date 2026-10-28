//! `fitting_module_cycles` — `dogmaIM.Activate` & `dogmaIM.Overload` active module cycle execution.

#![forbid(unsafe_code)]

use super::*;

#[path = "fitting_module_cycles_helpers.rs"]
pub(crate) mod helpers;
pub(crate) use helpers::*;

pub(crate) async fn handle_module_cycles(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dogmaIM", "Activate") | ("dogmaIM", "Overload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(
                ctx,
                char_id,
                "SAFE_LOGOFF_ABORT_MODULE",
            );
            let raw_module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let target_id_opt = args.get(2).and_then(|a| a.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()));
            let repeat = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1) as i32;

            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };

            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Cannot activate modules outside of space".into() }));
            };
            let sys = ctx.systems.get_or_spawn(sys_id);

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let module_id = if raw_module_id != 0 {
                raw_module_id
            } else {
                fit.modules
                    .values()
                    .find(|m| {
                        ctx.sde
                            .types
                            .get(&m.type_id)
                            .is_some_and(|t| t.group_id == 481)
                    })
                    .map(|m| m.item_id)
                    .unwrap_or(0)
            };

            let m_opt = fit.modules.get(&module_id).cloned().or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    let item = snap.items.get(&module_id)?;
                    if item.location_id != ship_id || !matches!(item.flag as u16, 11..=34) {
                        return None;
                    }
                    let is_online = snap.module_online_states.get(&module_id).copied().unwrap_or(true);
                    let charge_tid = snap
                        .items
                        .values()
                        .find(|c| {
                            c.location_id == ship_id
                                && c.flag == item.flag
                                && c.item_id != module_id
                                && c.quantity > 0
                        })
                        .map(|c| c.type_id);
                    Some(eve_dogma::FittedModule {
                        item_id: module_id,
                        type_id: item.type_id,
                        flag_id: item.flag as u16,
                        state: if is_online {
                            eve_dogma::ModuleState::Online
                        } else {
                            eve_dogma::ModuleState::Offline
                        },
                        charge_type_id: charge_tid,
                        charge_quantity: 100,
                        damage_hp: 0.0,
                        max_hp: 40.0,
                        dynamic_attribute_overrides: std::collections::HashMap::new(),
                    })
                })
            });
            let Some(m) = m_opt else {
                let has_online_weapon = fit.modules.values().any(|wm| {
                    (27..=34).contains(&wm.flag_id)
                        && wm.state != eve_dogma::ModuleState::Offline
                        && ctx.sde.types.get(&wm.type_id).is_some_and(|t| {
                            matches!(
                                t.group_id,
                                53 | 55 | 74 | 506..=511 | 524 | 1986 | 4060 | 4500 | 4805 | 4807
                            )
                        })
                });
                if method == "Activate" && args.len() == 1 && raw_module_id > 0 && has_online_weapon {
                    return None;
                }
                return Some(Err(BridgeRpcError { code: "ModuleNotFound".into(), message: format!("Module {module_id} not found on ship") }));
            };
            if let Err(e) = check_module_reloading(ctx, ship_id, module_id) {
                return Some(Err(e));
            }
            let is_burned = m.damage_hp >= m.max_hp - 1e-6
                || ctx.db.with_snapshot(|s| {
                    let it = s.items.get(&module_id);
                    let dmg = it.and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied()).unwrap_or(0.0);
                    let hp = it.and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
                        .or_else(|| ctx.sde.types.get(&m.type_id).and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied()))
                        .unwrap_or(40.0);
                    dmg >= hp - 1e-6
                });
            if is_burned {
                return Some(Err(BridgeRpcError { code: "ModuleBurnedOut".into(), message: format!("Module {module_id} is burned out and cannot be activated") }));
            }
            // Disables any module group / weapon bank it belongs to until repaired or ungrouped (`feat:modules.overheat-burnout-state`)
            let group_burned = ctx.db.with_snapshot(|snap| {
                if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
                    for &tid in bank {
                        let it = snap.items.get(&tid);
                        let dmg = it.and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied()).unwrap_or(0.0);
                        let hp = it.and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
                            .or_else(|| it.and_then(|i| ctx.sde.types.get(&i.type_id)).and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied()))
                            .unwrap_or(40.0);
                        if dmg >= hp - 1e-6 {
                            return Some(tid);
                        }
                    }
                }
                None
            });
            if let Some(b_mod_id) = group_burned {
                return Some(Err(BridgeRpcError {
                    code: "ModuleBurnedOut".into(),
                    message: format!("Module group contains burned-out module {b_mod_id} and is disabled until repaired or ungrouped"),
                }));
            }
            if m.state == eve_dogma::ModuleState::Offline
                || !ctx.db.with_snapshot(|s| s.module_online_states.get(&module_id).copied().unwrap_or(true))
            {
                return Some(Err(BridgeRpcError { code: "ModuleOffline".into(), message: format!("Module {module_id} is offline") }));
            }

            let Some(mod_type) = ctx.sde.types.get(&m.type_id) else {
                return Some(Err(BridgeRpcError { code: "ModuleNotFound".into(), message: format!("Module type {} not found in SDE", m.type_id) }));
            };
            let is_weapon = matches!(mod_type.group_id, 53 | 55 | 74 | 1986 | 4060 | 506..=511 | 524 | 1674 | 4500 | 4805 | 4807);
            let is_upwell_weapon_or_aoe = (target_id_opt.is_some_and(|tid| tid >= 1_000_000_000_000 || ctx.db.with_snapshot(|s| s.structures.contains_key(&tid))) && is_weapon)
                || (matches!(mod_type.group_id, 72 | 862) && ctx.db.with_snapshot(|s| s.structures.values().any(|st| st.solar_system_id == sys_id && st.state != "Destroyed")));
            if is_weapon
                || is_upwell_weapon_or_aoe
                || (!matches!(mod_type.group_id, 1122 | 650)
                    && target_id_opt.is_some_and(|tid| {
                        ctx.db.with_snapshot(|s| {
                            s.items
                                .get(&tid)
                                .and_then(|i| ctx.sde.types.get(&i.type_id))
                                .is_some_and(|t| matches!(t.group_id, 340 | 448))
                        })
                    }))
            {
                return crate::services::combat_crimewatch::combat_weapons::handle_weapons(ctx, session, service, method, args, kwargs).await;
            }

            if (matches!(
                mod_type.group_id,
                52 | 65 | 68 | 71 | 72 | 201 | 208 | 291 | 379 | 862 | 74 | 55 | 53 | 1986 | 4060 | 506..=511 | 524 | 1674 | 4500 | 4805 | 4807
            ) || matches!(
                effect_name.as_str(),
                "empWave" | "useMissiles" | "targetAttack" | "projectileFired"
            )) && sys
                .get_combat_state(ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_tethered)
            {
                return Some(Err(BridgeRpcError {
                    code: "CantActivateWhileTethered".into(),
                    message: "Cannot activate offensive modules, smartbombs, or bomb launchers while tethered".into(),
                }));
            }

            // Mining lasers, strip miners, ice and gas harvesters run on the repeating MN-04 cycle engine.
            if crate::services::mining_cycle::mining_kind_of(&ctx.sde, mod_type).is_some() {
                let Some(target_id) = target_id_opt.filter(|&t| t > 0) else {
                    return Some(Err(BridgeRpcError { code: "TargetNotLocked".into(), message: "Mining modules require a locked target asteroid".into() }));
                };
                return Some(
                    crate::services::mining_cycle::activate(ctx, char_id, ship_id, sys_id, module_id, target_id).await,
                );
            }

            if mod_type.group_id == 330 {
                if fit.modules.values().any(|fm| ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 588)) {
                    return Some(Err(BridgeRpcError { code: "CannotCloakWithLanceFitted".into(), message: "Cannot activate a cloaking device while a Disruptive Lance or Superweapon is fitted".into() }));
                }
                if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "cloak") {
                    return Some(Err(e));
                }
            }
            if mod_type.group_id == 588 {
                return Some(crate::services::tactical_modules::activate_superweapon_or_lance(ctx, char_id, ship_id, sys_id, module_id, "ActivateSlashModule", target_id_opt).await);
            }
            if mod_type.group_id == 1154 {
                let mod_state = fit.build_effective_module_state(module_id, m.type_id, false, &ctx.sde);
                return Some(crate::services::tactical_modules::activate_target_spectrum_breaker(ctx, &sys, char_id, ship_id, module_id, mod_type, &mod_state).await);
            }
            if mod_type.group_id == 96 {
                let mod_state = fit.build_effective_module_state(module_id, m.type_id, false, &ctx.sde);
                return Some(crate::services::tactical_modules::activate_auto_targeting_system(ctx, &sys, char_id, ship_id, module_id, mod_type, &mod_state, &fit).await);
            }

            if let Some(res) =
                handle_cyno_generator_activation(ctx, &sys, char_id, ship_id, sys_id, module_id, mod_type).await
            {
                return Some(res);
            }

            if matches!(mod_type.group_id, 1770 | 538 | 47 | 48 | 1313 | 815) {
                let eval_ctx = fit.evaluated_context(&ctx.sde);
                let mod_state = eval_ctx
                    .ship_located_items
                    .iter()
                    .find(|ms| ms.item_id == module_id)
                    .cloned()
                    .unwrap_or_else(|| eve_dogma::DogmaItemState::from_sde(module_id, m.type_id, &ctx.sde));
                if mod_type.group_id == 1770 {
                    let duration_s = mod_state
                        .get_attribute(attr_ids::DURATION, &ctx.sde)
                        .max(mod_type.default_attributes.get(&73).copied().unwrap_or(60_000.0))
                        / 1000.0;
                    return Some(
                        activate_command_burst_module(
                            ctx,
                            &sys,
                            char_id,
                            ship_id,
                            sys_id,
                            module_id,
                            mod_type,
                            &mod_state,
                            m.charge_type_id,
                            duration_s,
                            kwargs,
                        )
                        .await,
                    );
                }
                if let Some(res) = handle_scanner_analyzer_entosis_activation(
                    ctx,
                    &sys,
                    char_id,
                    ship_id,
                    module_id,
                    mod_type,
                    &mod_state,
                    target_id_opt,
                )
                .await
                {
                    return Some(res);
                }
            }

            if matches!(mod_type.group_id, 41 | 1697) {
                if let Some(tid) = target_id_opt.filter(|&t| t > 0) {
                    let is_mercden = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens.contains_key(&tid));
                    if is_mercden {
                        let rep_hp = mod_type.default_attributes.get(&68).copied().unwrap_or(1_500.0);
                        return Some(
                            crate::services::sov_gameplay::apply_mercenary_den_shield_repair(ctx, sys_id, ship_id, tid, rep_hp)
                                .await
                                .map(|(pct, enabled)| PyRep::keyval([
                                    ("moduleID", PyRep::Integer(module_id)), ("targetID", PyRep::Integer(tid)),
                                    ("shieldPct", PyRep::Integer(pct as i64)), ("denEnabled", PyRep::Bool(enabled)),
                                ])),
                        );
                    }
                }
            }

            // Scan Probe Launcher (`group_id == 481`, e.g., `Core Probe Launcher I` `17938`)
            if mod_type.group_id == 481 {
                return Some(
                    crate::services::space_destiny::activate_scan_probe_launcher(
                        ctx,
                        session,
                        &sys,
                        char_id,
                        ship_id,
                        sys_id,
                        module_id,
                        m.type_id,
                        m.flag_id,
                        effect_name.as_str(),
                        target_id_opt,
                        kwargs,
                    )
                    .await,
                );
            }

            // Interdiction Sphere Launcher (`group_id == 589`, e.g., `Interdiction Sphere Launcher I` `22782`)
            if mod_type.group_id == 589 {
                let sys_sec = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                let corruption_stage5 = ctx.db.with_snapshot(|snap| {
                    snap.system_states.get(&sys_id).map(|st| st.insurgency_corruption_stage >= 5).unwrap_or(false)
                });
                if sys_sec >= 0.45 || (sys_sec > 0.0 && !corruption_stage5) {
                    return Some(Err(BridgeRpcError {
                        code: "CannotLaunchInterdictionProbeInEmpire".into(),
                        message: format!(
                            "Interdiction sphere probes can only be launched in null-sec or wormhole space (system {sys_id} security {sys_sec:.2} > 0.0)"
                        ),
                    }));
                }
                let ship_group_id = ctx
                    .sde
                    .types
                    .get(&fit.ship.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                if ship_group_id != 541 {
                    return Some(Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Interdiction Sphere Launcher can only be activated on Interdictors (group 541)".into() }));
                }
                let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| matches!(t.group_id, 548 | 4088))
                        })
                        .map(|i| (i.item_id, i.type_id))
                });
                let Some((charge_item_id, probe_type_id)) = loaded_charge_opt else {
                    return Some(Err(BridgeRpcError { code: "NoProbesLoaded".into(), message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded".into() }));
                };

                let (probe_id, remaining_in_launcher) = ctx.db.with_mut_snapshot(|snap| {
                    let mut rem = 0;
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        ci.quantity = (ci.quantity - 1).max(0);
                        rem = ci.quantity;
                    }
                    if rem == 0 {
                        snap.items.remove(&charge_item_id);
                    }
                    snap.next_entity_id += 1;
                    (snap.next_entity_id, rem)
                });
                if remaining_in_launcher == 0 {
                    ctx.invalidate_ship_fitting(ship_id);
                }

                let probe_sde = ctx.sde.types.get(&probe_type_id);
                let is_wubble_probe = probe_type_id == 56748 || probe_sde.is_some_and(|t| t.group_id == 4088);
                let bubble_radius = probe_sde
                    .and_then(|t| t.default_attributes.get(&103).copied().or_else(|| t.default_attributes.get(&99).copied()))
                    .unwrap_or(if is_wubble_probe { 10_000.0 } else { 20_000.0 });
                let ship_pos = sys
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(DVec3::ZERO);

                if is_wubble_probe {
                    crate::services::tactical_modules::register_launched_wubble_probe(
                        ctx, probe_id, sys_id, ship_pos, bubble_radius,
                    );
                } else if let Err(e) = sys.add_interdiction_sphere(probe_id, ship_pos, bubble_radius).await {
                    return Some(Err(BridgeRpcError { code: "SystemActorError".into(), message: e }));
                }

                // V24 client derives `OnModuleActivated` from `OnGodmaShipEffect` (active=1).
                let _ = ctx.notify_module_effect_started(char_id, ship_id, module_id, m.type_id, Some(effect_name.as_str()), target_id_opt);
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("launcherID", PyRep::Integer(module_id)),
                    ("probeID", PyRep::Integer(probe_id)),
                    ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
                    ("bubbleRadius", PyRep::Real(bubble_radius)),
                    ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
                ])));
            }

            // Survey Probe Launcher (`group_id == 1226`, `33270` Survey Probe Launcher I, `33272` Survey Probe Launcher II)
            if mod_type.group_id == 1226 {
                let slot_flag = InventoryFlag::from_u16(m.flag_id);
                let launcher_cap = mod_type.capacity.max(10.0);

                // Auto-reload from Cargo / SpecializedAmmoHold if launcher slot is empty, then consume 1 probe,
                // and auto-reload again if the last probe in the launcher slot was just consumed.
                let launch_res = ctx.db.with_mut_snapshot(|snap| {
                    let mut in_slot_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                        })
                        .map(|i| (i.item_id, i.type_id));

                    let reload_into_slot = |snap: &mut eve_db::WorldStateSnapshot| -> Option<(i64, u32, i32)> {
                        let cargo_stack = snap
                            .items
                            .values()
                            .find(|i| {
                                i.location_id == ship_id
                                    && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
                                    && i.quantity > 0
                                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                            })
                            .map(|i| (i.item_id, i.type_id, i.quantity))?;
                        let (src_id, p_tid, avail) = cargo_stack;
                        let p_vol = ctx.sde.types.get(&p_tid).map(|t| t.volume.max(1.0)).unwrap_or(10.0);
                        let max_fit = ((launcher_cap / p_vol).floor() as i32).max(1);
                        let move_qty = avail.min(max_fit);
                        if move_qty >= avail {
                            if let Some(it) = snap.items.get_mut(&src_id) {
                                it.flag = slot_flag;
                            }
                            Some((src_id, p_tid, move_qty))
                        } else {
                            if let Some(it) = snap.items.get_mut(&src_id) {
                                it.quantity -= move_qty;
                            }
                            snap.next_entity_id += 1;
                            let new_id = snap.next_entity_id;
                            snap.items.insert(
                                new_id,
                                eve_db::InventoryItemRecord {
                                    item_id: new_id,
                                    type_id: p_tid,
                                    owner_id: char_id,
                                    location_id: ship_id,
                                    flag: slot_flag,
                                    quantity: move_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                            Some((new_id, p_tid, move_qty))
                        }
                    };

                    if in_slot_id.is_none() {
                        if let Some((nid, ptid, _)) = reload_into_slot(snap) {
                            in_slot_id = Some((nid, ptid));
                        }
                    }
                    let (charge_item_id, probe_type_id) = in_slot_id?;

                    let mut rem = 0;
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        ci.quantity = (ci.quantity - 1).max(0);
                        rem = ci.quantity;
                    }
                    if rem == 0 {
                        snap.items.remove(&charge_item_id);
                    }
                    let mut auto_reloaded = 0;
                    if rem == 0 {
                        if let Some((_, _, r_qty)) = reload_into_slot(snap) {
                            rem = r_qty;
                            auto_reloaded = r_qty;
                        }
                    }
                    // disallowRepeatingActivation (attr 1014 == 1.0): launcher does not auto-repeat
                    snap.module_active_states.remove(&module_id);
                    snap.next_entity_id += 1;
                    Some((snap.next_entity_id, probe_type_id, rem, auto_reloaded))
                });
                let Some((probe_id, probe_type_id, remaining_in_launcher, auto_reloaded)) = launch_res else {
                    return Some(Err(BridgeRpcError { code: "NoProbesLoaded".into(), message: "Survey probe launcher has no survey probes loaded".into() }));
                };
                ctx.invalidate_ship_fitting(ship_id);

                let probe_sde = ctx.sde.types.get(&probe_type_id);
                let base_delay_ms = probe_sde
                    .and_then(|t| t.default_attributes.get(&281).copied().or_else(|| t.default_attributes.get(&1370).copied()))
                    .unwrap_or(300_000.0);
                // T2 Survey Probe Launcher II (33272) has attribute 2701 `surveyProbeDurationBonus` = -10.0%
                let duration_bonus_pct = mod_type.default_attributes.get(&2701).copied().unwrap_or(0.0);
                let scan_delay_ms = (base_delay_ms * (1.0 + duration_bonus_pct / 100.0)).round() as i64;
                let max_scan_range_m = probe_sde
                    .and_then(|t| t.default_attributes.get(&2697).copied())
                    .unwrap_or(100_000_000.0);

                // Check ship alignment and 100,000 km (`maxScanRange` 2697) moon range
                let ship_ball_opt = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b);
                let ship_pos = ship_ball_opt.as_ref().map(|b| b.position).unwrap_or(DVec3::ZERO);
                let heading_opt: Option<DVec3> = if let Some(sb) = &ship_ball_opt {
                    if sb.velocity.length() > 1e-3 {
                        Some(sb.velocity.normalize())
                    } else {
                        match sb.mode {
                            eve_destiny::MovementMode::GotoDirection { direction } if direction.length() > 1e-3 => {
                                Some(direction.normalize())
                            }
                            eve_destiny::MovementMode::Follow { target_id, .. } => {
                                let tpos = if let Some((_, tb)) = sys.get_ball_snapshot(target_id).await.ok().flatten() {
                                    Some(tb.position)
                                } else {
                                    ctx.sde
                                        .celestials_in_system(sys_id)
                                        .into_iter()
                                        .find(|c| c.celestial_id == target_id)
                                        .map(|c| c.position)
                                };
                                tpos.and_then(|tp| ((tp - sb.position).length() > 1e-3).then(|| (tp - sb.position).normalize()))
                            }
                            _ => None,
                        }
                    }
                } else {
                    None
                };

                let moons: Vec<_> = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .into_iter()
                    .filter(|c| c.group_id == 8)
                    .collect();
                let hit_moon_id: Option<i64> = if let Some(target_moon_id) = target_id_opt {
                    moons.iter().find(|c| c.celestial_id == target_moon_id).and_then(|c| {
                        let to_moon = c.position - ship_pos;
                        let dist = to_moon.length();
                        let aligned = heading_opt.is_none_or(|dir| dir.dot(to_moon.normalize_or_zero()) >= 0.70);
                        (dist <= max_scan_range_m && aligned).then_some(c.celestial_id)
                    })
                } else if let Some(dir) = heading_opt {
                    moons
                        .iter()
                        .filter_map(|c| {
                            let to_moon = c.position - ship_pos;
                            let dist = to_moon.length();
                            (dist <= max_scan_range_m && dir.dot(to_moon.normalize_or_zero()) >= 0.90)
                                .then_some((c.celestial_id, dist))
                        })
                        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(cid, _)| cid)
                } else {
                    None
                };

                let flight_secs = (scan_delay_ms / 1000).max(1) as u64;
                let arrival_unix_secs = ctx.now_unix_secs().saturating_add(flight_secs);
                if let Ok(mut probes) = ctx.active_moon_probes.write() {
                    probes.push(crate::router::context::ActiveMoonSurveyProbe {
                        probe_id,
                        probe_type_id,
                        owner_char_id: char_id,
                        solar_system_id: sys_id,
                        hit_moon_id,
                        arrival_unix_secs,
                    });
                }

                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNewMoonProbe",
                    "charid",
                    vec![
                        PyRep::Integer(probe_id),
                        PyRep::Integer(probe_type_id as i64),
                        PyRep::Integer(hit_moon_id.unwrap_or(0)),
                    ],
                );

                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("launcherID", PyRep::Integer(module_id)),
                    ("probeID", PyRep::Integer(probe_id)),
                    ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
                    (
                        "moonID",
                        hit_moon_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("scanDelayMs", PyRep::Integer(scan_delay_ms)),
                    ("flightTimeSec", PyRep::Integer(flight_secs as i64)),
                    ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
                    ("autoReloadedCount", PyRep::Integer(auto_reloaded as i64)),
                ])));
            }

            {
                let already_active = matches!(
                    m.state,
                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                ) || ctx.db.with_snapshot(|s| s.module_active_states.contains(&module_id))
                    || sys.has_module_cycle(ship_id, module_id).await;
                if method == "Overload"
                    && (already_active
                        || (target_id_opt.is_none()
                            && matches!(
                                mod_type.group_id,
                                291 | 208 | 379 | 65 | 1672 | 52 | 201 | 41 | 325 | 2018 | 585 | 67 | 74 | 55 | 53 | 1986 | 4060 | 506..=511 | 524 | 1674 | 4500 | 4805 | 4807
                            )))
                {
                    let rep = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
                        ctx, &sys, char_id, ship_id, module_id, fit,
                    ).await;
                    return Some(Ok(rep));
                }
                if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
                    return Some(Err(BridgeRpcError { code: "ModuleReactivationDelay".into(), message: format!("Module {module_id} cannot reactivate before delay expires") }));
                }
                if method == "Activate"
                    && already_active
                    && matches!(
                        mod_type.group_id,
                        40 | 1156 | 41 | 1697 | 67 | 71 | 76 | 1122 | 650 | 80 | 353
                    )
                {
                    return Some(Err(BridgeRpcError {
                        code: "ModuleAlreadyActive".into(),
                        message: format!("Module {module_id} is already active in a cycle"),
                    }));
                }
                if matches!(mod_type.group_id, 1122 | 650) && target_id_opt.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "TargetRequired".into(),
                        message: "Salvager / Tractor Beam requires a locked target".into(),
                    }));
                }
                if let Some(target_id) = target_id_opt {
                    if target_id == ship_id && matches!(mod_type.group_id, 41 | 325 | 2018 | 585 | 67 | 1697 | 1698 | 52 | 65 | 68 | 71 | 201 | 208 | 291 | 379 | 1672) {
                        return Some(Err(BridgeRpcError {
                            code: "CannotActivateOnSelf".into(),
                            message: "Cannot activate remote assistance or offensive module on self".into(),
                        }));
                    }
                    if mod_type.group_id == 82 {
                        // Passive Targeter (`group_id == 82`, `feat:modules.passive-targeter`):
                        // locks target silently without sending `"OnTarget"` `"targetedBy"` to target pilot!
                        let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
                        let lock = eve_destiny::TargetLock {
                            target_id,
                            lock_time_ms: 0.0,
                            remaining_lock_ms: 0.0,
                            locked: true,
                            max_range_m: Some(max_range),
                        };
                        let _ = sys.add_target_lock(ship_id, lock).await;
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnTarget",
                            "charid",
                            vec![PyRep::Integer(target_id), PyRep::String("locked".into())],
                        );
                    } else if mod_type.group_id != 842 {
                        let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                        if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
                            return Some(Err(BridgeRpcError { code: "TargetNotLocked".into(), message: format!("Target {target_id} is not locked") }));
                        }
                    }
                    if matches!(mod_type.group_id, 52 | 65 | 68 | 71 | 201 | 208 | 291 | 379 | 1672)
                        && ctx.db.with_snapshot(|s| crate::services::structure_acl_defense_rules::is_structure_immune_to_ewar(s, target_id))
                    {
                        return Some(Ok(PyRep::keyval([
                            ("moduleID", PyRep::Integer(module_id)),
                            ("targetID", PyRep::Integer(target_id)),
                            ("ewarAssistRecorded", PyRep::Bool(false)),
                            ("immune", PyRep::Bool(true)),
                            ("damageDealt", PyRep::Real(0.0)),
                        ])));
                    }
                }
                let is_overloaded = method == "Overload" || m.state == eve_dogma::ModuleState::Overloaded;
                let mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);

                let ship_group_id = ctx
                    .sde
                    .types
                    .get(&fit.ship.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);

                let is_mwd = mod_type.group_id == 46
                    && (mod_type.default_attributes.get(&554).copied().unwrap_or(0.0) > 0.0
                        || mod_type.name.contains("Microwarpdrive"));
                let is_mjd = mod_type.group_id == 1189
                    || mod_type.group_id == 4769
                    || mod_type.name.contains("Micro Jump Drive");
                let is_mwd_or_mjd = is_mwd || is_mjd;

                if is_mwd_or_mjd {
                    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                        if cs.is_mwd_mjd_disabled || (is_mjd && cs.is_warp_scrambled()) {
                            return Some(Err(BridgeRpcError { code: "WarpScrambled".into(), message: "Cannot activate Microwarpdrive or Micro Jump Drive while warp scrambled".into() }));
                        }
                    }
                }


                let EwarCapitalPrep {
                    warp_scramble_points,
                    disables_target_mwd_mjd,
                    warp_core_stab_bonus,
                    hic_bubble_radius_m,
                    hic_velocity_multiplier,
                    hic_sig_multiplier,
                    hic_mass_addition,
                    mjd_jump_distance_m,
                    grants_interdiction_nullification,
                    target_velocity_multiplier,
                    immobilize_self,
                    self_disallow_assistance,
                    self_ewar_immune,
                    self_disable_mwd_mjd,
                    self_mass_multiplier,
                    is_adc,
                    remote_shield_amount,
                    remote_armor_amount,
                    remote_hull_amount,
                    remote_cap_amount,
                    remote_optimal,
                    remote_falloff,
                    mutadaptive_spool_step,
                    mutadaptive_spool_max,
                } = match prepare_ewar_and_capital_module(
                    ctx,
                    &sys, sys_id,
                    char_id,
                    ship_id,
                    ship_group_id,
                    module_id,
                    &m,
                    mod_type,
                    &mod_state,
                    &fit,
                    target_id_opt,
                    is_overloaded,
                    is_mwd,
                    is_mjd,
                    kwargs,
                )
                .await
                {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => return Some(Err(e)),
                    None => unreachable!(),
                };
            let duration_ms = {
                let d = mod_state.get_attribute(attr_ids::DURATION, &ctx.sde);
                if d > 0.0 {
                    d
                } else {
                    let d_high = mod_state.get_attribute(3115, &ctx.sde);
                    if d_high > 0.0 {
                        d_high
                    } else {
                        mod_state.get_attribute(attr_ids::SPEED, &ctx.sde)
                    }
                }
            };
            let duration_s = if duration_ms > 0.0 { duration_ms / 1000.0 } else { 1.0 };
            let mut cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
            let speed_factor = mod_state.get_attribute(attr_ids::SPEED_FACTOR, &ctx.sde);
            let speed_boost_factor = if mod_type.group_id != 330 && speed_factor > 0.0 { speed_factor / 100.0 } else { 0.0 };

            let raw_shield = mod_state.get_attribute(68, &ctx.sde);
            let raw_armor = mod_state.get_attribute(84, &ctx.sde);
            let raw_hull = mod_state.get_attribute(83, &ctx.sde);
            let overload_mult = crate::services::fitting_cycles_overheat::compute_overload_multiplier(
                is_overloaded, mod_type, &ctx.sde,
            );
            let shield_boost_amount = if matches!(mod_type.group_id, 41 | 1697) { 0.0 } else { raw_shield * overload_mult };
            let initial_armor_repair = if matches!(mod_type.group_id, 325 | 2018 | 1698) { 0.0 } else { raw_armor * overload_mult };
            let hull_repair_amount = if mod_type.group_id == 585 { 0.0 } else { raw_hull * overload_mult };
            let mut remote_armor_amount = if mod_type.group_id == 2018 && remote_armor_amount <= 0.0 {
                mod_type.default_attributes.get(&84).copied().unwrap_or(0.0)
            } else {
                remote_armor_amount
            };
            let mutadaptive_spool_step = if mod_type.group_id == 2018 && mutadaptive_spool_step <= 0.0 {
                mod_type.default_attributes.get(&2796).copied().unwrap_or(0.0)
            } else {
                mutadaptive_spool_step
            };
            let mutadaptive_spool_max = if mod_type.group_id == 2018 && mutadaptive_spool_max <= 0.0 {
                mod_type.default_attributes.get(&2797).copied().unwrap_or(0.0)
            } else {
                mutadaptive_spool_max
            };

            // Ancillary Shield Booster (`group_id == 1156 | 1697`) & Ancillary Armor Repairer (`group_id == 1199 | 1698`)
            let crate::services::fitting_cycles_overheat::AncillaryRepairPrep {
                is_asb,
                asb_fallback_cap_need,
                cap_need: anc_cap_need,
                is_aar,
                aar_paste_rate,
                aar_base_armor_repair,
                armor_repair_amount,
                remote_armor_amount: anc_remote_armor,
            } = crate::services::fitting_cycles_overheat::prepare_ancillary_module(
                ctx,
                ship_id,
                module_id,
                m.flag_id,
                mod_type,
                &fit,
                is_overloaded,
                cap_need,
                initial_armor_repair,
                remote_armor_amount,
                overload_mult,
            );
            cap_need = anc_cap_need;
            remote_armor_amount = anc_remote_armor;

            // Capacitor Booster (`group_id == 76`, `feat:modules.cap-booster`)
            let mut cap_booster_inject_amount = 0.0;
            if mod_type.group_id == 76 {
                let mut charge_tid_opt: Option<u32> = None;
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some((cid, ctid)) = snap
                        .items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                        })
                        .map(|i| (i.item_id, i.type_id))
                    {
                        charge_tid_opt = Some(ctid);
                        let mut rem = 0;
                        if let Some(ci) = snap.items.get_mut(&cid) {
                            ci.quantity = ci.quantity.saturating_sub(1);
                            rem = ci.quantity;
                        }
                        if rem == 0 {
                            snap.items.remove(&cid);
                        }
                    }
                });
                let Some(charge_tid) = charge_tid_opt else {
                    return Some(Err(BridgeRpcError { code: "NoChargesLoaded".into(), message: "Capacitor Booster has no Cap Booster charges loaded".into() }));
                };
                cap_booster_inject_amount = ctx
                    .sde
                    .types
                    .get(&charge_tid)
                    .and_then(|t| t.default_attributes.get(&67).copied())
                    .unwrap_or(100.0);
                cap_need = 0.0;
            }

            // Reactive Armor Hardener (`group_id == 1150`, `4403`, `feat:modules.reactive-armor-hardener`)
            let is_rah = mod_type.group_id == 1150 || m.type_id == 4403;
            let rah_shift_amount = mod_state.get_attribute(1849, &ctx.sde).max(6.0);

            // Auto-Targeting System (`group_id == 96`, `1182`/`1436`, `feat:modules.auto-targeting-system`)
            let is_auto_targeter = mod_type.group_id == 96;
            let auto_target_max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let auto_tgt_bonus = mod_type.default_attributes.get(&193).copied().unwrap_or(2.0).round() as usize;
            let ship_max_locks = (fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize) + auto_tgt_bonus;
            let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
            let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
            let auto_target_max_locks = eve_dogma::compute_effective_max_locked_targets(ship_max_locks, tm_lvl, atm_lvl) + auto_tgt_bonus;

            let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
            let reactivation_delay_s = if react_delay_ms > 0.0 { react_delay_ms / 1000.0 } else { 0.0 };
            let (duration_s, reactivation_delay_s) = if grants_interdiction_nullification {
                let null_skill_lvl = ctx
                    .character_skill_levels(char_id)
                    .get(&57319)
                    .copied()
                    .unwrap_or(0);
                crate::services::tactical_modules::compute_nullifier_duration_and_cooldown(
                    duration_s,
                    reactivation_delay_s,
                    null_skill_lvl,
                )
            } else {
                (duration_s, reactivation_delay_s)
            };

            let rack_idx = match m.flag_id {
                27..=34 => 0,
                19..=26 => 1,
                _ => 2,
            };
            let heat_damage_per_cycle = {
                let hd = mod_state.get_attribute(1211, &ctx.sde);
                if hd > 0.0 { hd } else { 3.4 }
            };

            let linked_turrets: Vec<i64> = ctx.db.with_snapshot(|snap| {
                if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
                    bank.clone()
                } else if matches!(m.flag_id, 27..=34) {
                    vec![module_id]
                } else {
                    Vec::new()
                }
            });

            let mut active_burst_charge_tid = m.charge_type_id;
            let mut crystal_volatility_chance = 0.0;
            let mut crystal_volatility_damage = 0.0;
            if !linked_turrets.is_empty() && mod_type.group_id != 899 {
                ctx.db.with_mut_snapshot(|snap| {
                    for &tid in &linked_turrets {
                        let tflag = snap.items.get(&tid).map(|i| i.flag);
                        if let Some(tf) = tflag {
                            let mut remove_cid: Option<i64> = None;
                            if let Some(charge_item) = snap.items.values_mut().find(|i| {
                                i.location_id == ship_id
                                    && i.flag == tf
                                    && i.item_id != tid
                                    && i.quantity > 0
                                    && ctx.sde.types.get(&i.type_id).is_none_or(|t| t.group_id != 908)
                            }) {
                                active_burst_charge_tid = Some(charge_item.type_id);
                                let c_sde = ctx.sde.types.get(&charge_item.type_id);
                                let vol_dmg = c_sde
                                    .and_then(|t| t.default_attributes.get(&784).copied())
                                    .unwrap_or(0.0);
                                let vol_chance = c_sde
                                    .and_then(|t| t.default_attributes.get(&783).copied())
                                    .unwrap_or(100.0);
                                if vol_dmg > 0.0 {
                                    crystal_volatility_damage = vol_dmg;
                                    crystal_volatility_chance = if vol_chance > 1.0 {
                                        (vol_chance / 100.0).clamp(0.0, 1.0)
                                    } else {
                                        vol_chance.clamp(0.0, 1.0)
                                    };
                                } else {
                                    charge_item.quantity = charge_item.quantity.saturating_sub(1);
                                    if charge_item.quantity == 0 {
                                        remove_cid = Some(charge_item.item_id);
                                    }
                                }
                            } else if mod_type.group_id == 1770 && active_burst_charge_tid.is_none() {
                                if let Some(cargo_charge) = snap.items.values_mut().find(|i| {
                                    i.location_id == ship_id
                                        && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
                                        && i.quantity > 0
                                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 1771)
                                }) {
                                    active_burst_charge_tid = Some(cargo_charge.type_id);
                                    cargo_charge.quantity = cargo_charge.quantity.saturating_sub(1);
                                    if cargo_charge.quantity == 0 {
                                        remove_cid = Some(cargo_charge.item_id);
                                    }
                                }
                            }
                            if let Some(rcid) = remove_cid {
                                snap.items.remove(&rcid);
                                snap.module_active_charges.remove(&tid);
                            }
                        }
                    }
                });
            }

            let raw_cap_drain = mod_state.get_attribute(97, &ctx.sde);
            let raw_cap_transfer = mod_state.get_attribute(90, &ctx.sde);
            let (cap_drain_target, cap_transfer_target, neut_optimal, neut_falloff) =
                if (raw_cap_drain > 0.0 || raw_cap_transfer > 0.0) && target_id_opt.is_some() {
                    let tid = target_id_opt.unwrap();
                    let opt_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
                    let ewar_resist = ctx
                        .db
                        .with_snapshot(|snap| {
                            snap.characters
                                .values()
                                .find(|c| c.active_ship_id == tid)
                                .map(|c| c.character_id)
                                .or_else(|| {
                                    snap.items
                                        .get(&tid)
                                        .map(|i| i.owner_id)
                                        .filter(|oid| snap.characters.contains_key(oid))
                                })
                        })
                        .and_then(|cid| ctx.get_or_rebuild_ship_fitting(cid, tid).ok())
                        .map(|t_fit| t_fit.ship.get_attribute(2045, &ctx.sde).clamp(0.0, 1.0))
                        .unwrap_or(1.0);
                    (
                        raw_cap_drain * ewar_resist,
                        raw_cap_transfer * ewar_resist,
                        opt_range,
                        falloff,
                    )
                } else {
                    (raw_cap_drain, raw_cap_transfer, 0.0, 0.0)
                };
            let nosferatu_blood_raider_override = fit.ship.get_attribute(1945, &ctx.sde) > 0.0
                || eve_dogma::is_blood_raider_nosferatu_hull(fit.ship.type_id, &ctx.sde);
            let is_cehe = matches!(m.type_id, 40714..=40718) || mod_type.effect_ids.contains(&6484);
            let ehe_hull_resists = if is_cehe {
                let em = mod_type.default_attributes.get(&974).copied().unwrap_or(0.05);
                let exp = mod_type.default_attributes.get(&975).copied().unwrap_or(0.05);
                let kin = mod_type.default_attributes.get(&976).copied().unwrap_or(0.05);
                let therm = mod_type.default_attributes.get(&977).copied().unwrap_or(0.05);
                Some([em, exp, kin, therm])
            } else {
                None
            };
            let repeat = if is_mjd
                || grants_interdiction_nullification
                || is_adc
                || is_cehe
            {
                0
            } else {
                repeat
            };
            let effective_max_capacitor = if mod_type.default_attributes.contains_key(&147) {
                Some(fit.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, &ctx.sde))
            } else {
                None
            };

            // Burst Jammer / ECM Burst (`group_id == 80 || group_id == 353`, e.g. `580` Burst Jammer I, `2117` Burst Jammer II)
            // & Smartbomb (`group_id == 72`) Crimewatch safety check: require Safety Red in all empire space (`sys_sec > 0.0`)
            if matches!(mod_type.group_id, 72 | 80 | 353) {
                let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| rt.modified_system_securities.get(&sys_id).copied())
                    .unwrap_or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0));
                let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.safety_levels.get(&char_id).copied().unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red)
                });
                if sys_sec > 0.0 && safety != crate::services::combat_crimewatch::SafetyLevel::Red {
                    return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                }
            }
            if mod_type.group_id == 1122 {
                if let Err(e) = crate::combat::drones_salvage_tractor::validate_wreck_salvage_target(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    target_id_opt.unwrap_or(0),
                    Some(mod_type.type_id),
                )
                .await
                {
                    return Some(Err(e));
                }
            }

            let cycle = crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id,
                module_type_id: m.type_id,
                owner_char_id: Some(char_id),
                effect_name: effect_name.clone(),
                target_id: target_id_opt,
                repeat,
                duration_s,
                elapsed_s: 0.0,
                cap_need_per_cycle: cap_need,
                speed_boost_factor,
                original_max_velocity: None,
                linked_turrets: linked_turrets.clone(),
                ammo_type_id: active_burst_charge_tid,
                damage_vector: if matches!(
                    mod_type.group_id,
                    53 | 55 | 74 | 506 | 507 | 508 | 509 | 510 | 511 | 512 | 524 | 771 | 862 | 1245 | 1986
                ) {
                    let dmg_mult = mod_state.get_attribute(64, &ctx.sde).max(1.0)
                        * (linked_turrets.len().max(1) as f64);
                    let (em, therm, kin, exp) = if let Some(ct) =
                        active_burst_charge_tid.and_then(|cid| ctx.sde.types.get(&cid))
                    {
                        (
                            ct.default_attributes.get(&114).copied().unwrap_or(0.0) * dmg_mult,
                            ct.default_attributes.get(&118).copied().unwrap_or(0.0) * dmg_mult,
                            ct.default_attributes.get(&117).copied().unwrap_or(0.0) * dmg_mult,
                            ct.default_attributes.get(&116).copied().unwrap_or(0.0) * dmg_mult,
                        )
                    } else {
                        (
                            mod_state.get_attribute(114, &ctx.sde) * dmg_mult,
                            mod_state.get_attribute(118, &ctx.sde) * dmg_mult,
                            mod_state.get_attribute(117, &ctx.sde) * dmg_mult,
                            mod_state.get_attribute(116, &ctx.sde) * dmg_mult,
                        )
                    };
                    if em + therm + kin + exp > 0.0 {
                        eve_destiny::DamageVector::new(em, therm, kin, exp)
                    } else {
                        eve_destiny::DamageVector::new(20.0, 20.0, 20.0, 20.0)
                    }
                } else {
                    eve_destiny::DamageVector::new(0.0, 0.0, 0.0, 0.0)
                },
                cap_drain_target,
                cap_transfer_target,
                nosferatu_blood_raider_override,
                shield_boost_amount,
                armor_repair_amount,
                hull_repair_amount,
                remote_shield_amount,
                remote_armor_amount,
                remote_hull_amount,
                remote_cap_amount,
                remote_optimal: if remote_optimal > 0.0 { remote_optimal } else { neut_optimal },
                remote_falloff: if remote_falloff > 0.0 { remote_falloff } else { neut_falloff },
                mutadaptive_spool_step,
                mutadaptive_spool_max,
                current_spool_cycles: 0,
                cap_booster_inject_amount,
                is_asb,
                asb_fallback_cap_need,
                is_aar,
                aar_paste_rate,
                aar_base_armor_repair,
                is_rah,
                rah_shift_amount,
                is_adc,
                adc_saved_resists: None,
                ehe_hull_resists,
                is_overloaded,
                rack_idx,
                heat_damage_per_cycle,
                reactivation_delay_s,
                is_mwd_or_mjd,
                warp_scramble_points,
                disables_target_mwd_mjd,
                warp_core_stab_bonus,
                hic_bubble_radius_m,
                hic_velocity_multiplier,
                hic_sig_multiplier,
                hic_mass_addition,
                original_signature_radius: None,
                original_mass: None,
                mjd_jump_distance_m,
                grants_interdiction_nullification,
                effective_max_capacitor,
                target_velocity_multiplier,
                target_original_max_velocity: None,
                immobilize_self,
                self_disallow_assistance,
                self_ewar_immune,
                self_disable_mwd_mjd,
                self_mass_multiplier,
                is_auto_targeter,
                auto_target_max_range,
                auto_target_max_locks,
                crystal_volatility_chance,
                crystal_volatility_damage,
                max_effective_range_m: if target_id_opt.is_some() {
                    let opt_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
                    let eff = if opt_range > 0.0 {
                        opt_range + 3.0 * falloff.max(0.0)
                    } else {
                        fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde)
                    };
                    Some(eff)
                } else {
                    None
                },
            };

            if let Err(e) = sys.add_module_cycle(cycle).await {
                ctx.remove_projected_ewar_by_module(module_id);
                let code = if e.contains("WarpScrambled") {
                    "WarpScrambled"
                } else {
                    "NotEnoughCapacitor"
                };
                return Some(Err(BridgeRpcError {
                    code: code.into(),
                    message: e,
                }));
            }

            if let Err(e) = evaluate_module_cycle_aggression_or_assistance(
                ctx, &sys, sys_id, char_id, ship_id, mod_type.group_id, target_id_opt,
            ).await {
                ctx.remove_projected_ewar_by_module(module_id);
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
                if cap_need > 0.0 {
                    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                        let _ = sys
                            .set_capacitor(ship_id, (cs.capacitor + cap_need).min(cs.max_capacitor))
                            .await;
                    }
                }
                return Some(Err(e));
            }

            let instant_rep_opt = match run_pre_notify_instant_module_effects(
                ctx,
                &sys,
                sys_id,
                char_id,
                ship_id,
                module_id,
                &m,
                mod_type,
                &mod_state,
                is_overloaded,
                target_id_opt,
                duration_s,
                kwargs,
            )
            .await
            {
                Ok(rep_opt) => rep_opt,
                Err(e) => {
                    ctx.remove_projected_ewar_by_module(module_id);
                    let _ = sys.remove_module_cycle(ship_id, module_id).await;
                    if cap_need > 0.0 {
                        if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                            let _ = sys
                                .set_capacitor(ship_id, (cs.capacitor + cap_need).min(cs.max_capacitor))
                                .await;
                        }
                    }
                    return Some(Err(e));
                }
            };
            if matches!(mod_type.group_id, 72 | 80 | 353 | 862) {
                ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).trigger_weapon_timer(char_id, ctx.now_unix());
            }
            ctx.db.with_mut_snapshot(|snap| {
                if snap.items.contains_key(&module_id) {
                    snap.module_active_states.insert(module_id);
                    if let Some(cid) = active_burst_charge_tid {
                        snap.module_active_charges.insert(module_id, cid);
                    }
                }
            });
            let _ = fit.activate_module(module_id, is_overloaded, &ctx.sde);
            let is_cycle_gated = matches!(
                mod_type.group_id,
                72 | 40 | 1156 | 41 | 1697 | 67 | 71 | 76 | 1122 | 650 | 80 | 353
            );
            if is_cycle_gated || (repeat == 0 && reactivation_delay_s > 0.0) {
                let total_delay = if mod_type.group_id == 40 {
                    duration_s.max(reactivation_delay_s)
                } else {
                    duration_s + reactivation_delay_s
                };
                ctx.record_module_reactivation_delay(
                    ship_id,
                    module_id,
                    total_delay,
                );
            }
            if is_overloaded {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnHeatAdded",
                    "charid",
                    vec![PyRep::Integer(module_id)],
                );
            }
            if let Some(cid) = active_burst_charge_tid {
                if let Some(fm) = fit.modules.get_mut(&module_id) {
                    fm.charge_type_id = Some(cid);
                    fm.charge_quantity = fm.charge_quantity.max(1);
                }
            }
            ctx.store_ship_fitting(ship_id, fit.clone());

            // V24 client derives `OnModuleActivated` from `OnGodmaShipEffect` (active=1).
            let _ = ctx.notify_module_effect_started(char_id, ship_id, module_id, m.type_id, Some(effect_name.as_str()), target_id_opt);
            emit_committed_ewar_and_system_wide_notifications(
                ctx, sys_id, char_id, ship_id, module_id, m.type_id, mod_type, m.charge_type_id,
                disables_target_mwd_mjd, hic_bubble_radius_m, target_id_opt,
            );

            if let Some(rep) = instant_rep_opt {
                return Some(Ok(rep));
            }

            if let Some(res) = build_specialized_module_result(
                method,
                &fit,
                ctx,
                module_id,
                mod_type,
                hic_bubble_radius_m,
                warp_scramble_points,
                active_burst_charge_tid,
            ) {
                Some(Ok(res))
            } else if mod_type.group_id == 49 || mod_type.default_attributes.contains_key(&197) {
                let rep = handle_survey_scanner_activation(
                    ctx,
                    &sys,
                    sys_id,
                    char_id,
                    ship_id,
                    module_id,
                    mod_type,
                    &mod_state,
                )
                .await;
                Some(Ok(rep))
            } else {
                let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)), ("active", PyRep::Bool(true)),
                    ("overloaded", PyRep::Bool(false)), ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                    ("duration", PyRep::Real(duration_s)),
                ])))
            }
            }
        }
        _ => None,
    }
}
