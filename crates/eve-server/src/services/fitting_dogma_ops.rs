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

            let rpc = crate::router::RpcArgs::positional(args);
            let source_id = match rpc.req_positive_i64(0, "sourceID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = match rpc.req_positive_i64(1, "targetID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let surface_offset = ship_ball.radius + target_ball.radius;
            let dist = ((ship_ball.position - target_ball.position).length() - surface_offset).max(0.0);
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

            let raw_locks = sys.get_target_locks(source_id).await.unwrap_or_default();
            let mut current_locks = Vec::with_capacity(raw_locks.len());
            for existing_lock in raw_locks {
                let Some((_, tb)) = sys
                    .get_ball_snapshot(existing_lock.target_id)
                    .await
                    .unwrap_or(None)
                else {
                    let _ = sys.remove_target_lock(source_id, existing_lock.target_id).await;
                    continue;
                };
                let tb_untarg = sys
                    .get_combat_state(existing_lock.target_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|c| c.is_cloaked || c.is_warp_exit_invulnerable());
                let tb_warping = matches!(tb.mode, eve_destiny::MovementMode::Warp { .. });
                let tb_out_of_range = existing_lock
                    .max_range_m
                    .is_some_and(|max_r| (ship_ball.position - tb.position).length() > max_r);
                if tb_untarg || tb_warping || tb_out_of_range {
                    let _ = sys.remove_target_lock(source_id, existing_lock.target_id).await;
                    continue;
                }
                current_locks.push(existing_lock);
            }
            if let Some(existing) = current_locks.iter().find(|l| l.target_id == target_id) {
                if existing.locked {
                    let locked_ids: Vec<PyRep> = current_locks
                        .iter()
                        .filter(|l| l.locked)
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::Tuple(vec![
                        PyRep::Integer(0),
                        PyRep::List(locked_ids),
                    ])));
                }
            } else if current_locks.len() >= max_locked {
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
                max_range_m: Some(max_range + surface_offset),
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
                PyRep::Integer(1),
                PyRep::List(Vec::new()),
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
            let mut removed = false;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                removed = sys.remove_target_lock(ship_id, target_id).await.unwrap_or(false);
            }
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(target_id));
            crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(ctx, ship_id, char_id, Some(target_id));
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
            );
            if removed {
                let _ = ctx.notify_target_events(char_id, &[("lost", target_id, "")]);
            }
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.RemoveTargetOBO
        "RemoveTargetOBO" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let source_id = match rpc.req_positive_i64(0, "sourceID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = match rpc.req_positive_i64(1, "targetID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

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

            let before_ship_attrs =
                crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs(
                    ctx, &fit,
                );
            let mut newly_onlined = Vec::new();
            for mid in mod_ids {
                let is_online = ctx.db.with_snapshot(|snap| {
                    snap.module_online_states.get(&mid).copied().unwrap_or(true)
                });
                if !is_online && fit.set_module_online(mid, &ctx.sde).is_ok() {
                    newly_onlined.push(mid);
                }
            }
            let after_ship_attrs =
                crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs(
                    ctx, &fit,
                );

            let committed_onlined = match ctx.db.try_with_mut_snapshot(|snap| {
                let ch_ship = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                if ch_ship != ship_id
                    || snap
                        .items
                        .get(&ship_id)
                        .is_none_or(|sh| sh.owner_id != char_id)
                {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is no longer the active owned ship of {char_id}"),
                    });
                }
                let mut valid = Vec::with_capacity(newly_onlined.len());
                for &mid in &newly_onlined {
                    let still_fitted = snap.items.get(&mid).is_some_and(|it| {
                        it.location_id == ship_id && (11..=34).contains(&(it.flag as u16))
                    });
                    if still_fitted {
                        snap.module_online_states.insert(mid, true);
                        valid.push(mid);
                    }
                }
                Ok::<Vec<i64>, BridgeRpcError>(valid)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.store_ship_fitting(ship_id, fit);
            ctx.invalidate_ship_fitting(ship_id);

            let any_onlined = !committed_onlined.is_empty();
            for mid in committed_onlined {
                let _ = ctx.notify_module_online_state(char_id, mid, true);
            }
            if any_onlined {
                crate::services::notify_emitters_skills_trade_godma::emit_ship_fitting_attr_changes(
                    ctx,
                    char_id,
                    ship_id,
                    &before_ship_attrs,
                    &after_ship_attrs,
                );
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
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairNaniteWhileDocked".into(),
                    message: "Nanite Repair Paste can only be used in space; use station/structure repair services while docked".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no solarSystemID".into(),
                }));
            };
            let sys = ctx.systems.get_or_spawn(sys_id);

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

            let in_space_overheating = session.station_id.is_none()
                && session.structure_id.is_some() == false
                && session
                    .solar_system_id
                    .is_some_and(|sid| ctx.systems.get_or_spawn(sid).has_active_overloaded_cycle_sync(ship_id));
            if in_space_overheating {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairWhileOverheating".into(),
                    message: "UserError: Cannot initiate in-space Nanite Repair Paste repair while modules are overheating".into(),
                }));
            }

            for module_id in mod_ids {
                let (is_valid, is_active, cur_dmg, max_hp, mod_tid, thermo_lvl, nanite_op_lvl, nanite_interfacing_lvl) =
                    ctx.db.with_snapshot(|snap| {
                        let active = snap.module_active_states.contains(&module_id);
                        let m_opt = snap.items.get(&module_id);
                        let is_valid = m_opt.map_or(false, |it| {
                            let is_ehe = matches!(it.type_id, 40714..=40718)
                                || ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.effect_ids.contains(&6484));
                            it.owner_id == char_id
                                && it.location_id == ship_id
                                && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
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
                        let mod_tid = m_opt.map(|i| i.type_id).unwrap_or(0);
                        let sk = snap.skills.get(&char_id);
                        let thermo_lvl = sk.and_then(|m| m.get(&28164)).map(|r| r.level).unwrap_or(0);
                        let op_lvl = sk.and_then(|m| m.get(&28879)).map(|r| r.level).unwrap_or(0);
                        let int_lvl = sk.and_then(|m| m.get(&28880)).map(|r| r.level).unwrap_or(0);
                        (is_valid, active, dmg, hp, mod_tid, thermo_lvl, op_lvl, int_lvl)
                    });

                if !is_valid
                    || is_active
                    || sys.has_active_module_cycle_sync(ship_id, module_id)
                    || cur_dmg >= max_hp - 1e-6
                    || sys.has_active_nanite_repair_sync(ship_id, module_id)
                {
                    failed.push(PyRep::Integer(module_id));
                    continue;
                }
                if cur_dmg <= 1e-6 {
                    skipped.push(PyRep::Integer(module_id));
                    continue;
                }

                let (paste_needed, _rate, duration_s) =
                    super::fitting_mining_lasers::compute_nanite_repair_params(
                        ctx.sde.types.get(&mod_tid),
                        cur_dmg,
                        max_hp,
                        thermo_lvl,
                        nanite_op_lvl,
                        nanite_interfacing_lvl,
                    );

                let initial_paste = 1_i32.min(paste_needed.max(1));
                let deduct_ok = ctx.db.try_with_mut_snapshot(|snap| {
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
                    if avail < initial_paste {
                        return Err(());
                    }
                    crate::services::tactical_module_mechanics::deduct_paste_from_cargo_in_snap(
                        snap,
                        char_id,
                        ship_id,
                        initial_paste,
                    );
                    Ok(())
                });

                if deduct_ok.is_ok() {
                    let _ = sys
                        .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
                            char_id,
                            ship_id,
                            module_id,
                            initial_damage_hp: cur_dmg,
                            paste_total_needed: paste_needed.max(1),
                            paste_consumed_so_far: initial_paste,
                            duration_s,
                            elapsed_s: 0.0,
                        })
                        .await;
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
                "OnDroneSettingChanged",
                "charid",
                setting_args,
            );

            let active_drones = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.owner_char_id == char_id)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            for d in active_drones {
                let target_rep = d.target_id.map(PyRep::Integer).unwrap_or(PyRep::None);
                let notif_args = vec![
                    PyRep::Integer(d.ball_id),
                    PyRep::Integer(char_id),
                    PyRep::Integer(d.parent_ship_id),
                    PyRep::Integer(d.activity_state as i64),
                    PyRep::Integer(d.type_id as i64),
                    PyRep::Integer(char_id),
                    target_rep,
                ];
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnDroneStateChange",
                    "charid",
                    notif_args,
                );
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

fn resolve_parent_ship_for_dogma_item(
    ctx: &ServerContext,
    char_id: i64,
    active_ship_id: i64,
    item_id: i64,
) -> (Option<eve_db::InventoryItemRecord>, Option<(i64, i64)>) {
    let (item_opt, db_parent) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&item_id).cloned();
        let parent = if let Some(ref item) = it {
            let item_cat = ctx.sde.types.get(&item.type_id).map(|t| t.category_id);
            if matches!(item_cat, Some(6 | 65)) {
                Some((item.owner_id, item.item_id))
            } else if active_ship_id > 0 && item.location_id == active_ship_id {
                Some((char_id, active_ship_id))
            } else if let Some(loc_item) = snap.items.get(&item.location_id) {
                let loc_cat = ctx.sde.types.get(&loc_item.type_id).map(|t| t.category_id);
                matches!(loc_cat, Some(6 | 65)).then_some((loc_item.owner_id, loc_item.item_id))
            } else {
                None
            }
        } else {
            None
        };
        (it, parent)
    });
    if db_parent.is_some() {
        return (item_opt, db_parent);
    }
    if let Some(sd_parent) = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.ball_id == item_id || d.drone_item_id == item_id)
            .map(|d| (d.owner_char_id, d.parent_ship_id))
    }) {
        return (item_opt, Some(sd_parent));
    }
    if let Some(probe_parent) = ctx
        .active_scanner_probes
        .read()
        .ok()
        .and_then(|m| m.get(&item_id).map(|p| (p.owner_char_id, p.ship_id)))
    {
        return (item_opt, Some(probe_parent));
    }
    let fallback_parent = (active_ship_id > 0).then_some((char_id, active_ship_id));
    (item_opt, fallback_parent)
}

fn evaluate_drone_or_probe_state_with_overrides(
    ctx: &ServerContext,
    fit: &eve_dogma::ShipFittingEngine,
    item_id: i64,
    item_opt: Option<&eve_db::InventoryItemRecord>,
) -> Option<eve_dogma::DogmaItemState> {
    let mut st = fit.drone_state(item_id, &ctx.sde)?;
    if let Some(it) = item_opt {
        for (&aid, &val) in &it.dynamic_attributes {
            if !matches!(aid, 3 | 18 | 265 | 266) {
                st.base_attributes.insert(aid, val);
            }
        }
    }
    let has_active_triage = fit.modules.values().any(|fm| {
        matches!(fm.type_id, 27951 | 4294)
            && matches!(
                fm.state,
                eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
            )
    });
    if has_active_triage && st.base_attributes.contains_key(&64) {
        st.base_attributes.insert(64, 0.0);
        st.modifiers.retain(|m| m.target_attribute_id != 64);
    }
    Some(st)
}

fn get_item_attribute_value(
    ctx: &ServerContext,
    char_id: i64,
    active_ship_id: i64,
    item_id: i64,
    attr_id: u16,
) -> f64 {
    if let Some(transient_val) = ctx.db.with_snapshot(|snap| {
        snap.transient_item_attributes
            .get(&item_id)
            .and_then(|tr| tr.get(&attr_id).copied())
    }) {
        return transient_val;
    }

    let (item_opt, parent_ship_opt) =
        resolve_parent_ship_for_dogma_item(ctx, char_id, active_ship_id, item_id);

    if let Some((owner_id, ship_id)) = parent_ship_opt {
        if item_id == ship_id && ship_id > 0 {
            if matches!(attr_id, 1175 | 1176 | 1177 | 3 | 18 | 265 | 266) {
                if let Some(&live_val) =
                    item_opt.as_ref().and_then(|it| it.dynamic_attributes.get(&attr_id))
                {
                    return live_val;
                }
            }
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id) {
                let mut ship_attrs = HashMap::from([(attr_id, fit.ship.get_attribute(attr_id, &ctx.sde))]);
                let already_applied: Vec<u32> =
                    fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
                crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
                    ctx,
                    ship_id,
                    &already_applied,
                    &mut ship_attrs,
                );
                return ship_attrs.get(&attr_id).copied().unwrap_or(0.0);
            }
        } else if ship_id > 0 {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id) {
                if let Some(fm) = fit.modules.get(&item_id) {
                    if attr_id == attr_ids::DAMAGE {
                        return item_opt
                            .as_ref()
                            .and_then(|it| it.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
                            .unwrap_or(fm.damage_hp);
                    }
                    if attr_id == attr_ids::HP_STRUCT {
                        return fm.max_hp;
                    }
                    if attr_id == 2 {
                        return if fm.state != eve_dogma::ModuleState::Offline {
                            1.0
                        } else {
                            0.0
                        };
                    }
                    let mut m_attrs = HashMap::from([(
                        attr_id,
                        fit.get_module_effective_attribute(item_id, attr_id, &ctx.sde),
                    )]);
                    if fm.state != eve_dogma::ModuleState::Offline {
                        crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                            ctx,
                            ship_id,
                            &mut m_attrs,
                        );
                    }
                    return m_attrs.get(&attr_id).copied().unwrap_or(0.0);
                }

                if let Some(ref it) = item_opt {
                    let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id);
                    if cat == Some(8) && it.location_id == ship_id {
                        if attr_id == attr_ids::DAMAGE {
                            return it
                                .dynamic_attributes
                                .get(&attr_ids::DAMAGE)
                                .copied()
                                .unwrap_or(0.0);
                        }
                        let slot_flag = it.flag as u16;
                        if let Some(launcher_id) = fit
                            .modules
                            .values()
                            .find(|m| m.flag_id == slot_flag)
                            .map(|m| m.item_id)
                        {
                            if let Some(c_state) = fit.charge_state(launcher_id, &ctx.sde) {
                                return c_state.get_attribute(attr_id, &ctx.sde);
                            }
                        }
                    }
                }

                if let Some(d_state) =
                    evaluate_drone_or_probe_state_with_overrides(ctx, &fit, item_id, item_opt.as_ref())
                {
                    if matches!(attr_id, 3 | 18 | 265 | 266) {
                        if let Some(&live_val) =
                            item_opt.as_ref().and_then(|it| it.dynamic_attributes.get(&attr_id))
                        {
                            return live_val;
                        }
                    }
                    return d_state.get_attribute(attr_id, &ctx.sde);
                }
            }
        }
    }

    if let Some(it) = item_opt {
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
}

fn get_all_item_attributes(
    ctx: &ServerContext,
    char_id: i64,
    active_ship_id: i64,
    item_id: i64,
) -> HashMap<u16, f64> {
    let mut map = HashMap::new();
    let (item_opt, parent_ship_opt) =
        resolve_parent_ship_for_dogma_item(ctx, char_id, active_ship_id, item_id);
    let mut evaluated_via_dogma = false;

    if let Some((owner_id, ship_id)) = parent_ship_opt {
        if item_id == ship_id && ship_id > 0 {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id) {
                for (&aid, _) in &fit.ship.base_attributes {
                    map.insert(aid, fit.ship.get_attribute(aid, &ctx.sde));
                }
                for m in &fit.ship.modifiers {
                    map.insert(
                        m.target_attribute_id,
                        fit.ship.get_attribute(m.target_attribute_id, &ctx.sde),
                    );
                }
                if let Some(ref it) = item_opt {
                    for (&aid, &v) in &it.dynamic_attributes {
                        if matches!(aid, 3 | 18 | 265 | 266 | 1175 | 1176 | 1177)
                            || !fit.ship.modifiers.iter().any(|m| m.target_attribute_id == aid)
                        {
                            map.insert(aid, v);
                        }
                    }
                }
                let already_applied: Vec<u32> =
                    fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
                crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
                    ctx,
                    ship_id,
                    &already_applied,
                    &mut map,
                );
                evaluated_via_dogma = true;
            }
        } else if ship_id > 0 {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id) {
                if let Some(fm) = fit.modules.get(&item_id) {
                    if let Some(m_state) = fit.module_state(item_id, &ctx.sde) {
                        for (&aid, _) in &m_state.base_attributes {
                            map.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                        }
                        for m in &m_state.modifiers {
                            map.insert(
                                m.target_attribute_id,
                                m_state.get_attribute(m.target_attribute_id, &ctx.sde),
                            );
                        }
                        crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                            ctx,
                            ship_id,
                            &mut map,
                        );
                        map.insert(2, 1.0);
                    } else {
                        if let Some(t) = ctx.sde.types.get(&fm.type_id) {
                            for (&aid, &v) in &t.default_attributes {
                                map.insert(aid, v);
                            }
                        }
                        for (&aid, &v) in &fm.dynamic_attribute_overrides {
                            map.insert(aid, v);
                        }
                        map.insert(2, 0.0);
                    }
                    let live_dmg = item_opt
                        .as_ref()
                        .and_then(|it| it.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
                        .unwrap_or(fm.damage_hp);
                    map.insert(attr_ids::DAMAGE, live_dmg);
                    map.insert(attr_ids::HP_STRUCT, fm.max_hp);
                    evaluated_via_dogma = true;
                } else if let Some(ref it) = item_opt {
                    let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id);
                    if cat == Some(8) && it.location_id == ship_id {
                        let slot_flag = it.flag as u16;
                        if let Some(launcher_id) = fit
                            .modules
                            .values()
                            .find(|m| m.flag_id == slot_flag)
                            .map(|m| m.item_id)
                        {
                            if let Some(c_state) = fit.charge_state(launcher_id, &ctx.sde) {
                                for (&aid, _) in &c_state.base_attributes {
                                    map.insert(aid, c_state.get_attribute(aid, &ctx.sde));
                                }
                                for m in &c_state.modifiers {
                                    map.insert(
                                        m.target_attribute_id,
                                        c_state.get_attribute(m.target_attribute_id, &ctx.sde),
                                    );
                                }
                                map.insert(
                                    attr_ids::DAMAGE,
                                    it.dynamic_attributes
                                        .get(&attr_ids::DAMAGE)
                                        .copied()
                                        .unwrap_or(0.0),
                                );
                                evaluated_via_dogma = true;
                            }
                        }
                    }
                }

                if !evaluated_via_dogma {
                    if let Some(d_state) = evaluate_drone_or_probe_state_with_overrides(
                        ctx,
                        &fit,
                        item_id,
                        item_opt.as_ref(),
                    ) {
                        for (&aid, _) in &d_state.base_attributes {
                            map.insert(aid, d_state.get_attribute(aid, &ctx.sde));
                        }
                        for m in &d_state.modifiers {
                            map.insert(
                                m.target_attribute_id,
                                d_state.get_attribute(m.target_attribute_id, &ctx.sde),
                            );
                        }
                        if let Some(ref it) = item_opt {
                            for &aid in &[3u16, 18, 265, 266] {
                                if let Some(&v) = it.dynamic_attributes.get(&aid) {
                                    map.insert(aid, v);
                                }
                            }
                            if !it.dynamic_attributes.contains_key(&attr_ids::DAMAGE) {
                                map.entry(attr_ids::DAMAGE).or_insert(0.0);
                            }
                        } else {
                            map.entry(attr_ids::DAMAGE).or_insert(0.0);
                        }
                        evaluated_via_dogma = true;
                    }
                }
            }
        }
    }

    if !evaluated_via_dogma {
        if let Some(it) = item_opt {
            if let Some(t) = ctx.sde.types.get(&it.type_id) {
                for (&aid, &v) in &t.default_attributes {
                    map.entry(aid).or_insert(v);
                }
            }
            if !it.dynamic_attributes.contains_key(&attr_ids::DAMAGE)
                && item_id != active_ship_id
            {
                map.insert(attr_ids::DAMAGE, 0.0);
            }
            for (&aid, &v) in &it.dynamic_attributes {
                map.insert(aid, v);
            }
        }
    }

    crate::services::tactical_modules::apply_transient_item_attrs(ctx, item_id, &mut map);
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
    }) || ctx
        .active_scanner_probes
        .read()
        .ok()
        .is_some_and(|m| m.get(&item_id).is_some_and(|p| p.owner_char_id == char_id));

    if has_access {
        Ok(())
    } else {
        Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: format!("Character {char_id} is not authorized to query attributes for item {item_id}"),
        })
    }
}

#[cfg(test)]
#[path = "fitting_dogma_ops_tests.rs"]
mod fitting_dogma_ops_tests;

