//! `combat_weapons` — Turret Families, Hit Chance, Wrecking Shots, Crystal Volatility, Entropic Spooling, Vorton Arcs & Post-Kill Pipeline (`C-1`, `C-3`).

#![forbid(unsafe_code)]

use super::*;

pub async fn handle_weapons(
    ctx: &ServerContext,
    session: &mut SessionState,
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
            crate::services::space_destiny::break_location_change_invulnerability(ctx, char_id);
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate combat modules while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let _effect_name = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("targetAttack")
                .to_string();
            let target_id_opt = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()));

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((attacker_tick, attacker_ball)) = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Active ship {ship_id} is not spawned in space"),
                }));
            };

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if !fit.modules.contains_key(&module_id) {
                ctx.invalidate_ship_fitting(ship_id);
                if let Ok(rebuilt) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                    fit = rebuilt;
                }
            }
            let m = if let Some(found) = fit.modules.get(&module_id).cloned() {
                found
            } else if let Some(item_rec) = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&module_id)
                    .filter(|i| i.location_id == ship_id)
                    .cloned()
            }) {
                let charge_tid = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && i.flag == item_rec.flag
                                && i.item_id != module_id
                                && i.quantity > 0
                        })
                        .map(|i| i.type_id)
                });
                let fm = eve_dogma::FittedModule {
                    item_id: item_rec.item_id,
                    type_id: item_rec.type_id,
                    flag_id: item_rec.flag as u16,
                    state: eve_dogma::ModuleState::Online,
                    charge_type_id: charge_tid,
                    charge_quantity: 1,
                    damage_hp: 0.0,
                    max_hp: 40.0,
                    dynamic_attribute_overrides: std::collections::HashMap::new(),
                };
                fit.modules.insert(module_id, fm.clone());
                fm
            } else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module {module_id} not found on ship {ship_id}"),
                }));
            };
            if m.state == eve_dogma::ModuleState::Offline {
                return Some(Err(BridgeRpcError {
                    code: "ModuleOffline".into(),
                    message: format!("Module {module_id} is offline"),
                }));
            }
            if let Err(e) =
                crate::services::inventory_fitting::check_module_reloading(ctx, ship_id, module_id)
            {
                return Some(Err(e));
            }
            let Some(mod_type) = ctx.sde.types.get(&m.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module type {} not found in SDE", m.type_id),
                }));
            };

            let is_overloaded = method == "Overload";
            if matches!(
                mod_type.group_id,
                52 | 65 | 68 | 71 | 72 | 201 | 208 | 291 | 379 | 862 | 74 | 55 | 53 | 1986 | 4060 | 506..=511 | 524 | 1674 | 4500 | 4805 | 4807
            ) && sys_handle.get_combat_state(ship_id).await.ok().flatten().is_some_and(|cs| cs.is_tethered)
            {
                return Some(Err(BridgeRpcError {
                    code: "CantActivateWhileTethered".into(),
                    message: "Cannot activate offensive modules, smartbombs, or bomb launchers while tethered".into(),
                }));
            }
            let mod_state =
                fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
            let now_unix = ctx.now_unix();
            let sys_sec = with_combat_runtime(ctx, |rt| {
                rt.modified_system_securities.get(&sys_id).copied()
            })
            .unwrap_or_else(|| {
                ctx.sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0)
            });

            // 1. EWAR / Tackle 0-damage modules (`feat:combat.killmail-assist-timeout-ewar`):
            // Warp Disruptor (52), Stasis Webifier (65),
            // ECM (201), Sensor Dampener (208), Weapon Disruptor (291), Target Painter (379)
            if matches!(mod_type.group_id, 52 | 65 | 201 | 208 | 291 | 379) {
                let Some(tid) = target_id_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "TargetRequired".into(),
                        message: "Targeted EWAR/tackle module requires a locked target".into(),
                    }));
                };
                let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
                if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                    return Some(Err(BridgeRpcError {
                        code: "TargetNotLocked".into(),
                        message: format!("Target {tid} is not locked"),
                    }));
                }
                if ctx.db.with_snapshot(|snap| crate::services::structure_acl_defense_rules::is_structure_immune_to_ewar(snap, tid)) {
                    return Some(Ok(PyRep::keyval([
                        ("moduleID", PyRep::Integer(module_id)),
                        ("targetID", PyRep::Integer(tid)),
                        ("ewarAssistRecorded", PyRep::Bool(false)),
                        ("immune", PyRep::Bool(true)),
                        ("damageDealt", PyRep::Real(0.0)),
                    ])));
                }
                let victim_char_opt = ctx.db.with_snapshot(|snap| {
                    if snap.characters.contains_key(&tid) {
                        Some(tid)
                    } else if let Some(ch) = snap.characters.values().find(|c| c.active_ship_id == tid) {
                        Some(ch.character_id)
                    } else if let Some(it) = snap.items.get(&tid) {
                        snap.characters.get(&it.owner_id).map(|c| c.character_id).or_else(|| {
                            snap.corporations.get(&it.owner_id).and_then(|corp| snap.characters.get(&corp.ceo_id)).map(|c| c.character_id)
                        })
                    } else {
                        None
                    }
                });
                if let Some(vc) = victim_char_opt {
                    if crate::services::space_destiny::is_location_change_invulnerable(ctx, vc, now_unix) {
                        return Some(Err(BridgeRpcError {
                            code: "TargetInvulnerable".into(),
                            message: format!("Target character {vc} is invulnerable due to recent location change"),
                        }));
                    }
                    if vc != char_id {
                        let is_pod_target = sys_handle
                            .get_ball_snapshot(tid)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|(_, b)| b.type_id == 670)
                            || ctx.db.with_snapshot(|s| s.items.get(&tid).is_some_and(|i| i.type_id == 670));
                        let v_suspect_or_crim = {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            let st = cw.get_or_create(vc);
                            st.is_suspect(now_unix) || st.is_criminal(now_unix)
                        };
                        let is_legal = ctx.db.with_snapshot(|snap| {
                            is_legal_war_or_suspect_target(snap, char_id, vc, now_unix)
                        }) || v_suspect_or_crim
                            || with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(vc, char_id, now_unix));
                        let is_mobile_struct = ctx.db.with_snapshot(|s| {
                            s.items.get(&tid).is_some_and(|it| crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, it.type_id))
                        });
                        if !is_legal {
                            let safety = with_combat_runtime(ctx, |rt| {
                                rt.safety_levels.get(&char_id).copied().unwrap_or(SafetyLevel::Red)
                            });
                            let concord_suppressed = !is_pod_target
                                && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
                            let would_be_criminal = !is_mobile_struct
                                && ((sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod_target));
                            let would_be_suspect = !would_be_criminal && sys_sec > 0.0;
                            if safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect) {
                                return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                            }
                            if safety == SafetyLevel::Yellow && would_be_criminal {
                                return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                            }
                        }
                        if v_suspect_or_crim || with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(char_id, vc, now_unix)) {
                            with_mut_combat_runtime(ctx, |rt| {
                                rt.add_limited_engagement(char_id, vc, now_unix);
                                rt.add_limited_engagement(vc, char_id, now_unix);
                            });
                            let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
                            ctx.notify(NotificationTarget::Character(char_id), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(vc), PyRep::Integer(timeout_ft)]);
                            ctx.notify(NotificationTarget::Character(vc), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(char_id), PyRep::Integer(timeout_ft)]);
                        }
                        let is_new_agg = ctx.crimewatch.read().map(|cw| cw.get_aggression(char_id, vc, now_unix).is_none()).unwrap_or(true);
                        let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
                        {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            cw.system_corruption_stages.insert(sys_id, c_stage);
                            let effective_sec = if is_mobile_struct { sys_sec.min(0.40) } else { sys_sec };
                            let _ = cw.handle_combat_aggression_with_pod(char_id, vc, ship_id, sys_id, effective_sec, is_legal, is_pod_target, now_unix);
                            if !is_legal && sys_sec > 0.0 && sys_sec < 0.45 && is_pod_target {
                                let st = cw.get_or_create(char_id);
                                st.criminal_flag_until_unix = now_unix.saturating_add(900);
                                st.suspect_flag_until_unix = 0;
                            }
                        }
                        if !is_legal && sys_sec > 0.0 && !is_mobile_struct && is_new_agg {
                            let old_sec = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
                            let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, is_pod_target, sys_sec);
                            ctx.db.with_mut_snapshot(|s| {
                                if let Some(ch) = s.characters.get_mut(&char_id) {
                                    ch.security_status = (old_sec - loss).clamp(-10.0, 10.0);
                                }
                            });
                        }
                    }
                }
                record_killmail_assist_action(
                    ctx,
                    tid,
                    char_id,
                    attacker_ball.type_id,
                    m.type_id,
                    0.0,
                    true,
                    now_unix,
                );
                if let Some(vcid) = victim_char_opt {
                    record_killmail_assist_action(
                        ctx,
                        vcid,
                        char_id,
                        attacker_ball.type_id,
                        m.type_id,
                        0.0,
                        true,
                        now_unix,
                    );
                }
                // If Target Painter (379), also register projected EWAR on target ship so signature radius blooms
                if mod_type.group_id == 379 {
                    let dist = if let Ok(Some((_, tb))) = sys_handle.get_ball_snapshot(tid).await {
                        attacker_ball.position.distance(tb.position)
                    } else {
                        5_000.0
                    };
                    ctx.upsert_projected_ewar_on_ship(
                        tid,
                        eve_dogma::ProjectedEwarEffect {
                            source_module_id: module_id,
                            source_module_type_id: m.type_id,
                            charge_type_id: m.charge_type_id,
                            distance: dist,
                            overloaded: is_overloaded,
                            effective_module: mod_state.clone(),
                        },
                    );
                    if let Some(vcid) = victim_char_opt {
                        let _ = evaluate_effective_signature_radius(ctx, vcid, tid, sys_id).await;
                    } else if let Ok(Some(mut t_cs)) = sys_handle.get_combat_state(tid).await {
                        let unpainted_base = t_cs.base_signature_radius.unwrap_or(t_cs.signature_radius);
                        t_cs.signature_radius =
                            ctx.effective_target_signature_radius(tid, unpainted_base);
                        let _ = sys_handle.set_combat_state(tid, t_cs).await;
                    }
                }
                if let Some(vcid) = victim_char_opt {
                    ctx.crimewatch
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .trigger_weapon_timer(char_id, now_unix);
                    crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                        ctx, char_id, vcid, now_unix,
                    );
                } else {
                    crate::sp6_lifecycle::record_npc_aggression_timer(ctx, char_id, now_unix);
                }
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("targetID", PyRep::Integer(tid)),
                    ("ewarAssistRecorded", PyRep::Bool(true)),
                    ("damageDealt", PyRep::Real(0.0)),
                    ("capacitorDrained", PyRep::Real(0.0)),
                ])));
            }


            // 1b. Salvager (`group_id == 1122`) and Tractor Beam (`group_id == 650`) (`C-7`)
            if matches!(mod_type.group_id, 1122 | 650) {
                return activate_salvager_or_tractor_module(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    module_id,
                    &m,
                    mod_type,
                    &attacker_ball,
                    target_id_opt,
                    kwargs,
                )
                .await;
            }

            // 2-4. Smartbomb (`72`), Bomb Launcher (`862`), and Defender Launcher (`512`) (`C-2`)
            if let Some(aoe_res) = activate_aoe_or_defender_module(
                ctx,
                char_id,
                ship_id,
                sys_id,
                sys_sec,
                now_unix,
                module_id,
                &m,
                mod_type,
                &attacker_ball,
                target_id_opt,
                kwargs,
            )
            .await
            {
                return Some(aoe_res);
            }
            // 5. Turrets (Hybrid 74, Projectile 55, Energy Laser 53, Entropic Disintegrator 1986, Vorton Projector 4060)
            //    and Missile Launchers (506..=511, 524, 1674, 4500, 4805, 4807)
            if !matches!(
                mod_type.group_id,
                74 | 55 | 53 | 1986 | 4060 | 506..=511 | 524 | 1674 | 4500 | 4805 | 4807
            ) {
                return None;
            }
            let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .find(|i| {
                        i.location_id == ship_id
                            && (i.flag as u16) == m.flag_id
                            && i.item_id != module_id
                            && i.quantity > 0
                            && ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .is_some_and(|ct| ServerContext::is_charge_compatible_with_weapon(mod_type, ct))
                    })
                    .map(|i| (i.item_id, i.type_id))
            });
            let Some((charge_item_id, charge_type_id)) = loaded_charge_opt else {
                return Some(Err(BridgeRpcError {
                    code: "NoAmmoLoaded".into(),
                    message: format!("Weapon module {module_id} has no compatible charge loaded"),
                }));
            };
            if let Some(fm) = fit.modules.get_mut(&module_id) {
                if fm.charge_type_id != Some(charge_type_id) {
                    fm.charge_type_id = Some(charge_type_id);
                    fm.charge_quantity = fm.charge_quantity.max(1);
                }
            }
            let mod_state =
                fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
            let charge_type = &ctx.sde.types[&charge_type_id];
            let is_fof_missile = matches!(charge_type.group_id, 394..=396);
            let is_entropic = mod_type.group_id == 1986;

            // Resolve target ball ID (FoF auto-targeting missiles can fire without a lock even while jammed!)
            let resolved_target_id = if let Some(tid) = target_id_opt {
                if !is_fof_missile {
                    let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
                    if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.module_active_states.remove(&module_id);
                            snap.module_active_charges.remove(&module_id);
                        });
                        let _ = fit.deactivate_module(module_id, &ctx.sde);
                        ctx.store_ship_fitting(ship_id, fit);
                        combat_weapons_spool::reset_entropic_spool(ctx, ship_id, module_id);
                        return Some(Err(BridgeRpcError {
                            code: "TargetNotLocked".into(),
                            message: format!("Target {tid} is not locked"),
                        }));
                    }
                }
                tid
            } else if is_fof_missile {
                let all_balls = sys_handle.list_balls().await.unwrap_or_default();
                let nearest = all_balls
                    .into_iter()
                    .filter(|b| b.id != ship_id && (b.id < 700_000_000 || (b.id >= 1_895_000_000 && b.id < 1_900_000_000)))
                    .min_by(|a, b| {
                        attacker_ball
                            .position
                            .distance(a.position)
                            .partial_cmp(&attacker_ball.position.distance(b.position))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|b| b.id);
                let Some(ntid) = nearest else {
                    return Some(Err(BridgeRpcError {
                        code: "NoValidAutoTarget".into(),
                        message: "No hostile entity within auto-targeting missile range".into(),
                    }));
                };
                ntid
            } else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Weapon activation requires a locked target".into(),
                }));
            };

            let player_target_info = ctx.db.with_snapshot(|snap| {
                if snap.structures.contains_key(&resolved_target_id) {
                    None
                } else if let Some(ch) = snap.characters.get(&resolved_target_id) {
                    Some((ch.character_id, ch.active_ship_id))
                } else if let Some(ch) = snap
                    .characters
                    .values()
                    .find(|c| c.active_ship_id == resolved_target_id)
                {
                    Some((ch.character_id, ch.active_ship_id))
                } else if let Some(it) = snap.items.get(&resolved_target_id) {
                    snap.characters
                        .get(&it.owner_id)
                        .map(|c| (c.character_id, resolved_target_id))
                        .or_else(|| {
                            snap.corporations
                                .get(&it.owner_id)
                                .and_then(|corp| snap.characters.get(&corp.ceo_id))
                                .map(|c| (c.character_id, resolved_target_id))
                        })
                } else {
                    None
                }
            });
            let target_ball_id = player_target_info
                .map(|(_, sid)| sid)
                .unwrap_or(resolved_target_id);
            let Some((_, target_ball)) = sys_handle
                .get_ball_snapshot(target_ball_id)
                .await
                .ok()
                .flatten()
            else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: format!("Target ball {target_ball_id} is not in space"),
                }));
            };

            // Safety setting gate (`feat:crimewatch.safety-settings`)
            if let Some((victim_char_id, _)) = player_target_info {
                if crate::services::space_destiny::is_location_change_invulnerable(ctx, victim_char_id, now_unix) {
                    return Some(Err(BridgeRpcError {
                        code: "TargetInvulnerable".into(),
                        message: format!("Target character {victim_char_id} is invulnerable due to recent location change"),
                    }));
                }
                if victim_char_id != char_id {
                    let is_legal = ctx.db.with_snapshot(|snap| {
                        is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                    }) || {
                        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                        let vst = cw.get_or_create(victim_char_id);
                        vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                    } || with_combat_runtime(ctx, |rt| {
                        rt.has_limited_engagement_with(victim_char_id, char_id, now_unix)
                    });
                    if !is_legal {
                        let safety = with_combat_runtime(ctx, |rt| {
                            rt.safety_levels
                                .get(&char_id)
                                .copied()
                                .unwrap_or(SafetyLevel::Red)
                        });
                        let is_pod_target = target_ball.type_id == 670;
                        let concord_suppressed = !is_pod_target
                            && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
                        let would_be_criminal = (sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod_target);
                        let would_be_suspect = !would_be_criminal && sys_sec > 0.0;
                        if (safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect))
                            || (safety == SafetyLevel::Yellow && would_be_criminal)
                        {
                            return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                        }
                    }
                }
            }

            let is_upwell_target = match crate::services::station_structure::structure_lifecycle_rpc::validate_upwell_structure_attack_precheck(
                ctx, sys_id, char_id, target_ball_id, now_unix,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            // Weapon classification & pre-validation
            let is_missile_launcher = matches!(
                mod_type.group_id,
                506..=511 | 524 | 1674 | 4500 | 4805 | 4807
            );
            let is_breacher_pod = matches!(mod_type.group_id, 4500 | 4805 | 4807);
            let is_vorton = mod_type.group_id == 4060;

            // Turret range validation BEFORE capacitor and ammo deduction (P1-ENTROPIC-PARTIAL-MUTATION-AND-AMMO-DEPLETION):
            // Calling dogmaIM.Activate out of range must fail without draining capacitor or ammo.
            let range_mult = charge_type
                .default_attributes
                .get(&120)
                .copied()
                .unwrap_or(1.0);
            let base_opt = {
                let v = mod_state.get_attribute(54, &ctx.sde);
                if v > 0.0 {
                    v
                } else {
                    mod_type.default_attributes.get(&54).copied().unwrap_or(5_000.0)
                }
            };
            let has_charge_mod = |attr: u16| {
                mod_state.modifiers.iter().any(|m| {
                    m.target_attribute_id == attr
                        && (m.source_item_id == charge_item_id
                            || (m.source_item_id == 0
                                && m.effect_id
                                    .is_some_and(|eid| charge_type.effect_ids.contains(&eid))))
                })
            };
            let optimal_range = if has_charge_mod(54) {
                base_opt
            } else {
                base_opt * range_mult
            };
            let falloff_range = {
                let v = mod_state.get_attribute(158, &ctx.sde);
                if v > 0.0 {
                    v
                } else {
                    mod_type.default_attributes.get(&158).copied().unwrap_or(5_000.0)
                }
            };
            let dist = if is_upwell_target {
                (attacker_ball.position.distance(target_ball.position) - target_ball.radius - attacker_ball.radius).max(0.0)
            } else {
                attacker_ball.position.distance(target_ball.position)
            };

            if !is_missile_launcher && dist > optimal_range + 3.0 * falloff_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetOutOfRange".into(),
                    message: format!(
                        "Target {target_ball_id} is {dist:.1} m away (exceeds range {:.1} m)",
                        optimal_range + 3.0 * falloff_range
                    ),
                }));
            }

            // Entropic Disintegrator pre-validation (P1-ENTROPIC-PARTIAL-MUTATION-AND-AMMO-DEPLETION):
            // Activating an already active entropic module on the same target must fail with AlreadyActive
            // BEFORE deducting capacitor or consuming ammo.
            if is_entropic {
                let is_already_active = {
                    let is_active_in_db = ctx.db.with_snapshot(|snap| {
                        snap.module_active_states.contains(&module_id)
                    });
                    is_active_in_db && with_combat_runtime(ctx, |rt| {
                        rt.entropic_spools
                            .get(&(ship_id, module_id))
                            .is_some_and(|entry| entry.target_id == target_ball_id)
                    })
                };
                if is_already_active {
                    return Some(Err(BridgeRpcError {
                        code: "AlreadyActive".into(),
                        message: format!("Module {module_id} is already active"),
                    }));
                }
            }

            // Capacitor check & deduction:
            // Projectile turrets (`group_id == 55`) and Missile Launchers require 0 capacitor!
            // Hybrid turrets (`74`), Energy Lasers (`53`), Entropic Disintegrators (`1986`), and Vorton Projectors (`4060`) consume capacitor.
            let base_cap_need = if mod_type.group_id == 55
                || matches!(mod_type.group_id, 506..=511 | 524 | 1674 | 4500 | 4805 | 4807)
            {
                0.0
            } else {
                let raw_cap = {
                    let v = mod_state.get_attribute(6, &ctx.sde);
                    if v > 0.0 {
                        v
                    } else {
                        mod_type.default_attributes.get(&6).copied().unwrap_or(0.0)
                    }
                };
                let cap_bonus_pct = charge_type.default_attributes.get(&317).copied().unwrap_or(0.0);
                if has_charge_mod(6) {
                    raw_cap.max(0.0)
                } else {
                    (raw_cap * (1.0 + cap_bonus_pct / 100.0)).max(0.0)
                }
            };
            let mut cap_before = 0.0;
            let mut cap_after = 0.0;
            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
                cap_before = cs.capacitor;
                if base_cap_need > 0.0 {
                    if cs.capacitor + 1e-6 < base_cap_need {
                        return Some(Err(BridgeRpcError {
                            code: "NotEnoughCapacitor".into(),
                            message: format!(
                                "Weapon requires {base_cap_need:.2} GJ capacitor (have {:.2} GJ)",
                                cs.capacitor
                            ),
                        }));
                    }
                    cs.capacitor = (cs.capacitor - base_cap_need).max(0.0);
                    let _ = sys_handle.set_combat_state(ship_id, cs.clone()).await;
                }
                cap_after = cs.capacitor;
            }

            // Charge consumption / Laser frequency crystal volatility (`feat:modules.turret-laser`)
            let mut crystal_damage_after = 0.0;
            let mut crystal_shattered = false;
            if mod_type.group_id == 53 {
                // Energy Laser Turret: uses frequency crystals (`group_id == 86 | 375`).
                // T1 crystals (`attr 786 == 0.0`) do not degrade.
                // T2 (`attr 786 == 1.0`, `783 == 0.1`, `784 == 0.01`) and Faction (`786 == 1.0`, `783 == 1.0`, `784 == 0.00025`) take volatility damage!
                let crystals_get_damaged =
                    charge_type.default_attributes.get(&786).copied().unwrap_or(0.0) > 0.0;
                if crystals_get_damaged {
                    let vol_chance = charge_type
                        .default_attributes
                        .get(&783)
                        .copied()
                        .unwrap_or(0.10);
                    let vol_dmg = charge_type
                        .default_attributes
                        .get(&784)
                        .copied()
                        .unwrap_or(0.01);
                    let crystal_hp = charge_type
                        .default_attributes
                        .get(&9)
                        .copied()
                        .unwrap_or(1.0)
                        .max(0.0001);
                    let vol_roll = if vol_chance >= 1.0 {
                        0.0
                    } else {
                        0.05 // DERIVED deterministic default < 0.10 so T2 volatility is observable per cycle
                    };
                    if vol_roll < vol_chance {
                        ctx.db.with_mut_snapshot(|snap| {
                            if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                                let cur = ci.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
                                let next_dmg = cur + vol_dmg;
                                crystal_damage_after = (next_dmg / crystal_hp).min(1.0);
                                if next_dmg + 1e-9 >= crystal_hp {
                                    crystal_shattered = true;
                                    if ci.quantity > 1 {
                                        ci.quantity -= 1;
                                        ci.dynamic_attributes.insert(3, 0.0);
                                    } else {
                                        snap.items.remove(&charge_item_id);
                                    }
                                } else {
                                    ci.dynamic_attributes.insert(3, next_dmg);
                                }
                            }
                        });
                        ctx.notify(
                            NotificationTarget::Character(char_id),
                            "OnModuleAttributeChanges",
                            "charid",
                            vec![PyRep::Integer(charge_item_id)],
                        );
                    } else {
                        crystal_damage_after = ctx.db.with_snapshot(|snap| {
                            snap.items
                                .get(&charge_item_id)
                                .and_then(|ci| ci.dynamic_attributes.get(&3).copied())
                                .unwrap_or(0.0)
                                / crystal_hp
                        });
                    }
                }
            } else {
                // Consume 1 round of projectile/hybrid/exotic-plasma/vorton/missile/breacher ammo
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        if ci.quantity > 1 {
                            ci.quantity -= 1;
                            ci.version += 1;
                        } else {
                            snap.items.remove(&charge_item_id);
                        }
                    }
                });
            }

            // Weapon damage calculation

            let dmg_mult = {
                let m_mult = mod_state.get_attribute(64, &ctx.sde);
                if m_mult > 0.0 {
                    m_mult
                } else {
                    mod_type.default_attributes.get(&64).copied().unwrap_or(1.0)
                }
            };
            let base_dmg = DamageVector {
                em: charge_type.default_attributes.get(&114).copied().unwrap_or(0.0) * dmg_mult,
                explosive: charge_type.default_attributes.get(&116).copied().unwrap_or(0.0)
                    * dmg_mult,
                kinetic: charge_type.default_attributes.get(&117).copied().unwrap_or(0.0)
                    * dmg_mult,
                thermal: charge_type.default_attributes.get(&118).copied().unwrap_or(0.0)
                    * dmg_mult,
            };

            let raw_sig = if let Some((vcid, _)) = player_target_info {
                evaluate_effective_signature_radius(ctx, vcid, target_ball_id, sys_id).await
            } else if let Ok(Some(t_cs)) = sys_handle.get_combat_state(target_ball_id).await {
                t_cs.base_signature_radius.unwrap_or(if t_cs.signature_radius > 0.0 { t_cs.signature_radius } else { target_ball.radius.max(35.0) })
            } else {
                target_ball.radius.max(35.0)
            };
            let target_sig = ctx.effective_target_signature_radius(target_ball_id, raw_sig);

            // Handle Missile Launcher / Breacher Pod in-flight physics (`feat:modules.missile-launchers`, `feat:combat.missile-flight-physics-destruction`)
            if is_missile_launcher {

                return Some(
                    activate_missile_or_breacher_launcher(
                        ctx,
                        char_id,
                        ship_id,
                        sys_id,
                        now_unix,
                        module_id,
                        &m,
                        &fit,
                        charge_type_id,
                        charge_type,
                        &attacker_ball,
                        target_ball_id,
                        base_dmg,
                        is_fof_missile,
                        is_breacher_pod,
                        target_id_opt.is_some(),
                    )
                    .await,
                );
            }
            // Turret tracking, hit chance, Entropic spooling, and Vorton arc chaining
            let tracking_mult = charge_type
                .default_attributes
                .get(&244)
                .copied()
                .unwrap_or(1.0);
            let base_tracking = {
                let v = mod_state.get_attribute(160, &ctx.sde);
                if v > 0.0 {
                    v
                } else {
                    mod_type.default_attributes.get(&160).copied().unwrap_or(100.0)
                }
            };
            let attacker_pylon_mult = sys_handle
                .get_combat_state(ship_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.tracking_speed_multiplier)
                .filter(|&m| m > 0.0)
                .unwrap_or(1.0);
            let tracking_speed = (if has_charge_mod(160) {
                base_tracking
            } else {
                base_tracking * tracking_mult
            }) * attacker_pylon_mult;
            let turret_sig_res = mod_type
                .default_attributes
                .get(&620)
                .copied()
                .unwrap_or(40_000.0);

            let angular_vel = eve_destiny::combat::compute_angular_velocity(
                attacker_ball.position,
                attacker_ball.velocity,
                target_ball.position,
                target_ball.velocity,
            );
            let p_hit = turret_chance_to_hit_with_res(
                angular_vel,
                tracking_speed,
                turret_sig_res,
                target_sig,
                dist,
                optimal_range,
                falloff_range,
            );
            let hit_roll =
                ctx.next_invention_roll(ship_id as u64, target_ball_id as u64, attacker_tick);
            let (shot_dmg, mut is_wrecking) = turret_shot_damage(base_dmg, p_hit, hit_roll);

            // Entropic Disintegrator consecutive target-specific spooling (`feat:modules.turret-precursor`)
            let (spool_cycles, spool_multiplier) = if is_entropic {
                let duration_s = mod_type.default_attributes.get(&51).map(|d| d / 1000.0).unwrap_or(3.5);
                match combat_weapons_spool::handle_entropic_activation(ctx, char_id, ship_id, sys_id, module_id, target_ball_id, mod_type, Some(charge_item_id), base_dmg, base_cap_need, duration_s) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                }
            } else {
                (0u32, 1.0)
            };

            let cycle_secs = {
                let v = mod_state.get_attribute(51, &ctx.sde);
                if v > 0.0 { v / 1000.0 } else { mod_type.default_attributes.get(&51).copied().unwrap_or(3000.0) / 1000.0 }
            }
            .max(0.1);

            // Vorton Projector arc chaining (`feat:modules.vorton-projector`)
            let mut vorton_secondary_targets = Vec::new();
            let final_dmg = if is_vorton {
                is_wrecking = false;
                let arc_range = mod_type
                    .default_attributes
                    .get(&3036)
                    .copied()
                    .unwrap_or(10_000.0);
                let max_total_targets = mod_type
                    .default_attributes
                    .get(&3037)
                    .or_else(|| mod_type.default_attributes.get(&2104))
                    .copied()
                    .unwrap_or(10.0) as usize;
                let max_secondary = max_total_targets.saturating_sub(1).max(1);
                let exp_vel = mod_type
                    .default_attributes
                    .get(&653)
                    .copied()
                    .unwrap_or(190.0);
                let exp_rad = mod_type
                    .default_attributes
                    .get(&654)
                    .copied()
                    .unwrap_or(43.0);
                let drf = mod_type
                    .default_attributes
                    .get(&1353)
                    .copied()
                    .unwrap_or(0.40);
                let all_balls = sys_handle.list_balls().await.unwrap_or_default();
                let candidates: Vec<(i64, DVec3)> = all_balls
                    .into_iter()
                    .filter(|b| {
                        b.id != ship_id
                            && b.id != target_ball_id
                            && super::combat_missiles_aoe::is_combatant_ship_or_entity_ball(ctx, b.id, b.type_id)
                    })
                    .map(|b| (b.id, b.position))
                    .collect();
                let hits = vorton_arc_targets_with_cap(
                    target_ball_id,
                    target_ball.position,
                    &candidates,
                    arc_range,
                    max_secondary,
                );
                let safety = with_combat_runtime(ctx, |rt| {
                    rt.safety_levels.get(&char_id).copied().unwrap_or(SafetyLevel::Red)
                });
                let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
                let mut had_secondary_pvp_hit = false;
                for arc_id in hits.into_iter().skip(1) {
                    if sys_handle.get_combat_state(arc_id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
                        continue;
                    }
                    if let Ok(Some((_, arc_ball))) = sys_handle.get_ball_snapshot(arc_id).await {
                        let arc_vic_opt = ctx.db.with_snapshot(|snap| {
                            if snap.characters.contains_key(&arc_id) {
                                Some(arc_id)
                            } else {
                                snap.characters
                                    .values()
                                    .find(|c| c.active_ship_id == arc_id)
                                    .map(|c| c.character_id)
                                    .or_else(|| {
                                        snap.items
                                            .get(&arc_id)
                                            .map(|i| i.owner_id)
                                            .filter(|oid| snap.characters.contains_key(oid))
                                    })
                            }
                        });
                        if let Some(vcid) = arc_vic_opt {
                            if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                                continue;
                            }
                            if vcid != char_id {
                                let is_legal = ctx.db.with_snapshot(|snap| {
                                    is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                }) || {
                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                    let vst = cw.get_or_create(vcid);
                                    vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                                } || with_combat_runtime(ctx, |rt| {
                                    rt.has_limited_engagement_with(vcid, char_id, now_unix)
                                });
                                if !is_legal {
                                    let is_pod = arc_ball.type_id == 670;
                                    let concord_suppressed = !is_pod
                                        && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
                                    let would_be_crim = (sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod);
                                    let would_be_susp = !would_be_crim && sys_sec > 0.0;
                                    if (safety == SafetyLevel::Green && (would_be_crim || would_be_susp))
                                        || (safety == SafetyLevel::Yellow && would_be_crim)
                                    {
                                        continue;
                                    }
                                }
                            }
                        }
                        let arc_factor = missile_damage_factor(
                            arc_ball.radius.max(35.0),
                            arc_ball.velocity.length(),
                            exp_rad,
                            exp_vel,
                            drf,
                        );
                        if let Some((arc_app, arc_destroyed, _)) =
                            crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                                ctx, &sys_handle, sys_id, char_id, arc_id, base_dmg.scaled(arc_factor), cycle_secs, now_unix,
                            )
                            .await
                            .ok()
                            .flatten()
                        {
                            vorton_secondary_targets.push(arc_id);
                            record_killmail_assist_action(
                                ctx, arc_id, char_id, attacker_ball.type_id, m.type_id, arc_app, false, now_unix,
                            );
                            if let Some(vcid) = arc_vic_opt.filter(|&v| v != char_id) {
                                record_killmail_assist_action(
                                    ctx, vcid, char_id, attacker_ball.type_id, m.type_id, arc_app, false, now_unix,
                                );
                                let is_legal = ctx.db.with_snapshot(|snap| {
                                    is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                }) || {
                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                    let vst = cw.get_or_create(vcid);
                                    vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                                } || with_combat_runtime(ctx, |rt| {
                                    rt.has_limited_engagement_with(vcid, char_id, now_unix)
                                });
                                let is_arc_pod = arc_ball.type_id == 670;
                                let is_arc_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, arc_ball.type_id);
                                let is_new_agg = ctx.crimewatch.read().map(|cw| cw.get_aggression(char_id, vcid, now_unix).is_none()).unwrap_or(true);
                                {
                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                    cw.system_corruption_stages.insert(sys_id, c_stage);
                                    let eff_sec = if is_arc_mobile { sys_sec.min(0.40) } else { sys_sec };
                                    let _ = cw.handle_combat_aggression_with_pod(
                                        char_id, vcid, ship_id, sys_id, eff_sec, is_legal, is_arc_pod, now_unix,
                                    );
                                    cw.get_or_create(char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
                                    cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
                                }
                                if !is_legal && sys_sec > 0.0 && !is_arc_mobile && is_new_agg {
                                    crate::services::combat_crimewatch::crimewatch_timers::apply_illegal_pvp_sec_status_loss(
                                        ctx, char_id, sys_id, sys_sec, is_arc_pod, now_unix,
                                    );
                                }
                                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                                    ctx, char_id, vcid, now_unix,
                                );
                                had_secondary_pvp_hit = true;
                            }
                            if arc_destroyed {
                                super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                                    ctx, sys_id, char_id, arc_id, arc_ball.type_id, arc_ball.position, now_unix,
                                )
                                .await;
                            }
                        }
                    }
                }
                if had_secondary_pvp_hit {
                    crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, char_id, now_unix);
                }
                let p_factor = missile_damage_factor(
                    target_sig,
                    target_ball.velocity.length(),
                    exp_rad,
                    exp_vel,
                    drf,
                );
                base_dmg.scaled(p_factor)
            } else if is_entropic {
                shot_dmg.scaled(spool_multiplier)
            } else {
                shot_dmg
            };

            let (applied_dmg, destroyed, sr, ar, hr, became_invuln) = if is_upwell_target {
                match crate::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure(
                    ctx, &sys_handle, sys_id, char_id, target_ball_id, final_dmg, cycle_secs, now_unix,
                )
                .await
                {
                    Ok(Some(res)) => res,
                    Ok(None) => (0.0, false, 0.0, 0.0, 0.0, false),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                let is_invuln = crate::services::insurgency_services::is_insurgency_stronghold_invulnerable(ctx, sys_id, target_ball_id);
                let (app, dest) = if !is_invuln && final_dmg.total() > 0.0 {
                    sys_handle
                        .apply_damage_with_attacker(target_ball_id, final_dmg, session.character_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|(a, d, _)| (a, d))
                        .unwrap_or((0.0, false))
                } else {
                    (0.0, false)
                };
                let (s_r, a_r, h_r) = if let Ok(Some(t_cs)) = sys_handle.get_combat_state(target_ball_id).await {
                    (
                        (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                        (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                        (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                    )
                } else {
                    (0.0, 0.0, 0.0)
                };
                (app, dest, s_r, a_r, h_r, false)
            };

            let victim_char_opt = player_target_info.map(|(vcid, _)| vcid);
            emit_combat_log_and_damage_state(
                ctx,
                char_id,
                victim_char_opt,
                ship_id,
                target_ball_id,
                m.type_id,
                applied_dmg,
                is_wrecking,
                sr,
                ar,
                hr,
            );

            // Record attacker damage in 15-minute assist log
            record_killmail_assist_action(
                ctx,
                target_ball_id,
                char_id,
                attacker_ball.type_id,
                m.type_id,
                applied_dmg,
                false,
                now_unix,
            );
            if let Some(vcid) = victim_char_opt {
                record_killmail_assist_action(
                    ctx,
                    vcid,
                    char_id,
                    attacker_ball.type_id,
                    m.type_id,
                    applied_dmg,
                    false,
                    now_unix,
                );
            }

            // CrimeWatch timers & low-sec/high-sec aggression (`feat:crimewatch.lowsec-suspect-vs-criminal`, `feat:crimewatch.pod-timer`, `feat:crimewatch.npc-timers`)
            if let Some((victim_char_id, _)) = player_target_info {
                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
                let is_legal = ctx.db.with_snapshot(|snap| {
                    is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                }) || {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let vst = cw.get_or_create(victim_char_id);
                    vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                } || with_combat_runtime(ctx, |rt| {
                    rt.has_limited_engagement_with(victim_char_id, char_id, now_unix)
                });
                let is_pod_target = target_ball.type_id == 670;
                let is_mobile_struct = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, target_ball.type_id);
                let (is_anchored_container, is_secure_container, is_unanchored_jetcan) =
                    ctx.db.with_snapshot(|snap| {
                        let anchored = snap
                            .sp6_lifecycle
                            .space_object_meta
                            .get(&resolved_target_id)
                            .is_some_and(|m| m.anchor_start_unix.is_some())
                            && !is_mobile_struct;
                        let secure = ctx
                            .sde
                            .types
                            .get(&target_ball.type_id)
                            .is_some_and(|t| matches!(t.group_id, 340 | 448));
                        let jetcan = (target_ball.type_id == 23 && !anchored) || is_mobile_struct;
                        (anchored, secure, jetcan)
                    });
                let is_new_agg = ctx.crimewatch.read().map(|cw| cw.get_aggression(char_id, victim_char_id, now_unix).is_none()).unwrap_or(true);
                let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
                {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    cw.system_corruption_stages.insert(sys_id, c_stage);
                    let effective_sec = if is_unanchored_jetcan {
                        sys_sec.min(0.40)
                    } else {
                        sys_sec
                    };
                    let _ = cw.handle_combat_aggression_with_pod(
                        char_id,
                        victim_char_id,
                        ship_id,
                        sys_id,
                        effective_sec,
                        is_legal,
                        is_pod_target,
                        now_unix,
                    );
                    cw.get_or_create(char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
                    cw.get_or_create(victim_char_id).pvp_timer_until_unix =
                        now_unix.saturating_add(900);
                    if !is_legal
                        && !is_mobile_struct
                        && ((sys_sec > 0.0 && sys_sec < 0.45 && is_pod_target)
                            || is_anchored_container
                            || (is_secure_container && sys_sec >= 0.45))
                    {
                        let st = cw.get_or_create(char_id);
                        st.criminal_flag_until_unix = now_unix.saturating_add(900);
                        st.suspect_flag_until_unix = 0;
                    }
                }
                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                    ctx,
                    char_id,
                    victim_char_id,
                    now_unix,
                );
                let victim_is_suspect_or_criminal = {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let st = cw.get_or_create(victim_char_id);
                    st.is_suspect(now_unix) || st.is_criminal(now_unix)
                };
                let le_created_or_refreshed = with_mut_combat_runtime(ctx, |rt| {
                    if victim_is_suspect_or_criminal
                        || rt.has_limited_engagement_with(char_id, victim_char_id, now_unix)
                    {
                        rt.add_limited_engagement(char_id, victim_char_id, now_unix);
                        rt.add_limited_engagement(victim_char_id, char_id, now_unix);
                        true
                    } else {
                        false
                    }
                });
                if le_created_or_refreshed {
                    let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
                    ctx.notify(NotificationTarget::Character(char_id), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(victim_char_id), PyRep::Integer(timeout_ft)]);
                    ctx.notify(NotificationTarget::Character(victim_char_id), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(char_id), PyRep::Integer(timeout_ft)]);
                }
                if !is_legal && sys_sec > 0.0 && !is_mobile_struct && is_new_agg {
                    crate::services::combat_crimewatch::crimewatch_timers::apply_illegal_pvp_sec_status_loss(
                        ctx, char_id, sys_id, sys_sec, is_pod_target, now_unix,
                    );
                }
            } else if is_upwell_target {
                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_create(char_id)
                    .pvp_timer_until_unix = now_unix.saturating_add(900);
            } else {
                // Attacking an NPC sets the 5-minute (300s) NPC timer (`feat:crimewatch.npc-timers`)
                crate::sp6_lifecycle::record_npc_aggression_timer(ctx, char_id, now_unix);
            }

            // Handle target destruction: 50% Loot Fairy wreck drop, DetailedKillmail with 15-min assist window & SHA-1 hash, and Pod Corpse / Implant destruction
            let (generated_kill_id, generated_wreck_id, generated_corpse_id, generated_km_hash) = if destroyed && is_upwell_target {
                let kid = ctx.db.with_snapshot(|snap| snap.killmails.keys().max().copied().unwrap_or(0));
                (kid, 0, 0, String::new())
            } else if destroyed {
                combat_weapons_spool::handle_ship_or_npc_destruction(
                    ctx,
                    &sys_handle,
                    char_id,
                    ship_id,
                    attacker_ball.type_id,
                    m.type_id,
                    target_ball_id,
                    &target_ball,
                    player_target_info,
                    sys_id,
                    applied_dmg,
                    now_unix,
                ).await
            } else {
                (0, 0, 0, String::new())
            };

            if became_invuln {
                let _ = sys_handle.remove_target_lock(ship_id, target_ball_id).await;
                ctx.db.with_mut_snapshot(|snap| {
                    snap.module_active_states.remove(&module_id);
                    snap.module_active_charges.remove(&module_id);
                });
                let _ = fit.deactivate_module(module_id, &ctx.sde);
                combat_weapons_spool::reset_entropic_spool(ctx, ship_id, module_id);
            } else {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.module_active_states.insert(module_id);
                    snap.module_active_charges.insert(module_id, charge_type_id);
                });
                let _ = fit.activate_module(module_id, is_overloaded, &ctx.sde);
            }
            ctx.store_ship_fitting(ship_id, fit);

            let st_state = if is_upwell_target {
                ctx.db.with_snapshot(|snap| snap.structures.get(&target_ball_id).map(|s| s.state.clone()))
            } else {
                None
            };

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_ball_id)),
                ("chargeTypeID", PyRep::Integer(charge_type_id as i64)),
                ("hitChance", PyRep::Real(p_hit)), ("appliedDamage", PyRep::Real(applied_dmg)),
                ("isWreckingShot", PyRep::Bool(is_wrecking)), ("capacitorBefore", PyRep::Real(cap_before)),
                ("capacitorAfter", PyRep::Real(cap_after)), ("crystalDamageFraction", PyRep::Real(crystal_damage_after)),
                ("crystalShattered", PyRep::Bool(crystal_shattered)), ("spoolCycles", PyRep::Integer(spool_cycles as i64)),
                ("spoolMultiplier", PyRep::Real(spool_multiplier)), ("secondaryTargetsHit", PyRep::Integer(vorton_secondary_targets.len() as i64)),
                ("destroyed", PyRep::Bool(destroyed)), ("killID", PyRep::Integer(generated_kill_id)),
                ("killmailHash", PyRep::String(generated_km_hash)), ("wreckItemID", PyRep::Integer(generated_wreck_id)),
                ("corpseItemID", PyRep::Integer(generated_corpse_id)),
                ("state", st_state.map(PyRep::String).unwrap_or(PyRep::None)),
            ])))
        }

        _ => None,
    }
}

pub use super::combat_weapons_spool::{clear_entropic_spool, get_entropic_spool_multiplier, handle_link_all_weapons, reset_entropic_spool};
