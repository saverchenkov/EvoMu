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

pub struct AncillaryRepairPrep {
    pub is_asb: bool,
    pub asb_fallback_cap_need: f64,
    pub cap_need: f64,
    pub is_aar: bool,
    pub aar_paste_rate: i32,
    pub aar_base_armor_repair: f64,
    pub armor_repair_amount: f64,
    pub remote_armor_amount: f64,
}

/// Evaluates ancillary module requirements without charge to maintain hull, skill, and rig modifiers
/// (`P1-ANCILLARY-RAW-SDE-FALLBACK`).
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

    let mut cap_need = initial_cap_need;
    if is_asb {
        let mut consumed_charge = false;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(cid) = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && (i.flag as u16) == flag_id
                        && i.item_id != module_id
                        && i.quantity > 0
                })
                .map(|i| i.item_id)
            {
                consumed_charge = true;
                let mut rem = 0;
                if let Some(ci) = snap.items.get_mut(&cid) {
                    ci.quantity = ci.quantity.saturating_sub(1);
                    rem = ci.quantity;
                }
                if rem == 0 {
                    snap.items.remove(&cid);
                }
            }
        });
        if consumed_charge {
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
        let mut fueled = false;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(cid) = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && (i.flag as u16) == flag_id
                        && i.item_id != module_id
                        && i.type_id == 28668
                        && i.quantity >= aar_paste_rate
                })
                .map(|i| i.item_id)
            {
                fueled = true;
                let mut rem = 0;
                if let Some(ci) = snap.items.get_mut(&cid) {
                    ci.quantity -= aar_paste_rate;
                    rem = ci.quantity;
                }
                if rem == 0 {
                    snap.items.remove(&cid);
                }
            }
        });
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
    }
}

/// Handles toggling overload on a module that is already cycling.
pub async fn handle_active_overload_toggle(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mut fit: ShipFittingEngine,
) -> PyRep {
    let _ = sys.update_module_overload(ship_id, module_id, true).await;
    if let Some(fm) = fit.modules.get_mut(&module_id) {
        fm.state = eve_dogma::ModuleState::Overloaded;
    }
    fit.recalculate_stats(&ctx.sde);
    let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnHeatAdded",
        "charid",
        vec![PyRep::Integer(module_id)],
    );
    PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("overloaded", PyRep::Bool(true)),
        ("effectiveMaxVelocity", PyRep::Real(max_vel)),
    ])
}
