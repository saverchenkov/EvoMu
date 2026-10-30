//! `notify_emitters_batch9_part2` — Server-side V24 notification emitters & handlers for Batch 9 (Part 2:
//! Trade, Market Repackage Errors, Godma, Inventory Cache, Structures, Industry Materials, Jump Queue,
//! Browser Lockdown, Cluster Notifications/Messages/Service Mapping, and Localization Text Cache).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
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
    let client_key = match error_code {
        "CannotSellAssembledItem" | "CannotSellItemWithContents" | "CantRepackageDamagedItem" => {
            "CantRepackageDamagedItem"
        }
        other => other,
    };
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSellRepackageErrors",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::String(client_key.to_string()),
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JumpQueueSystemConfig {
    threshold: usize,
    key_life: i64,
    queue: Vec<i64>,
    jump_keys: HashMap<i64, i64>,
}

/// Emits `OnJumpQueueUpdate(solarsystemID, space, queue, jumpKeys, keyLife)`.
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

/// Dispatches `browserLockdownSvc` read-only queries.
pub fn handle_misc_batch9(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
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
