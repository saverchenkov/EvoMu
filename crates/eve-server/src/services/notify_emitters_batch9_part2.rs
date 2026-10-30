//! `notify_emitters_batch9_part2` — Server-side V24 notification emitters & handlers for Batch 9 (Part 2:
//! Trade, Market Repackage Errors, Godma, Inventory Cache, Structures, Industry Materials, Jump Queue,
//! Browser Lockdown, Cluster Notifications/Messages/Service Mapping, and Localization Text Cache).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use crate::services::notify_emitters_batch3::{
    emit_ccp_notification, emit_industry_materials_consumed, emit_invalidate_cache_for_locations,
};
use crate::services::notify_emitters_batch3_space::{
    emit_module_activation_godma_notifications, flush_module_profile_and_ewar_end,
};
use eve_db::OwnerNoteRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Emits `OnPrimingNeededForTradeItems(itemIDsToPrime)` (`1` arg, `id_type = "charid"`) to the trade partner.
// parity: notify:OnPrimingNeededForTradeItems
pub fn emit_priming_needed_for_trade_items(
    ctx: &ServerContext,
    partner_char_id: i64,
    item_ids: &[i64],
) {
    if item_ids.is_empty() || partner_char_id <= 0 {
        return;
    }
    let list = item_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(partner_char_id),
        "OnPrimingNeededForTradeItems",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Emits `OnSellRepackageErrors(repackageErrorCount)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnSellRepackageErrors
pub fn emit_sell_repackage_errors(
    ctx: &ServerContext,
    char_id: i64,
    error_code: &str,
    count: i64,
) {
    if count <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSellRepackageErrors",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::String(error_code.to_string()),
            PyRep::Integer(count),
        )])],
    );
}

/// Extracted `dogmaIM.SetModuleOnline` handler emitting `OnGodmaPrimeItem` and `OnGodmaEffectInfo`.
// parity: notify:OnGodmaEffectInfo, notify:OnGodmaPrimeItem
pub async fn handle_dogma_set_module_online(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let module_id = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .unwrap_or(0);
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
        return crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
            ctx, session, ship_id, module_id, true,
        );
    }
    let (owns_ship, mod_type_id) = ctx.db.with_snapshot(|s| {
        let owns = s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id);
        let mtid = s.items.get(&module_id).map(|m| m.type_id).unwrap_or(0);
        (owns, mtid)
    });
    if !owns_ship {
        return Err(err(
            "ShipOwnershipViolation",
            format!("Ship {ship_id} is not owned by character {char_id}"),
        ));
    }

    let is_burned = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&module_id);
        let dmg = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
            .unwrap_or(0.0);
        let hp = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            .or_else(|| {
                it.and_then(|i| ctx.sde.types.get(&i.type_id))
                    .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        dmg >= hp - 1e-6
    });
    if is_burned && session.station_id.is_none() && session.structure_id.is_none() {
        return Err(err(
            "ModuleBurnedOut",
            format!("Module {module_id} is burned out and cannot be onlined in space"),
        ));
    }

    if session.station_id.is_none() && session.structure_id.is_none() {
        if let Some(sys_id) = session.solar_system_id {
            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.max_capacitor > 0.0 && cs.capacitor < 0.75 * cs.max_capacitor {
                    return Err(err(
                        "NotEnoughCapacitorToOnline",
                        format!(
                            "Capacitor level ({:.1} GJ / {:.1} GJ) is below 75% requirement to online modules in space",
                            cs.capacitor, cs.max_capacitor
                        ),
                    ));
                }
            }
        }
    }

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    if let Err(e) = fit.set_module_online(module_id, &ctx.sde) {
        return Err(err("FittingValidationFailed", e));
    }
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(module_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let _ = ctx.notify_module_online_state(char_id, module_id, true);
    if mod_type_id > 0 {
        emit_module_activation_godma_notifications(
            ctx,
            char_id,
            ship_id,
            module_id,
            mod_type_id,
            "online",
            1.0,
            None,
        );
    }

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("online", PyRep::Bool(true)),
    ]))
}

/// Extracted `dogmaIM.TakeModuleOffline` handler emitting `OnGodmaFlushLocationProfile`.
// parity: notify:OnGodmaFlushLocationProfile
pub async fn handle_dogma_take_module_offline(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let module_id = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .unwrap_or(0);
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
        return crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
            ctx, session, ship_id, module_id, false,
        );
    }
    let owns_ship = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
    if !owns_ship {
        return Err(err(
            "ShipOwnershipViolation",
            format!("Ship {ship_id} is not owned by character {char_id}"),
        ));
    }

    let is_burned = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&module_id);
        let dmg = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
            .unwrap_or(0.0);
        let hp = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            .or_else(|| {
                it.and_then(|i| ctx.sde.types.get(&i.type_id))
                    .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        dmg >= hp - 1e-6
    });
    if is_burned && session.station_id.is_none() && session.structure_id.is_none() {
        return Err(err(
            "ModuleBurnedOut",
            format!("Module {module_id} is burned out and cannot be offlined in space"),
        ));
    }

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let was_active_type = fit
        .modules
        .get(&module_id)
        .filter(|m| matches!(m.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded))
        .map(|m| m.type_id);
    if let Err(e) = fit.take_module_offline(module_id, &ctx.sde) {
        return Err(err("FittingValidationFailed", e));
    }
    if let Some(m_tid) = was_active_type {
        let mod_state = fit.build_effective_module_state(module_id, m_tid, false, &ctx.sde);
        let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
        if react_delay_ms > 0.0 {
            ctx.record_module_reactivation_delay(ship_id, module_id, react_delay_ms / 1000.0);
        }
    }
    let offline_mod_ids: Vec<i64> = fit
        .modules
        .values()
        .filter(|m| m.state == eve_dogma::ModuleState::Offline)
        .map(|m| m.item_id)
        .collect();
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(module_id, false);
        snap.module_active_states.remove(&module_id);
        for &mid in &offline_mod_ids {
            snap.module_online_states.insert(mid, false);
            snap.module_active_states.remove(&mid);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    if let Some(sys_id) = session.solar_system_id {
        let sys = ctx.systems.get_or_spawn(sys_id);
        let is_cycle_gated = ctx
            .db
            .with_snapshot(|s| s.items.get(&module_id).map(|i| i.type_id))
            .and_then(|tid| ctx.sde.types.get(&tid))
            .is_some_and(|t| {
                matches!(
                    t.group_id,
                    40 | 1156 | 41 | 1697 | 67 | 71 | 76 | 72 | 1122 | 650 | 80 | 353
                )
            });
        if let Ok(Some(rem_delay)) = sys.remove_module_cycle(ship_id, module_id).await {
            if is_cycle_gated && rem_delay > 0.0 {
                ctx.record_module_reactivation_delay(ship_id, module_id, rem_delay);
            }
        }
        for &mid in &offline_mod_ids {
            if mid != module_id {
                let _ = sys.remove_module_cycle(ship_id, mid).await;
            }
        }
    }

    let _ = ctx.notify_module_online_state(char_id, module_id, false);
    let _ = flush_module_profile_and_ewar_end(ctx, char_id, session.solar_system_id, ship_id, module_id);

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("online", PyRep::Bool(false)),
    ]))
}

/// Emits `OnGodmaEffectInfo` for a module activation event.
// parity: notify:OnGodmaEffectInfo
pub fn emit_godma_effect_info_for_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: TypeId,
    effect_name: &str,
    duration_s: f64,
    target_id_opt: Option<i64>,
) {
    emit_module_activation_godma_notifications(
        ctx,
        char_id,
        ship_id,
        module_id,
        module_type_id,
        effect_name,
        duration_s,
        target_id_opt,
    );
}

/// Handles bound inventory broker `InvalidateLocationCache`, validating access and emitting `OnInvalidateCacheForLocations`.
// parity: notify:OnInvalidateCacheForLocations
pub fn handle_invalidate_location_cache_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(location_id) = bound_entry.map(|b| b.object_id) else {
        return Err(err(
            "InvalidBoundObject",
            "InvalidateLocationCache must be called on a bound inventory manager",
        ));
    };
    let accessible = ctx.db.with_snapshot(|snap| {
        ctx.sde.stations.contains_key(&location_id)
            || snap.structures.contains_key(&location_id)
            || snap
                .items
                .get(&location_id)
                .is_some_and(|it| it.owner_id == char_id)
    });
    if !accessible {
        return Err(err(
            "DestinationAccessDenied",
            format!("Location {location_id} is not accessible for cache invalidation"),
        ));
    }
    emit_invalidate_cache_for_locations(
        ctx,
        NotificationTarget::Character(char_id),
        &[location_id],
    );
    Ok(PyRep::Bool(true))
}

/// Emits V24-conformant 6-column `OnCfgDataChanged("evelocations", [locationID, locationName, x, y, z, None])`
/// (`id_type = "solarsystemid"`) matching `config_service.rs` `evelocations` schema.
// parity: notify:OnCfgDataChanged
pub fn emit_structure_cfg_data_changed(
    ctx: &ServerContext,
    sys_id: u32,
    structure_id: i64,
    new_name: &str,
    pos: [f64; 3],
) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCfgDataChanged",
        "solarsystemid",
        vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::String(new_name.to_string()),
                PyRep::Real(pos[0]),
                PyRep::Real(pos[1]),
                PyRep::Real(pos[2]),
                PyRep::None,
            ]),
        ],
    );
}

/// Handles `industryManager.GetJobMaterials` / `LoadJobMaterials`, resolving SDE blueprint/type
/// materials for an existing industry job and emitting `OnIndustryMaterials(jobID, materials)`.
// parity: notify:OnIndustryMaterials
pub fn handle_industry_batch9(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "industryManager" || method != "GetJob" {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if job_id <= 0 {
        return Some(Err(err(
            "InvalidJobID",
            format!("{method} requires a positive jobID"),
        )));
    }
    let job_res = ctx.db.with_snapshot(|snap| {
        let Some(job) = snap.industry_jobs.get(&job_id) else {
            return Err(err("JobNotFound", format!("Industry job {job_id} not found")));
        };
        let char_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if job.installer_id != char_id && job.installer_id != char_corp && !session.has_gm_role() {
            return Err(err(
                "JobOwnershipViolation",
                format!("Character {char_id} does not own industry job {job_id}"),
            ));
        }
        let runs = i32::try_from(job.runs.max(1)).unwrap_or(1);
        let mut materials: Vec<(TypeId, i32)> = if let Some(bp) = ctx.sde.blueprints.get(&job.blueprint_type_id) {
            let act_mats = bp
                .activities
                .get(&job.activity_id.to_string())
                .map(|a| a.materials.as_slice())
                .filter(|m| !m.is_empty())
                .unwrap_or(bp.materials.as_slice());
            act_mats
                .iter()
                .map(|m| {
                    let qty = i32::try_from(m.quantity)
                        .unwrap_or(1)
                        .saturating_mul(runs)
                        .max(1);
                    (m.type_id, qty)
                })
                .collect()
        } else {
            Vec::new()
        };
        if materials.is_empty() {
            if let Some(tmats) = ctx.sde.type_materials.get(&job.product_type_id) {
                materials = tmats
                    .iter()
                    .map(|m| (m.material_type_id, (m.quantity.max(1) as i32).saturating_mul(runs)))
                    .collect();
            }
        }
        Ok((job.installer_id, materials))
    });
    let (_owner_id, materials) = match job_res {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    emit_industry_materials_consumed(ctx, char_id, job_id, &materials);
    let rows: Vec<PyRep> = materials
        .into_iter()
        .map(|(tid, qty)| {
            PyRep::keyval([
                ("typeID", PyRep::Integer(i64::from(tid))),
                ("quantity", PyRep::Integer(i64::from(qty))),
                ("missing", PyRep::Integer(0)),
            ])
        })
        .collect();
    Some(Ok(PyRep::List(rows)))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JumpQueueSystemConfig {
    threshold: usize,
    key_life: i64,
    queue: Vec<i64>,
    jump_keys: HashMap<i64, i64>,
}

/// Emits V24-conformant `OnJumpQueueUpdate(solarsystemID, space, queue, jumpKeys, keyLife)`
/// where `queue` is `List([charID, ...])` and `jumpKeys` is `Dict({charID: issuedFtTicks})`.
// parity: notify:OnJumpQueueUpdate
pub fn emit_jump_queue_update_v24(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    space: i64,
    queue: &[i64],
    jump_keys: &[(i64, i64)],
    key_life: i64,
) {
    let queue_rep = PyRep::List(queue.iter().copied().map(PyRep::Integer).collect());
    let keys_rep = PyRep::Dict(
        jump_keys
            .iter()
            .map(|&(cid, issued_ft)| (PyRep::Integer(cid), PyRep::Integer(issued_ft)))
            .collect(),
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpQueueUpdate",
        "charid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(space),
            queue_rep,
            keys_rep,
            PyRep::Integer(key_life),
        ],
    );
}

/// Evaluates whether `dest_sys_id` has an active jump-queue threshold on stargate jump and emits `OnJumpQueueUpdate`.
// parity: notify:OnJumpQueueUpdate
pub fn check_and_notify_stargate_jump_queue(
    ctx: &ServerContext,
    char_id: i64,
    dest_sys_id: u32,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let update_opt = ctx.db.with_mut_snapshot(|snap| {
        let mut cfgs: HashMap<u32, JumpQueueSystemConfig> = snap
            .service_state_blobs
            .get("jump_queue_configs")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let cfg = cfgs.get_mut(&dest_sys_id)?;
        if cfg.threshold == 0 {
            return None;
        }
        let pilots_in_dest = snap
            .characters
            .values()
            .filter(|c| c.solar_system_id == dest_sys_id)
            .count();
        if pilots_in_dest < cfg.threshold && !cfg.queue.contains(&char_id) {
            return None;
        }
        if !cfg.queue.contains(&char_id) {
            cfg.queue.push(char_id);
        }
        let key_life = cfg.key_life.max(30);
        cfg.jump_keys.insert(char_id, now_ft);
        let space = (cfg.threshold as i64).saturating_sub(pilots_in_dest as i64).max(0);
        let queue_vec = cfg.queue.clone();
        let mut keys_vec: Vec<(i64, i64)> = cfg.jump_keys.iter().map(|(&k, &v)| (k, v)).collect();
        keys_vec.sort_by_key(|&(k, _)| k);
        if let Ok(json) = serde_json::to_string(&cfgs) {
            snap.service_state_blobs
                .insert("jump_queue_configs".into(), json);
        }
        Some((space, queue_vec, keys_vec, key_life))
    });
    if let Some((space, queue_vec, keys_vec, key_life)) = update_opt {
        emit_jump_queue_update_v24(ctx, char_id, dest_sys_id, space, &queue_vec, &keys_vec, key_life);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlaggedSiteRecord {
    url: String,
    flag: i64,
}

/// Dispatches `jumpQueue` and `browserLockdownSvc` (`OnJumpQueueUpdate` and `OnFlaggedListsChange`).
// parity: notify:OnFlaggedListsChange, notify:OnJumpQueueUpdate
pub fn handle_misc_batch9(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("jumpQueue", "GetJumpQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
                .unwrap_or(0);
            if sys_id == 0 {
                return Some(Err(err(
                    "InvalidSolarSystem",
                    "GetJumpQueue requires a valid solarSystemID",
                )));
            }
            let cfg_opt = ctx.db.with_snapshot(|snap| {
                let cfgs: HashMap<u32, JumpQueueSystemConfig> = snap
                    .service_state_blobs
                    .get("jump_queue_configs")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                cfgs.get(&sys_id).cloned()
            });
            if let Some(cfg) = cfg_opt {
                let mut keys_vec: Vec<(i64, i64)> = cfg.jump_keys.iter().map(|(&k, &v)| (k, v)).collect();
                keys_vec.sort_by_key(|&(k, _)| k);
                if cfg.queue.contains(&char_id) {
                    emit_jump_queue_update_v24(
                        ctx,
                        char_id,
                        sys_id,
                        cfg.threshold as i64,
                        &cfg.queue,
                        &keys_vec,
                        cfg.key_life.max(30),
                    );
                }
                let queue_rep = PyRep::List(cfg.queue.into_iter().map(PyRep::Integer).collect());
                let keys_rep = PyRep::Dict(
                    keys_vec
                        .into_iter()
                        .map(|(cid, ft)| (PyRep::Integer(cid), PyRep::Integer(ft)))
                        .collect(),
                );
                Some(Ok(PyRep::keyval([
                    ("solarsystemID", PyRep::Integer(i64::from(sys_id))),
                    ("space", PyRep::Integer(cfg.threshold as i64)),
                    ("queue", queue_rep),
                    ("jumpKeys", keys_rep),
                    ("keyLife", PyRep::Integer(cfg.key_life.max(30))),
                ])))
            } else {
                Some(Ok(PyRep::None))
            }
        }
        ("jumpQueue", "SetSystemJumpQueueThreshold") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(err(
                    "InsufficientRole",
                    "SetSystemJumpQueueThreshold requires GM role",
                )));
            }
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            if sys_id == 0 {
                return Some(Err(err(
                    "InvalidSolarSystem",
                    "SetSystemJumpQueueThreshold requires a positive solarSystemID",
                )));
            }
            let threshold = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(0) as usize;
            let key_life = args.get(2).and_then(|a| a.as_i64()).unwrap_or(120).max(30);
            ctx.db.with_mut_snapshot(|snap| {
                let mut cfgs: HashMap<u32, JumpQueueSystemConfig> = snap
                    .service_state_blobs
                    .get("jump_queue_configs")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                let entry = cfgs.entry(sys_id).or_default();
                entry.threshold = threshold;
                entry.key_life = key_life;
                if let Ok(json) = serde_json::to_string(&cfgs) {
                    snap.service_state_blobs
                        .insert("jump_queue_configs".into(), json);
                }
            });
            Some(Ok(PyRep::keyval([
                ("solarsystemID", PyRep::Integer(i64::from(sys_id))),
                ("threshold", PyRep::Integer(threshold as i64)),
                ("keyLife", PyRep::Integer(key_life)),
            ])))
        }
        ("browserLockdownSvc", "GetDefaultHomePage") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::String("about:blank".into())))
        }
        ("browserLockdownSvc", "IsBrowserInLockdown") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let locked = ctx.db.with_snapshot(|snap| {
                snap.service_state_blobs
                    .get("browser_lockdown_enabled")
                    .and_then(|s| s.parse::<bool>().ok())
                    .unwrap_or(false)
            });
            Some(Ok(PyRep::Bool(locked)))
        }
        ("browserLockdownSvc", "GetFlaggedSitesHash") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let hash = ctx.db.with_snapshot(|snap| {
                snap.service_state_blobs
                    .get("browser_flagged_sites_hash")
                    .cloned()
                    .unwrap_or_else(|| "0000000000000000".into())
            });
            Some(Ok(PyRep::String(hash)))
        }
        ("browserLockdownSvc", "GetFlaggedSitesList") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let list: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let sites: Vec<FlaggedSiteRecord> = snap
                    .service_state_blobs
                    .get("browser_flagged_sites")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                sites
                    .into_iter()
                    .map(|s| {
                        PyRep::keyval([
                            ("siteUrl", PyRep::String(s.url)),
                            ("siteFlag", PyRep::Integer(s.flag)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(list)))
        }
        _ => None,
    }
}

/// Broadcasts `OnServerMessage(message)` to all connected clients.
// parity: notify:OnServerMessage
pub fn emit_server_message_broadcast(ctx: &ServerContext, message: &str) {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::All,
        "OnServerMessage",
        "clientID",
        vec![PyRep::String(trimmed.to_string())],
    );
}

/// Handles `machoNet.SetGlobalConfigValue` Batch 9 GM/Admin config updates, emitting
/// `OnCCPNotification`, `OnServerMessage`, `OnUpdateServiceMapping`, and `OnFlaggedListsChange`.
// parity: notify:OnCCPNotification, notify:OnServerMessage, notify:OnUpdateServiceMapping, notify:OnFlaggedListsChange
pub fn handle_macho_batch9_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method != "SetGlobalConfigValue" {
        return None;
    }
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }
    if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
        return Some(Err(err(
            "InsufficientRole",
            "SetGlobalConfigValue requires GM or Admin role",
        )));
    }
    let key = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
    match key {
        "ccp_notification" => {
            let title = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let subtitle = args.get(2).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let text = args.get(3).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            if title.is_empty() || text.is_empty() {
                return Some(Err(err(
                    "InvalidArgument",
                    "ccp_notification requires non-empty title and text",
                )));
            }
            emit_ccp_notification(ctx, NotificationTarget::All, &title, &subtitle, &text, 1, 1);
            Some(Ok(PyRep::Bool(true)))
        }
        "server_message" => {
            let msg = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            if msg.is_empty() {
                return Some(Err(err(
                    "InvalidArgument",
                    "server_message requires a non-empty message string",
                )));
            }
            emit_server_message_broadcast(ctx, &msg);
            Some(Ok(PyRep::Bool(true)))
        }
        "service_mapping" => {
            let svc_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let node_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            if svc_name.is_empty() || node_id <= 0 {
                return Some(Err(err(
                    "InvalidArgument",
                    "service_mapping requires non-empty service name and positive nodeID",
                )));
            }
            ctx.db.with_mut_snapshot(|snap| {
                let mut map: BTreeMap<String, i64> = snap
                    .service_state_blobs
                    .get("macho_service_mappings")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                map.insert(svc_name.clone(), node_id);
                if let Ok(json) = serde_json::to_string(&map) {
                    snap.service_state_blobs
                        .insert("macho_service_mappings".into(), json);
                }
            });
            ctx.notify(
                NotificationTarget::All,
                "OnUpdateServiceMapping",
                "clientID",
                vec![PyRep::String(svc_name), PyRep::Integer(node_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }
        "browser_flagged_sites" => {
            let url = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            if url.is_empty() {
                return Some(Err(err(
                    "InvalidArgument",
                    "browser_flagged_sites requires a non-empty URL",
                )));
            }
            let hash_val = format!("flagged-{:x}-1", ctx.now_unix());
            ctx.db.with_mut_snapshot(|snap| {
                let sites = vec![FlaggedSiteRecord { url, flag: 1 }];
                if let Ok(json) = serde_json::to_string(&sites) {
                    snap.service_state_blobs
                        .insert("browser_flagged_sites".into(), json);
                }
                snap.service_state_blobs
                    .insert("browser_flagged_sites_hash".into(), hash_val);
            });
            ctx.notify(
                NotificationTarget::All,
                "OnFlaggedListsChange",
                "clientID",
                vec![],
            );
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

/// Extracted `localizationServer.GetAllTextChanges` handler.
pub fn handle_get_all_text_changes(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    ctx.db.with_snapshot(|snap| {
        let overrides: BTreeMap<i64, String> = snap
            .owner_notes
            .get(&LOCALIZATION_NOTE_ID)
            .and_then(|n| serde_json::from_str(&n.note).ok())
            .unwrap_or_default();
        if overrides.is_empty() {
            Ok(PyRep::None)
        } else {
            Ok(PyRep::Dict(
                overrides
                    .into_iter()
                    .map(|(id, txt)| (PyRep::Integer(id), PyRep::String(txt)))
                    .collect(),
            ))
        }
    })
}

/// Extracted `localizationServer.UpdateLocalizationQAWrap` handler emitting `OnUpdateLocalizationTextCache`.
// parity: notify:OnUpdateLocalizationTextCache
pub fn handle_update_localization_qa_wrap(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    let qa_roles = eve_db::service_roles::ROLE_GMH
        | eve_db::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_PROGRAMMER
        | eve_db::service_roles::ROLE_CONTENT;
    if !session.has_gm_role() && (session.role & qa_roles) == 0 {
        return Err(err(
            "RoleRequired",
            "localizationServer.UpdateLocalizationQAWrap requires GM or Content/Programmer role",
        ));
    }
    let updated_entries = ctx.db.try_with_mut_snapshot(|snap| {
        let mut overrides: BTreeMap<i64, String> = snap
            .owner_notes
            .get(&LOCALIZATION_NOTE_ID)
            .and_then(|n| serde_json::from_str(&n.note).ok())
            .unwrap_or_default();
        let mut applied = Vec::new();
        if let Some(PyRep::Dict(entries)) = args.first() {
            for (k, v) in entries {
                if let (Some(msg_id), Some(text)) = (k.as_i64(), v.as_str()) {
                    overrides.insert(msg_id, text.to_string());
                    applied.push((PyRep::Integer(msg_id), PyRep::String(text.to_string())));
                }
            }
        } else if args.first().and_then(PyRep::as_bool).is_some() {
            return Err(err(
                "UnimplementedRPC",
                "Boolean QA-wrap toggle is not supported in UpdateLocalizationQAWrap",
            ));
        }
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: serde_json::to_string(&overrides).unwrap_or_default(),
                timestamp_unix: 0,
            },
        );
        Ok(applied)
    })?;
    if !updated_entries.is_empty() {
        let msg_per_lang = PyRep::Dict(vec![(
            PyRep::String("en-us".into()),
            PyRep::Dict(updated_entries),
        )]);
        let cache_data = PyRep::Tuple(vec![
            msg_per_lang,
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
        ]);
        ctx.notify(
            NotificationTarget::All,
            "OnUpdateLocalizationTextCache",
            "clientID",
            vec![cache_data],
        );
    }
    Ok(PyRep::Bool(true))
}
