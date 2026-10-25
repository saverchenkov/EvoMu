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

            let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (source_id, target_id) = if a1 == 0 {
                (session.ship_id.unwrap_or(0), a0)
            } else if session.ship_id == Some(a0) {
                (a0, a1)
            } else if session.ship_id == Some(a1) {
                (a1, a0)
            } else {
                let a0_is_source = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&a0).map_or(false, |it| it.owner_id == char_id));
                if a0_is_source {
                    (a0, a1)
                } else {
                    let active_ship = session.ship_id.unwrap_or(a0);
                    (active_ship, if a0 == active_ship { a1 } else { a0 })
                }
            };

            if source_id == 0 || target_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddTargetOBO requires targetID and sourceID".into(),
                }));
            }
            if target_id == source_id {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: "Cannot target self".into(),
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

            let fit_res = ctx.get_or_rebuild_ship_fitting(char_id, source_id);

            if let Ok(Some(cs)) = sys.get_combat_state(source_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "ShipJammed".into(),
                        message: "Targeting systems are jammed by ECM".into(),
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
            if let Err(e) = sys.add_target_lock(source_id, lock).await {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: e,
                }));
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
            let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (source_id, target_id) = if a1 == 0 {
                (session.ship_id.unwrap_or(0), a0)
            } else if session.ship_id == Some(a0) {
                (a0, a1)
            } else if session.ship_id == Some(a1) {
                (a1, a0)
            } else {
                let a0_is_source = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&a0).map_or(false, |it| it.owner_id == char_id));
                if a0_is_source {
                    (a0, a1)
                } else {
                    let active_ship = session.ship_id.unwrap_or(a0);
                    (active_ship, if a0 == active_ship { a1 } else { a0 })
                }
            };

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
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnTarget",
                        "charid",
                        vec![PyRep::Integer(tid), PyRep::String("lost".into())],
                    );
                } else {
                    let _ = sys.clear_target_locks(ship_id).await;
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnTargetClear",
                        "charid",
                        vec![],
                    );
                }
            }
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, None);
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.QueryAttributeValue
        "QueryAttributeValue" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
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

            let is_docked = session.station_id.is_some() || session.structure_id.is_some();

            for module_id in mod_ids {
                let (is_active, cur_dmg, max_hp, base_paste_cost, nanite_op_lvl, nanite_interfacing_lvl) =
                    ctx.db.with_snapshot(|snap| {
                        let active = snap.module_active_states.contains(&module_id);
                        let m_opt = snap.items.get(&module_id);
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
                        (active, dmg, hp, base_cost, op_lvl, int_lvl)
                    });

                if is_active {
                    failed.push(PyRep::Integer(module_id));
                    continue;
                }
                if cur_dmg <= 1e-6 {
                    skipped.push(PyRep::Integer(module_id));
                    continue;
                }
                if cur_dmg >= max_hp - 1e-6 {
                    failed.push(PyRep::Integer(module_id));
                    continue;
                }

                if is_docked {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(it) = snap.items.get_mut(&module_id) {
                            it.dynamic_attributes.insert(attr_ids::DAMAGE, 0.0);
                        }
                    });
                    repaired.push(PyRep::Integer(module_id));
                } else {
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
                        }
                        repaired.push(PyRep::Integer(module_id));
                    } else {
                        failed.push(PyRep::Integer(module_id));
                    }
                }
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

            let (aggressive, focus_fire) = if let Some(PyRep::Dict(d)) = args.first() {
                let agg = d
                    .iter()
                    .find(|(k, _)| k.as_str() == Some("aggressive"))
                    .and_then(|(_, v)| v.as_bool())
                    .unwrap_or(true);
                let ff = d
                    .iter()
                    .find(|(k, _)| k.as_str() == Some("focusFire"))
                    .and_then(|(_, v)| v.as_bool())
                    .unwrap_or(true);
                (agg, ff)
            } else if let Some(PyRep::Tuple(t)) | Some(PyRep::List(t)) = args.first() {
                let agg = t.first().and_then(|v| v.as_bool()).unwrap_or(true);
                let ff = t.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
                (agg, ff)
            } else {
                let agg = kwargs
                    .get("aggressive")
                    .and_then(|v| v.as_bool())
                    .or_else(|| args.first().and_then(|a| a.as_bool()))
                    .unwrap_or(true);
                let ff = kwargs
                    .get("focusFire")
                    .and_then(|v| v.as_bool())
                    .or_else(|| args.get(1).and_then(|a| a.as_bool()))
                    .unwrap_or(true);
                (agg, ff)
            };

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
                "OnDroneSettingChanged",
                "charid",
                setting_args.clone(),
            );
            session.push_notification("OnDroneSettingChanged", setting_args);

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
