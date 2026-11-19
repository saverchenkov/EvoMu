//! `fitting_cycles_overheat` — Module cycle helpers for heat, overload, and ancillary repairers.
//!
//! parity: feat:modules.overheat-burnout-state, feat:modules.overheat-damage-propagation, feat:modules.overheat-rack-heat-generation

use crate::router::context::ServerContext;
use crate::system_actor::SystemHandle;
use eve_dogma::{attr_ids, ShipFittingEngine};
use eve_proto::PyRep;
use eve_sde::InvType;

/// Checks if the module has any dogma effect with `effect_category == 5` (overload effects).
pub fn has_category5_overload(mod_type: &InvType, sde: &eve_sde::SdeRegistry) -> bool {
    mod_type.effect_ids.iter().any(|&eid| {
        sde.effects.get(&eid).is_some_and(|e| e.effect_category == 5)
    })
}

/// Computes the overload multiplier for local repairers, shield boosters, and tackle.
/// Modules carrying SDE `effect_category == 5` overload effects have their overload bonuses
/// already applied during Dogma effective state evaluation; applying 1.20x a second time
/// results in double-multiplication (`P1-OVERLOAD-DOUBLE-MULT`).
pub fn compute_overload_multiplier(
    is_overloaded: bool,
    mod_type: &InvType,
    sde: &eve_sde::SdeRegistry,
) -> f64 {
    if is_overloaded && !has_category5_overload(mod_type, sde) {
        1.20
    } else {
        1.0
    }
}

/// Maps a ship module slot `flag_id` to its thermodynamic rack index (`0 = high`, `1 = med`, `2 = low`).
pub fn rack_index_for_flag(flag_id: u16) -> Option<usize> {
    match flag_id {
        27..=34 => Some(0),
        19..=26 => Some(1),
        11..=18 => Some(2),
        _ => None,
    }
}

/// Maps a thermodynamic rack index (`0 = high`, `1 = med`, `2 = low`) to its ship rack heat attribute ID (`1175`/`1176`/`1177`).
pub fn rack_heat_attr_id_for_rack(rack_idx: usize) -> i64 {
    let attrs = crate::services::tactical_module_mechanics::RACK_HEAT_ATTR_IDS;
    i64::from(attrs.get(rack_idx).copied().unwrap_or(attrs[2]))
}

/// Maps a ship module slot `flag_id` to its ship rack heat attribute ID (`1175`/`1176`/`1177`).
pub fn rack_heat_attr_id_for_flag(flag_id: u16) -> i64 {
    rack_heat_attr_id_for_rack(rack_index_for_flag(flag_id).unwrap_or(2))
}

/// Resolves a fitted module's ship rack heat attribute ID (`1175`/`1176`/`1177`) from `fit` with DB snapshot fallback.
pub fn resolve_module_heat_id(
    ctx: &ServerContext,
    fit: &ShipFittingEngine,
    module_id: i64,
) -> i64 {
    let flag_id = fit.modules.get(&module_id).map(|fm| fm.flag_id).unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.items.get(&module_id).map_or(0, |i| i.flag as u16))
    });
    rack_heat_attr_id_for_flag(flag_id)
}

pub struct AncillaryRepairPrep {
    pub is_asb: bool,
    pub asb_fallback_cap_need: f64,
    pub cap_need: f64,
    pub is_aar: bool,
    pub aar_paste_rate: i32,
    pub aar_base_armor_repair: f64,
    pub armor_repair_amount: f64,
    pub remote_armor_amount: f64,
    pub pending_charge_consume: Option<(i64, i32)>,
}

/// Evaluates ancillary module requirements without mutating inventory so charge consumption can
/// be deferred until module cycle insertion and aggression/safety checks succeed.
#[allow(clippy::too_many_arguments)]
pub fn plan_ancillary_module(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
    flag_id: u16,
    mod_type: &InvType,
    fit: &ShipFittingEngine,
    is_overloaded: bool,
    initial_cap_need: f64,
    initial_armor_repair: f64,
    initial_remote_armor: f64,
    overload_mult: f64,
) -> AncillaryRepairPrep {
    let is_asb = matches!(mod_type.group_id, 1156 | 1697);
    let is_aar = matches!(mod_type.group_id, 1199 | 1698);

    let unfueled_mod_state = if is_asb || is_aar {
        let mut unfueled_fit = fit.clone();
        if let Some(m) = unfueled_fit.modules.get_mut(&module_id) {
            m.charge_type_id = None;
        }
        unfueled_fit.build_effective_module_state(module_id, mod_type.type_id, is_overloaded, &ctx.sde)
    } else {
        fit.build_effective_module_state(module_id, mod_type.type_id, is_overloaded, &ctx.sde)
    };

    let asb_fallback_cap_need = if is_asb {
        unfueled_mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde)
    } else {
        0.0
    };

    let mut pending_charge_consume: Option<(i64, i32)> = None;
    let mut cap_need = initial_cap_need;
    if is_asb {
        let cid_opt = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && (i.flag as u16) == flag_id
                        && i.item_id != module_id
                        && i.quantity > 0
                })
                .map(|i| i.item_id)
        });
        if let Some(cid) = cid_opt {
            pending_charge_consume = Some((cid, 1));
            cap_need = 0.0;
        } else {
            cap_need = asb_fallback_cap_need;
        }
    }

    let aar_paste_rate = mod_type
        .default_attributes
        .get(&56)
        .copied()
        .unwrap_or(4.0)
        .round()
        .max(1.0) as i32;

    let aar_base_armor_repair = if mod_type.group_id == 1698 {
        initial_remote_armor
    } else if is_aar {
        unfueled_mod_state.get_attribute(84, &ctx.sde) * overload_mult
    } else {
        0.0
    };

    let mut armor_repair_amount = initial_armor_repair;
    let mut remote_armor_amount = initial_remote_armor;
    if is_aar {
        let cid_opt = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && (i.flag as u16) == flag_id
                        && i.item_id != module_id
                        && i.type_id == 28668
                        && i.quantity >= aar_paste_rate
                })
                .map(|i| i.item_id)
        });
        let fueled = if let Some(cid) = cid_opt {
            pending_charge_consume = Some((cid, aar_paste_rate));
            true
        } else {
            false
        };
        if mod_type.group_id == 1698 {
            remote_armor_amount = if fueled {
                aar_base_armor_repair * 3.0
            } else {
                aar_base_armor_repair
            };
        } else {
            armor_repair_amount = if fueled {
                aar_base_armor_repair * 3.0
            } else {
                aar_base_armor_repair
            };
        }
    }

    AncillaryRepairPrep {
        is_asb,
        asb_fallback_cap_need,
        cap_need,
        is_aar,
        aar_paste_rate,
        aar_base_armor_repair,
        armor_repair_amount,
        remote_armor_amount,
        pending_charge_consume,
    }
}

/// Evaluates ancillary module requirements and immediately consumes charge/paste (legacy helper for direct unit tests).
#[allow(clippy::too_many_arguments)]
pub fn prepare_ancillary_module(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
    flag_id: u16,
    mod_type: &InvType,
    fit: &ShipFittingEngine,
    is_overloaded: bool,
    initial_cap_need: f64,
    initial_armor_repair: f64,
    initial_remote_armor: f64,
    overload_mult: f64,
) -> AncillaryRepairPrep {
    let mut prep = plan_ancillary_module(
        ctx,
        ship_id,
        module_id,
        flag_id,
        mod_type,
        fit,
        is_overloaded,
        initial_cap_need,
        initial_armor_repair,
        initial_remote_armor,
        overload_mult,
    );
    if let Some((cid, qty)) = prep.pending_charge_consume.take() {
        let consumed = ctx.db.with_mut_snapshot(|snap| {
            let pre = snap.items.get(&cid).cloned()?;
            let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(None, pre.owner_id);
            opts.reject_fitted = false;
            let post = snap.consume_exact_item_quantity(cid, qty, opts).ok()?;
            Some((pre, post))
        });
        if let Some((pre, post)) = consumed {
            ctx.notify_item_mutation(pre.owner_id, Some(&pre), post.as_ref());
            crate::services::inventory_fitting::fitting_module_cycles::emit_charge_sublocation_quantity_change(
                ctx,
                pre.owner_id,
                pre.location_id,
                pre.flag,
                pre.type_id,
                pre.quantity,
                post.as_ref().map_or(0, |p| p.quantity),
            );
        }
    }
    prep
}

/// Returns the trained `Thermodynamics` (`28164`) level (`0` if untrained) for `char_id`.
pub fn pilot_thermodynamics_level(ctx: &ServerContext, char_id: i64) -> u32 {
    ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|sk| sk.get(&28164))
            .map_or(0, |r| u32::from(r.level))
    })
}

fn is_snapshot_item_burned_out(
    ctx: &ServerContext,
    item_id: i64,
    fallback_type_id: Option<u32>,
) -> bool {
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&item_id);
        let dmg = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
            .unwrap_or(0.0);
        let type_id = fallback_type_id.or_else(|| it.map(|i| i.type_id));
        let hp = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            .or_else(|| {
                type_id
                    .and_then(|tid| ctx.sde.types.get(&tid))
                    .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        dmg >= hp - 1e-6
    })
}

/// Returns true if the fitted module is burned out (`damage >= max_hp - 1e-6`).
pub fn is_module_burned_out(
    ctx: &ServerContext,
    fm: &eve_dogma::FittedModule,
    module_id: i64,
) -> bool {
    fm.damage_hp >= fm.max_hp - 1e-6
        || is_snapshot_item_burned_out(ctx, module_id, Some(fm.type_id))
}

/// Rejects activation or overload if the module or any member of its weapon bank is burned out.
pub fn check_module_or_bank_not_burned_out(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
    fm: &eve_dogma::FittedModule,
) -> Result<(), eve_proto::BridgeRpcError> {
    if is_module_burned_out(ctx, fm, module_id) {
        return Err(eve_proto::BridgeRpcError {
            code: "ModuleBurnedOut".into(),
            message: format!("Module {module_id} is burned out and cannot be activated"),
            client_error: None,
        });
    }
    let bank_ids = ctx.db.with_snapshot(|snap| {
        snap.weapon_banks
            .get(&ship_id)
            .and_then(|b| b.get(&module_id))
            .cloned()
            .unwrap_or_default()
    });
    for tid in bank_ids {
        if tid != module_id && is_snapshot_item_burned_out(ctx, tid, None) {
            return Err(eve_proto::BridgeRpcError {
                code: "ModuleBurnedOut".into(),
                message: format!(
                    "Module group contains burned-out module {tid} and is disabled until repaired or ungrouped"
                ),
                client_error: None,
            });
        }
    }
    Ok(())
}

fn required_thermodynamics_level(mod_type: &InvType) -> u32 {
    mod_type
        .default_attributes
        .get(&1212)
        .copied()
        .map_or(1u32, |v| v.ceil().max(1.0) as u32)
}

/// Checks whether `mod_type_id` has an SDE `effect_category == 5` overload effect and
/// whether `thermo_lvl` satisfies `requiredThermoDynamicsSkill` (`1212`, minimum `1`).
pub fn can_overload_module(ctx: &ServerContext, mod_type_id: u32, thermo_lvl: u32) -> bool {
    let Some(mod_type) = ctx.sde.types.get(&mod_type_id) else {
        return false;
    };
    has_category5_overload(mod_type, &ctx.sde)
        && thermo_lvl >= required_thermodynamics_level(mod_type)
}

/// Enforces Category-5 overload capability and pilot Thermodynamics level for `dogmaIM.Overload`.
pub fn check_can_overload_module(
    ctx: &ServerContext,
    char_id: i64,
    mod_type_id: u32,
) -> Result<(), eve_proto::BridgeRpcError> {
    let Some(mod_type) = ctx.sde.types.get(&mod_type_id) else {
        return Err(eve_proto::BridgeRpcError {
            code: "ModuleNotFound".into(),
            message: format!("Module type {mod_type_id} not found in SDE"),
            client_error: None,
        });
    };
    if !has_category5_overload(mod_type, &ctx.sde) {
        return Err(eve_proto::BridgeRpcError {
            code: "CannotOverloadModule".into(),
            message: format!("Module type {mod_type_id} cannot be overloaded"),
            client_error: None,
        });
    }
    let req_thermo = required_thermodynamics_level(mod_type);
    let thermo_lvl = pilot_thermodynamics_level(ctx, char_id);
    if thermo_lvl < req_thermo {
        return Err(eve_proto::BridgeRpcError {
            code: "DontHaveThermoDynamicsSkillz".into(),
            message: format!(
                "Thermodynamics level {req_thermo} required to overload module type {mod_type_id} (current level {thermo_lvl})"
            ),
            client_error: None,
        });
    }
    Ok(())
}

/// Emits `OnHeatAdded { heat_id, module_id }` for a fitted module's thermodynamic rack.
pub fn emit_module_heat_added(
    ctx: &ServerContext,
    char_id: i64,
    fit: &ShipFittingEngine,
    module_id: i64,
) {
    let heat_id = resolve_module_heat_id(ctx, fit, module_id);
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnHeatAdded { heat_id, module_id },
    );
}

/// Handles toggling overload on a module that is already cycling, or pre-overloading an inactive targeted module.
pub async fn handle_active_overload_toggle(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mut fit: ShipFittingEngine,
    already_active: bool,
) -> PyRep {
    let _ = sys.update_module_overload(ship_id, module_id, true).await;
    if already_active {
        fit.pending_overload_modules.remove(&module_id);
    } else {
        if let Some(fm) = fit.modules.get_mut(&module_id) {
            fm.state = eve_dogma::ModuleState::Overloaded;
        }
        fit.pending_overload_modules.insert(module_id);
        fit.recalculate_stats(&ctx.sde);
        ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
        ctx.invalidate_and_push_fit(ship_id);
    }
    let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
    emit_module_heat_added(ctx, char_id, &fit, module_id);
    PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("overloaded", PyRep::Bool(true)),
        ("effectiveMaxVelocity", PyRep::Real(max_vel)),
    ])
}

/// Reconciles active and reactivation-delay state for `(ship_id, module_id)` before activation
/// or overload, clearing any stale `Active` state while preserving `Overloaded`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile_module_active_state(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
    in_actor_cycle: bool,
    cleaned_stale: bool,
    clear_non_actor_db_active: bool,
    m: &mut eve_dogma::FittedModule,
    fit: &mut ShipFittingEngine,
) -> (bool, bool) {
    let in_delay = ctx.is_module_in_reactivation_delay(ship_id, module_id);
    if cleaned_stale || (clear_non_actor_db_active && !in_delay && !in_actor_cycle) {
        ctx.db.with_mut_snapshot(|s| {
            s.module_active_states.remove(&module_id);
        });
    }
    let in_db_active = ctx
        .db
        .with_snapshot(|s| s.module_active_states.contains(&module_id));
    // Why: Only clear stale Active state; pre-overloaded idle modules must retain Overloaded across failed activations.
    if !in_db_active && !in_actor_cycle && m.state == eve_dogma::ModuleState::Active {
        m.state = eve_dogma::ModuleState::Online;
        if let Some(fm) = fit.modules.get_mut(&module_id) {
            fm.state = eve_dogma::ModuleState::Online;
        }
        fit.recalculate_stats(&ctx.sde);
        ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
        ctx.invalidate_and_push_fit(ship_id);
    }
    let already_active = m.state == eve_dogma::ModuleState::Active
        || in_db_active
        || in_actor_cycle
        || in_delay;
    (in_delay, already_active)
}

/// Validates module overload eligibility (`effect_category == 5` + `attr 1212`
/// `requiredThermoDynamicsSkill`, minimum 1) and checks cycle/delay state: returns `Ok(true)` for
/// mid-cycle overload toggle, `Ok(false)` for idle pre-overload, or `Err(ModuleAlreadyActive)`
/// during reactivation delay / entropic spool.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_overload_cycle_or_delay(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    in_actor_cycle: bool,
    cleaned_stale: bool,
    clear_non_actor_db_active: bool,
    is_entropic: bool,
    m: &mut eve_dogma::FittedModule,
    fit: &mut ShipFittingEngine,
) -> Result<bool, eve_proto::BridgeRpcError> {
    check_can_overload_module(ctx, char_id, m.type_id)?;
    let (in_delay, already_active) = reconcile_module_active_state(
        ctx,
        ship_id,
        module_id,
        in_actor_cycle,
        cleaned_stale,
        clear_non_actor_db_active,
        m,
        fit,
    );
    if !is_entropic && in_actor_cycle {
        return Ok(true);
    }
    if in_delay || already_active {
        return Err(
            crate::services::inventory_fitting::fitting_module_cycles::module_already_active_rpc_error(
                ctx,
                Some(m.type_id),
                format!("Module {module_id} cannot overload while active or in reactivation delay"),
            ),
        );
    }
    Ok(false)
}

/// Handles targetless `dogmaIM.Overload(itemID[, effectID|effectName])` early before activation
/// guards (warp/cloak/tether/safety) and per-group activation paths.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_early_targetless_overload(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    clear_non_actor_db_active: bool,
    m: &mut eve_dogma::FittedModule,
    mut fit: ShipFittingEngine,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    if matches!(mod_type.group_id, 1327 | 1328 | 1330 | 1331 | 1333 | 1562) {
        return Err(crate::services::structure_weapons_ops::cannot_overload_structure_module_error());
    }
    let (actor_cycle_state, cleaned_stale) = sys
        .reconcile_module_cycle_before_activate(ship_id, module_id, ctx.now_unix_secs())
        .await;
    let active_toggle = check_overload_cycle_or_delay(
        ctx,
        char_id,
        ship_id,
        module_id,
        actor_cycle_state.is_some(),
        cleaned_stale,
        clear_non_actor_db_active,
        mod_type.group_id == 1986,
        m,
        &mut fit,
    )?;
    Ok(crate::router::box_fut(|| {
        handle_active_overload_toggle(ctx, sys, char_id, ship_id, module_id, fit, active_toggle)
    })
    .await)
}
/// Enforces CCP in-space Nanite Repair Paste lockouts (`feat:modules.nanite-repair-paste-in-space`):
/// a module currently being repaired cannot be activated, and no module on the ship may be overloaded
/// while any module on the ship is undergoing active Nanite Repair Paste repair.
pub fn check_nanite_repair_overheat_lockout(
    sys: &SystemHandle,
    ship_id: i64,
    module_id: i64,
    method: &str,
    mod_state: eve_dogma::ModuleState,
) -> Result<(), eve_proto::BridgeRpcError> {
    if sys.is_module_being_nanite_repaired_sync(ship_id, module_id) {
        return Err(eve_proto::BridgeRpcError {
            code: "CannotActivateRepairingModule".into(),
            message: format!(
                "UserError: Module {module_id} cannot be activated while Nanite Repair Paste repair is in progress"
            ),
            client_error: None,
        });
    }
    if (method == "Overload" || mod_state == eve_dogma::ModuleState::Overloaded)
        && sys.has_any_active_nanite_repair_sync(ship_id)
    {
        return Err(eve_proto::BridgeRpcError {
            code: "CannotOverheatWhileRepairing".into(),
            message: "UserError: Cannot overheat modules while in-space Nanite Repair Paste repair is active".into(),
            client_error: None,
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "fitting_cycles_overheat_tests.rs"]
mod fitting_cycles_overheat_tests;

