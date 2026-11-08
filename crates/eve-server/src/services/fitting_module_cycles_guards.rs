//! Space module & action cloak, gate-cloak break, and active-warp guards (`#494`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_proto::BridgeRpcError;

/// Returns `true` when `mod_type` (`group_id == 330`) is a Covert Ops warp-safe cloaking device.
pub(crate) fn is_warp_safe_cloak_module(ctx: &ServerContext, mod_type: &eve_sde::InvType) -> bool {
    mod_type.group_id == 330
        && (mod_type.type_id == 11578
            || mod_type.effect_ids.contains(&980)
            || mod_type
                .effect_ids
                .iter()
                .any(|eid| ctx.sde.effects.get(eid).is_some_and(|e| e.is_warp_safe))
            || mod_type.name.contains("Covert Ops"))
}

/// Returns `true` when `ship_id` has an active non-gate cloak in `sys` or `ctx.db`.
pub(crate) async fn is_ship_actively_cloaked(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
) -> bool {
    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
            || cs.cloak_velocity_multiplier.is_some()
        {
            return true;
        }
    }
    ctx.db.with_snapshot(|s| {
        s.items.values().any(|it| {
            it.location_id == ship_id
                && s.module_active_states.contains(&it.item_id)
                && ctx
                    .sde
                    .types
                    .get(&it.type_id)
                    .is_some_and(|t| t.group_id == 330)
        })
    })
}

/// Enforces active-warp (`CantActivateInWarp`), gate-cloak break (`gate_cloak_remaining_ticks > 0`),
/// and non-gate cloak (`CannotActivateCloaked`) checks before activating a ship module in space (`#494`).
pub(crate) async fn enforce_space_module_cloak_and_warp_guard(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
) -> Result<(), BridgeRpcError> {
    if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
        if matches!(
            ship_ball.mode,
            eve_destiny::MovementMode::Warp { phase, .. }
                if phase != eve_destiny::WarpPhase::Aligning
        ) {
            // Interdiction Nullifiers (`4117`) preserve their dedicated `CannotActivateInWarp` check;
            // Covert Ops Cloaks (`330` warp-safe) are allowed in warp.
            if mod_type.group_id != 4117 && !is_warp_safe_cloak_module(ctx, mod_type) {
                return Err(BridgeRpcError {
                    code: "CantActivateInWarp".into(),
                    message: "Cannot activate modules while in warp".into(),
                });
            }
        }
    }

    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.gate_cloak_remaining_ticks > 0 {
            let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(
                ctx, sys, char_id, ship_id,
            )
            .await;
        }
    }

    // Cloaking devices (`330`), Scan Probe Launchers (`481`), and Interdiction Nullifiers (`4117`)
    // may be activated while cloaked; all weapons, bombs, smartbombs, bursts, tank, and EWAR cannot.
    if !matches!(mod_type.group_id, 330 | 481 | 4117)
        && is_ship_actively_cloaked(ctx, sys, ship_id).await
    {
        return Err(BridgeRpcError::user_error("CannotActivateCloaked"));
    }

    Ok(())
}

/// Enforces active-warp (`CantActivateInWarp`), gate-cloak break, and non-gate cloak
/// (`CannotActivateCloaked`) checks for non-module space actions (`LaunchDrones`, `CmdEngage`,
/// `superWeaponMgr`, `#494`, `#495`, `#496`).
pub(crate) async fn enforce_space_action_cloak_and_warp_guard(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    action_label: &str,
) -> Result<(), BridgeRpcError> {
    if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
        if matches!(
            ship_ball.mode,
            eve_destiny::MovementMode::Warp { phase, .. }
                if phase != eve_destiny::WarpPhase::Aligning
        ) {
            return Err(BridgeRpcError {
                code: "CantActivateInWarp".into(),
                message: format!("Cannot {action_label} while in warp"),
            });
        }
    }

    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.gate_cloak_remaining_ticks > 0 {
            let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(
                ctx, sys, char_id, ship_id,
            )
            .await;
        }
    }

    if is_ship_actively_cloaked(ctx, sys, ship_id).await {
        return Err(BridgeRpcError::user_error("CannotActivateCloaked"));
    }

    Ok(())
}

/// Resolves a fallback `FittedModule` from `snap.items` when `ship_id` has a raw fitted item
/// not yet hydrated in `fit.modules` (`combat_weapons`).
pub(crate) fn fallback_fitted_weapon_module(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
) -> Option<eve_dogma::FittedModule> {
    ctx.db.with_snapshot(|snap| {
        let item_rec = snap
            .items
            .get(&module_id)
            .filter(|i| i.location_id == ship_id)?;
        let charge_tid = snap
            .items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && i.flag == item_rec.flag
                    && i.item_id != module_id
                    && i.quantity > 0
            })
            .map(|i| i.type_id);
        Some(eve_dogma::FittedModule {
            item_id: item_rec.item_id,
            type_id: item_rec.type_id,
            flag_id: item_rec.flag as u16,
            state: eve_dogma::ModuleState::Online,
            charge_type_id: charge_tid,
            charge_quantity: 1,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: item_rec.dynamic_attributes.clone(),
        })
    })
}
