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
                eve_dogma::FittedModule {
                    item_id: item_rec.item_id,
                    type_id: item_rec.type_id,
                    flag_id: item_rec.flag as u16,
                    state: eve_dogma::ModuleState::Online,
                    charge_type_id: charge_tid,
                    charge_quantity: 0,
                    damage_hp: 0.0,
                    max_hp: 40.0,
                    dynamic_attribute_overrides: std::collections::HashMap::new(),
                }
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
            let Some(mod_type) = ctx.sde.types.get(&m.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module type {} not found in SDE", m.type_id),
                }));
            };

            let is_overloaded = method == "Overload";
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
            // Warp Disruptor (52), Stasis Webifier (65), Energy Nosferatu (68), Energy Neutralizer (71),
            // ECM (201), Sensor Dampener (208), Weapon Disruptor (291), Target Painter (379)
            if matches!(mod_type.group_id, 52 | 65 | 68 | 71 | 201 | 208 | 291 | 379) {
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
                let victim_char_opt = ctx.db.with_snapshot(|snap| {
                    if snap.characters.contains_key(&tid) {
                        Some(tid)
                    } else {
                        snap.characters
                            .values()
                            .find(|c| c.active_ship_id == tid)
                            .map(|c| c.character_id)
                    }
                });
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
                    }
                }
                // Energy Neutralizer (71) & Energy Nosferatu (68) (`feat:npc.sleeper-capacitor-immunity-and-leash`)
                if matches!(mod_type.group_id, 68 | 71) {
                    let is_sleeper = with_combat_runtime(ctx, |rt| {
                        rt.npc_entities.get(&tid).is_some_and(|n| n.archetype == "Sleeper")
                    }) || sys_handle
                        .get_ball_snapshot(tid)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|(_, b)| matches!(b.type_id, 30185..=30500));
                    let drain_amt = mod_type
                        .default_attributes
                        .get(&97)
                        .or_else(|| mod_type.default_attributes.get(&90))
                        .copied()
                        .unwrap_or(120.0)
                        .max(60.0);
                    if let Ok(Some(mut t_cs)) = sys_handle.get_combat_state(tid).await {
                        if !is_sleeper {
                            t_cs.capacitor = (t_cs.capacitor - drain_amt).max(0.0);
                            let _ = sys_handle.set_combat_state(tid, t_cs.clone()).await;
                        }
                        if mod_type.group_id == 68 {
                            if let Ok(Some(mut s_cs)) = sys_handle.get_combat_state(ship_id).await {
                                if is_sleeper || t_cs.capacitor > s_cs.capacitor {
                                    s_cs.capacitor = (s_cs.capacitor + drain_amt).min(s_cs.max_capacitor);
                                    let _ = sys_handle.set_combat_state(ship_id, s_cs).await;
                                }
                            }
                        }
                    }
                }
                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
                if let Some(vcid) = victim_char_opt {
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
            let charge_type = &ctx.sde.types[&charge_type_id];
            let is_fof_missile = matches!(charge_type.group_id, 394..=396);

            // Resolve target ball ID (FoF auto-targeting missiles can fire without a lock even while jammed!)
            let resolved_target_id = if let Some(tid) = target_id_opt {
                if !is_fof_missile {
                    let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
                    let enforce_lock = !locks.is_empty();
                    if enforce_lock && !locks.iter().any(|l| l.target_id == tid && l.locked) {
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
                    .filter(|b| b.id != ship_id && b.id < 700_000_000)
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
                if let Some(ch) = snap.characters.get(&resolved_target_id) {
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
                        let would_be_criminal = sys_sec >= 0.45 || (sys_sec > 0.0 && is_pod_target);
                        let would_be_suspect = sys_sec > 0.0 && sys_sec < 0.45 && !is_pod_target;
                        if safety == SafetyLevel::Green
                            && (would_be_criminal || would_be_suspect)
                        {
                            return Some(Err(BridgeRpcError {
                                code: "SafetyLevelRestricted".into(),
                                message: "Ship safety is set to Green; illegal suspect/criminal aggression is prevented".into(),
                            }));
                        }
                        if safety == SafetyLevel::Yellow && would_be_criminal {
                            return Some(Err(BridgeRpcError {
                                code: "SafetyLevelRestricted".into(),
                                message: "Ship safety is set to Yellow; criminal aggression is prevented".into(),
                            }));
                        }
                    }
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
                let raw_cap = mod_type.default_attributes.get(&6).copied().unwrap_or(0.0);
                let cap_bonus_pct = charge_type.default_attributes.get(&317).copied().unwrap_or(0.0);
                (raw_cap * (1.0 + cap_bonus_pct / 100.0)).max(0.0)
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

            // Weapon kind & damage calculation
            let is_missile_launcher = matches!(
                mod_type.group_id,
                506..=511 | 524 | 1674 | 4500 | 4805 | 4807
            );
            let is_breacher_pod = matches!(mod_type.group_id, 4500 | 4805 | 4807);
            let is_entropic = mod_type.group_id == 1986;
            let is_vorton = mod_type.group_id == 4060;

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

            let target_sig = if let Some((vcid, _)) = player_target_info {
                evaluate_effective_signature_radius(ctx, vcid, target_ball_id, sys_id).await
            } else if let Ok(Some(t_cs)) = sys_handle.get_combat_state(target_ball_id).await {
                if t_cs.signature_radius > 0.0 {
                    t_cs.signature_radius
                } else {
                    target_ball.radius.max(35.0)
                }
            } else {
                target_ball.radius.max(35.0)
            };

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
            // Turret range, tracking, hit chance, Entropic spooling, and Vorton arc chaining
            let range_mult = charge_type
                .default_attributes
                .get(&120)
                .copied()
                .unwrap_or(1.0);
            let tracking_mult = charge_type
                .default_attributes
                .get(&244)
                .copied()
                .unwrap_or(1.0);
            let optimal_range = mod_type
                .default_attributes
                .get(&54)
                .copied()
                .unwrap_or(5_000.0)
                * range_mult;
            let falloff_range = mod_type
                .default_attributes
                .get(&158)
                .copied()
                .unwrap_or(5_000.0);
            let tracking_speed = mod_type
                .default_attributes
                .get(&160)
                .copied()
                .unwrap_or(100.0)
                * tracking_mult;
            let turret_sig_res = mod_type
                .default_attributes
                .get(&620)
                .copied()
                .unwrap_or(40_000.0);

            let dist = attacker_ball.position.distance(target_ball.position);
            if dist > optimal_range + 3.0 * falloff_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetOutOfRange".into(),
                    message: format!(
                        "Target {target_ball_id} is {dist:.1} m away (exceeds range {:.1} m)",
                        optimal_range + 3.0 * falloff_range
                    ),
                }));
            }

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
            let mut spool_multiplier = 1.0;
            let mut spool_cycles = 0u32;
            if is_entropic {
                let per_cycle = mod_type
                    .default_attributes
                    .get(&2733)
                    .copied()
                    .unwrap_or(0.07);
                let max_bonus = mod_type
                    .default_attributes
                    .get(&2734)
                    .copied()
                    .unwrap_or(2.125);
                with_mut_combat_runtime(ctx, |rt| {
                    let entry = rt.entropic_spools.entry((ship_id, module_id)).or_insert(
                        EntropicSpoolState {
                            target_id: target_ball_id,
                            cycles: 0,
                            per_cycle_bonus: per_cycle,
                            max_bonus,
                            current_multiplier: 1.0,
                        },
                    );
                    if entry.target_id == target_ball_id {
                        spool_cycles = entry.cycles;
                        spool_multiplier =
                            1.0 + (entry.cycles as f64 * per_cycle).min(max_bonus);
                        entry.cycles = entry.cycles.saturating_add(1);
                        entry.current_multiplier =
                            1.0 + (entry.cycles as f64 * per_cycle).min(max_bonus);
                    } else {
                        // Target changed -> reset spool!
                        entry.target_id = target_ball_id;
                        entry.cycles = 1;
                        spool_cycles = 0;
                        spool_multiplier = 1.0;
                        entry.current_multiplier = 1.0 + per_cycle.min(max_bonus);
                    }
                });
            }

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
                    .filter(|b| b.id != ship_id && b.id != target_ball_id && b.id < 700_000_000)
                    .map(|b| (b.id, b.position))
                    .collect();
                let hits = vorton_arc_targets_with_cap(
                    target_ball_id,
                    target_ball.position,
                    &candidates,
                    arc_range,
                    max_secondary,
                );
                vorton_secondary_targets = hits.into_iter().skip(1).collect();
                for &arc_id in &vorton_secondary_targets {
                    if let Ok(Some((_, arc_ball))) = sys_handle.get_ball_snapshot(arc_id).await {
                        let arc_factor = missile_damage_factor(
                            arc_ball.radius.max(35.0),
                            arc_ball.velocity.length(),
                            exp_rad,
                            exp_vel,
                            drf,
                        );
                        let _ = sys_handle
                            .apply_damage_with_attacker(arc_id, base_dmg.scaled(arc_factor), session.character_id)
                            .await;
                    }
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

            let (applied_dmg, destroyed) = if final_dmg.total() > 0.0 {
                if let Ok(Some((app, dest, _))) =
                    sys_handle.apply_damage_with_attacker(target_ball_id, final_dmg, session.character_id).await
                {
                    (app, dest)
                } else {
                    (0.0, false)
                }
            } else {
                (0.0, false)
            };

            let (sr, ar, hr) = if let Ok(Some(t_cs)) =
                sys_handle.get_combat_state(target_ball_id).await
            {
                (
                    (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                )
            } else {
                (0.0, 0.0, 0.0)
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
            ctx.crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .trigger_weapon_timer(char_id, now_unix);
            if let Some((victim_char_id, _)) = player_target_info {
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
                {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let _ = cw.handle_combat_aggression(
                        char_id,
                        victim_char_id,
                        ship_id,
                        sys_id,
                        sys_sec,
                        is_legal,
                        now_unix,
                    );
                    cw.get_or_create(char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
                    cw.get_or_create(victim_char_id).pvp_timer_until_unix =
                        now_unix.saturating_add(900);
                    if !is_legal && sys_sec > 0.0 && sys_sec < 0.45 && is_pod_target {
                        // In low-sec (< 0.45), attacking a capsule triggers a Criminal flag rather than Suspect
                        let st = cw.get_or_create(char_id);
                        st.criminal_flag_until_unix = now_unix.saturating_add(900);
                        st.suspect_flag_until_unix = 0;
                    }
                }
                // parity: feat:crimewatch.pvp-logoff-timer-renewal, plan:SP-6
                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                    ctx,
                    char_id,
                    victim_char_id,
                    now_unix,
                );
                if !is_legal {
                    with_mut_combat_runtime(ctx, |rt| {
                        rt.add_limited_engagement(char_id, victim_char_id, now_unix);
                        rt.add_limited_engagement(victim_char_id, char_id, now_unix);
                    });
                } else {
                    with_mut_combat_runtime(ctx, |rt| {
                        if rt.has_limited_engagement_with(char_id, victim_char_id, now_unix) {
                            rt.add_limited_engagement(char_id, victim_char_id, now_unix);
                            rt.add_limited_engagement(victim_char_id, char_id, now_unix);
                        }
                    });
                }
                if !is_legal && sys_sec > 0.0 {
                    let old_sec = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(&char_id)
                            .map(|c| c.security_status)
                            .unwrap_or(0.0)
                    });
                    let loss = crate::combat::crimewatch::compute_sec_status_loss(
                        old_sec,
                        is_pod_target,
                        sys_sec,
                    );
                    let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.security_status = new_sec;
                        }
                    });
                    with_mut_combat_runtime(ctx, |rt| {
                        rt.sec_status_transactions
                            .entry(char_id)
                            .or_default()
                            .push(SecStatusTransaction {
                                timestamp_unix: now_unix,
                                old_sec,
                                new_sec,
                                delta: new_sec - old_sec,
                                reason: if is_pod_target {
                                    "Illegal Capsule Aggression".into()
                                } else {
                                    "Illegal Ship Aggression".into()
                                },
                                solar_system_id: sys_id,
                            });
                    });
                }
            } else {
                // Attacking an NPC sets the 5-minute (300s) NPC timer (`feat:crimewatch.npc-timers`)
                crate::sp6_lifecycle::record_npc_aggression_timer(ctx, char_id, now_unix);
            }

            // Handle target destruction: 50% Loot Fairy wreck drop, DetailedKillmail with 15-min assist window & SHA-1 hash, and Pod Corpse / Implant destruction
            let mut generated_kill_id: i64 = 0;
            let mut generated_wreck_id: i64 = 0;
            let mut generated_corpse_id: i64 = 0;
            let mut generated_km_hash = String::new();

            if destroyed {
                let is_pod_target = target_ball.type_id == 670;
                let victim_id = victim_char_opt.unwrap_or(target_ball_id);
                let wreck_owner_id = if victim_char_opt.is_some() { victim_id } else { char_id };
                let wreck_type_id = ctx.sde.wrecks.get(&target_ball.type_id).copied().unwrap_or(26557);
                let fitted_mods: Vec<(i64, u32, u16)> = ctx
                    .ship_fittings
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&target_ball_id)
                    .map(|f| f.modules.values().map(|m| (m.item_id, m.type_id, m.flag_id)).collect())
                    .unwrap_or_default();
                let (vic_corp, vic_all, active_implants) = ctx.db.with_snapshot(|snap| {
                    let (c, a) = snap
                        .characters
                        .get(&victim_id)
                        .map(|ch| (ch.corporation_id, ch.alliance_id))
                        .unwrap_or((1_000_125, None));
                    let imps = snap
                        .active_implants
                        .get(&victim_id)
                        .cloned()
                        .unwrap_or_default();
                    (c, a, imps)
                });

                // Collect all items inside `target_ball_id` and roll 50% Loot Fairy (`feat:combat.wreck-loot`)
                let mut dropped_entries = Vec::new();
                let mut destroyed_entries = Vec::new();
                ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    generated_wreck_id = snap.next_entity_id;
                    if !is_pod_target {
                        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                            snap,
                            generated_wreck_id,
                            now_unix,
                            victim_char_opt.is_none(),
                        );
                        snap.items.insert(
                            generated_wreck_id,
                            InventoryItemRecord {
                                item_id: generated_wreck_id,
                                type_id: wreck_type_id,
                                owner_id: wreck_owner_id,
                                location_id: sys_id as i64,
                                flag: InventoryFlag::Cargo,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                        snap.item_positions.insert(
                            generated_wreck_id,
                            [
                                target_ball.position.x,
                                target_ball.position.y,
                                target_ball.position.z,
                            ],
                        );
                    }
                    for &(mid, mtype, mflag) in &fitted_mods {
                        snap.items.entry(mid).or_insert_with(|| InventoryItemRecord {
                            item_id: mid,
                            type_id: mtype,
                            owner_id: wreck_owner_id,
                            location_id: target_ball_id,
                            flag: InventoryFlag::from_u16(mflag),
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        });
                    }

                    let mut contained: Vec<InventoryItemRecord> = snap.items.values().filter(|i| i.location_id == target_ball_id).cloned().collect();
                    contained.sort_by_key(|i| i.item_id);

                    let drop_threshold = if ctx.sde.solar_systems.get(&sys_id).is_some_and(|s| s.region_id == 10_000_070) { 0.60 } else { 0.50 };
                    let hull_isk = ctx.sde.types.get(&target_ball.type_id).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(if is_pod_target { 1_000.0 } else { 250_000.0 });
                    let mut items_isk = 0.0;

                    for item in contained {
                        let unit_isk = ctx.sde.types.get(&item.type_id).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(100.0);
                        items_isk += unit_isk * (item.quantity.max(1) as f64);
                        let is_rig_or_sub = matches!(item.flag as i64, 89 | 92..=94 | 125..=132);
                        let roll = (((now_unix ^ (target_ball_id as u64)).wrapping_add((item.item_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as f64 / 4_294_967_296.0;
                        if !is_pod_target && !is_rig_or_sub && roll < drop_threshold {
                            if let Some(it_mut) = snap.items.get_mut(&item.item_id) {
                                it_mut.location_id = generated_wreck_id;
                                it_mut.owner_id = wreck_owner_id;
                                it_mut.flag = InventoryFlag::Cargo;
                                it_mut.version += 1;
                            }
                            dropped_entries.push(KillmailItemEntry { type_id: item.type_id, flag: item.flag as u16, qty_dropped: item.quantity, qty_destroyed: 0, singleton: item.singleton });
                        } else {
                            snap.items.remove(&item.item_id);
                            destroyed_entries.push(KillmailItemEntry { type_id: item.type_id, flag: item.flag as u16, qty_dropped: 0, qty_destroyed: item.quantity, singleton: item.singleton });
                        }
                    }

                    if is_pod_target {
                        for &imp_tid in &active_implants {
                            items_isk += ctx.sde.types.get(&imp_tid).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(1_000.0);
                            destroyed_entries.push(KillmailItemEntry { type_id: imp_tid, flag: InventoryFlag::Implant as u16, qty_dropped: 0, qty_destroyed: 1, singleton: true });
                        }
                        if let Some(imps) = snap.active_implants.get_mut(&victim_id) {
                            imps.clear();
                        }
                        snap.next_entity_id += 1;
                        generated_corpse_id = snap.next_entity_id;
                        snap.items.insert(
                            generated_corpse_id,
                            InventoryItemRecord {
                                item_id: generated_corpse_id, type_id: 25, owner_id: victim_id, location_id: sys_id as i64,
                                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
                                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0,
                                is_blueprint_copy: false, blueprint_runs_remaining: None,
                            },
                        );
                        snap.item_positions.insert(generated_corpse_id, [target_ball.position.x, target_ball.position.y, target_ball.position.z]);
                    }

                    snap.next_entity_id += 1;
                    generated_kill_id = snap.next_entity_id;
                    snap.record_killmail(
                        KillmailRecord {
                            kill_id: generated_kill_id, victim_character_id: victim_id, attacker_character_id: char_id,
                            solar_system_id: sys_id, ship_type_id: target_ball.type_id, total_value_isk: hull_isk + items_isk,
                            ..Default::default()
                        },
                        ctx.now_unix(),
                    );
                });

                if !is_pod_target && generated_wreck_id > 0 {
                    let _ = sys_handle
                        .spawn_ball_with_combat(
                            eve_destiny::Ball {
                                id: generated_wreck_id, type_id: wreck_type_id, position: target_ball.position, velocity: DVec3::ZERO,
                                max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 30.0, speed_fraction: 0.0,
                                mode: eve_destiny::MovementMode::Stop,
                            },
                            ctx.build_type_combat_state(wreck_type_id),
                        )
                        .await;
                    let is_crim = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(wreck_owner_id).is_criminal(now_unix);
                    register_container_lifetime(ctx, generated_wreck_id, sys_id, wreck_type_id, wreck_owner_id, now_unix, is_crim);
                }

                if is_pod_target {
                    if generated_corpse_id > 0 {
                        let _ = sys_handle
                            .spawn_ball_with_combat(
                                eve_destiny::Ball {
                                    id: generated_corpse_id, type_id: 25, position: target_ball.position, velocity: DVec3::ZERO,
                                    max_velocity: 0.0, inertia: 1.0, mass: 80.0, radius: 2.0, speed_fraction: 0.0,
                                    mode: eve_destiny::MovementMode::Stop,
                                },
                                ctx.build_type_combat_state(25),
                            )
                            .await;
                    }
                    if let Some((vcid, vship_id)) = player_target_info {
                        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
                        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
                            st.disconnected_ships.remove(&vcid);
                        });
                        let _ = claim_player_bounty_on_kill(ctx, char_id, vcid, 670);
                        let _ = ctx
                            .db
                            .respawn_character_at_home_station(vcid, vship_id, &ctx.sde);
                    }
                } else if let Some((vcid, vship_id)) = player_target_info {
                    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
                    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
                        st.disconnected_ships.remove(&vcid);
                    });
                    let _ = process_ship_insurance_payout(ctx, vcid, vship_id, target_ball.type_id, false);
                    let _ = claim_player_bounty_on_kill(ctx, char_id, vcid, target_ball.type_id);
                    // Eject victim into a Capsule in space at the wreck position
                    let cap_id = ctx.db.with_mut_snapshot(|snap| {
                        snap.items.remove(&vship_id);
                        snap.next_entity_id += 1;
                        let cid = snap.next_entity_id;
                        snap.items.insert(
                            cid,
                            InventoryItemRecord {
                                item_id: cid,
                                type_id: 670,
                                owner_id: vcid,
                                location_id: sys_id as i64,
                                flag: InventoryFlag::None,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                        if let Some(vch) = snap.characters.get_mut(&vcid) {
                            vch.active_ship_id = cid;
                        }
                        cid
                    });
                    let _ = sys_handle
                        .spawn_ball_with_combat(
                            eve_destiny::Ball {
                                id: cap_id,
                                type_id: 670,
                                position: target_ball.position,
                                velocity: DVec3::ZERO,
                                max_velocity: 335.0,
                                inertia: 0.25,
                                mass: 16_000.0,
                                radius: 15.0,
                                speed_fraction: 0.0,
                                mode: eve_destiny::MovementMode::Stop,
                            },
                            ctx.build_type_combat_state(670),
                        )
                        .await;
                } else {
                    // NPC pirate killed -> raise security status capped at +5.0 (`feat:crimewatch.sec-status-gain`, `feat:crimewatch.sec-status-gain-caps`)
                    let bounty = ctx
                        .sde
                        .types
                        .get(&target_ball.type_id)
                        .and_then(|t| {
                            t.default_attributes
                                .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                                .copied()
                        })
                        .unwrap_or(50_000.0);
                    if bounty > 0.0 {
                        let old_sec = ctx.db.with_snapshot(|snap| {
                            snap.characters
                                .get(&char_id)
                                .map(|c| c.security_status)
                                .unwrap_or(0.0)
                        });
                        if old_sec < 5.0 {
                            let gain = crate::combat::npc_ai::compute_sec_status_gain(old_sec, bounty);
                            let new_sec = (old_sec + gain).min(5.0).clamp(-10.0, 10.0);
                            ctx.db.with_mut_snapshot(|snap| {
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    ch.security_status = new_sec;
                                }
                            });
                            with_mut_combat_runtime(ctx, |rt| {
                                rt.sec_status_transactions
                                    .entry(char_id)
                                    .or_default()
                                    .push(SecStatusTransaction {
                                        timestamp_unix: now_unix,
                                        old_sec,
                                        new_sec,
                                        delta: new_sec - old_sec,
                                        reason: "NPC Pirate Bounty".into(),
                                        solar_system_id: sys_id,
                                    });
                            });
                        }
                    }
                }

                // Prune >15m (900s) assists and build DetailedKillmail (`feat:combat.killmail-assist-timeout-ewar`, `feat:combat.killmail-esi-hash`)
                let valid_assists: Vec<AssistLogEntry> = with_mut_combat_runtime(ctx, |rt| {
                    let mut out = Vec::new();
                    for key in [target_ball_id, victim_id] {
                        if let Some(list) = rt.aggression_assist_log.remove(&key) {
                            for e in list {
                                if now_unix.saturating_sub(e.last_action_unix)
                                    <= KILLMAIL_ASSIST_WINDOW_SECS
                                    && !out
                                        .iter()
                                        .any(|x: &AssistLogEntry| x.attacker_char_id == e.attacker_char_id)
                                {
                                    out.push(e);
                                }
                            }
                        }
                    }
                    out
                });

                let km_attackers: Vec<KillmailAttackerEntry> = valid_assists
                    .iter()
                    .map(|a| KillmailAttackerEntry {
                        character_id: a.attacker_char_id,
                        corporation_id: a.corporation_id,
                        alliance_id: a.alliance_id,
                        ship_type_id: a.ship_type_id,
                        weapon_type_id: a.weapon_type_id,
                        damage_done: a.damage_done,
                        final_blow: a.attacker_char_id == char_id,
                        security_status: 0.0,
                    })
                    .collect();
                let total_dmg_taken: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
                let km_total_isk = ctx
                    .db
                    .with_snapshot(|s| s.killmails.get(&generated_kill_id).map(|k| k.total_value_isk))
                    .unwrap_or(250_000.0);
                generated_km_hash =
                    compute_killmail_esi_hash(victim_id, char_id, target_ball.type_id, now_unix);
                with_mut_combat_runtime(ctx, |rt| {
                    rt.detailed_killmails.insert(
                        generated_kill_id,
                        DetailedKillmail {
                            kill_id: generated_kill_id,
                            kill_time_unix: now_unix,
                            solar_system_id: sys_id,
                            victim_character_id: victim_id,
                            victim_corporation_id: vic_corp,
                            victim_alliance_id: vic_all,
                            victim_ship_type_id: target_ball.type_id,
                            victim_damage_taken: total_dmg_taken.max(applied_dmg),
                            victim_position: [target_ball.position.x, target_ball.position.y, target_ball.position.z],
                            attackers: km_attackers,
                            dropped_items: dropped_entries,
                            destroyed_items: destroyed_entries,
                            total_value_isk: km_total_isk,
                            killmail_hash: generated_km_hash.clone(),
                        },
                    );
                });
            }

            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.insert(module_id);
                snap.module_active_charges.insert(module_id, charge_type_id);
            });
            let _ = fit.activate_module(module_id, is_overloaded, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_ball_id)),
                ("chargeTypeID", PyRep::Integer(charge_type_id as i64)),
                ("hitChance", PyRep::Real(p_hit)),
                ("appliedDamage", PyRep::Real(applied_dmg)),
                ("isWreckingShot", PyRep::Bool(is_wrecking)),
                ("capacitorBefore", PyRep::Real(cap_before)),
                ("capacitorAfter", PyRep::Real(cap_after)),
                ("crystalDamageFraction", PyRep::Real(crystal_damage_after)),
                ("crystalShattered", PyRep::Bool(crystal_shattered)),
                ("spoolCycles", PyRep::Integer(spool_cycles as i64)),
                ("spoolMultiplier", PyRep::Real(spool_multiplier)),
                (
                    "secondaryTargetsHit",
                    PyRep::Integer(vorton_secondary_targets.len() as i64),
                ),
                ("destroyed", PyRep::Bool(destroyed)),
                ("killID", PyRep::Integer(generated_kill_id)),
                ("killmailHash", PyRep::String(generated_km_hash)),
                ("wreckItemID", PyRep::Integer(generated_wreck_id)),
                ("corpseItemID", PyRep::Integer(generated_corpse_id)),
            ])))
        }

        _ => None,
    }
}

pub fn handle_link_all_weapons(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Ship {ship_id} is not owned by {char_id}"),
            });
        }
        let is_linkable_weapon = |ty: &eve_sde::InvType| -> bool {
            ty.effect_ids.contains(&42)
                || ty.effect_ids.contains(&40)
                || ty.default_attributes.contains_key(&604)
        };
        let mut by_type: std::collections::BTreeMap<u32, Vec<i64>> =
            std::collections::BTreeMap::new();
        for item in snap.items.values() {
            if item.location_id == ship_id && matches!(item.flag as u16, 27..=34) {
                if let Some(ty) = ctx.sde.types.get(&item.type_id) {
                    if is_linkable_weapon(ty) {
                        by_type.entry(item.type_id).or_default().push(item.item_id);
                    }
                }
            }
        }
        let banks = snap.weapon_banks.entry(ship_id).or_default();
        banks.clear();
        let mut result_entries: Vec<(PyRep, PyRep)> = Vec::new();
        for (_type_id, mut mods) in by_type {
            if mods.len() >= 2 {
                mods.sort();
                let master = mods[0];
                banks.insert(master, mods.clone());
                let mod_list: Vec<PyRep> = mods.into_iter().map(PyRep::Integer).collect();
                result_entries.push((PyRep::Integer(master), PyRep::List(mod_list)));
            }
        }
        result_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        Ok(PyRep::Dict(result_entries))
    })
}
