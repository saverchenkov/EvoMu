//! `fitting_module_cycles` — `dogmaIM.Activate` & `dogmaIM.Overload` active module cycle execution.

#![forbid(unsafe_code)]

use super::*;

#[path = "fitting_module_cycles_guards.rs"]
pub(crate) mod guards;
pub(crate) use guards::*;
#[path = "fitting_module_cycles_helpers.rs"]
pub(crate) mod helpers;
pub(crate) use helpers::*;

/// Builds a `ModuleAlreadyActive` error carrying V24 `EffectAlreadyActive2` (`3252`).
pub(crate) fn module_already_active_rpc_error(
    ctx: &ServerContext,
    module_type_id: Option<u32>,
    message: impl Into<String>,
) -> BridgeRpcError {
    let mod_name = module_type_id
        .and_then(|tid| ctx.sde.types.get(&tid))
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "Module".to_string());
    BridgeRpcError::with_client_error(
        "ModuleAlreadyActive",
        message,
        eve_proto::ClientError::user_with_args(
            eve_proto::UserErrorKey::EffectAlreadyActive2,
            [(
                "modulename",
                eve_proto::UserErrorArg::Str(mod_name),
            )],
        ),
    )
}

/// Active module groups that are gated on natural cycle end (`RequestCycleDeactivation`
/// instead of immediate removal, plus reactivation delay on manual deactivate/offline).
pub(crate) fn is_cycle_gated_module_group(group_id: u32) -> bool {
    matches!(
        group_id,
        40 | 1156
            | 41
            | 1697
            | 67
            | 71
            | 76
            | 72
            | 1122
            | 650
            | 80
            | 201
            | 353
    )
}

/// Builds a `TargetNotLocked` error carrying V24 `UserError::TargetNotLocked` (`1627`).
pub(crate) fn target_not_locked_rpc_error(message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError::with_client_error(
        "TargetNotLocked",
        message,
        eve_proto::ClientError::user(eve_proto::UserErrorKey::TargetNotLocked),
    )
}

/// Builds a `ShipDocked` error for space-only module/overload calls made while docked.
pub(crate) fn ship_docked_rpc_error(message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: "ShipDocked".into(),
        message: message.into(),
        client_error: None,
    }
}

fn standup_module_requires_structure_error(module_id: i64) -> BridgeRpcError {
    BridgeRpcError::with_client_error(
        "StructureNotFound",
        format!("Standup weapon module {module_id} requires an active deployed structure"),
        eve_proto::ClientError::user(eve_proto::UserErrorKey::StructureServiceUnavailable),
    )
}

fn module_activation_unhandled_error(service: &str, method: &str) -> BridgeRpcError {
    let message = format!("Module invocation {service}.{method} could not be activated");
    BridgeRpcError::with_client_error(
        "CannotActivateModule",
        message.clone(),
        eve_proto::ClientError::user_with_args(
            eve_proto::UserErrorKey::CustomError,
            [("error", eve_proto::UserErrorArg::Str(message))],
        ),
    )
}

enum ChargeVolatility {
    VolatileCrystal { damage: f64, chance: f64 },
    NonVolatileCrystal,
    Consumable,
}

fn classify_charge_volatility(
    sde: &eve_sde::SdeRegistry,
    c_sde: Option<&eve_sde::InvType>,
    ch_state_opt: Option<&eve_dogma::DogmaItemState>,
) -> ChargeVolatility {
    let vol_dmg = ch_state_opt
        .filter(|cs| {
            cs.base_attributes.contains_key(&784)
                || cs.modifiers.iter().any(|cm| cm.target_attribute_id == 784)
        })
        .map(|cs| cs.get_attribute(784, sde))
        .or_else(|| c_sde.and_then(|t| t.default_attributes.get(&784).copied()))
        .unwrap_or(0.0);
    let vol_chance = ch_state_opt
        .filter(|cs| {
            cs.base_attributes.contains_key(&783)
                || cs.modifiers.iter().any(|cm| cm.target_attribute_id == 783)
        })
        .map(|cs| cs.get_attribute(783, sde))
        .or_else(|| c_sde.and_then(|t| t.default_attributes.get(&783).copied()))
        .unwrap_or(100.0);
    if vol_dmg > 0.0 {
        let normalized_chance = if vol_chance > 1.0 {
            (vol_chance / 100.0).clamp(0.0, 1.0)
        } else {
            vol_chance.clamp(0.0, 1.0)
        };
        ChargeVolatility::VolatileCrystal {
            damage: vol_dmg,
            chance: normalized_chance,
        }
    } else if c_sde.is_some_and(|t| matches!(t.group_id, 86 | 374 | 375 | 482 | 663)) {
        ChargeVolatility::NonVolatileCrystal
    } else {
        ChargeVolatility::Consumable
    }
}

struct ResolvedCycleStart<'a> {
    char_id: i64,
    ship_id: i64,
    effect_name: String,
    target_id_opt: Option<i64>,
    repeat: i32,
    is_docked: bool,
    sys_id: u32,
    sys: crate::system_actor::SystemHandle,
    fit: Box<eve_dogma::ShipFittingEngine>,
    module_id: i64,
    m: eve_dogma::FittedModule,
    mod_type: &'a eve_sde::InvType,
    is_weapon: bool,
}

struct CyclePrecheckResult {
    is_overloaded: bool,
    is_mwd: bool,
    is_mjd: bool,
    is_mwd_or_mjd: bool,
    passive_targeter_lock_opt: Option<eve_destiny::TargetLock>,
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn try_start_or_resolve_cycle<'a>(
    ctx: &'a ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> std::ops::ControlFlow<Option<Result<PyRep, BridgeRpcError>>, ResolvedCycleStart<'a>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };
    let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(ctx, char_id, "SAFE_LOGOFF_ABORT_MODULE");
    let rpc = crate::router::RpcArgs::new(args, kwargs);
    let raw_module_id = match rpc.opt_positive_i64(0, "itemID") {
        Ok(v) => v.unwrap_or(0),
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };
    let effect_name = rpc.opt_str(1, "effectName").ok().flatten().unwrap_or("defaultEffect").to_string();
    let target_id_opt = match rpc.opt_i64(2, "target").transpose().or_else(|| {
        (method != "Overload").then(|| args.get(1).and_then(|a| a.as_i64())).flatten().map(Ok)
    }) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => return std::ops::ControlFlow::Break(Some(Err(e))),
        None => None,
    };
    let repeat = match rpc.opt_i32(3, "repeat") {
        Ok(v) => v.unwrap_or(1),
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };
    let Some(ship_id) = session.ship_id else {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "No active ship in session".into(),
            client_error: None,
        })));
    };
    if let Some(st_id) = ctx.db.with_snapshot(|s| {
        s.structures.contains_key(&ship_id).then_some(ship_id).or_else(|| {
            s.items.get(&raw_module_id).and_then(|it| s.structures.contains_key(&it.location_id).then_some(it.location_id))
        })
    }) {
        return std::ops::ControlFlow::Break(Some(
            crate::services::structure_weapons_ops::handle_structure_module_activate(
                ctx, session, char_id, st_id, raw_module_id, &effect_name, target_id_opt, method == "Overload",
            ).await,
        ));
    }
    let is_docked = session.station_id.is_some() || session.structure_id.is_some();
    if method == "Activate" && effect_name == "online" {
        let is_currently_offline = ctx.db.with_snapshot(|s| s.module_online_states.get(&raw_module_id).copied()) == Some(false)
            || ctx.fit(char_id, ship_id).ok().and_then(|f| f.modules.get(&raw_module_id).map(|m| m.state == eve_dogma::ModuleState::Offline)).unwrap_or(false);
        // When docked or when the module is currently offline in space, `Activate(id, "online")`
        // brings the fitted module online (`const.effectOnline = 16`). When the module is already
        // online in space, allow cycle activation to proceed through `handle_module_cycles`.
        if is_docked || is_currently_offline {
            if let Some(sys_id) = session.solar_system_id {
                if ctx.systems.get_or_spawn(sys_id).has_active_nanite_repair_sync(ship_id, raw_module_id) {
                    return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
                        code: "ModuleBeingRepaired".into(),
                        message: format!("Module {raw_module_id} cannot be onlined while undergoing nanite repair"),
                        client_error: None,
                    })));
                }
            }
            let godma_args = [PyRep::Integer(ship_id), PyRep::Integer(raw_module_id)];
            return std::ops::ControlFlow::Break(Some(
                crate::services::notify_emitters_skills_trade_godma::handle_dogma_set_module_online(
                    ctx, session, &godma_args,
                ).await,
            ));
        }
    }
    if method == "Overload" && is_docked {
        return std::ops::ControlFlow::Break(Some(Err(ship_docked_rpc_error("Cannot overload modules while docked"))));
    }
    let Some(sys_id) = session.solar_system_id else {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Cannot activate modules outside of space".into(), client_error: None })));
    };
    let sys = ctx.systems.get_or_spawn(sys_id);
    let fit = match ctx.fit(char_id, ship_id) {
        Ok(f) => f,
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };
    let module_id = if raw_module_id != 0 {
        raw_module_id
    } else {
        fit.modules.values().find(|m| ctx.sde.types.get(&m.type_id).is_some_and(|t| t.group_id == 481)).map(|m| m.item_id)
            .or_else(|| ctx.db.with_snapshot(|s| {
                s.items.values().find(|i| i.location_id == ship_id && (11..=34).contains(&(i.flag as u16)) && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 481)).map(|i| i.item_id)
            })).unwrap_or(0)
    };
    let Some(mut m) = fit.modules.get(&module_id).cloned().or_else(|| resolve_snapshot_fitted_module(ctx, ship_id, module_id)) else {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::module_not_found(module_id, Some(ship_id), None, false, format!("Module {module_id} not found on ship")))));
    };
    if let Err(e) = check_module_reloading(ctx, ship_id, module_id) {
        return std::ops::ControlFlow::Break(Some(Err(e)));
    }
    if sys.has_active_nanite_repair_sync(ship_id, module_id) {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError { code: "ModuleBeingRepaired".into(), message: format!("Module {module_id} cannot be activated or overloaded while undergoing nanite repair"), client_error: None })));
    }
    if let Err(e) = crate::services::fitting_cycles_overheat::check_module_or_bank_not_burned_out(ctx, ship_id, module_id, &m) {
        return std::ops::ControlFlow::Break(Some(Err(e)));
    }
    if m.state == eve_dogma::ModuleState::Offline || !ctx.db.with_snapshot(|s| s.module_online_states.get(&module_id).copied().unwrap_or(true)) {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError { code: "ModuleOffline".into(), message: format!("Module {module_id} is offline"), client_error: None })));
    }
    let Some(mod_type) = ctx.sde.types.get(&m.type_id) else {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::module_not_found(module_id, Some(ship_id), Some(m.type_id as i64), false, format!("Module type {} not found in SDE", m.type_id)))));
    };
    let is_weapon = is_weapon_group(mod_type.group_id);
    if method == "Overload" && matches!(mod_type.group_id, 1327 | 1328 | 1330 | 1331 | 1333 | 1562) {
        return std::ops::ControlFlow::Break(Some(Err(crate::services::structure_weapons_ops::cannot_overload_structure_module_error())));
    }
    if let Err(e) = crate::services::fitting_cycles_overheat::check_nanite_repair_overheat_lockout(&sys, ship_id, module_id, method, m.state) {
        return std::ops::ControlFlow::Break(Some(Err(e)));
    }
    if method == "Overload" && target_id_opt.is_none() {
        return std::ops::ControlFlow::Break(Some(
            crate::services::fitting_cycles_overheat::handle_early_targetless_overload(
                ctx, &sys, char_id, ship_id, module_id, mod_type, is_weapon, &mut m, fit,
            ).await,
        ));
    }
    std::ops::ControlFlow::Continue(ResolvedCycleStart {
        char_id, ship_id, effect_name, target_id_opt, repeat, is_docked,
        sys_id, sys, fit: Box::new(fit), module_id, m, mod_type, is_weapon,
    })
}

enum CycleSafetyRouting {
    Continue,
    Ready(Option<Result<PyRep, BridgeRpcError>>),
    RouteToWeapons,
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn check_cycle_safety_and_weapon_routing(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    mod_type: &eve_sde::InvType,
    is_weapon: bool,
    is_in_space: bool,
    method: &str,
    effect_name: &str,
    target_id_opt: Option<i64>,
) -> CycleSafetyRouting {
    if is_in_space {
        if let Err(e) = check_space_module_cloak_and_warp_readonly(ctx, sys, ship_id, mod_type).await {
            return CycleSafetyRouting::Ready(Some(Err(e)));
        }
    }
    let is_upwell_weapon_or_aoe = (target_id_opt.is_some_and(|tid| {
        tid >= 1_000_000_000_000
            || ctx.db.with_snapshot(|s| {
                s.structures.contains_key(&tid) || s.sov_runtime.mercenary_dens.contains_key(&tid) || s.sov_runtime.mto_site_balls.contains_key(&tid)
                    || (s.system_states.get(&sys_id).is_some_and(|st| st.sov_alliance_id.is_some())
                        && !s.characters.contains_key(&tid) && !s.characters.values().any(|c| c.active_ship_id == tid))
            })
    }) && is_weapon)
        || (matches!(mod_type.group_id, 72 | 862) && ctx.db.with_snapshot(|s| s.structures.values().any(|st| st.solar_system_id == sys_id && st.state != "Destroyed")));
    let is_fw_pvp = is_weapon && target_id_opt.is_some_and(|tid| {
        ctx.db.with_snapshot(|s| {
            s.fw_system_occupations.contains_key(&sys_id) && {
                let kf = s.characters.get(&char_id).and_then(|c| c.war_faction_id);
                let vf = s.characters.get(&tid).map(|c| c.character_id)
                    .or_else(|| s.characters.values().find(|c| c.active_ship_id == tid).map(|c| c.character_id))
                    .or_else(|| s.items.get(&tid).and_then(|i| s.characters.get(&i.owner_id).map(|c| c.character_id)))
                    .and_then(|vc| s.characters.get(&vc).and_then(|c| c.war_faction_id));
                matches!((kf, vf), (Some(k), Some(v)) if k != v)
            }
        })
    });
    if matches!(mod_type.group_id, 1328 | 1330 | 1333) {
        return CycleSafetyRouting::Ready(Some(Err(standup_module_requires_structure_error(
            module_id,
        ))));
    }
    let is_combat_or_ewar_module = is_weapon
        || matches!(
            mod_type.group_id,
            52 | 65 | 72 | 201 | 208 | 291 | 379 | 512 | 862
        );
    let is_special_target = !matches!(mod_type.group_id, 1122 | 650)
        && (matches!(mod_type.group_id, 862 | 1986 | 4060 | 4500 | 4805 | 4807)
            || (is_combat_or_ewar_module
                && target_id_opt.is_some_and(|tid| {
                    ctx.living_world.get_convoy(tid).is_some()
                        || ctx.db.with_snapshot(|s| {
                            s.items
                                .get(&tid)
                                .and_then(|i| ctx.sde.types.get(&i.type_id))
                                .is_some_and(|t| matches!(t.group_id, 340 | 448 | 4644))
                        })
                })));
    let is_arena_pvp = is_weapon && (32_000_000..33_000_000).contains(&sys_id)
        && ctx.db.with_snapshot(|s| crate::services::pvp_filament_mgr::load_pvp_store(s).active_matches.values().any(|m| m.active && m.arena_solar_system_id == sys_id));
    if is_fw_pvp || is_arena_pvp || is_upwell_weapon_or_aoe || is_special_target {
        return CycleSafetyRouting::RouteToWeapons;
    }
    if method == "Activate" && is_weapon && ctx.is_module_in_reactivation_delay(ship_id, module_id) {
        return CycleSafetyRouting::Ready(Some(Err(module_already_active_rpc_error(
            ctx, Some(module_type_id), format!("Module {module_id} is already active in a cycle"),
        ))));
    }
    if (matches!(mod_type.group_id, 52 | 65 | 68 | 71 | 72 | 201 | 208 | 291 | 379 | 588 | 862)
        || is_weapon || matches!(effect_name, "empWave" | "useMissiles" | "targetAttack" | "projectileFired"))
        && sys.get_combat_state(ship_id).await.ok().flatten().is_some_and(|cs| cs.is_tethered)
    {
        return CycleSafetyRouting::Ready(Some(Err(BridgeRpcError {
            code: "CantActivateWhileTethered".into(),
            message: "Cannot activate offensive modules, smartbombs, or bomb launchers while tethered".into(),
            client_error: None,
        })));
    }
    if matches!(mod_type.group_id, 72 | 80 | 353 | 842) {
        let sys_sec = ctx
            .db
            .with_snapshot(|s| s.combat_crimewatch_state.modified_system_securities.get(&sys_id).copied())
            .unwrap_or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0));
        let safety = crate::services::combat_crimewatch::char_safety_level(ctx, char_id).unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red);
        if sys_sec > 0.0 && safety != crate::services::combat_crimewatch::SafetyLevel::Red {
            return CycleSafetyRouting::Ready(Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"))));
        }
    }
    if method == "Overload" {
        if let Err(e) = crate::services::fitting_cycles_overheat::check_can_overload_module(ctx, char_id, module_type_id) {
            return CycleSafetyRouting::Ready(Some(Err(e)));
        }
    }
    CycleSafetyRouting::Continue
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn try_activate_burst_or_scanner(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>,
    is_in_space: bool,
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
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
        let res = activate_command_burst_module(
            ctx,
            sys,
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
        .await;
        return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res).await;
    }
    if let Some(res) = handle_scanner_analyzer_entosis_activation(
        ctx,
        sys,
        char_id,
        ship_id,
        module_id,
        mod_type,
        &mod_state,
        target_id_opt,
    )
    .await
    {
        return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res).await;
    }
    None
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn try_activate_early_special_modules(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &eve_dogma::ShipFittingEngine,
    effect_name: &str,
    target_id_opt: Option<i64>,
    is_weapon: bool,
    is_in_space: bool,
    method: &str,
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = crate::services::pve_ess_reserve_banks::check_ess_module_activation_allowed(
        ctx,
        sys_id,
        ship_id,
        mod_type,
        effect_name,
    )
    .await
    {
        return Some(Err(e));
    }
    if mod_type.group_id == 330 {
        if (32_000_000..=32_999_999).contains(&sys_id) {
            return Some(Err(BridgeRpcError {
                code: "CannotCloakInAbyssalSpace".into(),
                message: "Cloaking devices cannot be activated inside Abyssal Deadspace".into(),
                client_error: None,
            }));
        }
        if fit
            .modules
            .values()
            .any(|fm| ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 588))
        {
            return Some(Err(BridgeRpcError {
                code: "CannotCloakWithLanceFitted".into(),
                message: "Cannot activate a cloaking device while a Disruptive Lance or Superweapon is fitted".into(),
                client_error: None,
            }));
        }
        if let Err(e) =
            crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "cloak")
        {
            return Some(Err(e));
        }
    }
    if mod_type.group_id == 588 {
        let is_lance_or_slash = matches!(mod_type.type_id, 77398..=77401)
            || mod_type.default_attributes.contains_key(&2263)
            || mod_type.name.contains("Lance")
            || mod_type.name.contains("Slash")
            || mod_type.name.contains("Bosonic")
            || mod_type.name.contains("Reaper");
        let sw_method = if is_lance_or_slash {
            "ActivateSlashModule"
        } else {
            "ActivateSinglePointTargetedModule"
        };
        let res =         crate::services::tactical_modules::activate_superweapon_or_lance(
            ctx,
            char_id,
            ship_id,
            sys_id,
            module_id,
            sw_method,
            target_id_opt,
        )
        .await;
        return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res).await;
    }
    if mod_type.group_id == 1154 {
        let mod_state = fit.build_effective_module_state(module_id, m.type_id, false, &ctx.sde);
        let res = crate::services::tactical_modules::activate_target_spectrum_breaker(
            ctx, sys, char_id, ship_id, module_id, mod_type, &mod_state,
        )
        .await;
        return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res).await;
    }

    if let Some(res) = handle_cyno_generator_activation(ctx, sys, char_id, ship_id, sys_id, module_id, mod_type)
    .await
    {
        return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res).await;
    }

    if sys_id == 30_100_000 {
        let script_clears_hazard_ban = mod_type.group_id == 899
            && m.charge_type_id.is_some_and(|cid| {
                cid == 29003
                    || cid == 45010
                    || ctx.sde.types.get(&cid).is_some_and(|t| t.group_id == 908)
            });
        if !script_clears_hazard_ban
            && mod_type.default_attributes.get(&5561).copied().unwrap_or(0.0) > 0.0
        {
            return Some(Err(BridgeRpcError::user_error("ModuleDisallowedInHazardSystem")));
        }
    }
    if method == "Activate" && target_id_opt.is_none() {
        let is_recorded_active = sys.has_module_cycle(ship_id, module_id).await
            || ctx.is_module_in_reactivation_delay(ship_id, module_id)
            || (!is_weapon
                && (m.state == eve_dogma::ModuleState::Active
                    || ctx
                        .db
                        .with_snapshot(|s| s.module_active_states.contains(&module_id))));
        if !is_recorded_active
            && activation_requires_explicit_target(ctx, ship_id, m, mod_type.group_id, is_weapon)
        {
            return Some(Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "Target required for module activation".into(),
                client_error: None,
            }));
        }
    }

    if matches!(mod_type.group_id, 1770 | 538 | 47 | 48 | 1313 | 815) {
        if let Some(res) = try_activate_burst_or_scanner(
            ctx,
            sys,
            char_id,
            ship_id,
            sys_id,
            module_id,
            m,
            mod_type,
            fit,
            target_id_opt,
            is_in_space,
            kwargs,
        )
        .await
        {
            return Some(res);
        }
    }

    if matches!(mod_type.group_id, 41 | 1697) {
        if let Some(tid) = target_id_opt.filter(|&t| t > 0) {
            if ctx
                .db
                .with_snapshot(|s| s.sov_runtime.mercenary_dens.contains_key(&tid))
            {
                let res = helpers::try_handle_mercden_remote_shield_repair(
                    ctx, sys, sys_id, char_id, ship_id, module_id, m, mod_type, fit, method,
                    tid,
                )
                .await
                .and_then(|opt| opt.ok_or_else(|| BridgeRpcError::user_error("MercDenRepairFailed")));
                return finish_space_activation(ctx, sys, is_in_space, char_id, ship_id, res)
                    .await;
            }
        }
    }
    None
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn activate_interdiction_sphere_module(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    effect_name: &str,
    target_id_opt: Option<i64>,
    is_docked: bool,
    is_in_space: bool,
) -> Result<PyRep, BridgeRpcError> {
    if is_docked {
        return Err(ship_docked_rpc_error(
            "Cannot activate Interdiction Sphere Launcher while docked",
        ));
    }
    if sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_tethered)
    {
        return Err(BridgeRpcError {
            code: "CantActivateWhileTethered".into(),
            message: "Cannot activate Interdiction Sphere Launcher while tethered".into(),
            client_error: None,
        });
    }
    let res =     crate::services::tactical_module_mechanics::activate_interdiction_sphere_launcher(
        ctx,
        sys,
        sys_id,
        char_id,
        ship_id,
        ship_type_id,
        module_id,
        m,
        mod_type,
        effect_name,
        target_id_opt,
    )
    .await;
    if res.is_ok() {
        if is_in_space {
            break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
        }
        ctx.db.with_mut_snapshot(|s| {
            s.module_active_states.remove(&module_id);
        });
        let stop_eff_id = ctx
            .effect_id_for_module(m.type_id, Some(effect_name))
            .unwrap_or(101) as i32;
        let _ = ctx.notify_godma_ship_effect(
            crate::router::NotificationTarget::Character(char_id),
            module_id,
            stop_eff_id,
            false,
            0,
            0,
        );
    }
    res
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn activate_survey_probe_launcher(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    flag_id: u16,
    mod_type: &eve_sde::InvType,
    target_id_opt: Option<i64>,
    is_in_space: bool,
) -> Result<PyRep, BridgeRpcError> {
    let launcher_cap = mod_type.capacity.max(10.0);
    let slot_flag = InventoryFlag::from_u16(flag_id);
    // Auto-reload from Cargo / SpecializedAmmoHold if launcher slot is empty, then consume 1 probe,
    // and auto-reload again if the last probe in the launcher slot was just consumed.
    let Some((probe_id, probe_type_id, remaining_in_launcher, auto_reloaded, item_diffs)) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut touched_pre: HashMap<i64, Option<eve_db::InventoryItemRecord>> = HashMap::new();
            let mut in_slot_id = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id && (i.flag as u16) == flag_id && i.item_id != module_id && i.quantity > 0
                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                })
                .map(|i| (i.item_id, i.type_id));
            let reload_into_slot = |snap: &mut eve_db::WorldStateSnapshot,
                                    touched_pre: &mut HashMap<i64, Option<eve_db::InventoryItemRecord>>|
             -> Option<(i64, u32, i32)> {
                let (src_id, p_tid, avail) = snap
                    .items
                    .values()
                    .find(|i| {
                        i.location_id == ship_id && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold) && i.quantity > 0
                            && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                    })
                    .map(|i| (i.item_id, i.type_id, i.quantity))?;
                let p_vol = ctx.sde.types.get(&p_tid).map(|t| t.volume.max(1.0)).unwrap_or(10.0);
                let move_qty = avail.min(((launcher_cap / p_vol).floor() as i32).max(1));
                if move_qty >= avail {
                    if let Some(it) = snap.items.get_mut(&src_id) {
                        touched_pre.entry(src_id).or_insert_with(|| Some(it.clone()));
                        it.flag = slot_flag;
                        it.version += 1;
                    }
                    Some((src_id, p_tid, move_qty))
                } else {
                    if let Some(it) = snap.items.get_mut(&src_id) {
                        touched_pre.entry(src_id).or_insert_with(|| Some(it.clone()));
                        it.quantity -= move_qty;
                        it.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    touched_pre.insert(new_id, None);
                    snap.items.insert(new_id, eve_db::InventoryItemRecord { item_id: new_id, type_id: p_tid, owner_id: char_id, location_id: ship_id, flag: slot_flag, quantity: move_qty, singleton: false, version: 1, ..Default::default() });
                    Some((new_id, p_tid, move_qty))
                }
            };
            if in_slot_id.is_none() {
                if let Some((nid, ptid, _)) = reload_into_slot(snap, &mut touched_pre) {
                    in_slot_id = Some((nid, ptid));
                }
            }
            let (charge_item_id, probe_type_id) = in_slot_id?;
            if let Some(cur_slot_it) = snap.items.get(&charge_item_id).cloned() {
                touched_pre.entry(charge_item_id).or_insert(Some(cur_slot_it));
            }
            let c_owner = snap.items.get(&charge_item_id).map(|ci| ci.owner_id)?;
            let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
                Some(char_id),
                c_owner,
                eve_db::ItemClassPolicy::ExactAssembledItem,
            );
            opts.reject_fitted = false;
            opts.active_ship_id = Some(ship_id);
            let mut rem = match snap.consume_exact_item_quantity(charge_item_id, 1, opts) {
                Ok(Some(it)) => it.quantity,
                Ok(None) => 0,
                Err(_) => return None,
            };
            let mut auto_reloaded = 0;
            if rem == 0 {
                if let Some((_, _, r_qty)) = reload_into_slot(snap, &mut touched_pre) {
                    rem = r_qty;
                    auto_reloaded = r_qty;
                }
            }
            // disallowRepeatingActivation (attr 1014 == 1.0): launcher does not auto-repeat
            snap.module_active_states.remove(&module_id);
            snap.next_entity_id += 1;
            let mut diffs: Vec<(Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>)> =
                touched_pre.into_iter().map(|(iid, pre_opt)| (pre_opt, snap.items.get(&iid).cloned())).collect();
            diffs.sort_by_key(|(pre, post)| post.as_ref().or(pre.as_ref()).map_or(0, |r| r.item_id));
            Some((snap.next_entity_id, probe_type_id, rem, auto_reloaded, diffs))
        })
    else {
        return Err(BridgeRpcError { code: "NoProbesLoaded".into(), message: "Survey probe launcher has no survey probes loaded".into(), client_error: None });
    };
    ctx.invalidate_and_push_fit(ship_id);
    emit_ship_charge_sublocation_diffs(ctx, char_id, ship_id, &item_diffs);

    let probe_sde = ctx.sde.types.get(&probe_type_id);
    let base_delay_ms = probe_sde.and_then(|t| t.default_attributes.get(&281).copied().or_else(|| t.default_attributes.get(&1370).copied())).unwrap_or(300_000.0);
    // T2 Survey Probe Launcher II (33272) has attribute 2701 `surveyProbeDurationBonus` = -10.0%
    let duration_bonus_pct = mod_type.default_attributes.get(&2701).copied().unwrap_or(0.0);
    let scan_delay_ms = (base_delay_ms * (1.0 + duration_bonus_pct / 100.0)).round() as i64;
    let max_scan_range_m = probe_sde.and_then(|t| t.default_attributes.get(&2697).copied()).unwrap_or(100_000_000.0);

    // Check ship alignment and 100,000 km (`maxScanRange` 2697) moon range
    let ship_pos_vel_mode = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| (b.position, b.velocity, b.mode));
    let ship_pos = ship_pos_vel_mode.as_ref().map(|(p, _, _)| *p).unwrap_or(DVec3::ZERO);
    let heading_opt: Option<DVec3> = if let Some((sb_pos, sb_vel, sb_mode)) = ship_pos_vel_mode {
        if sb_vel.length() > 1e-3 {
            Some(sb_vel.normalize())
        } else {
            match sb_mode {
                eve_destiny::MovementMode::GotoDirection { direction } if direction.length() > 1e-3 => Some(direction.normalize()),
                eve_destiny::MovementMode::Follow { target_id, .. } => {
                    let tpos = if let Some((_, tb)) = sys.get_ball_snapshot(target_id).await.ok().flatten() {
                        Some(tb.position)
                    } else {
                        ctx.sde.celestial(target_id).filter(|c| c.solar_system_id == sys_id).map(|c| c.position)
                    };
                    tpos.and_then(|tp| ((tp - sb_pos).length() > 1e-3).then(|| (tp - sb_pos).normalize()))
                }
                _ => None,
            }
        }
    } else {
        None
    };

    let moons: Vec<_> = ctx.sde.celestials_in_system_slice(sys_id).iter().filter(|c| c.group_id == 8).collect();
    let hit_moon_id: Option<i64> = if let Some(target_moon_id) = target_id_opt {
        moons.iter().find(|c| c.celestial_id == target_moon_id).and_then(|c| {
            let to_moon = c.position - ship_pos;
            let aligned = heading_opt.is_none_or(|dir| dir.dot(to_moon.normalize_or_zero()) >= 0.70);
            (to_moon.length() <= max_scan_range_m && aligned).then_some(c.celestial_id)
        })
    } else if let Some(dir) = heading_opt {
        moons.iter().filter_map(|c| {
            let to_moon = c.position - ship_pos;
            let dist = to_moon.length();
            (dist <= max_scan_range_m && dir.dot(to_moon.normalize_or_zero()) >= 0.90).then_some((c.celestial_id, dist))
        }).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(cid, _)| cid)
    } else {
        None
    };

    let flight_secs = (scan_delay_ms / 1000).max(1) as u64;
    let arrival_unix_secs = ctx.now_unix_secs().saturating_add(flight_secs);
    if let Ok(mut probes) = ctx.active_moon_probes.write() {
        probes.push(crate::router::context::ActiveMoonSurveyProbe { probe_id, probe_type_id, owner_char_id: char_id, solar_system_id: sys_id, hit_moon_id, arrival_unix_secs });
    }
    ctx.emit_typed(crate::router::NotificationTarget::Character(char_id), crate::router::notify_registry::OnNewMoonProbe {
        probe: PyRep::keyval([("probeID", PyRep::Integer(probe_id)), ("typeID", PyRep::Integer(probe_type_id as i64)), ("moonID", hit_moon_id.map(PyRep::Integer).unwrap_or(PyRep::None)), ("expiry", PyRep::Integer(arrival_unix_secs as i64))]),
    });
    if is_in_space {
        break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
    }
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)), ("launcherID", PyRep::Integer(module_id)),
        ("probeID", PyRep::Integer(probe_id)), ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
        ("moonID", hit_moon_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("scanDelayMs", PyRep::Integer(scan_delay_ms)), ("flightTimeSec", PyRep::Integer(flight_secs as i64)),
        ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)), ("autoReloadedCount", PyRep::Integer(auto_reloaded as i64)),
    ]))
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn reconcile_and_validate_cycle_activation(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    method: &str,
    is_weapon: bool,
    is_in_space: bool,
    target_id_opt: Option<i64>,
    mod_type: &eve_sde::InvType,
    m: &mut eve_dogma::FittedModule,
    fit: &mut eve_dogma::ShipFittingEngine,
) -> std::ops::ControlFlow<Option<Result<PyRep, BridgeRpcError>>, CyclePrecheckResult> {
    let (actor_cycle_state, cleaned_stale) = sys
        .reconcile_module_cycle_before_activate(ship_id, module_id, ctx.now_unix_secs())
        .await;
    let in_actor_cycle = actor_cycle_state.is_some();
    let is_entropic = mod_type.group_id == 1986;
    let (in_delay, already_active) = if method == "Overload" {
        match crate::services::fitting_cycles_overheat::check_overload_cycle_or_delay(
            ctx,
            char_id,
            ship_id,
            module_id,
            in_actor_cycle,
            cleaned_stale,
            is_weapon,
            is_entropic,
            m,
            fit,
        ) {
            Ok(true) => {
                let overload_fit = fit.clone();
                let rep =                 crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
                    ctx,
                    sys,
                    char_id,
                    ship_id,
                    module_id,
                    overload_fit,
                    true,
                )
                .await;
                if is_in_space {
                    break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
                }
                return std::ops::ControlFlow::Break(Some(Ok(rep)));
            }
            Ok(false) => (false, false),
            Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
        }
    } else {
        crate::services::fitting_cycles_overheat::reconcile_module_active_state(
            ctx,
            ship_id,
            module_id,
            in_actor_cycle,
            cleaned_stale,
            is_weapon,
            m,
            fit,
        )
    };
    if in_delay {
        if is_weapon {
            return std::ops::ControlFlow::Break(Some(Err(module_already_active_rpc_error(
                ctx,
                Some(m.type_id),
                format!("Module {module_id} cannot reactivate before delay expires"),
            ))));
        }
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
            code: "ModuleReactivationDelay".into(),
            message: format!("Module {module_id} cannot reactivate before delay expires"),
            client_error: None,
        })));
    }
    let allow_mutadaptive_retarget = mod_type.group_id == 2018
        && target_id_opt.is_some()
        && actor_cycle_state.is_some_and(|(_, cur_t)| cur_t != target_id_opt);
    let allow_smartbomb_retrigger = mod_type.group_id == 72;
    if method == "Activate"
        && already_active
        && !allow_mutadaptive_retarget
        && !allow_smartbomb_retrigger
    {
        if actor_cycle_state.is_some_and(|(rep, _)| rep == 0)
            && matches!(mod_type.group_id, 41 | 67 | 325 | 585 | 1697 | 1698 | 2018)
        {
            if let Some(tid) = target_id_opt {
                if let Err(e) = crate::services::combat_crimewatch::crimewatch_timers::can_perform_remote_assistance_in_safety(
                    ctx, char_id, sys_id, tid,
                ) {
                    return std::ops::ControlFlow::Break(Some(Err(e)));
                }
            }
        }
        return std::ops::ControlFlow::Break(Some(Err(module_already_active_rpc_error(
            ctx,
            Some(m.type_id),
            format!("Module {module_id} is already active in a cycle"),
        ))));
    }
    if target_id_opt.is_none()
        && activation_requires_explicit_target(ctx, ship_id, m, mod_type.group_id, is_weapon)
    {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "Target required for module activation".into(),
            client_error: None,
        })));
    }
    let mut passive_targeter_lock_opt: Option<eve_destiny::TargetLock> = None;
    if let Some(target_id) = target_id_opt {
        if crate::services::tactical_module_mechanics::is_untargetable_interdiction_probe(
            ctx, sys, target_id,
        )
        .await
        {
            return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::invalid_target(
                Some(target_id),
                Some(module_id),
                &mod_type.name,
                "interdiction_probe_untargetable",
                "Interdiction probes cannot be targeted directly; use area-of-effect weapons",
            ))));
        }
        if target_id == ship_id
            && matches!(
                mod_type.group_id,
                41 | 325
                    | 2018
                    | 585
                    | 67
                    | 1697
                    | 1698
                    | 52
                    | 65
                    | 68
                    | 71
                    | 201
                    | 208
                    | 291
                    | 379
                    | 1672
            )
        {
            return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
                code: "CannotActivateOnSelf".into(),
                message: "Cannot activate remote assistance or offensive module on self".into(),
                client_error: None,
            })));
        }
        if mod_type.group_id == 82 {
            match             validate_passive_targeter_activation(ctx, sys, char_id, ship_id, target_id, fit)
            .await
            {
                Ok(lock) => passive_targeter_lock_opt = Some(lock),
                Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
            }
        } else if mod_type.group_id != 842 {
            let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.module_active_states.remove(&module_id);
                    snap.module_active_charges.remove(&module_id);
                });
                ctx.invalidate_and_push_fit(ship_id);
                crate::services::combat_weapons_spool::reset_entropic_spool(
                    ctx, ship_id, module_id,
                );
                return std::ops::ControlFlow::Break(Some(Err(target_not_locked_rpc_error(
                    format!("Target {target_id} is not locked"),
                ))));
            }
        }
        if (matches!(
            mod_type.group_id,
            52 | 65 | 68 | 71 | 201 | 208 | 291 | 379 | 1672
        ) && ctx.db.with_snapshot(|s| {
            crate::services::structure_acl_defense_rules::is_structure_immune_to_ewar(s, target_id)
        }))
            || (mod_type.group_id == 65
                && sys
                    .get_combat_state(target_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|cs| cs.is_web_immune))
        {
            if is_in_space {
                break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
            }
            return std::ops::ControlFlow::Break(Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_id)),
                ("ewarAssistRecorded", PyRep::Bool(false)),
                ("immune", PyRep::Bool(true)),
                ("damageDealt", PyRep::Real(0.0)),
            ]))));
        }
    }
    let is_overloaded = method == "Overload" || m.state == eve_dogma::ModuleState::Overloaded;
    let is_mwd = mod_type.group_id == 46
        && (mod_type.effect_ids.contains(&6730)
            || mod_type.default_attributes.get(&554).copied().unwrap_or(0.0) > 0.0
            || mod_type.name.contains("Microwarpdrive"));
    let is_mjd = matches!(mod_type.group_id, 1189 | 1533 | 4769)
        || mod_type.name.contains("Micro Jump Drive")
        || mod_type.name.contains("Micro Jump Field Generator");
    let is_mwd_or_mjd = is_mwd || is_mjd;

    if is_mwd {
        if let Err(e) =
            crate::services::pve_dungeon_runtime_rpc::check_deadspace_mwd_disallowed(
                ctx, sys, sys_id, ship_id,
            )
            .await
        {
            return std::ops::ControlFlow::Break(Some(Err(e)));
        }
    }
    if is_mwd_or_mjd {
        if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            if cs.is_mwd_mjd_disabled {
                return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
                    code: "WarpScrambled".into(),
                    message: "Cannot activate Microwarpdrive or Micro Jump Drive while warp scrambled"
                        .into(),
                    client_error: None,
                })));
            }
        }
    }
    if let Err(e) =     precheck_module_cycle_aggression_or_assistance_safety(
        ctx,
        sys,
        sys_id,
        char_id,
        mod_type.group_id,
        target_id_opt,
    )
    .await
    {
        return std::ops::ControlFlow::Break(Some(Err(e)));
    }
    std::ops::ControlFlow::Continue(CyclePrecheckResult {
        is_overloaded,
        is_mwd,
        is_mjd,
        is_mwd_or_mjd,
        passive_targeter_lock_opt,
    })
}

struct PendingWeaponVolley {
    sys: crate::system_actor::SystemHandle,
    sys_id: u32,
    ship_id: i64,
    module_id: i64,
    is_overloaded: bool,
    duration_s: f64,
    max_vel: f64,
    is_overload_only_resp: bool,
}

enum CycleExecutionOutcome {
    Ready(Option<Result<PyRep, BridgeRpcError>>),
    PendingWeaponVolley(PendingWeaponVolley),
    RouteToWeapons,
}

struct NonSpecialCycleStart<'a> {
    start: ResolvedCycleStart<'a>,
    is_in_space: bool,
}

enum EarlyCycleOutcome<'a> {
    Ready(Option<Result<PyRep, BridgeRpcError>>),
    Continue(NonSpecialCycleStart<'a>),
    RouteToWeapons,
}

#[inline(never)]
fn build_weapon_volley_rpc_rep(
    module_id: i64,
    is_overloaded: bool,
    max_vel: f64,
    duration_s: f64,
    is_overload_only_resp: bool,
    volley_destroyed_target: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if is_overload_only_resp {
        return Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("overloaded", PyRep::Bool(true)),
            ("effectiveMaxVelocity", PyRep::Real(max_vel)),
        ])));
    }
    let mut resp = vec![
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("overloaded", PyRep::Bool(is_overloaded)),
        ("effectiveMaxVelocity", PyRep::Real(max_vel)),
        ("duration", PyRep::Real(duration_s)),
    ];
    if volley_destroyed_target {
        resp.push(("destroyed", PyRep::Bool(true)));
    }
    Some(Ok(PyRep::keyval(resp)))
}

#[inline(never)]
async fn fetch_weapon_volley_actions(
    sys: crate::system_actor::SystemHandle,
    ship_id: i64,
    module_id: i64,
) -> (bool, Option<Vec<crate::system_actor::PendingCycleAction>>) {
    match sys.fire_weapon_cycle_volley(ship_id, module_id).await {
        Ok((destroyed, weapon_actions)) => {
            let volley_destroyed_target = !destroyed.is_empty();
            drop(destroyed);
            let actions_opt = (!weapon_actions.is_empty()).then_some(weapon_actions);
            (volley_destroyed_target, actions_opt)
        }
        Err(_) => (false, None),
    }
}

fn dispatch_dogma_module_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> crate::router::rpc_registry::RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) = crate::services::corp_registry_ops::handle_fleet_burst_fallback(
            ctx, session, service, method, args,
        )
        .await
        {
            return Some(res);
        }
        if let Some(res) =
            super::handle(ctx, session, bound_entry, service, method, args, kwargs).await
        {
            if res.is_ok() {
                crate::services::character_skill_ops::post_inventory_fitting_hook(
                    ctx,
                    session,
                    bound_entry,
                    method,
                    args,
                )
                .await;
            }
            return Some(res);
        }
        Some(
            crate::services::combat_crimewatch::combat_weapons::handle_weapons(
                ctx, session, service, method, args, kwargs,
            )
            .await
            .unwrap_or_else(|| Err(module_activation_unhandled_error(service, method))),
        )
    })
}

pub const RPCS: &[crate::router::rpc_registry::RpcEntry] = &[
    crate::router::rpc_registry::rpc_entry(
        "dogmaIM",
        "Activate",
        crate::router::rpc_registry::RpcDispatchHandler::BoundAsync(dispatch_dogma_module_rpc),
    ),
    crate::router::rpc_registry::rpc_entry(
        "dogmaIM",
        "Overload",
        crate::router::rpc_registry::RpcDispatchHandler::BoundAsync(dispatch_dogma_module_rpc),
    ),
];

#[inline(never)]
pub(crate) async fn handle_module_cycles(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!((service, method), ("dogmaIM", "Activate") | ("dogmaIM", "Overload")) {
        return None;
    }
    match handle_module_cycles_inner(ctx, session, _bound_entry, method, args, kwargs).await {
        CycleExecutionOutcome::Ready(res) => res,
        CycleExecutionOutcome::RouteToWeapons => Some(
            crate::services::combat_crimewatch::combat_weapons::handle_weapons(
                ctx, session, service, method, args, kwargs,
            )
            .await
            .unwrap_or_else(|| Err(module_activation_unhandled_error(service, method))),
        ),
        CycleExecutionOutcome::PendingWeaponVolley(volley) => {
            let PendingWeaponVolley {
                sys,
                sys_id,
                ship_id,
                module_id,
                is_overloaded,
                duration_s,
                max_vel,
                is_overload_only_resp,
            } = volley;
            let (volley_destroyed_target, actions_opt) =
                fetch_weapon_volley_actions(sys, ship_id, module_id).await;
            if let Some(weapon_actions) = actions_opt {
                execute_pending_cycle_actions_list(ctx, sys_id, weapon_actions).await;
            }
            build_weapon_volley_rpc_rep(
                module_id,
                is_overloaded,
                max_vel,
                duration_s,
                is_overload_only_resp,
                volley_destroyed_target,
            )
        }
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_module_cycles_inner(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> CycleExecutionOutcome {
    match resolve_and_run_early_cycle_paths(ctx, session, method, args, kwargs).await {
        EarlyCycleOutcome::Ready(res) => CycleExecutionOutcome::Ready(res),
        EarlyCycleOutcome::RouteToWeapons => CycleExecutionOutcome::RouteToWeapons,
        EarlyCycleOutcome::Continue(started) => {
            run_standard_module_cycle(ctx, method, kwargs, started).await
        }
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn resolve_and_run_early_cycle_paths<'a>(
    ctx: &'a ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> EarlyCycleOutcome<'a> {
    let start = match try_start_or_resolve_cycle(ctx, session, method, args, kwargs).await {
        std::ops::ControlFlow::Continue(v) => v,
        std::ops::ControlFlow::Break(res) => return EarlyCycleOutcome::Ready(res),
    };
    let is_in_space = !start.is_docked;
    match check_cycle_safety_and_weapon_routing(
        ctx, &start.sys, start.sys_id, start.char_id, start.ship_id, start.module_id,
        start.m.type_id, start.mod_type, start.is_weapon, is_in_space, method,
        start.effect_name.as_str(), start.target_id_opt,
    ).await {
        CycleSafetyRouting::Continue => {}
        CycleSafetyRouting::Ready(res) => return EarlyCycleOutcome::Ready(res),
        CycleSafetyRouting::RouteToWeapons => {
            drop(start);
            return EarlyCycleOutcome::RouteToWeapons;
        }
    }

    // Mining lasers, strip miners, ice and gas harvesters run on the repeating MN-04 cycle engine.
    if crate::services::mining_cycle::mining_kind_of(&ctx.sde, start.mod_type).is_some() {
        let Some(target_id) = start.target_id_opt.filter(|&t| t > 0) else {
            return EarlyCycleOutcome::Ready(Some(Err(target_not_locked_rpc_error("Mining modules require a locked target asteroid"))));
        };
        let res = crate::services::mining_cycle::activate(ctx, start.char_id, start.ship_id, start.sys_id, start.module_id, target_id).await;
        return EarlyCycleOutcome::Ready(finish_space_activation(ctx, &start.sys, is_in_space, start.char_id, start.ship_id, res).await);
    }
    if let Some(res) = try_activate_early_special_modules(
        ctx, &start.sys, start.sys_id, start.char_id, start.ship_id, start.module_id, &start.m, start.mod_type, &start.fit,
        start.effect_name.as_str(), start.target_id_opt, start.is_weapon, is_in_space, method, kwargs,
    ).await {
        return EarlyCycleOutcome::Ready(Some(res));
    }
    // Scan Probe Launcher (`group_id == 481`, e.g., `Core Probe Launcher I` `17938`)
    if start.mod_type.group_id == 481 {
        let res = crate::services::space_destiny::activate_scan_probe_launcher(
            ctx, session, &start.sys, start.char_id, start.ship_id, start.sys_id, start.module_id, start.m.type_id, start.m.flag_id,
            start.effect_name.as_str(), start.target_id_opt, kwargs,
        ).await;
        return EarlyCycleOutcome::Ready(finish_space_activation(ctx, &start.sys, is_in_space, start.char_id, start.ship_id, res).await);
    }
    // Interdiction Sphere Launcher (`group_id == 589`, e.g., `Interdiction Sphere Launcher I` `22782`)
    if start.mod_type.group_id == 589 {
        return EarlyCycleOutcome::Ready(Some(activate_interdiction_sphere_module(
            ctx, &start.sys, start.sys_id, start.char_id, start.ship_id, start.fit.ship.type_id, start.module_id, &start.m, start.mod_type,
            start.effect_name.as_str(), start.target_id_opt, start.is_docked, is_in_space,
        ).await));
    }
    // Survey Probe Launcher (`group_id == 1226`, `33270` Survey Probe Launcher I, `33272` Survey Probe Launcher II)
    if start.mod_type.group_id == 1226 {
        return EarlyCycleOutcome::Ready(Some(activate_survey_probe_launcher(
            ctx, &start.sys, start.sys_id, start.char_id, start.ship_id, start.module_id, start.m.flag_id, start.mod_type, start.target_id_opt, is_in_space,
        ).await));
    }
    EarlyCycleOutcome::Continue(NonSpecialCycleStart {
        start,
        is_in_space,
    })
}

#[inline(never)]
async fn run_standard_module_cycle(
    ctx: &ServerContext, method: &str, kwargs: &HashMap<String, PyRep>, started: NonSpecialCycleStart<'_>,
) -> CycleExecutionOutcome {
    match prepare_committed_cycle_work(ctx, method, kwargs, started).await {
        std::ops::ControlFlow::Break(res) => CycleExecutionOutcome::Ready(res),
        std::ops::ControlFlow::Continue(work) => commit_and_finalize_boxed_cycle(ctx, method, kwargs, work).await,
    }
}

#[inline(never)]
async fn prepare_committed_cycle_work<'a>(
    ctx: &ServerContext, method: &str, kwargs: &HashMap<String, PyRep>, started: NonSpecialCycleStart<'a>,
) -> std::ops::ControlFlow<Option<Result<PyRep, BridgeRpcError>>, Box<CommittedCycleWork<'a>>> {
    let NonSpecialCycleStart {
        start: ResolvedCycleStart {
            char_id, ship_id, mut effect_name, target_id_opt, repeat, is_docked: _,
            sys_id, sys, mut fit, module_id, mut m, mod_type, is_weapon,
        },
        is_in_space,
    } = started;

    let CyclePrecheckResult {
        is_overloaded, is_mwd, is_mjd, is_mwd_or_mjd, passive_targeter_lock_opt,
    } = reconcile_and_validate_cycle_activation(
        ctx, &sys, sys_id, char_id, ship_id, module_id, method, is_weapon, is_in_space,
        target_id_opt, mod_type, &mut m, &mut fit,
    ).await?;

    let ship_group_id = ctx.sde.types.get(&fit.ship.type_id).map(|t| t.group_id).unwrap_or(0);
    let mut prep = {
        let pre_mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
        match prepare_ewar_and_capital_module(
            ctx, &sys, sys_id, char_id, ship_id, ship_group_id, module_id, &m, mod_type, &pre_mod_state,
            &fit, target_id_opt, is_overloaded, is_mwd, is_mjd, kwargs,
        ).await {
            Some(Ok(v)) => v,
            Some(Err(e)) => return std::ops::ControlFlow::Break(Some(Err(e))),
            None => unreachable!(),
        }
    };
    prep.deferred_post_cycle.passive_targeter_lock = passive_targeter_lock_opt;

    let params = {
        let pre_mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
        match prepare_cycle_params_sync(
            ctx, &sys, char_id, ship_id, ship_group_id, module_id, &m, mod_type, &pre_mod_state, &fit,
            target_id_opt, is_overloaded, is_mjd, &mut effect_name, repeat, &mut prep,
        ) {
            Ok(p) => p,
            Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
        }
    };
    if mod_type.group_id == 1122 {
        if let Err(e) = crate::combat::drones_salvage_tractor::validate_wreck_salvage_target(
            ctx, sys_id, char_id, ship_id, target_id_opt.unwrap_or(0), Some(mod_type.type_id),
        ).await {
            return std::ops::ControlFlow::Break(Some(Err(e)));
        }
    }
    if mod_type.group_id == 650 {
        if let Err(e) = crate::services::combat_crimewatch::precheck_tractor_beam_activation(
            ctx, &sys, char_id, ship_id, module_id, mod_type, target_id_opt,
        ).await {
            return std::ops::ControlFlow::Break(Some(Err(e)));
        }
    }
    let (mod_state, kin) = match prepare_weapon_kinematics_sync(
        ctx, sys_id, char_id, module_id, &m, mod_type, &mut fit, is_overloaded, &params, &prep.deferred_post_cycle,
    ) {
        Ok(v) => v,
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };
    if (kin.is_missile_group || kin.is_turret_group || kin.is_vorton_group) && target_id_opt.is_some() {
        let tid = target_id_opt.unwrap_or(0);
        if let Err(e) = check_weapon_cycle_target_range(ctx, &sys, ship_id, tid, mod_type, &kin).await {
            return std::ops::ControlFlow::Break(Some(Err(e)));
        }
    }
    if matches!(mod_type.group_id, 80 | 353) {
        let burst_plan = match prepare_burst_jammer_plan(
            ctx, &sys, sys_id, char_id, ship_id, mod_type, &mod_state, params.duration_s,
        ).await {
            Ok(p) => p,
            Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
        };
        prep.deferred_post_cycle.burst_jammer_plan = Some(burst_plan);
    }
    std::ops::ControlFlow::Continue(Box::new(CommittedCycleWork {
        sys, sys_id, char_id, ship_id, module_id, mod_type, target_id_opt,
        is_overloaded, is_mwd_or_mjd, is_weapon, is_in_space,
        fit, m, mod_state, prep, params, kin, effect_name,
    }))
}

struct CommittedCycleWork<'a> {
    sys: crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &'a eve_sde::InvType,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    is_mwd_or_mjd: bool,
    is_weapon: bool,
    is_in_space: bool,
    fit: Box<eve_dogma::ShipFittingEngine>,
    m: eve_dogma::FittedModule,
    mod_state: eve_dogma::DogmaItemState,
    prep: EwarCapitalPrep,
    params: PreparedCycleParams,
    kin: WeaponKinematics,
    effect_name: String,
}

#[inline(never)]
async fn commit_and_finalize_boxed_cycle(
    ctx: &ServerContext, method: &str, kwargs: &HashMap<String, PyRep>, mut work: Box<CommittedCycleWork<'_>>,
) -> CycleExecutionOutcome {
    let w = &mut *work;
    let (target_deltas, instant_rep_opt) = match commit_cycle_to_actor_and_effects(
        ctx, &w.sys, w.sys_id, w.char_id, w.ship_id, w.module_id, &w.m, w.mod_type, &w.mod_state, &w.fit,
        w.target_id_opt, w.is_overloaded, w.is_mwd_or_mjd, w.effect_name.as_str(), w.is_in_space,
        kwargs, &mut w.prep, &w.params, &w.kin,
    ).await {
        Ok(v) => v,
        Err(e) => return CycleExecutionOutcome::Ready(Some(Err(e))),
    };
    finalize_cycle_activation_and_response(
        ctx, &w.sys, w.sys_id, w.char_id, w.ship_id, w.module_id, &w.m, w.mod_type, &w.mod_state, &mut w.fit,
        w.target_id_opt, w.is_overloaded, w.is_weapon, method,
        w.effect_name.as_str(), &w.prep, &w.params, &w.kin, target_deltas, instant_rep_opt,
    ).await
}

struct PreparedCycleParams {
    duration_s: f64,
    cap_need: f64,
    speed_boost_factor: f64,
    shield_boost_amount: f64,
    armor_repair_amount: f64,
    hull_repair_amount: f64,
    remote_armor_amount: f64,
    mutadaptive_spool_step: f64,
    mutadaptive_spool_max: f64,
    is_asb: bool,
    asb_fallback_cap_need: f64,
    is_aar: bool,
    aar_paste_rate: i32,
    aar_base_armor_repair: f64,
    cap_booster_inject_amount: f64,
    is_rah: bool,
    rah_shift_amount: f64,
    is_auto_targeter: bool,
    auto_target_max_range: f64,
    auto_target_max_locks: usize,
    auto_target_scan_res: f64,
    reactivation_delay_s: f64,
    rack_idx: usize,
    heat_damage_per_cycle: f64,
    linked_turrets: Vec<i64>,
    active_burst_charge_tid: Option<u32>,
    crystal_volatility_chance: f64,
    crystal_volatility_damage: f64,
    cap_drain_target: f64,
    cap_transfer_target: f64,
    neut_optimal: f64,
    neut_falloff: f64,
    nosferatu_blood_raider_override: bool,
    ehe_hull_resists: Option<[f64; 4]>,
    repeat: i32,
    effective_max_capacitor: Option<f64>,
}

struct WeaponKinematics {
    is_turret_group: bool,
    is_missile_group: bool,
    is_guided_missile: bool,
    is_vorton_group: bool,
    weapon_optimal_m: f64,
    weapon_falloff_m: f64,
    weapon_tracking_speed: f64,
    weapon_sig_resolution_m: f64,
    missile_explosion_velocity_ms: f64,
    missile_explosion_radius_m: f64,
    missile_drf: f64,
    missile_max_velocity_ms: f64,
    missile_flight_time_s: f64,
    missile_ball_radius: f64,
    missile_reload_ms: f64,
    missile_charge_capacity: u32,
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn prepare_cycle_params_sync(
    ctx: &ServerContext, sys: &crate::system_actor::SystemHandle, char_id: i64, ship_id: i64, ship_group_id: u32, module_id: i64,
    m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType, mod_state: &eve_dogma::DogmaItemState, fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>, is_overloaded: bool, is_mjd: bool, effect_name: &mut String, repeat: i32, prep: &mut EwarCapitalPrep,
) -> Result<PreparedCycleParams, BridgeRpcError> {
    let duration_ms = [mod_state.get_attribute(attr_ids::DURATION, &ctx.sde), mod_state.get_attribute(3115, &ctx.sde), mod_state.get_attribute(attr_ids::SPEED, &ctx.sde)]
        .into_iter().find(|&v| v > 0.0).unwrap_or(if prep.mjd_jump_distance_m.is_some() { 12_000.0 } else { 0.0 });
    let duration_s = if duration_ms > 0.0 { duration_ms / 1000.0 } else { 1.0 };
    let mut cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
    let speed_factor = mod_state.get_attribute(attr_ids::SPEED_FACTOR, &ctx.sde);
    let has_speed_factor_attr = mod_type.group_id == 46
        || mod_type.effect_ids.iter().any(|&eid| matches!(eid, 6730 | 6731))
        || mod_type.default_attributes.contains_key(&attr_ids::SPEED_FACTOR)
        || mod_state.base_attributes.contains_key(&attr_ids::SPEED_FACTOR);
    let speed_boost_factor = if mod_type.group_id == 330 {
        let is_covert_cloak = mod_type.effect_ids.iter().any(|&eid| ctx.sde.effects.get(&eid).is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe));
        if is_covert_cloak && fit.ship.get_attribute(906, &ctx.sde) <= 0.0 && !matches!(ship_group_id, 830 | 833 | 898 | 963 | 1202 | 1283 | 1527 | 1972) {
            return Err(BridgeRpcError { code: "ShipCannotUseCovertCloak".into(), message: "Ship cannot activate Covert Ops Cloaking Device".into(), client_error: None });
        }
        if !effect_name.starts_with("cloaking") {
            *effect_name = if is_covert_cloak { "cloakingWarpSafe" } else { "cloaking" }.to_string();
        }
        let vel_mod = mod_state.get_attribute(306, &ctx.sde);
        if is_covert_cloak || (vel_mod - 1.0).abs() < 1e-6 || vel_mod <= 0.0 || vel_mod >= 1.0 { 0.0 } else { vel_mod - 1.0 }
    } else if has_speed_factor_attr && speed_factor > 0.0 {
        let speed_boost_attr = mod_state.get_attribute(567, &ctx.sde);
        let ship_mass = fit.ship.get_attribute(attr_ids::MASS, &ctx.sde);
        let mass_addition = if !fit.pending_overload_modules.contains(&module_id) && matches!(m.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded) {
            0.0
        } else {
            mod_state.get_attribute(eve_dogma::propulsion_mass_addition_attr_id(mod_state, &ctx.sde), &ctx.sde)
        };
        eve_dogma::compute_propulsion_speed_boost(speed_factor, speed_boost_attr, ship_mass, mass_addition)
    } else {
        0.0
    };
    let overload_mult = crate::services::fitting_cycles_overheat::compute_overload_multiplier(is_overloaded, mod_type, &ctx.sde);
    let shield_boost_amount = if matches!(mod_type.group_id, 41 | 1697) { 0.0 } else { mod_state.get_attribute(68, &ctx.sde) * overload_mult };
    let duration_s = if shield_boost_amount > 0.0 {
        let boost_mult = sys.view().combat_state(ship_id).map(|cs| cs.shield_boost_multiplier).filter(|&m| m > 0.0).unwrap_or(1.0);
        duration_s * boost_mult
    } else {
        duration_s
    };
    let initial_armor_repair = if matches!(mod_type.group_id, 325 | 2018 | 1698) { 0.0 } else { mod_state.get_attribute(84, &ctx.sde) * overload_mult };
    let hull_repair_amount = if mod_type.group_id == 585 { 0.0 } else { mod_state.get_attribute(83, &ctx.sde) * overload_mult };
    let remote_armor_amount = if mod_type.group_id == 2018 && prep.remote_armor_amount <= 0.0 { mod_type.default_attributes.get(&84).copied().unwrap_or(0.0) } else { prep.remote_armor_amount };
    let mutadaptive_spool_step = if mod_type.group_id == 2018 && prep.mutadaptive_spool_step <= 0.0 { mod_type.default_attributes.get(&2796).copied().unwrap_or(0.0) } else { prep.mutadaptive_spool_step };
    let mutadaptive_spool_max = if mod_type.group_id == 2018 && prep.mutadaptive_spool_max <= 0.0 { mod_type.default_attributes.get(&2797).copied().unwrap_or(0.0) } else { prep.mutadaptive_spool_max };

    // Ancillary Shield Booster (`group_id == 1156 | 1697`) & Ancillary Armor Repairer (`group_id == 1199 | 1698`)
    let crate::services::fitting_cycles_overheat::AncillaryRepairPrep {
        is_asb, asb_fallback_cap_need, cap_need: anc_cap_need, is_aar, aar_paste_rate, aar_base_armor_repair,
        armor_repair_amount, remote_armor_amount, pending_charge_consume,
    } = crate::services::fitting_cycles_overheat::plan_ancillary_module(
        ctx, ship_id, module_id, m.flag_id, mod_type, fit, is_overloaded, cap_need, initial_armor_repair, remote_armor_amount, overload_mult,
    );
    cap_need = anc_cap_need;
    prep.deferred_post_cycle.ancillary_charge_to_consume = pending_charge_consume;

    // Capacitor Booster (`group_id == 76`, `feat:modules.cap-booster`)
    let mut cap_booster_inject_amount = 0.0;
    if mod_type.group_id == 76 {
        let Some((cid, charge_tid)) = ctx.db.with_snapshot(|snap| {
            snap.items.values().find(|i| i.location_id == ship_id && (i.flag as u16) == m.flag_id && i.item_id != module_id && i.quantity > 0).map(|i| (i.item_id, i.type_id))
        }) else {
            return Err(BridgeRpcError::no_charges_loaded(mod_type.type_id as i64, "Capacitor Booster has no Cap Booster charges loaded"));
        };
        prep.deferred_post_cycle.cap_booster_charge_id = Some(cid);
        let eval_inject = fit.get_charge_effective_attribute(module_id, 67, &ctx.sde);
        cap_booster_inject_amount = if eval_inject > 0.0 { eval_inject } else { ctx.sde.types.get(&charge_tid).and_then(|t| t.default_attributes.get(&67).copied()).unwrap_or(100.0) };
        cap_need = 0.0;
    }

    // Reactive Armor Hardener (`group_id == 1150`, `4403`, `feat:modules.reactive-armor-hardener`)
    let is_rah = mod_type.group_id == 1150 || m.type_id == 4403;
    let rah_shift_amount = mod_state.get_attribute(1849, &ctx.sde).max(6.0);
    // Auto-Targeting System (`group_id == 96`, `1182`/`1436`, `feat:modules.auto-targeting-system`)
    let is_auto_targeter = mod_type.group_id == 96;
    let auto_target_max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde).min(mod_state.get_attribute(143, &ctx.sde).max(50_000.0));
    let ship_max_locks = fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize;
    let skills = ctx.character_skill_levels(char_id);
    let auto_target_max_locks = eve_dogma::compute_effective_max_locked_targets(ship_max_locks, skills.get(&3429).copied().unwrap_or(0), skills.get(&3430).copied().unwrap_or(0));
    let auto_target_scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);

    let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
    let reactivation_delay_s = if react_delay_ms > 0.0 { react_delay_ms / 1000.0 } else { 0.0 };
    let (duration_s, reactivation_delay_s) = if prep.grants_interdiction_nullification {
        let null_skill_lvl = skills.get(&57319).copied().unwrap_or(0);
        crate::services::tactical_modules::compute_nullifier_duration_and_cooldown(duration_s, reactivation_delay_s, null_skill_lvl)
    } else {
        (duration_s, reactivation_delay_s)
    };

    let rack_idx = crate::services::fitting_cycles_overheat::rack_index_for_flag(m.flag_id).unwrap_or(2);
    let heat_damage_per_cycle = {
        let mut pre_thermo = mod_state.clone();
        pre_thermo.modifiers.retain(|m| m.effect_id != Some(3196) && m.source_item_id != 28164);
        let hd = pre_thermo.get_attribute(1211, &ctx.sde);
        if hd > 0.0 { hd } else { 3.4 }
    };

    let linked_turrets: Vec<i64> = if is_asb || is_aar || matches!(mod_type.group_id, 41 | 67 | 76 | 325 | 585 | 899 | 2008 | 2018) {
        Vec::new()
    } else {
        ctx.db.with_snapshot(|snap| {
            if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) { bank.clone() }
            else if matches!(m.flag_id, 27..=34) { vec![module_id] }
            else { Vec::new() }
        })
    };

    let (mut active_burst_charge_tid, mut crystal_volatility_chance, mut crystal_volatility_damage) = (m.charge_type_id, 0.0, -1.0);
    if !linked_turrets.is_empty() && mod_type.group_id != 899 {
        let first_charge_tid = ctx.db.with_snapshot(|snap| {
            linked_turrets.iter().find_map(|&tid| {
                let tf = snap.items.get(&tid).map(|i| i.flag)?;
                snap.items.values().find(|i| is_slot_loaded_charge_item(ctx, mod_type, i, ship_id, tf as u16, tid)).map(|i| i.type_id)
            })
        });
        let is_non_volatile_crystal = first_charge_tid.and_then(|ctid| ctx.sde.types.get(&ctid))
            .is_some_and(|t| matches!(t.group_id, 86 | 374 | 375 | 482 | 663) && t.default_attributes.get(&784).copied().unwrap_or(0.0) <= 0.0);
        if let Some(ctid) = first_charge_tid {
            active_burst_charge_tid = Some(ctid);
        } else if let Some(plan) = prep.deferred_post_cycle.weapon_autoload.as_mut().filter(|p| !p.slot_allocations.is_empty()) {
            active_burst_charge_tid = Some(plan.ammo_type_id);
            match classify_charge_volatility(&ctx.sde, ctx.sde.types.get(&plan.ammo_type_id), None) {
                ChargeVolatility::VolatileCrystal { damage, chance } => { crystal_volatility_damage = damage; crystal_volatility_chance = chance; plan.consume_one_round_per_slot = false; }
                ChargeVolatility::NonVolatileCrystal => { crystal_volatility_damage = -1.0; plan.consume_one_round_per_slot = false; }
                ChargeVolatility::Consumable => { crystal_volatility_damage = 0.0; crystal_volatility_chance = -1.0; plan.consume_one_round_per_slot = true; }
            }
        }
        if !is_non_volatile_crystal && prep.deferred_post_cycle.weapon_autoload.is_none() && (first_charge_tid.is_some() || active_burst_charge_tid.is_some()) {
            ctx.db.with_snapshot(|snap| {
                for &tid in &linked_turrets {
                    if let Some(tf) = snap.items.get(&tid).map(|i| i.flag) {
                        if let Some(ci) = snap.items.values().find(|i| is_slot_loaded_charge_item(ctx, mod_type, i, ship_id, tf as u16, tid)) {
                            active_burst_charge_tid = Some(ci.type_id);
                            let ch_state_opt = fit.charge_state(tid, &ctx.sde).filter(|cs| cs.type_id == ci.type_id);
                            match classify_charge_volatility(&ctx.sde, ctx.sde.types.get(&ci.type_id), ch_state_opt.as_ref()) {
                                ChargeVolatility::VolatileCrystal { damage, chance } => { crystal_volatility_damage = damage; crystal_volatility_chance = chance; }
                                ChargeVolatility::NonVolatileCrystal => { crystal_volatility_damage = -1.0; }
                                ChargeVolatility::Consumable => {
                                    crystal_volatility_damage = 0.0; crystal_volatility_chance = -1.0;
                                    prep.deferred_post_cycle.turret_charge_consumptions.push((tid, ci.item_id));
                                }
                            }
                        } else if active_burst_charge_tid.is_some() && crystal_volatility_damage < 0.0 {
                            crystal_volatility_damage = 0.0;
                        }
                    }
                }
            });
        }
    }
    ctx.db.with_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let mut checked = Vec::new();
        let mut check_cid = |ctid: u32| -> Result<(), BridgeRpcError> {
            if !checked.contains(&ctid) { ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid)?; checked.push(ctid); }
            Ok(())
        };
        if let Some(ctid) = active_burst_charge_tid { check_cid(ctid)?; }
        if !linked_turrets.is_empty() && mod_type.group_id != 899 {
            for &tid in &linked_turrets {
                if let Some(tf) = snap.items.get(&tid).map(|i| i.flag) {
                    for i in snap.items.values().filter(|i| is_slot_loaded_charge_item(ctx, mod_type, i, ship_id, tf as u16, tid)) { check_cid(i.type_id)?; }
                }
                if let Some(&ctid) = snap.module_active_charges.get(&tid) { check_cid(ctid)?; }
            }
        }
        Ok(())
    })?;

    let (raw_cap_drain, raw_cap_transfer) = (mod_state.get_attribute(97, &ctx.sde), mod_state.get_attribute(90, &ctx.sde));
    let (cap_drain_target, cap_transfer_target, neut_optimal, neut_falloff) =
        if let Some(tid) = target_id_opt.filter(|_| raw_cap_drain > 0.0 || raw_cap_transfer > 0.0) {
            let ewar_resist = ctx.db.with_snapshot(|snap| {
                snap.characters.values().find(|c| c.active_ship_id == tid).map(|c| c.character_id)
                    .or_else(|| snap.items.get(&tid).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)))
            }).and_then(|cid| ctx.fit(cid, tid).ok()).map(|tf| tf.ship.get_attribute(2045, &ctx.sde).clamp(0.0, 1.0)).unwrap_or(1.0);
            (raw_cap_drain * ewar_resist, raw_cap_transfer * ewar_resist, mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde), eve_dogma::get_module_falloff(mod_state, &ctx.sde))
        } else {
            (raw_cap_drain, raw_cap_transfer, 0.0, 0.0)
        };
    let nosferatu_blood_raider_override = fit.ship.get_attribute(1945, &ctx.sde) > 0.0 || eve_dogma::is_blood_raider_nosferatu_hull(fit.ship.type_id, &ctx.sde);
    let is_cehe = matches!(m.type_id, 40714..=40718) || mod_type.effect_ids.contains(&6484);
    let ehe_hull_resists = is_cehe.then(|| {
        [974, 975, 976, 977].map(|aid| mod_type.default_attributes.get(&aid).copied().unwrap_or(0.05))
    });
    let repeat = if is_mjd || prep.grants_interdiction_nullification || prep.is_adc || is_cehe { 0 } else { repeat };
    let effective_max_capacitor = mod_type.default_attributes.contains_key(&147).then(|| fit.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, &ctx.sde));

    Ok(PreparedCycleParams {
        duration_s, cap_need, speed_boost_factor, shield_boost_amount, armor_repair_amount, hull_repair_amount, remote_armor_amount,
        mutadaptive_spool_step, mutadaptive_spool_max, is_asb, asb_fallback_cap_need, is_aar, aar_paste_rate, aar_base_armor_repair,
        cap_booster_inject_amount, is_rah, rah_shift_amount, is_auto_targeter, auto_target_max_range, auto_target_max_locks, auto_target_scan_res,
        reactivation_delay_s, rack_idx, heat_damage_per_cycle, linked_turrets, active_burst_charge_tid, crystal_volatility_chance, crystal_volatility_damage,
        cap_drain_target, cap_transfer_target, neut_optimal, neut_falloff, nosferatu_blood_raider_override, ehe_hull_resists, repeat, effective_max_capacitor,
    })
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn prepare_weapon_kinematics_sync(
    ctx: &ServerContext, sys_id: u32, char_id: i64, module_id: i64, m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine, is_overloaded: bool, params: &PreparedCycleParams, deferred_post_cycle: &DeferredPostCycleEffects,
) -> Result<(eve_dogma::DogmaItemState, WeaponKinematics), BridgeRpcError> {
    if let Some(ctid) = params.active_burst_charge_tid {
        if let Some(fm) = fit.modules.get_mut(&module_id).filter(|fm| fm.charge_type_id != Some(ctid)) {
            fm.charge_type_id = Some(ctid);
            fm.charge_quantity = fm.charge_quantity.max(1);
            fit.recalculate_stats(&ctx.sde);
        }
    }
    let mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
    let is_turret_group = matches!(mod_type.group_id, 53 | 55 | 74 | 1986);
    let is_missile_group = is_missile_launcher_group(mod_type.group_id) || matches!(mod_type.group_id, 512 | 862);
    let planned_autoload_slots = deferred_post_cycle.weapon_autoload.as_ref().map_or(0, |plan| plan.slot_allocations.len());
    let is_guided_missile = crate::system_actor::is_guided_missile_launcher_group(i64::from(mod_type.group_id));
    let is_vorton_group = mod_type.group_id == 4060;
    if is_guided_missile
        && (params.active_burst_charge_tid.is_none()
            || (!params.linked_turrets.is_empty() && deferred_post_cycle.turret_charge_consumptions.len() + planned_autoload_slots < params.linked_turrets.len()))
    {
        return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
    }
    let weapon_dark_mult = crate::services::pve_abyssal_hazards::dark_weather_midpocket_turret_mult(ctx, char_id, sys_id, is_missile_group, module_id, mod_type);
    let active_charge_sde = params.active_burst_charge_tid.and_then(|cid| ctx.sde.types.get(&cid));
    let has_mod_charge_modifier = |attr: u16| {
        mod_state.modifiers.iter().any(|mm| mm.target_attribute_id == attr && mm.source_item_id == 0 && mm.effect_id.is_some_and(|eid| active_charge_sde.is_some_and(|ct| ct.effect_ids.contains(&eid))))
    };
    let pos_or = |v: f64, fallback: f64| if v > 0.0 { v } else { fallback };
    let def_attr = |t: &eve_sde::InvType, aid: u16, d: f64| t.default_attributes.get(&aid).copied().unwrap_or(d);
    let chg_attr = |aid: u16, d: f64| active_charge_sde.and_then(|ct| ct.default_attributes.get(&aid).copied()).unwrap_or(d);
    let mod_pos_or = |aid: u16, d: f64| pos_or(mod_state.get_attribute(aid, &ctx.sde), def_attr(mod_type, aid, d));

    let (missile_max_velocity_ms, missile_flight_time_s, missile_max_range_m) = if is_missile_group {
        crate::services::combat_crimewatch::combat_missiles_aoe::compute_missile_flight_kinematics(fit, module_id, active_charge_sde, &ctx.sde)
    } else {
        (0.0, 0.0, 0.0)
    };
    let weapon_optimal_m = if is_turret_group {
        let base_opt = mod_pos_or(54, 5_000.0);
        (if has_mod_charge_modifier(54) { base_opt } else { base_opt * chg_attr(120, 1.0) }) * weapon_dark_mult
    } else if is_missile_group {
        missile_max_range_m
    } else if is_vorton_group {
        mod_pos_or(54, 25_000.0)
    } else {
        0.0
    };
    let weapon_falloff_m = if is_turret_group { pos_or(eve_dogma::get_module_falloff(&mod_state, &ctx.sde), def_attr(mod_type, 158, 5_000.0)) * weapon_dark_mult } else { 0.0 };
    let weapon_tracking_speed = if is_turret_group {
        let base_tr = mod_pos_or(160, 100.0);
        if has_mod_charge_modifier(160) { base_tr } else { base_tr * chg_attr(244, 1.0) }
    } else {
        0.0
    };
    let weapon_sig_resolution_m = if is_turret_group { mod_pos_or(620, 40_000.0) } else { 0.0 };
    let (missile_explosion_velocity_ms, missile_explosion_radius_m, missile_drf) = if is_missile_group {
        (
            pos_or(fit.get_charge_effective_attribute(module_id, 653, &ctx.sde), chg_attr(653, 170.0)),
            pos_or(fit.get_charge_effective_attribute(module_id, 654, &ctx.sde), chg_attr(654, 40.0)),
            pos_or(fit.get_charge_effective_attribute(module_id, 1353, &ctx.sde), chg_attr(1353, 0.604)),
        )
    } else if is_vorton_group {
        (mod_pos_or(653, 190.0), mod_pos_or(654, 43.0), mod_pos_or(1353, 0.40))
    } else {
        (0.0, 0.0, 0.0)
    };
    let missile_ball_radius = active_charge_sde.map(|ct| ct.radius).filter(|&r| r > 0.0).unwrap_or(300.0);
    let missile_reload_ms = if is_guided_missile {
        let v = mod_state.get_attribute(1795, &ctx.sde);
        if v > 0.0 { v } else { mod_type.default_attributes.get(&1795).copied().filter(|&x| x > 0.0).unwrap_or(if matches!(mod_type.group_id, 511 | 1245) { 35_000.0 } else { 10_000.0 }) }
    } else {
        0.0
    };
    let missile_charge_capacity = if is_guided_missile {
        let charge_vol = active_charge_sde.map(|t| t.volume.max(0.0001)).unwrap_or(0.0025);
        let mod_cap = if mod_type.capacity > 0.0 { mod_type.capacity } else { def_attr(mod_type, 38, 0.0) };
        if mod_cap > 0.0 { (((mod_cap / charge_vol) + 1e-9).floor() as i32).max(1) as u32 } else { 1 }
    } else {
        0
    };

    Ok((mod_state, WeaponKinematics {
        is_turret_group, is_missile_group, is_guided_missile, is_vorton_group,
        weapon_optimal_m, weapon_falloff_m, weapon_tracking_speed, weapon_sig_resolution_m,
        missile_explosion_velocity_ms, missile_explosion_radius_m, missile_drf,
        missile_max_velocity_ms, missile_flight_time_s, missile_ball_radius, missile_reload_ms, missile_charge_capacity,
    }))
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn check_weapon_cycle_target_range(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    tid: i64,
    mod_type: &eve_sde::InvType,
    kin: &WeaponKinematics,
) -> Result<(), BridgeRpcError> {
    if let Some(surface_dist) =
        super::fitting_ewar_capital::measure_ball_surface_distance_m(sys, ship_id, tid).await
    {
        // Why: Both launch-time validation and `cycles.rs` compare `Ball::surface_distance` (#1004, #1247).
        if kin.is_guided_missile {
            if kin.weapon_optimal_m > 0.0 && surface_dist > kin.weapon_optimal_m + 1e-6 {
                return Err(BridgeRpcError::target_too_far(
                    resolve_target_display_name(ctx, tid),
                    mod_type.type_id as i64,
                    surface_dist,
                ));
            }
        } else if kin.is_turret_group {
            // Why: Reject out-of-range Cycle-0 turret activations before cap/ammo/timer commit (#1247).
            let turret_max_range_m = kin.weapon_optimal_m + 3.0 * kin.weapon_falloff_m;
            if turret_max_range_m > 0.0 && surface_dist > turret_max_range_m {
                return Err(BridgeRpcError::target_out_of_range(
                    resolve_target_display_name(ctx, tid),
                    mod_type.type_id as i64,
                    surface_dist,
                    format!(
                        "Target {tid} is {surface_dist:.1} m away (exceeds range {turret_max_range_m:.1} m)"
                    ),
                ));
            }
        } else if mod_type.group_id != 862
            && kin.weapon_optimal_m > 0.0
            && surface_dist > kin.weapon_optimal_m
        {
            // Why: Reject out-of-range Cycle-0 Vorton / targeted non-guided missile activations before cap/ammo/timer commit (#1247).
            return Err(BridgeRpcError::target_out_of_range(
                resolve_target_display_name(ctx, tid),
                mod_type.type_id as i64,
                surface_dist,
                format!(
                    "Target {tid} is {surface_dist:.1} m away (exceeds range {:.1} m)",
                    kin.weapon_optimal_m
                ),
            ));
        }
    }
    Ok(())
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn build_active_module_cycle_sync(
    ctx: &ServerContext, sys_id: u32, char_id: i64, ship_id: i64, module_id: i64,
    m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType, mod_state: &eve_dogma::DogmaItemState, fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>, is_overloaded: bool, is_mwd_or_mjd: bool, effect_name: &str,
    prep: &EwarCapitalPrep, params: &PreparedCycleParams, kin: &WeaponKinematics,
) -> crate::system_actor::ActiveModuleCycle {
    let active_charge_sde = params.active_burst_charge_tid.and_then(|cid| ctx.sde.types.get(&cid));
    let damage_vector = if kin.is_turret_group || kin.is_missile_group || kin.is_vorton_group {
        let mod_mult = if kin.is_missile_group {
            fit.modules.get(&module_id).and_then(|fm| fm.dynamic_attribute_overrides.get(&64).copied()).filter(|&v| v > 0.0).unwrap_or(1.0)
        } else {
            let m_mult = mod_state.get_attribute(64, &ctx.sde);
            if m_mult > 0.0 { m_mult } else { mod_type.default_attributes.get(&64).copied().unwrap_or(1.0) }
        };
        let dmg_mult = mod_mult * (params.linked_turrets.len().max(1) as f64);
        let eval_dmg = |aid: u16| -> f64 {
            if let Some(ct) = active_charge_sde {
                let v = fit.get_charge_effective_attribute(module_id, aid, &ctx.sde);
                (if v > 0.0 { v } else { ct.default_attributes.get(&aid).copied().unwrap_or(0.0) }) * dmg_mult
            } else {
                mod_state.get_attribute(aid, &ctx.sde) * dmg_mult
            }
        };
        let (em, therm, kin_dmg, exp) = (eval_dmg(114), eval_dmg(118), eval_dmg(117), eval_dmg(116));
        if em + therm + kin_dmg + exp > 0.0 { eve_destiny::DamageVector::new(em, therm, kin_dmg, exp) }
        else { eve_destiny::DamageVector::new(20.0, 20.0, 20.0, 20.0) }
    } else {
        eve_destiny::DamageVector::new(0.0, 0.0, 0.0, 0.0)
    };
    let max_effective_range_m = target_id_opt.map(|_| {
        let mut opt_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
        if mod_type.group_id == 65 || prep.disables_target_mwd_mjd {
            opt_range *= crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(ctx, char_id, sys_id);
        }
        let mut falloff = eve_dogma::get_module_falloff(mod_state, &ctx.sde);
        let dark_mult = crate::services::pve_abyssal_hazards::dark_weather_midpocket_turret_mult(ctx, char_id, sys_id, false, module_id, mod_type);
        if (dark_mult - 1.0).abs() > 1e-9 { opt_range *= dark_mult; falloff *= dark_mult; }
        if (kin.is_guided_missile || kin.is_vorton_group || kin.is_missile_group) && kin.weapon_optimal_m > 0.0 { kin.weapon_optimal_m }
        else if kin.is_turret_group && (kin.weapon_optimal_m > 0.0 || kin.weapon_falloff_m > 0.0) { kin.weapon_optimal_m + 3.0 * kin.weapon_falloff_m.max(0.0) }
        else if opt_range > 0.0 { opt_range + 3.0 * falloff.max(0.0) }
        else { fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde) }
    });
    crate::system_actor::ActiveModuleCycle {
        ship_id, module_id, module_type_id: m.type_id, owner_char_id: Some(char_id), effect_name: effect_name.to_string(),
        target_id: target_id_opt, repeat: if mod_type.group_id == 330 { params.repeat.max(1) } else { params.repeat },
        duration_s: params.duration_s, elapsed_s: 0.0, cap_need_per_cycle: params.cap_need,
        speed_boost_factor: params.speed_boost_factor, pending_overloaded: None,
        linked_turrets: params.linked_turrets.clone(), ammo_type_id: params.active_burst_charge_tid,
        damage_vector, cap_drain_target: params.cap_drain_target, cap_transfer_target: params.cap_transfer_target,
        nosferatu_blood_raider_override: params.nosferatu_blood_raider_override,
        shield_boost_amount: params.shield_boost_amount, armor_repair_amount: params.armor_repair_amount, hull_repair_amount: params.hull_repair_amount,
        remote_shield_amount: prep.remote_shield_amount, remote_armor_amount: params.remote_armor_amount,
        remote_hull_amount: prep.remote_hull_amount, remote_cap_amount: prep.remote_cap_amount,
        remote_optimal: if prep.remote_optimal > 0.0 { prep.remote_optimal } else { params.neut_optimal },
        remote_falloff: if prep.remote_falloff > 0.0 { prep.remote_falloff } else { params.neut_falloff },
        mutadaptive_spool_step: params.mutadaptive_spool_step, mutadaptive_spool_max: params.mutadaptive_spool_max, current_spool_cycles: 0,
        cap_booster_inject_amount: params.cap_booster_inject_amount, is_asb: params.is_asb, asb_fallback_cap_need: params.asb_fallback_cap_need,
        is_aar: params.is_aar, aar_paste_rate: params.aar_paste_rate, aar_base_armor_repair: params.aar_base_armor_repair,
        is_rah: params.is_rah, rah_shift_amount: params.rah_shift_amount, is_adc: prep.is_adc, ehe_hull_resists: params.ehe_hull_resists,
        is_overloaded, rack_idx: params.rack_idx, heat_damage_per_cycle: params.heat_damage_per_cycle, reactivation_delay_s: params.reactivation_delay_s,
        is_mwd_or_mjd, warp_scramble_points: prep.warp_scramble_points, disables_target_mwd_mjd: prep.disables_target_mwd_mjd,
        warp_core_stab_bonus: prep.warp_core_stab_bonus, hic_bubble_radius_m: prep.hic_bubble_radius_m,
        hic_velocity_multiplier: prep.hic_velocity_multiplier, hic_sig_multiplier: prep.hic_sig_multiplier, hic_mass_addition: prep.hic_mass_addition,
        mjd_jump_distance_m: prep.mjd_jump_distance_m, mjfg_field_params: prep.mjfg_field_params,
        grants_interdiction_nullification: prep.grants_interdiction_nullification, effective_max_capacitor: params.effective_max_capacitor,
        lance_beam_dir: None, grappler_optimal_m: prep.grappler_optimal_m, grappler_falloff_m: prep.grappler_falloff_m,
        grappler_speed_factor_pct: prep.grappler_speed_factor_pct, weapon_is_turret: kin.is_turret_group,
        weapon_is_missile: kin.is_missile_group || kin.is_vorton_group, weapon_is_guided_missile: kin.is_guided_missile,
        weapon_optimal_m: kin.weapon_optimal_m, weapon_falloff_m: kin.weapon_falloff_m,
        weapon_tracking_speed: kin.weapon_tracking_speed, weapon_sig_resolution_m: kin.weapon_sig_resolution_m,
        missile_explosion_radius_m: kin.missile_explosion_radius_m, missile_explosion_velocity_ms: kin.missile_explosion_velocity_ms,
        missile_drf: kin.missile_drf, missile_max_velocity_ms: kin.missile_max_velocity_ms, missile_flight_time_s: kin.missile_flight_time_s,
        missile_ball_radius: kin.missile_ball_radius, missile_reload_ms: kin.missile_reload_ms,
        missile_charge_type_id: params.active_burst_charge_tid, missile_charge_capacity: kin.missile_charge_capacity,
        reload_remaining_sim_s: 0.0, target_velocity_multiplier: prep.target_velocity_multiplier,
        immobilize_self: prep.immobilize_self, self_disallow_assistance: prep.self_disallow_assistance,
        self_ewar_immune: prep.self_ewar_immune, self_disable_mwd_mjd: prep.self_disable_mwd_mjd,
        self_mass_multiplier: prep.self_mass_multiplier, is_auto_targeter: params.is_auto_targeter,
        auto_target_max_range: params.auto_target_max_range, auto_target_max_locks: params.auto_target_max_locks,
        auto_target_scan_res: params.auto_target_scan_res, crystal_volatility_chance: params.crystal_volatility_chance,
        crystal_volatility_damage: params.crystal_volatility_damage, max_effective_range_m, started_at_unix: ctx.now_unix_secs(),
        origin_scene_epoch: None,
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn commit_cycle_to_actor_and_effects(
    ctx: &ServerContext, sys: &crate::system_actor::SystemHandle, sys_id: u32, char_id: i64, ship_id: i64, module_id: i64,
    m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType, mod_state: &eve_dogma::DogmaItemState, fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>, is_overloaded: bool, is_mwd_or_mjd: bool, effect_name: &str, is_in_space: bool,
    kwargs: &HashMap<String, PyRep>, prep: &mut EwarCapitalPrep, params: &PreparedCycleParams, kin: &WeaponKinematics,
) -> Result<((f64, f64), Option<PyRep>), BridgeRpcError> {
    let cycle = build_active_module_cycle_sync(ctx, sys_id, char_id, ship_id, module_id, m, mod_type, mod_state, fit, target_id_opt, is_overloaded, is_mwd_or_mjd, effect_name, prep, params, kin);
    let required_cap_gj = cycle.cap_need_per_cycle;
    let effective_max_cap = cycle.effective_max_capacitor;
    let allow_while_warping = mod_type.group_id == 4117 || is_warp_safe_cloak_module(ctx, mod_type);
    let allow_while_cloaked = matches!(mod_type.group_id, 330 | 481 | 4117);
    let replace_existing_cycle = mod_type.group_id == 72;
    let outcome = commit_module_cycle_atomic_with_options_notifications(
        ctx,
        sys,
        char_id,
        ship_id,
        cycle,
        allow_while_warping,
        allow_while_cloaked,
        replace_existing_cycle,
        is_in_space,
        crate::system_actor::ProtectionBreakReason::ModuleActivation,
        false,
    )
    .await
    .map_err(|e| {
        map_atomic_cycle_activation_error_for_ship(
            ctx,
            sys,
            char_id,
            ship_id,
            m.type_id,
            Some(required_cap_gj),
            effective_max_cap,
            e,
        )
    });
    let outcome = match outcome {
        Ok(o) => o,
        Err(fut) => return Err(fut.await),
    };
    if let Err(e) = evaluate_module_cycle_aggression_or_assistance(ctx, sys, sys_id, char_id, ship_id, mod_type.group_id, target_id_opt).await {
        let _ = sys.remove_module_cycle(ship_id, module_id).await;
        return Err(e);
    }
    let deferred = std::mem::take(&mut prep.deferred_post_cycle);
    let burst_already_handled = deferred.burst_jammer_plan.is_some();
    if let Err(e) = commit_post_cycle_activation_effects(ctx, sys, char_id, ship_id, module_id, m, mod_type, mod_state, target_id_opt, deferred).await {
        let _ = sys.remove_module_cycle(ship_id, module_id).await;
        return Err(e);
    }
    let instant_rep_opt = if burst_already_handled {
        None
    } else {
        match run_pre_notify_instant_module_effects(ctx, sys, sys_id, char_id, ship_id, module_id, m, mod_type, mod_state, is_overloaded, target_id_opt, params.duration_s, kwargs).await {
            Ok(rep_opt) => rep_opt,
            Err(e) => {
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
                return Err(e);
            }
        }
    };
    Ok(((outcome.target_cap_drained, outcome.target_shield_boosted), instant_rep_opt))
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn activate_post_cycle_snapshot_and_fit_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
    is_overloaded: bool,
    params: &PreparedCycleParams,
) -> bool {
    let m_params = [("typeID", m.type_id.to_string()), ("groupID", mod_type.group_id.to_string())];
    for ev in ["moduleActivate", "activateModule", "moduleActivated", "activateShipModule"] {
        crate::services::operations_manager_helpers::record_server_operation_event(ctx, char_id, ev, &m_params, 1);
    }
    if matches!(mod_type.group_id, 72 | 80 | 353 | 862) {
        ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).trigger_weapon_timer(char_id, ctx.now_unix());
    }
    let mut slot_still_has_charge = true;
    ctx.db.with_mut_snapshot(|snap| {
        if snap.items.contains_key(&module_id) {
            snap.module_active_states.insert(module_id);
            let tf = snap.items.get(&module_id).map(|i| i.flag as u16).unwrap_or(m.flag_id);
            let has_item = snap.items.values().any(|i| is_slot_loaded_charge_item(ctx, mod_type, i, ship_id, tf, module_id));
            if !has_item && params.crystal_volatility_chance < 0.0 {
                slot_still_has_charge = false;
                snap.module_active_charges.remove(&module_id);
            } else if let Some(cid) = params.active_burst_charge_tid {
                snap.module_active_charges.insert(module_id, cid);
            }
        }
    });
    fit.pending_overload_modules.remove(&module_id);
    let _ = fit.activate_module(module_id, is_overloaded, &ctx.sde);
    slot_still_has_charge
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn finalize_post_activation_fit_updates_and_push_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
    is_weapon: bool,
    is_overloaded: bool,
    slot_still_has_charge: bool,
    params: &PreparedCycleParams,
) {
    if is_cycle_gated_module_group(mod_type.group_id) || is_weapon || (params.repeat == 0 && params.reactivation_delay_s > 0.0) {
        let total_delay = if mod_type.group_id == 40 { params.duration_s.max(params.reactivation_delay_s) } else { params.duration_s + params.reactivation_delay_s };
        ctx.record_module_reactivation_delay(ship_id, module_id, total_delay);
    }
    if is_overloaded {
        ctx.emit_typed(crate::router::NotificationTarget::Character(char_id), crate::router::notify_registry::OnHeatAdded {
            heat_id: crate::services::fitting_cycles_overheat::rack_heat_attr_id_for_rack(params.rack_idx),
            module_id,
        });
    }
    if !slot_still_has_charge {
        if let Some(fm) = fit.modules.get_mut(&module_id) { fm.charge_type_id = None; fm.charge_quantity = 0; }
    } else if let Some(cid) = params.active_burst_charge_tid {
        if let Some(fm) = fit.modules.get_mut(&module_id) { fm.charge_type_id = Some(cid); fm.charge_quantity = fm.charge_quantity.max(1); }
    }
    if is_overloaded {
        ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    }
    ctx.invalidate_and_push_fit(ship_id);
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn emit_module_activation_godma_notifications_sync(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    mod_group_id: u32,
    fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>,
    effect_name: &str,
    params: &PreparedCycleParams,
    is_guided_missile: bool,
) {
    // V24 client derives `OnModuleActivated` from `OnGodmaShipEffect` (active=1).
    let godma_repeat = if params.repeat == 0 { 0 } else { 1000 };
    let godma_duration_ms = if mod_group_id == 330 { 0.0 } else { (params.duration_s * 1_000.0).max(0.0) };
    if is_guided_missile {
        let _ = ctx.notify_module_godma_effect_started_only(char_id, ship_id, module_id, module_type_id, Some(effect_name), target_id_opt, Some(godma_repeat), Some(godma_duration_ms));
    } else if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) {
        let _ = ctx.notify_module_effect_started_with_cycle_and_charge(scene, char_id, ship_id, module_id, module_type_id, Some(effect_name), target_id_opt, Some(godma_repeat), Some(godma_duration_ms), params.active_burst_charge_tid);
        for &slave_mod_id in &params.linked_turrets {
            if slave_mod_id == module_id { continue; }
            let slave_type_id = fit.modules.get(&slave_mod_id).map(|fm| fm.type_id).or_else(|| ctx.db.with_snapshot(|snap| snap.items.get(&slave_mod_id).map(|i| i.type_id))).unwrap_or(module_type_id);
            ctx.notify_module_special_fx_started(scene, char_id, ship_id, slave_mod_id, slave_type_id, Some(effect_name), target_id_opt, godma_repeat, godma_duration_ms, params.active_burst_charge_tid);
        }
    } else {
        let _ = ctx.notify_module_godma_effect_started_only(char_id, ship_id, module_id, module_type_id, Some(effect_name), target_id_opt, Some(godma_repeat), Some(godma_duration_ms));
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn finalize_cycle_activation_and_response(
    ctx: &ServerContext, sys: &crate::system_actor::SystemHandle, sys_id: u32, char_id: i64, ship_id: i64, module_id: i64,
    m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType, mod_state: &eve_dogma::DogmaItemState, fit: &mut eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>, is_overloaded: bool, is_weapon: bool,
    method: &str, effect_name: &str, prep: &EwarCapitalPrep, params: &PreparedCycleParams, kin: &WeaponKinematics,
    target_deltas: (f64, f64), instant_rep_opt: Option<PyRep>,
) -> CycleExecutionOutcome {
    let slot_still_has_charge = activate_post_cycle_snapshot_and_fit_sync(ctx, char_id, ship_id, module_id, m, mod_type, fit, is_overloaded, params);
    if mod_type.group_id == 330 || m.type_id == 83485 {
        crate::services::space_destiny::space_nav_travel::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    }
    finalize_post_activation_fit_updates_and_push_sync(ctx, char_id, ship_id, module_id, mod_type, fit, is_weapon, is_overloaded, slot_still_has_charge, params);
    if mod_type.group_id == 330 || m.type_id == 83485 {
        crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    }
    emit_module_activation_godma_notifications_sync(ctx, sys_id, char_id, ship_id, module_id, m.type_id, mod_type.group_id, fit, target_id_opt, effect_name, params, kin.is_guided_missile);
    if params.cap_need > 0.0 || params.cap_booster_inject_amount > 0.0 || params.effective_max_capacitor.is_some() {
        if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            let old_cap = if params.cap_need > 0.0 && params.cap_booster_inject_amount == 0.0 {
                Some((cs.capacitor + params.cap_need).min(cs.max_capacitor))
            } else if params.cap_booster_inject_amount > 0.0 && params.cap_need == 0.0 {
                Some((cs.capacitor - params.cap_booster_inject_amount).max(0.0))
            } else { None };
            let _ = ctx.notify_ship_capacitor_change(char_id, ship_id, cs.capacitor, cs.max_capacitor, old_cap);
        }
    }
    emit_committed_ewar_and_system_wide_notifications(
        ctx, sys_id, char_id, ship_id, module_id, m.type_id, mod_type, m.charge_type_id, prep.disables_target_mwd_mjd, prep.hic_bubble_radius_m, target_id_opt,
    );
    if let Some(tid) = target_id_opt {
        let (target_cap_drained, target_shield_boosted) = target_deltas;
        if target_cap_drained > 0.0 || prep.remote_shield_amount > 0.0 {
            finalize_target_combat_hooks(ctx, char_id, ship_id, tid, target_cap_drained, target_shield_boosted, prep.remote_shield_amount);
        }
    }
    let is_weapon_volley =
        (kin.is_turret_group || kin.is_missile_group || kin.is_vorton_group) && target_id_opt.is_some();
    if is_weapon_volley
        && instant_rep_opt.is_none()
        && !matches!(mod_type.group_id, 899 | 515 | 1770 | 4174)
        && !params.is_auto_targeter
        && mod_type.group_id != 49
        && !mod_type.default_attributes.contains_key(&197)
    {
        let is_overload_only_resp = method == "Overload";
        let max_vel = if mod_type.group_id == 330 && !is_overload_only_resp {
            sys.get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.max_velocity)
                .unwrap_or_else(|| fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
        } else {
            fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde)
        };
        return CycleExecutionOutcome::PendingWeaponVolley(PendingWeaponVolley {
            sys: sys.clone(),
            sys_id,
            ship_id,
            module_id,
            is_overloaded,
            duration_s: params.duration_s,
            max_vel,
            is_overload_only_resp,
        });
    }
    if let Some(rep) = instant_rep_opt {
        return CycleExecutionOutcome::Ready(Some(Ok(rep)));
    }
    if let Some(res) = build_specialized_module_result(method, fit, ctx, module_id, mod_type, prep.hic_bubble_radius_m, prep.warp_scramble_points, params.active_burst_charge_tid) {
        CycleExecutionOutcome::Ready(Some(Ok(res)))
    } else if params.is_auto_targeter {
        CycleExecutionOutcome::Ready(Some(crate::services::tactical_modules::activate_auto_targeting_system(ctx, sys, char_id, ship_id, module_id, mod_type, mod_state, fit).await))
    } else if mod_type.group_id == 49 || mod_type.default_attributes.contains_key(&197) {
        let already_applied_dbuffs: Vec<u32> = fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
        CycleExecutionOutcome::Ready(Some(Ok(handle_survey_scanner_activation_with_excluded_dbuffs(ctx, sys, sys_id, char_id, ship_id, module_id, mod_type, mod_state, &already_applied_dbuffs).await)))
    } else {
        let max_vel = if mod_type.group_id == 330 {
            sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.max_velocity).unwrap_or_else(|| fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
        } else {
            fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde)
        };
        let resp = vec![
            ("moduleID", PyRep::Integer(module_id)), ("active", PyRep::Bool(true)),
            ("overloaded", PyRep::Bool(is_overloaded)), ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ("duration", PyRep::Real(params.duration_s)),
        ];
        CycleExecutionOutcome::Ready(Some(Ok(PyRep::keyval(resp))))
    }
}

/// Records post-cycle career goals outside `handle_module_cycles_inner`'s coroutine frame
/// so debug builds stay under 2 MiB test stacks.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn finalize_target_combat_hooks(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    target_id: i64,
    drained: f64,
    boosted: f64,
    remote_shield_amount: f64,
) {
    if drained > 0.0 {
        crate::services::quasar_career_goals::record_cap_warfare_career(
            ctx,
            char_id,
            ship_id,
            target_id,
            "neutralizer",
            drained,
        );
    }
    if remote_shield_amount > 0.0 {
        crate::services::quasar_career_goals::record_remote_repair_career(
            ctx,
            char_id,
            target_id,
            "shield_boost",
            boosted,
        );
    }
}

#[inline(never)]
async fn finish_space_activation(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    is_in_space: bool,
    char_id: i64,
    ship_id: i64,
    res: Result<PyRep, BridgeRpcError>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if is_in_space && res.is_ok() {
        break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
    }
    Some(res)
}

/// Returns `true` when `dogmaIM.Activate` requires an explicit target (`TargetRequired`),
/// preserving the FoF auto-targeting missile exception (`group_id 394..=396`).
fn activation_requires_explicit_target(
    ctx: &ServerContext,
    ship_id: i64,
    m: &eve_dogma::FittedModule,
    mod_group_id: u32,
    is_weapon: bool,
) -> bool {
    if matches!(
        mod_group_id,
        1122 | 650 | 82
            | 52 | 65 | 68 | 71 | 201 | 208 | 291 | 379 | 1672
            | 41 | 67 | 209 | 290 | 325 | 585 | 1697 | 1698 | 2018
    ) {
        return true;
    }
    let is_missile_launcher = is_missile_launcher_group(mod_group_id);
    if !is_weapon && !is_missile_launcher {
        return false;
    }
    if is_missile_launcher {
        let fof_loaded = m.charge_type_id.is_some_and(|cid| {
            ctx.sde
                .types
                .get(&cid)
                .is_some_and(|ct| matches!(ct.group_id, 394..=396))
        }) || ctx.db.with_snapshot(|snap| {
            snap.module_active_charges
                .get(&m.item_id)
                .is_some_and(|&cid| {
                    ctx.sde
                        .types
                        .get(&cid)
                        .is_some_and(|ct| matches!(ct.group_id, 394..=396))
                })
                || snap.items.values().any(|i| {
                    i.location_id == ship_id
                        && (i.flag as u16) == m.flag_id
                        && i.item_id != m.item_id
                        && i.quantity > 0
                        && ctx
                            .sde
                            .types
                            .get(&i.type_id)
                            .is_some_and(|ct| matches!(ct.group_id, 394..=396))
                })
        });
        if fof_loaded {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "fitting_module_cycles_tests.rs"]
mod fitting_module_cycles_tests;
#[cfg(test)]
#[path = "fitting_module_cycles_edges2_tests.rs"]
mod fitting_module_cycles_edges2_tests;
