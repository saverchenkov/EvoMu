//! `combat_weapons` — Turret Families, Hit Chance, Wrecking Shots, Crystal Volatility, Entropic Spooling, Vorton Arcs & Post-Kill Pipeline (`C-1`, `C-3`).

#![forbid(unsafe_code)]

use super::*;

fn emit_limited_engagement_created(ctx: &ServerContext, char_a: i64, char_b: i64) {
    let timeout_ft = ctx.now_machonet_wall_filetime().saturating_add_secs(300);
    ctx.emit_typed(
        NotificationTarget::Character(char_a),
        crate::router::notify_registry::OnCrimewatchEngagementCreated {
            other_char_id: char_b,
            timeout: timeout_ft,
        },
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_b),
        crate::router::notify_registry::OnCrimewatchEngagementCreated {
            other_char_id: char_a,
            timeout: timeout_ft,
        },
    );
}

pub(crate) fn emit_weapon_charge_diff(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    diff: Option<(
        Option<eve_db::InventoryItemRecord>,
        Option<eve_db::InventoryItemRecord>,
    )>,
) {
    let Some((pre_rec, post_rec)) = diff else {
        return;
    };
    let depleted = post_rec.is_none();
    crate::services::inventory_fitting::fitting_module_cycles::emit_ship_charge_sublocation_diffs(
        ctx,
        char_id,
        ship_id,
        &[(pre_rec, post_rec)],
    );
    if depleted {
        ctx.invalidate_and_push_fit(ship_id);
    }
}

/// Constructs a typed `NoAmmoLoaded` error carrying V24 `UserError('NoCharges', {'launcher': [4, typeID]})`
/// so the client resolves the weapon module name via `cfg.invtypes` instead of showing an empty quote.
pub(crate) fn weapon_no_ammo_loaded_error(
    module_id: Option<i64>,
    mod_type: &eve_sde::InvType,
    required_charge_group: Option<u32>,
    message: impl Into<String>,
) -> BridgeRpcError {
    let required_charge_group = required_charge_group.or_else(|| {
        mod_type
            .default_attributes
            .get(&604)
            .copied()
            .filter(|&v| v > 0.0)
            .map(|v| v as u32)
    });
    BridgeRpcError::no_ammo_loaded(
        module_id,
        Some(i64::from(mod_type.type_id)),
        required_charge_group,
        Some(mod_type.name.as_str()),
        message,
    )
}

use super::combat_weapons_targeting::{
    apply_vorton_arc_secondary_hits, resolve_target_effective_signature, weapon_mod_attr_pos,
};

pub(crate) fn consume_one_weapon_round_and_emit(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    charge_item_id: i64,
) {
    let ammo_diff = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ci) = snap.items.get_mut(&charge_item_id) {
            let pre_rec = ci.clone();
            let post_rec = if ci.quantity > 1 {
                ci.quantity -= 1;
                ci.version += 1;
                Some(ci.clone())
            } else {
                snap.items.remove(&charge_item_id);
                snap.module_active_charges.remove(&module_id);
                None
            };
            Some((Some(pre_rec), post_rec))
        } else {
            None
        }
    });
    emit_weapon_charge_diff(ctx, char_id, ship_id, ammo_diff);
}

/// Computes the effective surface-to-surface maximum range (`optimal + 3 * falloff`) for an EWAR/tackle module,
/// falling back to ship lock range only when the module defines neither optimal range nor falloff (#1247).
fn compute_ewar_effective_range(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    fit: &eve_dogma::ShipFittingEngine,
) -> f64 {
    let mut opt_range = mod_state
        .get_attribute(eve_sde::attr_ids::MAX_RANGE, &ctx.sde)
        .max(0.0);
    let disables_mwd = mod_type.group_id == 52
        && eve_dogma::eval_warp_scrambler_effects(&[mod_state], &[0.0], &ctx.sde).blocks_mwd;
    if mod_type.group_id == 65 || disables_mwd {
        opt_range *= crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(
            ctx, char_id, sys_id,
        );
    }
    let mut falloff = eve_dogma::get_module_falloff(mod_state, &ctx.sde).max(0.0);
    let dark_mult = crate::services::pve_abyssal_hazards::dark_weather_midpocket_turret_mult(
        ctx, char_id, sys_id, false, module_id, mod_type,
    );
    if (dark_mult - 1.0).abs() > 1e-9 {
        opt_range *= dark_mult;
        falloff *= dark_mult;
    }
    if opt_range > 0.0 || falloff > 0.0 {
        opt_range + 3.0 * falloff
    } else {
        fit.ship
            .get_attribute(eve_sde::attr_ids::MAX_TARGET_RANGE, &ctx.sde)
            .max(0.0)
    }
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_or_toggle_targeted_overload(
    ctx: &ServerContext, sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64, ship_id: i64, module_id: i64, method: &str, in_cycle: bool,
    m: &mut eve_dogma::FittedModule, fit: &mut eve_dogma::ShipFittingEngine,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method == "Overload" {
        match crate::services::fitting_cycles_overheat::check_overload_cycle_or_delay(
            ctx, char_id, ship_id, module_id, in_cycle, false, true, false, m, fit,
        ) {
            Ok(true) => {
                let fit_owned = fit.clone();
                let rep = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
                    ctx, sys_handle, char_id, ship_id, module_id, fit_owned, true,
                ).await;
                return Some(Ok(rep));
            }
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        }
    } else {
        let _ = crate::services::fitting_cycles_overheat::reconcile_module_active_state(
            ctx, ship_id, module_id, in_cycle, false, true, m, fit,
        );
        None
    }
}

fn finalize_weapon_activation_overload_and_delay(
    ctx: &ServerContext, char_id: i64, ship_id: i64, module_id: i64,
    fit: &mut eve_dogma::ShipFittingEngine, is_overloaded: bool,
    mark_db_active: bool, record_delay_secs: Option<f64>,
) {
    if mark_db_active {
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(module_id); });
    }
    fit.pending_overload_modules.remove(&module_id);
    let _ = fit.activate_module(module_id, is_overloaded, &ctx.sde);
    if is_overloaded {
        crate::services::fitting_cycles_overheat::emit_module_heat_added(ctx, char_id, fit, module_id);
    }
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.invalidate_and_push_fit(ship_id);
    if let Some(delay_secs) = record_delay_secs {
        ctx.record_module_reactivation_delay(ship_id, module_id, delay_secs);
    }
}

fn sync_remaining_weapon_charge_in_fit(
    ctx: &ServerContext, module_id: i64, charge_item_id_opt: Option<i64>,
    charge_type_id_opt: Option<u32>, fit: &mut eve_dogma::ShipFittingEngine,
) {
    let remaining_charge_qty = ctx.db.with_mut_snapshot(|snap| {
        let rem_qty = charge_item_id_opt.and_then(|cid| snap.items.get(&cid).map(|i| i.quantity)).unwrap_or(0);
        if let (Some(charge_type_id), true) = (charge_type_id_opt, rem_qty > 0) {
            snap.module_active_charges.insert(module_id, charge_type_id);
        } else {
            snap.module_active_charges.remove(&module_id);
        }
        rem_qty
    });
    if let Some(fm) = fit.modules.get_mut(&module_id) {
        fm.charge_quantity = remaining_charge_qty.max(0) as u32;
        if remaining_charge_qty <= 0 {
            fm.charge_type_id = None;
        }
    }
}

struct PendingAoeActivation {
    attacker_ball: eve_destiny::Ball,
    char_id: i64, ship_id: i64, sys_id: u32, module_id: i64,
    eff_m: eve_dogma::FittedModule, mod_type_id: u32,
    target_id_opt: Option<i64>, now_unix: u64, sys_sec: f64, is_overloaded: bool,
}

enum WeaponDispatchOutcome {
    EarlyDone(Option<Result<PyRep, BridgeRpcError>>),
    Dispatched(Option<Result<PyRep, BridgeRpcError>>),
    PendingAoe(Box<PendingAoeActivation>),
}

#[inline(never)]
async fn execute_pending_aoe_activation(
    ctx: &ServerContext, aoe: Box<PendingAoeActivation>, kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let mod_type = ctx.sde.types.get(&aoe.mod_type_id)?;
    let aoe_res = activate_aoe_or_defender_module(
        ctx, aoe.char_id, aoe.ship_id, aoe.sys_id, aoe.sys_sec, aoe.now_unix,
        aoe.module_id, &aoe.eff_m, mod_type, &aoe.attacker_ball, aoe.target_id_opt, kwargs,
    ).await?;
    if aoe.is_overloaded && aoe_res.is_ok() {
        let PendingAoeActivation {
            char_id,
            ship_id,
            module_id,
            eff_m,
            ..
        } = *aoe;
        if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
            if !fit.modules.contains_key(&module_id) {
                fit.modules.insert(module_id, eff_m);
            }
            finalize_weapon_activation_overload_and_delay(
                ctx, char_id, ship_id, module_id, &mut fit, true, false, None,
            );
        }
    }
    Some(aoe_res)
}

#[inline(never)]
pub async fn handle_weapons(
    ctx: &ServerContext, session: &mut SessionState, service: &str, method: &str,
    args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!((service, method), ("dogmaIM", "Activate") | ("dogmaIM", "Overload")) {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    handle_weapons_inner(
        ctx, char_id, session.ship_id, session.station_id, session.structure_id,
        session.solar_system_id, method, args, kwargs,
    ).await
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_weapons_inner(
    ctx: &ServerContext, char_id: i64, session_ship_id: Option<i64>,
    session_station_id: Option<i64>, session_structure_id: Option<i64>,
    session_solar_system_id: Option<u32>, method: &str, args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(
        ctx, char_id, "SAFE_LOGOFF_ABORT_MODULE",
    );
    let controlling_structure =
        session_structure_id.is_some() && session_structure_id == session_ship_id;
    if session_station_id.is_some()
        || (session_structure_id.is_some() && !controlling_structure)
    {
        return Some(Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate combat modules while docked".into(),
            client_error: None,
        }));
    }
    let Some(ship_id) = session_ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        }));
    };
    let is_structure_ship = controlling_structure
        || ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id));
    if method == "Overload" && is_structure_ship {
        return Some(Err(
            crate::services::structure_weapons_ops::cannot_overload_structure_module_error(),
        ));
    }
    let Some(sys_id) = session_solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
            client_error: None,
        }));
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((attacker_tick, attacker_ball)) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Some(Err(BridgeRpcError::ship_not_in_space(
            ship_id,
            Some(char_id),
            Some(sys_id),
            "FireWeapon",
            format!("Active ship {ship_id} is not spawned in space"),
        )));
    };

    let rpc = RpcArgs::new(args, kwargs);
    let module_id = match rpc.req_positive_i64(0, "moduleID") {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let target_id_opt = match rpc
        .opt_i64(2, "target")
        .transpose()
        .or_else(|| {
            args.get(1)
                .and_then(|a| a.as_i64())
                .filter(|&id| {
                    method != "Overload"
                        || !u16::try_from(id)
                            .ok()
                            .is_some_and(|eid| ctx.sde.effects.contains_key(&eid))
                })
                .map(Ok)
        })
    {
        Some(Ok(id)) => Some(id),
        Some(Err(e)) => return Some(Err(e)),
        None => None,
    };

    let attacker_ball = Box::new(attacker_ball);

    let (res, should_break_cloak) = match run_resolved_weapon_module(
        ctx, char_id, ship_id, sys_id, sys_handle, attacker_tick, attacker_ball,
        controlling_structure, method, module_id, target_id_opt, kwargs,
    ).await {
        WeaponDispatchOutcome::EarlyDone(res) => (res, false),
        WeaponDispatchOutcome::Dispatched(res) => (res, true),
        WeaponDispatchOutcome::PendingAoe(aoe) => {
            (execute_pending_aoe_activation(ctx, aoe, kwargs).await, true)
        }
    };
    if should_break_cloak && !controlling_structure && matches!(res, Some(Ok(_))) {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        crate::services::inventory_fitting::fitting_module_cycles::break_gate_cloak_on_space_action(
            ctx, &sys_handle, char_id, ship_id,
        ).await;
    }
    res
}

struct ResolvedWeaponFit {
    fit: eve_dogma::ShipFittingEngine,
    m: eve_dogma::FittedModule,
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn run_resolved_weapon_module(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: crate::system_actor::SystemHandle, attacker_tick: u64, attacker_ball: Box<eve_destiny::Ball>,
    controlling_structure: bool, method: &str, module_id: i64, target_id_opt: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> WeaponDispatchOutcome {
    if controlling_structure && method != "Overload" {
        if let Some(res) = crate::services::structure_standup_weapons::try_activate_standup_weapon(
            ctx, &sys_handle, char_id, ship_id, sys_id, module_id, target_id_opt, &attacker_ball,
        )
        .await {
            return WeaponDispatchOutcome::EarlyDone(Some(res));
        }
    }

    let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        Ok(f) => f,
        Err(e) => return WeaponDispatchOutcome::EarlyDone(Some(Err(e))),
    };
    if !fit.modules.contains_key(&module_id) {
        ctx.invalidate_ship_fitting(ship_id);
        if let Ok(rebuilt) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
            fit = rebuilt;
        }
    }
    let m = if let Some(found) = fit.modules.get(&module_id).cloned() {
        found
    } else if let Some(fm) = crate::services::inventory_fitting::fitting_module_cycles::fallback_fitted_weapon_module(ctx, ship_id, module_id) {
        fit.modules.insert(module_id, fm.clone());
        fm
    } else {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(BridgeRpcError::module_not_found(
            module_id,
            Some(ship_id),
            None,
            false,
            format!("Module {module_id} not found on ship {ship_id}"),
        ))));
    };
    let mut resolved = Box::new(ResolvedWeaponFit { fit, m });
    if let Err(e) =
        crate::services::inventory_fitting::check_module_reloading(ctx, ship_id, module_id)
    {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(e)));
    }
    if sys_handle.has_active_nanite_repair_sync(ship_id, module_id) {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(BridgeRpcError {
            code: "ModuleBeingRepaired".into(),
            message: format!(
                "Module {module_id} cannot be activated or overloaded while undergoing nanite repair"
            ),
            client_error: None,
        })));
    }
    if let Err(e) = crate::services::fitting_cycles_overheat::check_module_or_bank_not_burned_out(
        ctx, ship_id, module_id, &resolved.m,
    ) {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(e)));
    }
    if resolved.m.state == eve_dogma::ModuleState::Offline
        || !ctx.db.with_snapshot(|s| s.module_online_states.get(&module_id).copied().unwrap_or(true))
    {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(BridgeRpcError {
            code: "ModuleOffline".into(),
            message: format!("Module {module_id} is offline"),
            client_error: None,
        })));
    }
    let Some(mod_type) = ctx.sde.types.get(&resolved.m.type_id) else {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(BridgeRpcError::module_not_found(
            module_id,
            Some(ship_id),
            Some(resolved.m.type_id as i64),
            false,
            format!("Module type {} not found in SDE", resolved.m.type_id),
        ))));
    };
    if method == "Overload" {
        if matches!(mod_type.group_id, 1327 | 1328 | 1330 | 1331 | 1333 | 1562) {
            return WeaponDispatchOutcome::EarlyDone(Some(Err(
                crate::services::structure_weapons_ops::cannot_overload_structure_module_error(),
            )));
        }
    } else if let Some(res) = crate::services::structure_standup_weapons::try_activate_standup_weapon(
        ctx, &sys_handle, char_id, ship_id, sys_id, module_id, target_id_opt, &attacker_ball,
    )
    .await {
        return WeaponDispatchOutcome::EarlyDone(Some(res));
    }
    if let Err(e) = crate::services::fitting_cycles_overheat::check_nanite_repair_overheat_lockout(
        &sys_handle, ship_id, module_id, method, resolved.m.state,
    ) {
        return WeaponDispatchOutcome::EarlyDone(Some(Err(e)));
    }
    if method == "Overload" && target_id_opt.is_none() {
        let ResolvedWeaponFit { fit, mut m } = *resolved;
        return WeaponDispatchOutcome::EarlyDone(Some(
            crate::services::fitting_cycles_overheat::handle_early_targetless_overload(
                ctx,
                &sys_handle,
                char_id,
                ship_id,
                module_id,
                mod_type,
                true,
                &mut m,
                fit,
            )
            .await,
        ));
    }
    if !controlling_structure {
        if let Err(e) = crate::services::inventory_fitting::fitting_module_cycles::check_space_module_cloak_and_warp_readonly(
            ctx, &sys_handle, ship_id, mod_type,
        ).await {
            return WeaponDispatchOutcome::EarlyDone(Some(Err(e)));
        }
    }
    handle_weapons_after_cloak_guard(
        ctx, char_id, ship_id, sys_id, &sys_handle, attacker_tick, &attacker_ball,
        method, module_id, target_id_opt, &mut resolved.m, mod_type, &mut resolved.fit, kwargs,
    ).await
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_weapons_after_cloak_guard(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle, attacker_tick: u64,
    attacker_ball: &eve_destiny::Ball, method: &str, module_id: i64, target_id_opt: Option<i64>,
    m: &mut eve_dogma::FittedModule, mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine, kwargs: &HashMap<String, PyRep>,
) -> WeaponDispatchOutcome {
    let is_overloaded = method == "Overload" || m.state == eve_dogma::ModuleState::Overloaded;
    if let Some(tid) = target_id_opt.filter(|&t| t > 0) {
        if crate::services::tactical_module_mechanics::is_untargetable_interdiction_probe(
            ctx, sys_handle, tid,
        ).await {
            return WeaponDispatchOutcome::Dispatched(Some(Err(BridgeRpcError::invalid_target(
                Some(tid),
                Some(module_id),
                &mod_type.name,
                "interdiction_probe_untargetable",
                "Interdiction probes cannot be targeted directly; use area-of-effect weapons",
            ))));
        }
    }
    if (matches!(
        mod_type.group_id,
        52 | 65 | 68 | 71 | 72 | 201 | 208 | 291 | 379 | 862
    ) || crate::services::inventory_fitting::fitting_module_cycles::is_weapon_group(
        mod_type.group_id,
    )) && sys_handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_tethered)
    {
        return WeaponDispatchOutcome::Dispatched(Some(Err(BridgeRpcError {
            code: "CantActivateWhileTethered".into(),
            message: "Cannot activate offensive modules, smartbombs, or bomb launchers while tethered".into(),
            client_error: None,
        })));
    }
    let now_unix = ctx.now_unix();
    let sys_sec = ctx
        .db
        .with_snapshot(|s| {
            s.combat_crimewatch_state
                .modified_system_securities
                .get(&sys_id)
                .copied()
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
        return WeaponDispatchOutcome::Dispatched(handle_targeted_ewar_module(
            ctx, char_id, ship_id, sys_id, sys_handle, attacker_ball, method, module_id,
            target_id_opt, now_unix, sys_sec, is_overloaded, m, mod_type, fit,
        )
        .await);
    }

    // 1b. Salvager (`group_id == 1122`) and Tractor Beam (`group_id == 650`) (`C-7`)
    if matches!(mod_type.group_id, 1122 | 650) {
        if method == "Overload" {
            if let Err(e) = crate::services::fitting_cycles_overheat::check_can_overload_module(
                ctx, char_id, m.type_id,
            ) {
                return WeaponDispatchOutcome::Dispatched(Some(Err(e)));
            }
        }
        return WeaponDispatchOutcome::Dispatched(activate_salvager_or_tractor_module(
            ctx,
            char_id,
            ship_id,
            sys_id,
            module_id,
            m,
            mod_type,
            attacker_ball,
            target_id_opt,
            kwargs,
        )
        .await);
    }

    // 2-4. Smartbomb (`72`), Bomb Launcher (`862`), and Defender Launcher (`512`)
    if matches!(mod_type.group_id, 72 | 862 | 512) {
        return handle_aoe_or_defender_dispatch(
            ctx, char_id, ship_id, sys_id, sys_handle, attacker_ball, method, module_id,
            target_id_opt, now_unix, sys_sec, is_overloaded, m, mod_type, fit,
        )
        .await;
    }

    // 5. Turrets (Hybrid 74, Projectile 55, Energy Laser 53, Entropic Disintegrator 1986, Vorton Projector 4060)
    //    and Missile Launchers (506..=511, 524, 1674, 4500, 4805, 4807)
    if !crate::services::inventory_fitting::fitting_module_cycles::is_weapon_group(mod_type.group_id) {
        return WeaponDispatchOutcome::Dispatched(None);
    }
    WeaponDispatchOutcome::Dispatched(handle_direct_weapon_activation(
        ctx, char_id, ship_id, sys_id, sys_handle, attacker_tick, attacker_ball,
        method, module_id, target_id_opt, now_unix, sys_sec, is_overloaded, m, mod_type, fit,
    ).await)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_targeted_ewar_module(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle, attacker_ball: &eve_destiny::Ball,
    method: &str, module_id: i64, target_id_opt: Option<i64>, now_unix: u64, sys_sec: f64,
    is_overloaded: bool, m: &mut eve_dogma::FittedModule, mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let mod_state = Box::new(fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde));
        let Some(tid) = target_id_opt else {
            return Some(Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "Targeted EWAR/tackle module requires a locked target".into(),
                client_error: None,
            }));
        };
        let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
        if !locks.iter().any(|l| l.target_id == tid && l.locked) {
            return Some(Err(
                crate::services::inventory_fitting::fitting_module_cycles::target_not_locked_rpc_error(
                    format!("Target {tid} is not locked"),
                ),
            ));
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
        let Some((_, target_ball)) = sys_handle
            .get_ball_snapshot(tid)
            .await
            .ok()
            .flatten()
        else {
            return Some(Err(BridgeRpcError {
                code: "TargetNotInSpace".into(),
                message: format!("Target ball {tid} is not in space"),
                client_error: None,
            }));
        };
        let target_ball = Box::new(target_ball);
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
        if sys_handle
            .get_combat_state(tid)
            .await
            .ok()
            .flatten()
            .is_some_and(|mut tcs| {
                tcs.sync_action_breaks();
                tcs.blocks_targeted_effect()
            })
        {
            return Some(Err(BridgeRpcError {
                code: "TargetInvulnerable".into(),
                message: format!("Target {tid} is cloaked, tethered, or invulnerable"),
                client_error: None,
            }));
        }
        let mut pvp_precheck: Option<(bool, bool, bool, bool)> = None;
        if let Some(vc) = victim_char_opt {
            if vc != char_id {
                let is_pod_target = target_ball.type_id == 670
                    || ctx.db.with_snapshot(|s| s.items.get(&tid).is_some_and(|i| i.type_id == 670));
                let v_suspect_or_crim = {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let st = cw.get_or_create(vc);
                    st.is_suspect(now_unix) || st.is_criminal(now_unix)
                };
                let is_legal = ctx.db.with_snapshot(|snap| {
                    is_legal_war_or_suspect_target(snap, char_id, vc, now_unix)
                        || has_limited_engagement_in_snap(snap, vc, char_id, now_unix)
                }) || v_suspect_or_crim;
                let is_mobile_struct = ctx.db.with_snapshot(|s| {
                    s.items.get(&tid).is_some_and(|it| crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, it.type_id))
                });
                if !is_legal {
                    let safety = char_safety_level(ctx, char_id).unwrap_or(SafetyLevel::Red);
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
                pvp_precheck = Some((is_pod_target, v_suspect_or_crim, is_legal, is_mobile_struct));
            }
        }
        let ewar_in_cycle = sys_handle.has_module_cycle(ship_id, module_id).await;
        if let Some(res) = reconcile_or_toggle_targeted_overload(
            ctx, sys_handle, char_id, ship_id, module_id, method, ewar_in_cycle, m, fit,
        )
        .await
        {
            return Some(res);
        }
        // Why: Reject out-of-range EWAR (including special-target convoys/containers) before mutating CrimeWatch, assists, or projected EWAR (#1247).
        let ewar_dist = attacker_ball.surface_distance(&target_ball);
        let eff_max_range = compute_ewar_effective_range(
            ctx, char_id, sys_id, module_id, mod_type, &mod_state, fit,
        );
        if eff_max_range > 0.0 && ewar_dist > eff_max_range {
            return Some(Err(BridgeRpcError::target_out_of_range(
                crate::services::inventory_fitting::resolve_target_display_name(ctx, tid),
                mod_type.type_id as i64,
                ewar_dist,
                format!(
                    "Target {tid} is {ewar_dist:.1} m away (exceeds range {eff_max_range:.1} m)"
                ),
            )));
        }
        if let (Some(vc), Some((is_pod_target, v_suspect_or_crim, is_legal, is_mobile_struct))) =
            (victim_char_opt, pvp_precheck)
        {
            if ensure_mutual_limited_engagement_if(ctx, char_id, vc, v_suspect_or_crim, now_unix) {
                emit_limited_engagement_created(ctx, char_id, vc);
            }
            let is_new_agg = crate::services::combat_crimewatch::crimewatch_timers::is_new_pvp_aggression(ctx, char_id, vc, is_pod_target, now_unix);
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
            if !is_legal && !is_mobile_struct {
                crate::services::combat_crimewatch::crimewatch_timers::record_illegal_pvp_penalties_if_new(
                    ctx, char_id, vc, sys_id, sys_sec, is_pod_target, is_new_agg, now_unix,
                );
            }
        }
        crate::services::combat_crimewatch::crimewatch_timers::on_weapon_aggression_post_hook(ctx, char_id, victim_char_opt, sys_id, now_unix);
        record_killmail_assist_action(ctx, tid, char_id, attacker_ball.type_id, m.type_id, 0.0, true, now_unix);
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
            ctx.upsert_projected_ewar_on_ship(
                ship_id,
                tid,
                eve_dogma::ProjectedEwarEffect {
                    source_module_id: module_id,
                    source_module_type_id: m.type_id,
                    charge_type_id: m.charge_type_id,
                    distance: ewar_dist,
                    overloaded: is_overloaded,
                    effective_module: *mod_state,
                },
            );
            drop(target_ball);
            if let Some(vcid) = victim_char_opt {
                let _ = evaluate_effective_signature_radius(ctx, vcid, tid, sys_id).await;
            } else {
                let tp_mult = ctx.effective_target_painter_multiplier(tid);
                sys_handle.try_recompute_combat_attributes(tid, |t_cs| {
                    t_cs.tp_sig_multiplier = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
                });
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
        if is_overloaded {
            finalize_weapon_activation_overload_and_delay(
                ctx, char_id, ship_id, module_id, fit, true, false, None,
            );
        }
        return Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("targetID", PyRep::Integer(tid)),
            ("ewarAssistRecorded", PyRep::Bool(true)),
            ("damageDealt", PyRep::Real(0.0)),
            ("capacitorDrained", PyRep::Real(0.0)),
        ])));
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_aoe_or_defender_dispatch(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_ball: &eve_destiny::Ball,
    method: &str,
    module_id: i64,
    target_id_opt: Option<i64>,
    now_unix: u64,
    sys_sec: f64,
    is_overloaded: bool,
    m: &mut eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
) -> WeaponDispatchOutcome {
        if mod_type.group_id == 72 {
            let safety = char_safety_level(ctx, char_id).unwrap_or(SafetyLevel::Red);
            if sys_sec > 0.0 && safety != SafetyLevel::Red {
                return WeaponDispatchOutcome::Dispatched(Some(Err(BridgeRpcError::with_client_error(
                    "CantDoInCurrentSafetyLevel",
                    "CantDoInCurrentSafetyLevel",
                    eve_proto::ClientError::user_with_args(
                        eve_proto::UserErrorKey::AoeModuleActivationDeniedSafety,
                        [(
                            "moduleName",
                            eve_proto::UserErrorArg::Str(mod_type.name.clone()),
                        )],
                    ),
                ))));
            }
        }
        if mod_type.group_id == 862 {
            if let Err(e) = super::combat_missiles_aoe::check_bomb_launcher_preconditions(
                ctx, char_id, sys_id, sys_sec,
            ) {
                return WeaponDispatchOutcome::Dispatched(Some(Err(e)));
            }
        }
        let ewar_in_cycle = sys_handle.has_module_cycle(ship_id, module_id).await;
        if let Some(res) = reconcile_or_toggle_targeted_overload(
            ctx, sys_handle, char_id, ship_id, module_id, method, ewar_in_cycle, m, fit,
        )
        .await
        {
            return WeaponDispatchOutcome::Dispatched(Some(res));
        }
        if matches!(mod_type.group_id, 862 | 512) {
            let allowed_groups: &[u32] = if mod_type.group_id == 862 {
                &[90, 863, 864]
            } else {
                &[88]
            };
            if let Some((_cid, ctid)) =
                super::combat_missiles_aoe::find_loaded_aoe_launcher_charge(
                    ctx,
                    ship_id,
                    module_id,
                    m.flag_id,
                    allowed_groups,
                )
            {
                if let Err(e) = ctx.db.with_snapshot(|snap| {
                    ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid)
                }) {
                    return WeaponDispatchOutcome::Dispatched(Some(Err(e)));
                }
            } else {
                match crate::services::inventory_fitting::fitting_ewar_capital::plan_weapon_autoload(
                    ctx, char_id, ship_id, module_id, mod_type,
                ) {
                    Ok(Some(plan)) => {
                        if let Err(e) = crate::services::inventory_fitting::fitting_ewar_capital::commit_and_emit_weapon_autoload(
                            ctx, char_id, ship_id, mod_type, &plan,
                        ) {
                            return WeaponDispatchOutcome::Dispatched(Some(Err(e)));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => return WeaponDispatchOutcome::Dispatched(Some(Err(e))),
                }
            }
        }
        let mut eff_m = m.clone();
        if is_overloaded {
            eff_m.state = eve_dogma::ModuleState::Overloaded;
        }
        WeaponDispatchOutcome::PendingAoe(Box::new(PendingAoeActivation {
            attacker_ball: attacker_ball.clone(),
            char_id,
            ship_id,
            sys_id,
            module_id,
            eff_m,
            mod_type_id: mod_type.type_id,
            target_id_opt,
            now_unix,
            sys_sec,
            is_overloaded,
        }))
}

fn weapon_charge_has_mod(
    mod_state: &eve_dogma::DogmaItemState,
    charge_type_opt: Option<&eve_sde::InvType>,
    attr: u16,
    active_charge_item_id: Option<i64>,
) -> bool {
    let Some(charge_type) = charge_type_opt else {
        return false;
    };
    mod_state.modifiers.iter().any(|m| {
        m.target_attribute_id == attr
            && (active_charge_item_id.is_some_and(|cid| m.source_item_id == cid)
                || (m.source_item_id == 0
                    && m.effect_id
                        .is_some_and(|eid| charge_type.effect_ids.contains(&eid))))
    })
}

struct PreparedDirectWeaponShot {
    mod_state: eve_dogma::DogmaItemState,
    charge_item_id_opt: Option<i64>,
    charge_type_id_opt: Option<u32>,
    is_fof_missile: bool,
    is_entropic: bool,
    is_missile_launcher: bool,
    is_breacher_pod: bool,
    is_vorton: bool,
    resolved_target_id: i64,
    player_target_info: Option<(i64, i64)>,
    target_ball_id: i64,
    target_ball: eve_destiny::Ball,
    is_upwell_target: bool,
    optimal_range: f64,
    falloff_range: f64,
    dist: f64,
    base_cap_need: f64,
    cap_before: f64,
    cap_after: f64,
}

#[allow(clippy::too_many_arguments)]
async fn resolve_direct_weapon_target(
    ctx: &ServerContext,
    ship_id: i64,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_ball: &eve_destiny::Ball,
    module_id: i64,
    target_id_opt: Option<i64>,
    is_fof_missile: bool,
) -> Result<i64, BridgeRpcError> {
    if let Some(tid) = target_id_opt {
        if !is_fof_missile {
            let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.module_active_states.remove(&module_id);
                    snap.module_active_charges.remove(&module_id);
                });
                ctx.invalidate_and_push_fit(ship_id);
                combat_weapons_spool::reset_entropic_spool(ctx, ship_id, module_id);
                return Err(
                    crate::services::inventory_fitting::fitting_module_cycles::target_not_locked_rpc_error(
                        format!("Target {tid} is not locked"),
                    ),
                );
            }
        }
        Ok(tid)
    } else if is_fof_missile {
        let all_balls = sys_handle.list_balls().await.unwrap_or_default();
        sys_handle.ensure_active_drones_hydrated().await;
        let sys_view = sys_handle.view();
        let nearest = all_balls
            .into_iter()
            .filter(|b| {
                b.id != ship_id
                    && (sys_view.active_drones_in_space.contains_key(&b.id)
                        || with_combat_runtime(ctx, |rt| rt.npc_entities.contains_key(&b.id))
                        || ctx.sde.types.get(&b.type_id).is_some_and(|t| {
                            matches!(t.category_id, 6 | 11 | 18 | 65)
                        }))
            })
            .min_by(|a, b| {
                attacker_ball
                    .surface_distance(a)
                    .partial_cmp(&attacker_ball.surface_distance(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|b| b.id);
        let Some(ntid) = nearest else {
            return Err(BridgeRpcError {
                code: "NoValidAutoTarget".into(),
                message: "No hostile entity within auto-targeting missile range".into(),
                client_error: None,
            });
        };
        Ok(ntid)
    } else {
        Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "Weapon activation requires a locked target".into(),
            client_error: None,
        })
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn prepare_direct_weapon_shot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_ball: &eve_destiny::Ball,
    method: &str,
    module_id: i64,
    target_id_opt: Option<i64>,
    now_unix: u64,
    sys_sec: f64,
    is_overloaded: bool,
    m: &mut eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
) -> std::ops::ControlFlow<Option<Result<PyRep, BridgeRpcError>>, Box<PreparedDirectWeaponShot>> {
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
    let is_missile_launcher =
        crate::services::inventory_fitting::fitting_module_cycles::is_missile_launcher_group(
            mod_type.group_id,
        );
    let is_civilian_turret =
        mod_type.capacity <= 0.0 || matches!(m.type_id, 3634 | 3636 | 3638 | 3640);
    let mut pending_weapon_autoload: Option<
        crate::services::inventory_fitting::fitting_ewar_capital::PendingWeaponAutoload,
    > = None;
    let (mut charge_item_id_opt, charge_type_id_opt) = match loaded_charge_opt {
        Some((cid, ctid)) => {
            if let Err(e) = ctx.db.with_snapshot(|snap| {
                ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid)
            }) {
                return std::ops::ControlFlow::Break(Some(Err(e)));
            }
            (Some(cid), Some(ctid))
        }
        None if is_civilian_turret && !is_missile_launcher => (None, None),
        None => {
            match crate::services::inventory_fitting::fitting_ewar_capital::plan_weapon_autoload(
                ctx, char_id, ship_id, module_id, mod_type,
            ) {
                Ok(Some(plan)) => {
                    let ammo_tid = plan.ammo_type_id;
                    pending_weapon_autoload = Some(plan);
                    (None, Some(ammo_tid))
                }
                Ok(None) => {
                    return std::ops::ControlFlow::Break(Some(Err(weapon_no_ammo_loaded_error(
                        Some(module_id),
                        mod_type,
                        None,
                        format!("Weapon module {module_id} has no compatible charge loaded"),
                    ))));
                }
                Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
            }
        }
    };
    let (autoload_fit_opt, mod_state) = if let Some(plan) = pending_weapon_autoload.as_ref() {
        let mut pre_fit = fit.clone();
        if let Some(fm) = pre_fit.modules.get_mut(&module_id) {
            fm.charge_type_id = Some(plan.ammo_type_id);
            fm.charge_quantity = plan
                .slot_allocations
                .iter()
                .find(|&&(tid, _, _)| tid == module_id)
                .map_or(1, |&(_, _, q)| q.max(0) as u32);
        }
        pre_fit.recalculate_stats(&ctx.sde);
        let ms = pre_fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
        (Some(Box::new(pre_fit)), Box::new(ms))
    } else {
        if let Some(charge_type_id) = charge_type_id_opt {
            if let Some(fm) = fit.modules.get_mut(&module_id) {
                if fm.charge_type_id != Some(charge_type_id) {
                    fm.charge_type_id = Some(charge_type_id);
                    fm.charge_quantity = fm.charge_quantity.max(1);
                    fit.recalculate_stats(&ctx.sde);
                }
            }
        }
        (None, Box::new(fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde)))
    };
    let charge_type_opt = charge_type_id_opt.and_then(|ctid| ctx.sde.types.get(&ctid));
    let is_fof_missile = charge_type_opt.is_some_and(|ct| matches!(ct.group_id, 394..=396));
    let is_entropic = mod_type.group_id == 1986;


    let resolved_target_id = match resolve_direct_weapon_target(
        ctx, ship_id, sys_handle, attacker_ball, module_id, target_id_opt, is_fof_missile,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
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
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
            code: "TargetNotInSpace".into(),
            message: format!("Target ball {target_ball_id} is not in space"),
            client_error: None,
        })));
    };
    let target_ball = Box::new(target_ball);

    if sys_handle
        .get_combat_state(target_ball_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|mut tcs| {
            tcs.sync_action_breaks();
            tcs.blocks_targeted_effect()
        })
    {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError {
            code: "TargetInvulnerable".into(),
            message: format!("Target {target_ball_id} is cloaked, tethered, or invulnerable"),
            client_error: None,
        })));
    }

    // Safety setting gate (`feat:crimewatch.safety-settings`)
    if let Some((victim_char_id, _)) = player_target_info {
        if victim_char_id != char_id {
            let is_legal = ctx.db.with_snapshot(|snap| {
                is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                    || has_limited_engagement_in_snap(snap, victim_char_id, char_id, now_unix)
            }) || {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                let vst = cw.get_or_create(victim_char_id);
                vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
            };
            if !is_legal {
                let safety = char_safety_level(ctx, char_id).unwrap_or(SafetyLevel::Red);
                let is_pod_target = target_ball.type_id == 670;
                let concord_suppressed = !is_pod_target
                    && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
                let would_be_criminal = (sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod_target);
                let would_be_suspect = !would_be_criminal && sys_sec > 0.0;
                if (safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect))
                    || (safety == SafetyLevel::Yellow && would_be_criminal)
                {
                    return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"))));
                }
            }
        }
    }

    if method == "Overload" {
        if let Err(e) = crate::services::fitting_cycles_overheat::check_can_overload_module(
            ctx, char_id, m.type_id,
        ) {
            return std::ops::ControlFlow::Break(Some(Err(e)));
        }
    }

    let is_upwell_target = match crate::services::station_structure::structure_lifecycle_rpc::validate_upwell_structure_attack_precheck(
        ctx, sys_id, char_id, target_ball_id, now_unix,
    ) {
        Ok(v) => v,
        Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
    };

    // Weapon classification & pre-validation
    let is_missile_launcher =
        crate::services::inventory_fitting::fitting_module_cycles::is_missile_launcher_group(
            mod_type.group_id,
        );
    let is_breacher_pod = matches!(mod_type.group_id, 4500 | 4805 | 4807);
    let is_vorton = mod_type.group_id == 4060;

    // Turret range validation BEFORE capacitor and ammo deduction (P1-ENTROPIC-PARTIAL-MUTATION-AND-AMMO-DEPLETION):
    // Calling dogmaIM.Activate out of range must fail without draining capacitor or ammo.
    let range_mult = charge_type_opt
        .and_then(|ct| ct.default_attributes.get(&120).copied())
        .unwrap_or(1.0);
    let base_opt = {
        let v = mod_state.get_attribute(54, &ctx.sde);
        if v > 0.0 {
            v
        } else {
            mod_type.default_attributes.get(&54).copied().unwrap_or(5_000.0)
        }
    };
    let dark_mult = crate::services::pve_abyssal_hazards::dark_weather_midpocket_turret_mult(
        ctx, char_id, sys_id, is_missile_launcher, module_id, &mod_type,
    );
    let optimal_range = (if weapon_charge_has_mod(&mod_state, charge_type_opt, 54, charge_item_id_opt) { base_opt } else { base_opt * range_mult }) * dark_mult;
    // Why: Vorton Projectors (group 4060) have strict optimal range with zero falloff; defaulting missing attr 158 to 5_000m adds +15km phantom range (#1247).
    let falloff_range = if is_vorton || is_missile_launcher {
        0.0
    } else {
        let falloff_v = mod_state.get_attribute(158, &ctx.sde);
        (if falloff_v > 0.0 { falloff_v } else {
            mod_type.default_attributes.get(&158).copied().unwrap_or(5_000.0)
        }) * dark_mult
    };
    // Why: EVE weapon range and falloff are always measured surface-to-surface across all target types, not only Upwell structures (#1247).
    let dist = attacker_ball.surface_distance(&target_ball);

    if is_missile_launcher {
        if target_id_opt.is_some() {
            let (_, _, max_flight_range) =
                super::combat_missiles_aoe::compute_missile_flight_kinematics(
                    autoload_fit_opt.as_deref().unwrap_or(fit),
                    module_id,
                    charge_type_opt,
                    &ctx.sde,
                );
            // Why: Enforce Cycle-0 missile/Breacher Pod surface flight range before autoload commit, capacitor, or ammo deduction (#1247).
            if max_flight_range > 0.0 && dist > max_flight_range + 1e-6 {
                let target_name =
                    crate::services::inventory_fitting::resolve_target_display_name(
                        ctx,
                        target_ball_id,
                    );
                if crate::system_actor::is_guided_missile_launcher_group(i64::from(
                    mod_type.group_id,
                )) {
                    return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::target_too_far(
                        target_name,
                        mod_type.type_id as i64,
                        dist,
                    ))));
                }
                return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::target_out_of_range(
                    target_name,
                    mod_type.type_id as i64,
                    dist,
                    format!(
                        "Target {target_ball_id} is {dist:.1} m away (exceeds range {max_flight_range:.1} m)"
                    ),
                ))));
            }
        }
    } else if dist > optimal_range + 3.0 * falloff_range {
        return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError::target_out_of_range(
            crate::services::inventory_fitting::resolve_target_display_name(
                ctx,
                target_ball_id,
            ),
            mod_type.type_id as i64,
            dist,
            format!(
                "Target {target_ball_id} is {dist:.1} m away (exceeds range {:.1} m)",
                optimal_range + 3.0 * falloff_range
            ),
        ))));
    }
    drop(autoload_fit_opt);

    let in_cycle = sys_handle.has_module_cycle(ship_id, module_id).await;
    let (in_delay, already_active) =
        crate::services::fitting_cycles_overheat::reconcile_module_active_state(
            ctx, ship_id, module_id, in_cycle, false, true, m, fit,
        );
    if is_entropic {
        let is_already_active = ctx.db.with_snapshot(|snap| snap.module_active_states.contains(&module_id))
            && sys_handle.get_entropic_spool(ship_id, module_id).await.is_some_and(|entry| entry.target_id == target_ball_id);
        if is_already_active {
            return std::ops::ControlFlow::Break(Some(Err(BridgeRpcError { code: "AlreadyActive".into(), message: format!("Module {module_id} is already active"), client_error: None })));
        }
    }
    if method == "Overload" {
        if !is_entropic && in_cycle {
            let rep = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
                ctx,
                sys_handle,
                char_id,
                ship_id,
                module_id,
                fit.clone(),
                true,
            )
            .await;
            return std::ops::ControlFlow::Break(Some(Ok(rep)));
        }
        if in_delay || already_active {
            return std::ops::ControlFlow::Break(Some(Err(
                crate::services::inventory_fitting::fitting_module_cycles::module_already_active_rpc_error(
                    ctx,
                    Some(m.type_id),
                    format!("Module {module_id} cannot overload while active or in reactivation delay"),
                ),
            )));
        }
    }
    if method == "Activate" && (in_delay || already_active) {
        return std::ops::ControlFlow::Break(Some(Err(
            crate::services::inventory_fitting::fitting_module_cycles::module_already_active_rpc_error(
                ctx,
                Some(m.type_id),
                format!("Module {module_id} is already active in a cycle"),
            ),
        )));
    }

    // Capacitor check & deduction:
    // Projectile turrets (`group_id == 55`) and Missile Launchers require 0 capacitor!
    // Hybrid turrets (`74`), Energy Lasers (`53`), Entropic Disintegrators (`1986`), and Vorton Projectors (`4060`) consume capacitor.
    let base_cap_need = if mod_type.group_id == 55
        || crate::services::inventory_fitting::fitting_module_cycles::is_missile_launcher_group(
            mod_type.group_id,
        )
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
        let cap_bonus_pct = charge_type_opt
            .and_then(|ct| ct.default_attributes.get(&317).copied())
            .unwrap_or(0.0);
        if weapon_charge_has_mod(&mod_state, charge_type_opt, 6, charge_item_id_opt) {
            raw_cap.max(0.0)
        } else {
            (raw_cap * (1.0 + cap_bonus_pct / 100.0)).max(0.0)
        }
    };
    let mut cap_before = 0.0;
    let mut cap_after = 0.0;
    let mut pending_cap_after: Option<f64> = None;
    if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
        cap_before = cs.capacitor;
        if base_cap_need > 0.0 {
            if cs.capacitor + 1e-6 < base_cap_need {
                return std::ops::ControlFlow::Break(Some(Err(
                    BridgeRpcError::not_enough_capacitor(
                        Some(ship_id),
                        &mod_type.name,
                        base_cap_need,
                        cs.capacitor,
                        format!(
                            "Weapon requires {base_cap_need:.2} GJ capacitor (have {:.2} GJ)",
                            cs.capacitor
                        ),
                    ),
                )));
            }
            cap_after = (cs.capacitor - base_cap_need).max(0.0);
            pending_cap_after = Some(cap_after);
        } else {
            cap_after = cs.capacitor;
        }
    }
    if let Some(plan) = pending_weapon_autoload.take() {
        let committed = match crate::services::inventory_fitting::fitting_ewar_capital::commit_and_emit_weapon_autoload(
            ctx,
            char_id,
            ship_id,
            mod_type,
            &plan,
        ) {
            Ok(c) => c,
            Err(e) => return std::ops::ControlFlow::Break(Some(Err(e))),
        };
        charge_item_id_opt = committed
            .created_slot_charges
            .iter()
            .find(|&&(tid, _)| tid == module_id)
            .map(|&(_, cid)| cid);
        if let Some(fm) = fit.modules.get_mut(&module_id) {
            fm.charge_type_id = Some(plan.ammo_type_id);
            fm.charge_quantity = plan
                .slot_allocations
                .iter()
                .find(|&&(tid, _, _)| tid == module_id)
                .map_or(1, |&(_, _, q)| q.max(0) as u32);
        }
        m.charge_type_id = Some(plan.ammo_type_id);
        fit.recalculate_stats(&ctx.sde);
    }
    if let Some(new_cap) = pending_cap_after {
        let _ = sys_handle.set_capacitor(ship_id, new_cap).await;
    }

    drop(pending_weapon_autoload);
    std::ops::ControlFlow::Continue(Box::new(PreparedDirectWeaponShot {
        mod_state: *mod_state,
        charge_item_id_opt,
        charge_type_id_opt,
        is_fof_missile,
        is_entropic,
        is_missile_launcher,
        is_breacher_pod,
        is_vorton,
        resolved_target_id,
        player_target_info,
        target_ball_id,
        target_ball: *target_ball,
        is_upwell_target,
        optimal_range,
        falloff_range,
        dist,
        base_cap_need,
        cap_before,
        cap_after,
    }))
}

fn apply_weapon_charge_volatility_and_base_damage(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    fit: &eve_dogma::ShipFittingEngine,
    mod_state: &eve_dogma::DogmaItemState,
    charge_item_id_opt: Option<i64>,
    charge_type_opt: Option<&eve_sde::InvType>,
    is_missile_launcher: bool,
) -> (f64, bool, DamageVector) {
    let charge_attr = |aid: u16, fallback: f64| -> f64 {
        if charge_type_opt.is_some() {
            if let Some(ch) = fit.charge_state(module_id, &ctx.sde) {
                if ch.base_attributes.contains_key(&aid)
                    || ch.modifiers.iter().any(|cm| cm.target_attribute_id == aid)
                {
                    return ch.get_attribute(aid, &ctx.sde);
                }
            }
        }
        charge_type_opt
            .and_then(|ct| ct.default_attributes.get(&aid).copied())
            .unwrap_or(fallback)
    };

    // Charge consumption / Laser frequency crystal volatility (`feat:modules.turret-laser`)
    let mut crystal_damage_after = 0.0;
    let mut crystal_shattered = false;
    if let (Some(charge_item_id), Some(_charge_type)) = (charge_item_id_opt, charge_type_opt) {
        if mod_type.group_id == 53 {
        // Energy Laser Turret: uses frequency crystals (`group_id == 86 | 375`).
        // T1 crystals (`attr 786 == 0.0`) do not degrade.
        // T2 (`attr 786 == 1.0`, `783 == 0.1`, `784 == 0.01`) and Faction (`786 == 1.0`, `783 == 1.0`, `784 == 0.00025`) take volatility damage!
        let crystals_get_damaged = charge_attr(786, 0.0) > 0.0;
        if crystals_get_damaged {
            let vol_chance = charge_attr(783, 0.10).clamp(0.0, 1.0);
            let vol_dmg = charge_attr(784, 0.01).max(0.0);
            let crystal_hp = charge_attr(9, 1.0).max(0.0001);
            if ctx.mechanic_rng.roll(vol_chance) {
                let (prev_dmg, next_dmg, shatter_diff) = ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        let cur = ci.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
                        let next_dmg = cur + vol_dmg;
                        crystal_damage_after = (next_dmg / crystal_hp).min(1.0);
                        if next_dmg + 1e-9 >= crystal_hp {
                            crystal_shattered = true;
                            let pre_rec = ci.clone();
                            let post_rec = if ci.quantity > 1 {
                                ci.quantity -= 1;
                                ci.version += 1;
                                ci.dynamic_attributes.remove(&3);
                                Some(ci.clone())
                            } else {
                                snap.items.remove(&charge_item_id);
                                snap.module_active_charges.remove(&module_id);
                                None
                            };
                            (cur, 0.0, Some((Some(pre_rec), post_rec)))
                        } else {
                            ci.dynamic_attributes.insert(3, next_dmg);
                            if ci.quantity == 1 {
                                ci.singleton = true;
                            }
                            ci.version += 1;
                            (cur, next_dmg, None)
                        }
                    } else {
                        (0.0, 0.0, None)
                    }
                });
                emit_weapon_charge_diff(ctx, char_id, ship_id, shatter_diff);
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnModuleAttributeChanges {
                        changes: PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::String("OnModuleAttributeChange".into()),
                            PyRep::Integer(char_id),
                            PyRep::Integer(charge_item_id),
                            PyRep::Integer(eve_sde::attr_ids::DAMAGE as i64),
                            PyRep::Integer(ctx.now_client_sim_filetime_ticks()),
                            PyRep::Real(next_dmg),
                            PyRep::Real(prev_dmg),
                            PyRep::None,
                        ])]),
                    },
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
        consume_one_weapon_round_and_emit(ctx, char_id, ship_id, module_id, charge_item_id);
    }
    }

    // Weapon damage calculation (evaluated via `ShipFittingEngine` for both charge & launcher/turret)
    let dmg_mult = if is_missile_launcher {
        fit.modules
            .get(&module_id)
            .and_then(|fm| fm.dynamic_attribute_overrides.get(&64).copied())
            .filter(|&v| v > 0.0)
            .unwrap_or(1.0)
    } else {
        let m_mult = mod_state.get_attribute(64, &ctx.sde);
        if m_mult > 0.0 {
            m_mult
        } else {
            mod_type.default_attributes.get(&64).copied().unwrap_or(1.0)
        }
    };
    let eval_charge_dmg = |aid: u16| -> f64 {
        if charge_type_opt.is_none() {
            let mv = mod_state.get_attribute(aid, &ctx.sde);
            if mv > 0.0 {
                return mv;
            }
            return mod_type.default_attributes.get(&aid).copied().unwrap_or(0.0);
        }
        let v = fit.get_charge_effective_attribute(module_id, aid, &ctx.sde);
        if v > 0.0 {
            v
        } else {
            charge_attr(aid, 0.0)
        }
    };
    let base_dmg = DamageVector {
        em: eval_charge_dmg(114) * dmg_mult,
        explosive: eval_charge_dmg(116) * dmg_mult,
        kinetic: eval_charge_dmg(117) * dmg_mult,
        thermal: eval_charge_dmg(118) * dmg_mult,
    };


    (crystal_damage_after, crystal_shattered, base_dmg)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn activate_and_finalize_missile_shot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    now_unix: u64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine,
    charge_item_id_opt: Option<i64>,
    charge_type_id_opt: Option<u32>,
    charge_type_opt: Option<&eve_sde::InvType>,
    attacker_ball: &eve_destiny::Ball,
    target_ball_id: i64,
    base_dmg: DamageVector,
    is_fof_missile: bool,
    is_breacher_pod: bool,
    is_overloaded: bool,
    has_explicit_target: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
        let (Some(charge_type_id), Some(charge_type)) = (charge_type_id_opt, charge_type_opt) else {
            return Some(Err(weapon_no_ammo_loaded_error(
                Some(module_id),
                mod_type,
                None,
                format!("Weapon module {module_id} has no compatible charge loaded"),
            )));
        };

        let launcher_res = activate_missile_or_breacher_launcher(
            ctx,
            char_id,
            ship_id,
            sys_id,
            now_unix,
            module_id,
            &m,
            fit,
            charge_type_id,
            charge_type,
            &attacker_ball,
            target_ball_id,
            base_dmg,
            is_fof_missile,
            is_breacher_pod,
            has_explicit_target,
        )
        .await;
        if launcher_res.is_ok() {
            sync_remaining_weapon_charge_in_fit(
                ctx,
                module_id,
                charge_item_id_opt,
                charge_type_id_opt,
                fit,
            );
            if is_overloaded {
                finalize_weapon_activation_overload_and_delay(
                    ctx,
                    char_id,
                    ship_id,
                    module_id,
                    fit,
                    true,
                    false,
                    None,
                );
            } else {
                ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
                ctx.invalidate_and_push_fit(ship_id);
            }
        }
        return Some(launcher_res);

}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn apply_primary_weapon_damage_and_finalize(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle, attacker_ball: &eve_destiny::Ball,
    module_id: i64, m: &eve_dogma::FittedModule, fit: &mut eve_dogma::ShipFittingEngine,
    resolved_target_id: i64, player_target_info: Option<(i64, i64)>, target_ball_id: i64,
    target_ball: &eve_destiny::Ball, is_upwell_target: bool, final_dmg: DamageVector,
    cycle_secs: f64, now_unix: u64, sys_sec: f64, is_overloaded: bool,
    charge_item_id_opt: Option<i64>, charge_type_id_opt: Option<u32>, p_hit: f64, is_wrecking: bool,
    cap_before: f64, cap_after: f64, crystal_damage_after: f64, crystal_shattered: bool,
    spool_cycles: u32, spool_multiplier: f64, vorton_secondary_targets: &[i64],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (applied_dmg, destroyed, sr, ar, hr, became_invuln) = if is_upwell_target {
        match crate::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure(
            ctx, sys_handle, sys_id, char_id, target_ball_id, final_dmg, cycle_secs, now_unix,
        ).await {
            Ok(Some(res)) => (res.applied_damage, res.destroyed, res.shield_ratio, res.armor_ratio, res.hull_ratio, res.became_invulnerable),
            Ok(None) => (0.0, false, 0.0, 0.0, 0.0, false),
            Err(e) => return Some(Err(e)),
        }
    } else {
        let is_invuln = crate::services::insurgency_services::is_insurgency_stronghold_invulnerable(ctx, sys_id, target_ball_id);
        let is_capsuleer_or_corp_ship =
            super::combat_missiles_aoe::active_ship_victim_char_id(ctx, target_ball_id).is_some()
                || super::combat_missiles_aoe::is_corp_owned_ship_ball(ctx, target_ball_id);
        let attacker_for_npc_hook = (!is_capsuleer_or_corp_ship).then_some(char_id);
        let (app, dest) = if !is_invuln && final_dmg.total() > 0.0 {
            sys_handle.apply_damage_with_attacker(target_ball_id, final_dmg, attacker_for_npc_hook).await
                .ok().flatten().map(|(a, d, _)| (a, d)).unwrap_or((0.0, false))
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

    if !is_upwell_target && applied_dmg >= 1.0 {
        crate::services::station_structure::structure_lifecycle_rpc::record_ship_damage_progression(
            ctx,
            sys_id,
            char_id,
            target_ball_id,
            applied_dmg,
        );
    }

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
        Some(sys_id),
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
        crate::services::combat_crimewatch::crimewatch_timers::on_weapon_aggression_post_hook(ctx, char_id, victim_char_opt, sys_id, now_unix);
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
                || has_limited_engagement_in_snap(snap, victim_char_id, char_id, now_unix)
        }) || {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let vst = cw.get_or_create(victim_char_id);
            vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
        };
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
        let is_new_agg = crate::services::combat_crimewatch::crimewatch_timers::is_new_pvp_aggression(
            ctx,
            char_id,
            victim_char_id,
            is_pod_target,
            now_unix,
        );
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
        let le_created_or_refreshed = ensure_mutual_limited_engagement_if(
            ctx,
            char_id,
            victim_char_id,
            victim_is_suspect_or_criminal,
            now_unix,
        );
        if le_created_or_refreshed {
            emit_limited_engagement_created(ctx, char_id, victim_char_id);
        }
        if !is_legal && sys_sec > 0.0 && !is_mobile_struct && is_new_agg {
            crate::services::combat_crimewatch::crimewatch_timers::record_illegal_pvp_penalties_if_new(
                ctx,
                char_id,
                victim_char_id,
                sys_id,
                sys_sec,
                is_pod_target,
                is_new_agg,
                now_unix,
            );
        }
        if !is_legal && sys_sec > 0.0 && sys_sec < 0.45 && !is_mobile_struct {
            if let Some(sentry_origin) = crate::services::security_mgr::nearest_sentry_gun_origin(
                ctx,
                sys_id,
                attacker_ball.position,
            ) {
                let _ = crate::services::combat_crimewatch::crimewatch_timers::step_sentry_gun_grid_aggression(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    sentry_origin,
                    is_new_agg,
                )
                .await;
            }
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
    let combat_weapons_spool::ShipOrNpcDestructionResult {
        kill_id: generated_kill_id,
        wreck_id: generated_wreck_id,
        corpse_id: generated_corpse_id,
        killmail_hash: generated_km_hash,
    } = if destroyed && is_upwell_target {
        let kid = ctx.db.with_snapshot(|snap| snap.killmails.keys().max().copied().unwrap_or(0));
        combat_weapons_spool::ShipOrNpcDestructionResult {
            kill_id: kid,
            ..Default::default()
        }
    } else if destroyed {
        combat_weapons_spool::handle_ship_or_npc_destruction(
            ctx,
            sys_handle,
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
        combat_weapons_spool::ShipOrNpcDestructionResult::default()
    };

    if became_invuln {
        let _ = sys_handle.remove_target_lock(ship_id, target_ball_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&module_id);
            snap.module_active_charges.remove(&module_id);
        });
        let _ = fit.deactivate_module(module_id, &ctx.sde);
        combat_weapons_spool::reset_entropic_spool(ctx, ship_id, module_id);
        ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
        ctx.invalidate_and_push_fit(ship_id);
        ctx.record_module_reactivation_delay(ship_id, module_id, cycle_secs);
    } else {
        sync_remaining_weapon_charge_in_fit(
            ctx,
            module_id,
            charge_item_id_opt,
            charge_type_id_opt,
            fit,
        );
        finalize_weapon_activation_overload_and_delay(
            ctx,
            char_id,
            ship_id,
            module_id,
            fit,
            is_overloaded,
            true,
            Some(cycle_secs),
        );
    }

    let st_state = if is_upwell_target {
        ctx.db.with_snapshot(|snap| snap.structures.get(&target_ball_id).map(|s| s.state.clone()))
    } else {
        None
    };

    Some(Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("targetID", PyRep::Integer(target_ball_id)),
        ("chargeTypeID", charge_type_id_opt.map(|cid| PyRep::Integer(cid as i64)).unwrap_or(PyRep::None)),
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

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn execute_direct_turret_or_vorton_shot(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle, attacker_tick: u64,
    attacker_ball: &eve_destiny::Ball, module_id: i64, now_unix: u64, sys_sec: f64,
    is_overloaded: bool, m: &eve_dogma::FittedModule, mod_type: &eve_sde::InvType,
    fit: &mut eve_dogma::ShipFittingEngine, prep: Box<PreparedDirectWeaponShot>,
    base_dmg: DamageVector, crystal_damage_after: f64, crystal_shattered: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let charge_type_opt = prep.charge_type_id_opt.and_then(|ctid| ctx.sde.types.get(&ctid));
    let target_sig = resolve_target_effective_signature(
        ctx, sys_handle, sys_id, prep.target_ball_id, &prep.target_ball, prep.player_target_info,
    ).await;
    let cycle_secs = (weapon_mod_attr_pos(&prep.mod_state, mod_type, &ctx.sde, 51, 3_000.0) / 1000.0).max(0.1);
    let tracking_mult = charge_type_opt.and_then(|ct| ct.default_attributes.get(&244).copied()).unwrap_or(1.0);
    let base_tracking = {
        let v = prep.mod_state.get_attribute(160, &ctx.sde);
        if v > 0.0 { v } else { mod_type.default_attributes.get(&160).copied().unwrap_or(100.0) }
    };
    let attacker_pylon_mult = sys_handle.get_combat_state(ship_id).await.ok().flatten()
        .map(|cs| cs.tracking_speed_multiplier).filter(|&m| m > 0.0).unwrap_or(1.0);
    let tracking_speed = (if weapon_charge_has_mod(&prep.mod_state, charge_type_opt, 160, prep.charge_item_id_opt) {
        base_tracking
    } else {
        base_tracking * tracking_mult
    }) * attacker_pylon_mult;
    let turret_sig_res = weapon_mod_attr_pos(&prep.mod_state, mod_type, &ctx.sde, 620, 40_000.0);

    let (p_hit, shot_dmg, is_wrecking) = if prep.is_vorton {
        // Why: Vorton Projectors skip turret angular/wrecking rolls and apply via missile DRF within optimal (#1247).
        (1.0, base_dmg, false)
    } else {
        let angular_vel = eve_destiny::combat::compute_angular_velocity(
            attacker_ball.position, attacker_ball.velocity,
            prep.target_ball.position, prep.target_ball.velocity,
        );
        let p_hit = turret_chance_to_hit_with_res(
            angular_vel, tracking_speed, turret_sig_res, target_sig,
            prep.dist, prep.optimal_range, prep.falloff_range,
        );
        let hit_roll = ctx.next_invention_roll(ship_id as u64, prep.target_ball_id as u64, attacker_tick);
        let (shot_dmg, is_wrecking) = turret_shot_damage(base_dmg, p_hit, hit_roll);
        (p_hit, shot_dmg, is_wrecking)
    };

    // Entropic Disintegrator consecutive target-specific spooling (`feat:modules.turret-precursor`)
    let (spool_cycles, spool_multiplier) = if prep.is_entropic {
        let duration_s = weapon_mod_attr_pos(&prep.mod_state, mod_type, &ctx.sde, 51, 3_500.0) / 1000.0;
        match combat_weapons_spool::handle_entropic_activation(ctx, char_id, ship_id, sys_id, module_id, prep.target_ball_id, mod_type, prep.charge_item_id_opt, base_dmg, prep.base_cap_need, duration_s) {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        }
    } else {
        (0u32, 1.0)
    };

    let (vorton_secondary_targets, final_dmg) = if prep.is_vorton {
        apply_vorton_arc_secondary_hits(
            ctx, char_id, ship_id, sys_id, sys_handle, attacker_ball,
            m, mod_type, &prep.mod_state, prep.target_ball_id, &prep.target_ball,
            target_sig, base_dmg, cycle_secs, now_unix, sys_sec,
        ).await
    } else if prep.is_entropic {
        (Vec::new(), shot_dmg.scaled(spool_multiplier))
    } else {
        (Vec::new(), shot_dmg)
    };

    apply_primary_weapon_damage_and_finalize(
        ctx, char_id, ship_id, sys_id, sys_handle, attacker_ball, module_id, m, fit,
        prep.resolved_target_id, prep.player_target_info, prep.target_ball_id, &prep.target_ball,
        prep.is_upwell_target, final_dmg, cycle_secs, now_unix, sys_sec, is_overloaded,
        prep.charge_item_id_opt, prep.charge_type_id_opt, p_hit, is_wrecking,
        prep.cap_before, prep.cap_after, crystal_damage_after, crystal_shattered,
        spool_cycles, spool_multiplier, &vorton_secondary_targets,
    ).await
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_direct_weapon_activation(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle, attacker_tick: u64,
    attacker_ball: &eve_destiny::Ball, method: &str, module_id: i64, target_id_opt: Option<i64>,
    now_unix: u64, sys_sec: f64, is_overloaded: bool, m: &mut eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType, fit: &mut eve_dogma::ShipFittingEngine,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let prep = match prepare_direct_weapon_shot(
        ctx, char_id, ship_id, sys_id, sys_handle, attacker_ball, method, module_id,
        target_id_opt, now_unix, sys_sec, is_overloaded, m, mod_type, fit,
    ).await {
        std::ops::ControlFlow::Continue(p) => p,
        std::ops::ControlFlow::Break(res) => return res,
    };

    let charge_type_opt = prep.charge_type_id_opt.and_then(|ctid| ctx.sde.types.get(&ctid));
    let (crystal_damage_after, crystal_shattered, base_dmg) = apply_weapon_charge_volatility_and_base_damage(
        ctx, char_id, ship_id, module_id, mod_type, fit, &prep.mod_state,
        prep.charge_item_id_opt, charge_type_opt, prep.is_missile_launcher,
    );

    if prep.is_missile_launcher {
        let PreparedDirectWeaponShot {
            charge_item_id_opt, charge_type_id_opt, target_ball_id, is_fof_missile, is_breacher_pod, ..
        } = *prep;
        return activate_and_finalize_missile_shot(
            ctx, char_id, ship_id, sys_id, now_unix, module_id, m, mod_type, fit,
            charge_item_id_opt, charge_type_id_opt, charge_type_opt, attacker_ball,
            target_ball_id, base_dmg, is_fof_missile, is_breacher_pod,
            is_overloaded, target_id_opt.is_some(),
        ).await;
    }

    execute_direct_turret_or_vorton_shot(
        ctx, char_id, ship_id, sys_id, sys_handle, attacker_tick, attacker_ball, module_id,
        now_unix, sys_sec, is_overloaded, m, mod_type, fit, prep, base_dmg,
        crystal_damage_after, crystal_shattered,
    ).await
}

pub use super::combat_weapons_spool::{clear_entropic_spool, get_entropic_spool_multiplier, handle_link_all_weapons, reset_entropic_spool};

#[cfg(test)]
#[path = "combat_weapons_tests.rs"] pub(in crate::services) mod combat_weapons_tests;

#[cfg(test)]
#[path = "combat_weapons_edges2_tests.rs"]
mod combat_weapons_edges2_tests;

