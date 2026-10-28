//! `fitting_dogma_ops` — dogmaIM targeting, module repair, online batched, drone settings, attribute queries.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;

pub(crate) async fn handle_dogma_ops(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "dogmaIM" {
        return None;
    }

    match method {
        // parity: rpc:dogmaIM.AddTargetOBO
        "AddTargetOBO" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let source_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            if source_id == 0 || target_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddTargetOBO requires sourceID and targetID".into(),
                }));
            }
            if target_id == source_id {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: "Cannot target self".into(),
                }));
            }

            let is_authorized = session.ship_id == Some(source_id)
                || ctx.db.with_snapshot(|s| s.items.get(&source_id).is_some_and(|it| it.owner_id == char_id))
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.values().any(|d| {
                        (d.ball_id == source_id || d.drone_item_id == source_id)
                            && d.owner_char_id == char_id
                    })
                });

            if !is_authorized {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Source entity {source_id} is not owned or controlled by character {char_id}"),
                }));
            }

            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session is not in space".into(),
                }));
            };

            if let Err(e) = crate::services::tourney_service::check_tourney_lock_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys.get_ball_snapshot(source_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Source ball {source_id} not found in system"),
                }));
            };
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target ball {target_id} not found in system"),
                }));
            };

            let target_char_id = ctx.db.with_snapshot(|s| {
                if s.characters.contains_key(&target_id) {
                    Some(target_id)
                } else if let Some(c) = s.characters.values().find(|c| c.active_ship_id == target_id) {
                    Some(c.character_id)
                } else {
                    s.items.get(&target_id).and_then(|it| {
                        if s.characters.contains_key(&it.owner_id) {
                            Some(it.owner_id)
                        } else {
                            None
                        }
                    })
                }
            });

            if let Some(t_char) = target_char_id {
                if crate::services::space_warp_jump::is_location_change_invulnerable(ctx, t_char, ctx.now_unix_secs()) {
                    return Some(Err(BridgeRpcError {
                        code: "TargetInvulnerable".into(),
                        message: "Target is invulnerable due to recent location change".into(),
                    }));
                }
            }

            if sys.get_combat_state(target_id).await.ok().flatten().is_some_and(|tcs| tcs.is_tethered) {
                return Some(Err(BridgeRpcError {
                    code: "TargetInvulnerable".into(),
                    message: format!("Target {target_id} is tethered and invulnerable to target locks"),
                }));
            }
            if session.ship_id == Some(source_id) {
                crate::services::space_warp_jump::break_location_change_invulnerability(ctx, char_id);
            }
            let fit_res = ctx.get_or_rebuild_ship_fitting(char_id, source_id);

            if let Ok(Some(cs)) = sys.get_combat_state(source_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "ShipJammed".into(),
                        message: "Targeting systems are jammed by ECM".into(),
                    }));
                }
                if (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
                    || cs.decloak_targeting_delay_remaining_s > 0.0
                {
                    return Some(Err(BridgeRpcError {
                        code: "TargetingFailed".into(),
                        message: "Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay".into(),
                    }));
                }
            }

            let max_range = fit_res
                .as_ref()
                .map(|f| f.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde))
                .unwrap_or(100_000.0);
            let dist = (ship_ball.position - target_ball.position).length();
            if dist > max_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)"),
                }));
            }

            let max_locked = fit_res
                .as_ref()
                .map(|f| {
                    let ship_max = f.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize;
                    let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
                    let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
                    eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl)
                })
                .unwrap_or(7);

            let current_locks = sys.get_target_locks(source_id).await.unwrap_or_default();
            if current_locks.len() >= max_locked {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Max locked targets exceeded ({max_locked})"),
                }));
            }

            let scan_res = fit_res
                .as_ref()
                .map(|f| f.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0))
                .unwrap_or(250.0);
            let raw_sig = sys
                .get_combat_state(target_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.base_signature_radius.unwrap_or(cs.signature_radius))
                .filter(|&s| s > 0.0)
                .unwrap_or(0.0)
                .max(target_ball.radius)
                .max(35.0);
            let target_sig = ctx.effective_target_signature_radius(target_id, raw_sig);
            let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
            let lock_time_ms = lock_sec * 1000.0;

            let lock = eve_destiny::TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range),
            };
            if let Err(e) = sys.add_target_lock(source_id, lock).await {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: e,
                }));
            }

            if session.ship_id == Some(source_id) {
                crate::services::space_warp_jump::break_location_change_invulnerability(ctx, char_id);
            }

            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("locking".into())],
            );
            let _ = ctx.notify_target_events(char_id, &[("add", target_id, "")]);

            if let Some(target_owner_id) = ctx
                .db
                .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
                .filter(|&oid| oid != char_id && oid > 0)
            {
                ctx.notify(
                    NotificationTarget::Character(target_owner_id),
                    "OnTarget",
                    "charid",
                    vec![PyRep::Integer(source_id), PyRep::String("targetedBy".into())],
                );
            }

            Some(Ok(PyRep::Tuple(vec![
                PyRep::Bool(true),
                PyRep::Real(lock_time_ms),
            ])))
        }

        // parity: rpc:dogmaIM.CancelAddTarget
        "CancelAddTarget" => {
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
                    message: "CancelAddTarget requires targetID".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_target_lock(ship_id, target_id).await;
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
            );
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.RemoveTargetOBO
        "RemoveTargetOBO" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let source_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            if source_id == 0 || target_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveTargetOBO requires sourceID and targetID".into(),
                }));
            }

            let is_authorized = session.ship_id == Some(source_id)
                || ctx.db.with_snapshot(|s| s.items.get(&source_id).is_some_and(|it| it.owner_id == char_id))
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.values().any(|d| {
                        (d.ball_id == source_id || d.drone_item_id == source_id)
                            && d.owner_char_id == char_id
                    })
                });

            if !is_authorized {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Source entity {source_id} is not owned or controlled by character {char_id}"),
                }));
            }

            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_target_lock(source_id, target_id).await;
            }
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, source_id, Some(target_id));
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
            );
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.RemoveTargets
        "RemoveTargets" => {
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
                if let Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) = args.first() {
                    for it in list {
                        if let Some(tid) = it.as_i64() {
                            let _ = sys.remove_target_lock(ship_id, tid).await;
                            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(tid));
                            ctx.notify(
                                NotificationTarget::Character(char_id),
                                "OnTarget",
                                "charid",
                                vec![PyRep::Integer(tid), PyRep::String("lost".into())],
                            );
                        }
                    }
                } else if let Some(tid) = args.first().and_then(|a| a.as_i64()) {
                    let _ = sys.remove_target_lock(ship_id, tid).await;
                    crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(tid));
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnTarget",
                        "charid",
                        vec![PyRep::Integer(tid), PyRep::String("lost".into())],
                    );
                } else {
                    let current_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let _ = sys.clear_target_locks(ship_id).await;
                    for lock in current_locks {
                        crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(lock.target_id));
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnTargetClear",
                        "charid",
                        vec![],
                    );
                }
            }
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.QueryAttributeValue
        "QueryAttributeValue" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if let Err(e) = check_dogma_item_access(ctx, session, char_id, item_id) {
                return Some(Err(e));
            }
            let attr_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .map(|v| v as u16)
                .unwrap_or(0);

            let val = get_item_attribute_value(ctx, char_id, session.ship_id.unwrap_or(0), item_id, attr_id);
            Some(Ok(PyRep::Real(val)))
        }

        // parity: rpc:dogmaIM.QueryAllAttributesForItem
        "QueryAllAttributesForItem" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if let Err(e) = check_dogma_item_access(ctx, session, char_id, item_id) {
                return Some(Err(e));
            }
            let attrs = get_all_item_attributes(ctx, char_id, session.ship_id.unwrap_or(0), item_id);
            let entries = attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            Some(Ok(PyRep::Dict(entries)))
        }

        // parity: rpc:dogmaIM.FullyDescribeAttribute
        "FullyDescribeAttribute" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if let Err(e) = check_dogma_item_access(ctx, session, char_id, item_id) {
                return Some(Err(e));
            }
            let attr_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .map(|v| v as u16)
                .unwrap_or(0);

            let val = get_item_attribute_value(ctx, char_id, session.ship_id.unwrap_or(0), item_id, attr_id);
            let base_val = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&item_id)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .and_then(|t| t.default_attributes.get(&attr_id).copied())
                    .unwrap_or(val)
            });

            let attr_meta = ctx.sde.attributes.get(&attr_id);
            let name = attr_meta.map(|a| a.name.as_str()).unwrap_or("UnknownAttribute");
            let def_val = attr_meta.map(|a| a.default_value).unwrap_or(0.0);
            let stackable = attr_meta.map(|a| a.stackable).unwrap_or(true);
            let published = attr_meta.map(|a| a.published).unwrap_or(false);

            Some(Ok(PyRep::Tuple(vec![
                PyRep::String(format!("{name}: {val:.4}")),
                PyRep::String(format!("attributeID={attr_id} value={val:.4} defaultValue={def_val:.4}")),
                PyRep::String(format!("baseValue={base_val:.4} modifiers=none stackable={stackable}")),
                PyRep::String(format!("published={published}")),
            ])))
        }

        // parity: rpc:dogmaIM.ShipOnlineModules
        "ShipOnlineModules" => {
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

            let dock_id = session.station_id.or(session.structure_id).or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id))
                })
            });
            if let Some(did) = dock_id {
                if did < 1_000_000_000_000
                    && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, did, 17)
                {
                    return Some(Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {did} does not offer fitting facilities"),
                    }));
                }
            } else if let Some(sys_id) = session.solar_system_id {
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

            let mod_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.location_id == ship_id && (11..=34).contains(&(i.flag as u16)))
                    .map(|i| i.item_id)
                    .collect()
            });

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let mut newly_onlined = Vec::new();
            for mid in mod_ids {
                let is_online = ctx.db.with_snapshot(|snap| {
                    snap.module_online_states.get(&mid).copied().unwrap_or(true)
                });
                if !is_online && fit.set_module_online(mid, &ctx.sde).is_ok() {
                    newly_onlined.push(mid);
                }
            }

            ctx.store_ship_fitting(ship_id, fit);
            ctx.db.with_mut_snapshot(|snap| {
                for &mid in &newly_onlined {
                    snap.module_online_states.insert(mid, true);
                }
            });
            ctx.invalidate_ship_fitting(ship_id);

            for mid in newly_onlined {
                let _ = ctx.notify_module_online_state(char_id, mid, true);
            }

            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.InitiateModuleRepairMany
        "InitiateModuleRepairMany" => {
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

            let mut mod_ids = Vec::new();
            if let Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) = args.first() {
                for it in list {
                    if let Some(id) = it.as_i64() {
                        mod_ids.push(id);
                    }
                }
            } else if let Some(id) = args.first().and_then(|a| a.as_i64()) {
                mod_ids.push(id);
            }

            let mut repaired = Vec::new();
            let mut failed = Vec::new();
            let mut skipped = Vec::new();

            for module_id in mod_ids {
                let (is_valid, is_active, cur_dmg, max_hp, base_paste_cost, nanite_op_lvl, nanite_interfacing_lvl) =
                    ctx.db.with_snapshot(|snap| {
                        let active = snap.module_active_states.contains(&module_id);
                        let m_opt = snap.items.get(&module_id);
                        let is_valid = m_opt.map_or(false, |it| {
                            let is_ehe = matches!(it.type_id, 40714 | 40717)
                                || ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.effect_ids.contains(&6484));
                            it.owner_id == char_id
                                && it.location_id == ship_id
                                && (11..=34).contains(&(it.flag as i64))
                                && !is_ehe
                        });
                        let dmg = m_opt
                            .and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
                            .unwrap_or(0.0);
                        let hp = m_opt
                            .and_then(|i| {
                                i.dynamic_attributes
                                    .get(&attr_ids::HP_STRUCT)
                                    .copied()
                                    .or_else(|| {
                                        ctx.sde
                                            .types
                                            .get(&i.type_id)
                                            .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                                    })
                            })
                            .unwrap_or(40.0);
                        let base_cost = m_opt
                            .and_then(|i| ctx.sde.types.get(&i.type_id))
                            .and_then(|t| t.default_attributes.get(&1231).copied())
                            .unwrap_or(4.0)
                            .max(1.0);
                        let sk = snap.skills.get(&char_id);
                        let op_lvl = sk.and_then(|m| m.get(&28879)).map(|r| r.level).unwrap_or(0);
                        let int_lvl = sk.and_then(|m| m.get(&28880)).map(|r| r.level).unwrap_or(0);
                        (is_valid, active, dmg, hp, base_cost, op_lvl, int_lvl)
                    });

                if !is_valid || is_active || cur_dmg >= max_hp - 1e-6 {
                    failed.push(PyRep::Integer(module_id));
                    continue;
                }
                if cur_dmg <= 1e-6 {
                    skipped.push(PyRep::Integer(module_id));
                    continue;
                }

                let effective_paste_per_hp =
                    (base_paste_cost / max_hp.max(1.0)) * (1.0 - 0.05 * (nanite_op_lvl as f64));
                let paste_needed =
                    eve_dogma::thermodynamics::nanite_paste_required(cur_dmg, effective_paste_per_hp)
                        .max(1) as i32;

                let deduct_ok = ctx.db.try_with_mut_snapshot(|snap| {
                    let paste_item_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && i.flag == InventoryFlag::Cargo
                                && i.type_id == 28668
                                && i.quantity >= paste_needed
                        })
                        .map(|i| i.item_id);
                    let Some(pid) = paste_item_id else {
                        return Err(());
                    };
                    if let Some(p_item) = snap.items.get_mut(&pid) {
                        p_item.quantity -= paste_needed;
                        if p_item.quantity == 0 {
                            snap.items.remove(&pid);
                        }
                    }
                    Ok(())
                });

                if deduct_ok.is_ok() {
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
                                m_item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                            }
                        });
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    repaired.push(PyRep::Integer(module_id));
                } else {
                    failed.push(PyRep::Integer(module_id));
                }
            }

            if !repaired.is_empty() {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnMultiModuleRepairStarted",
                    "charid",
                    vec![PyRep::List(repaired.clone())],
                );
                ctx.invalidate_ship_fitting(ship_id);
            }

            Some(Ok(PyRep::Tuple(vec![
                PyRep::List(repaired),
                PyRep::List(failed),
                PyRep::List(skipped),
            ])))
        }

        // parity: rpc:dogmaIM.ChangeDroneSettings
        "ChangeDroneSettings" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let parse_bool_or_int = |rep: &PyRep| -> Option<bool> {
                match rep {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(0) => Some(false),
                    PyRep::Integer(1) => Some(true),
                    _ => None,
                }
            };

            let (agg_val, ff_val) = if let Some(PyRep::Dict(d)) = args.first() {
                let agg = d
                    .iter()
                    .find(|(k, _)| {
                        matches!(k.as_str(), Some("aggressive") | Some("aggression") | Some("1275"))
                            || matches!(k.as_i64(), Some(1275))
                    })
                    .map(|(_, v)| v);
                let ff = d
                    .iter()
                    .find(|(k, _)| {
                        matches!(k.as_str(), Some("focusFire") | Some("focus_fire") | Some("1297"))
                            || matches!(k.as_i64(), Some(1297))
                    })
                    .map(|(_, v)| v);
                (agg, ff)
            } else if let Some(PyRep::Tuple(t) | PyRep::List(t)) = args.first() {
                (t.first(), t.get(1))
            } else {
                let agg = kwargs
                    .get("aggressive")
                    .or_else(|| kwargs.get("aggression"))
                    .or_else(|| kwargs.get("1275"))
                    .or_else(|| args.first());
                let ff = kwargs
                    .get("focusFire")
                    .or_else(|| kwargs.get("focus_fire"))
                    .or_else(|| kwargs.get("1297"))
                    .or_else(|| args.get(1));
                (agg, ff)
            };

            if agg_val.is_none() && ff_val.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ChangeDroneSettings requires aggression and focus_fire".into(),
                }));
            }

            let aggressive = match agg_val {
                Some(v) => match parse_bool_or_int(v) {
                    Some(b) => b,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "aggression must be boolean or 0..=1".into(),
                        }));
                    }
                },
                None => true,
            };

            let focus_fire = match ff_val {
                Some(v) => match parse_bool_or_int(v) {
                    Some(b) => b,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "focus_fire must be boolean or 0..=1".into(),
                        }));
                    }
                },
                None => true,
            };

            ctx.db.with_mut_snapshot(|snap| {
                snap.combat_crimewatch_state
                    .drone_settings
                    .insert(char_id, (aggressive, focus_fire));
            });

            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.drone_settings.insert(
                    char_id,
                    crate::services::combat_crimewatch::DroneBehaviorSettings {
                        aggressive,
                        focus_fire,
                    },
                );
            });

            let setting_args = vec![PyRep::Bool(aggressive), PyRep::Bool(focus_fire)];
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDroneStateChange",
                "charid",
                setting_args.clone(),
            );
            session.push_notification("OnDroneStateChange", setting_args.clone());
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDroneSettingChanged",
                "charid",
                setting_args.clone(),
            );
            session.push_notification("OnDroneSettingChanged", setting_args);

            let active_drones = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.owner_char_id == char_id)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            for d in active_drones {
                let notif_args = vec![
                    PyRep::Integer(d.drone_item_id),
                    PyRep::Integer(char_id),
                    PyRep::Integer(d.parent_ship_id),
                    PyRep::Integer(d.activity_state as i64),
                    PyRep::Integer(d.type_id as i64),
                    PyRep::Integer(char_id),
                    PyRep::Integer(d.target_id.unwrap_or(0)),
                ];
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnDroneStateChange",
                    "charid",
                    notif_args.clone(),
                );
                session.push_notification("OnDroneStateChange", notif_args);
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

fn get_item_attribute_value(
    ctx: &ServerContext,
    char_id: i64,
    active_ship_id: i64,
    item_id: i64,
    attr_id: u16,
) -> f64 {
    if item_id == active_ship_id && active_ship_id > 0 {
        if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, item_id) {
            return fit.ship.get_attribute(attr_id, &ctx.sde);
        }
    }
    if active_ship_id > 0 {
        if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, active_ship_id) {
            if let Some(fm) = fit.modules.get(&item_id) {
                if attr_id == attr_ids::DAMAGE {
                    return fm.damage_hp;
                }
                if attr_id == attr_ids::HP_STRUCT {
                    return fm.max_hp;
                }
                if let Some(&v) = fm.dynamic_attribute_overrides.get(&attr_id) {
                    return v;
                }
                if let Some(t) = ctx.sde.types.get(&fm.type_id) {
                    if let Some(&v) = t.default_attributes.get(&attr_id) {
                        return v;
                    }
                }
            }
        }
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(it) = snap.items.get(&item_id) {
            if let Some(&val) = it.dynamic_attributes.get(&attr_id) {
                return val;
            }
            if let Some(t) = ctx.sde.types.get(&it.type_id) {
                if let Some(&val) = t.default_attributes.get(&attr_id) {
                    return val;
                }
            }
        }
        0.0
    })
}

fn get_all_item_attributes(
    ctx: &ServerContext,
    char_id: i64,
    active_ship_id: i64,
    item_id: i64,
) -> HashMap<u16, f64> {
    let mut map = HashMap::new();
    if item_id == active_ship_id && active_ship_id > 0 {
        if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, item_id) {
            let eval_ctx = fit.evaluated_context(&ctx.sde);
            for (&aid, _) in &eval_ctx.ship.base_attributes {
                map.insert(aid, eval_ctx.ship.get_attribute(aid, &ctx.sde));
            }
            for m in &eval_ctx.ship.modifiers {
                map.insert(m.target_attribute_id, eval_ctx.ship.get_attribute(m.target_attribute_id, &ctx.sde));
            }
        }
    } else if active_ship_id > 0 {
        if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, active_ship_id) {
            if let Some(fm) = fit.modules.get(&item_id) {
                if let Some(t) = ctx.sde.types.get(&fm.type_id) {
                    for (&aid, &v) in &t.default_attributes {
                        map.insert(aid, v);
                    }
                }
                for (&aid, &v) in &fm.dynamic_attribute_overrides {
                    map.insert(aid, v);
                }
                map.insert(attr_ids::DAMAGE, fm.damage_hp);
                map.insert(attr_ids::HP_STRUCT, fm.max_hp);
            }
        }
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(it) = snap.items.get(&item_id) {
            if let Some(t) = ctx.sde.types.get(&it.type_id) {
                for (&aid, &v) in &t.default_attributes {
                    map.entry(aid).or_insert(v);
                }
            }
            for (&aid, &v) in &it.dynamic_attributes {
                map.insert(aid, v);
            }
        }
    });
    map
}

/// Parity: F-03 (P1) Verifies that caller owns or controls `item_id`, or has GM role.
fn check_dogma_item_access(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    item_id: i64,
) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() {
        return Ok(());
    }
    if item_id <= 0 {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Cannot query attributes for invalid item ID".into(),
        });
    }
    if session.ship_id == Some(item_id) {
        return Ok(());
    }
    let has_access = ctx.db.with_snapshot(|s| {
        if let Some(item) = s.items.get(&item_id) {
            if item.owner_id == char_id {
                return true;
            }
            if let Some(active_ship) = session.ship_id {
                if item.location_id == active_ship {
                    return true;
                }
            }
            if s.items.get(&item.location_id).is_some_and(|loc| loc.owner_id == char_id) {
                return true;
            }
        }
        false
    }) || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().any(|d| {
            (d.ball_id == item_id || d.drone_item_id == item_id) && d.owner_char_id == char_id
        })
    });

    if has_access {
        Ok(())
    } else {
        Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: format!("Character {char_id} is not authorized to query attributes for item {item_id}"),
        })
    }
}
