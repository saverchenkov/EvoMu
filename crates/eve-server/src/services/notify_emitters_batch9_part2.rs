//! `notify_emitters_batch9_part2` — Server-side V24 notification emitters & handlers for Batch 9 (Part 2:
//! Trade, Market Repackage Errors, Godma, Inventory Cache, Structures, Industry Materials, Jump Queue,
//! Browser Lockdown, Cluster Notifications/Messages/Service Mapping, and Localization Text Cache).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_batch3_space::{
    emit_module_activation_godma_notifications, flush_module_profile_and_ewar_end,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::BTreeMap;

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

/// Counts how many items in a multi-sell order are assembled and damaged (hull, armor, or fitted module/heat damage).
pub fn count_damaged_sell_items(
    snap: &eve_db::WorldStateSnapshot,
    damaged_fitted_ships: &std::collections::HashSet<i64>,
    char_id: i64,
    caller_corp_id: i64,
    use_corp: bool,
    parsed_items: &[(i64, u32, i64, eve_db::IskCents, i32, i64)],
) -> i64 {
    let mut damaged_count: i64 = 0;
    for (station_id, type_id, raw_item_id, _, _, _) in parsed_items {
        let candidate_opt = if *raw_item_id > 0 {
            snap.items.get(raw_item_id)
        } else {
            snap.items.values().find(|i| {
                i.type_id == *type_id
                    && i.singleton
                    && (snap.is_item_consumable_by(i, char_id, Some(*station_id), None)
                        || (use_corp
                            && i.owner_id == caller_corp_id
                            && i.location_id == *station_id
                            && i.flag != eve_db::InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)))
            })
        };
        if let Some(item) = candidate_opt {
            if item.singleton {
                let hull_or_armor_dmg = item
                    .dynamic_attributes
                    .get(&eve_sde::attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                    || item
                        .dynamic_attributes
                        .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0;
                let module_dmg = damaged_fitted_ships.contains(&item.item_id);
                if hull_or_armor_dmg || module_dmg {
                    damaged_count += 1;
                }
            }
        }
    }
    damaged_count
}

/// Emits `OnSellRepackageErrors(repackageErrorCount)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnSellRepackageErrors
pub fn emit_sell_repackage_errors(
    ctx: &ServerContext,
    char_id: i64,
    error_code: &str,
    count: i64,
) {
    if count <= 0 || error_code != "CantRepackageDamagedItem" {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSellRepackageErrors",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::String("CantRepackageDamagedItem".into()),
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

/// Dispatches `browserLockdownSvc` queries (fail-closed until real lockdown state exists).
pub fn handle_misc_batch9(
    _ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        (
            "browserLockdownSvc",
            "GetDefaultHomePage"
            | "IsBrowserInLockdown"
            | "GetFlaggedSitesHash"
            | "GetFlaggedSitesList",
        ) => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            Some(Err(err(
                "UnimplementedRPC",
                format!("browserLockdownSvc.{method} is not yet implemented"),
            )))
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

/// Extracted `localizationServer.UpdateLocalizationQAWrap` handler (fail-closed until QA-wrap state exists).
pub fn handle_update_localization_qa_wrap(
    _ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
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
    Err(err(
        "UnimplementedRPC",
        "localizationServer.UpdateLocalizationQAWrap is not yet implemented",
    ))
}
