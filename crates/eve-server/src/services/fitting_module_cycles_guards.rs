//! Space module & action cloak, gate-cloak break, and active-warp guards (`#494`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
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

/// Returns `true` when `group_id` is a missile launcher or zero-cap launcher-branch weapon (`#1403`).
pub(crate) fn is_missile_launcher_group(group_id: u32) -> bool {
    matches!(
        group_id,
        506..=511 | 524 | 771 | 1245 | 1674 | 4500 | 4805 | 4807
    )
}

/// Returns `true` when `group_id` is a weapon module routed to weapon / combat activation paths (`#1403`).
pub(crate) fn is_weapon_group(group_id: u32) -> bool {
    matches!(group_id, 53 | 55 | 74 | 1986 | 4060) || is_missile_launcher_group(group_id)
}

fn has_active_db_cloak_module(ctx: &ServerContext, ship_id: i64) -> bool {
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

/// Returns `true` when `ship_id` has an active non-gate cloak in `sys` or `ctx.db`.
pub(crate) async fn is_ship_actively_cloaked(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
) -> bool {
    sys.is_ship_actively_cloaked(ship_id).await || has_active_db_cloak_module(ctx, ship_id)
}

/// Rejects undocked module online/offline/deactivate calls while actively module-cloaked
/// (`CannotActivateCloaked`), excluding gate cloaks (`#1351`).
pub(crate) async fn enforce_space_active_cloak_module_guard(
    ctx: &ServerContext,
    session: &SessionState,
    ship_id: i64,
) -> Result<(), BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Ok(());
    }
    let sys_opt = session
        .solar_system_id
        .and_then(|sys_id| ctx.systems.get_handle(sys_id));
    for check_ship_id in std::iter::once(ship_id).chain(
        session
            .ship_id
            .filter(|&sess_ship_id| sess_ship_id != ship_id),
    ) {
        let actively_cloaked = match sys_opt.as_ref() {
            Some(sys) => is_ship_actively_cloaked(ctx, sys, check_ship_id).await,
            None => has_active_db_cloak_module(ctx, check_ship_id),
        };
        if actively_cloaked {
            return Err(BridgeRpcError {
                code: "CannotActivateCloaked".into(),
                message: "CannotActivateCloaked".into(),
                client_error: None,
            });
        }
    }
    Ok(())
}

/// Validates active-warp (`CantActivateInWarp`) and non-gate cloak (`CannotActivateCloaked`)
/// read-only before activating a ship module in space (`#494`, `#1351`), leaving gate cloak intact
/// until downstream validation succeeds.
pub(crate) async fn check_space_module_cloak_and_warp_readonly(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
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
                    client_error: None,
                });
            }
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

fn finalize_atomic_protection_break(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    location_change_invuln: Option<crate::system_actor::LocationInvulnToken>,
    real_now_unix: u64,
    protection_events: &mut crate::system_actor::ProtectionBreakEvents,
) {
    if ship_id <= 0 {
        return;
    }
    crate::services::space_destiny::finalize_committed_location_change_invuln(
        ctx,
        char_id,
        location_change_invuln,
        real_now_unix,
        protection_events,
    );
    crate::services::space_destiny::emit_protection_break_notifications(
        ctx,
        char_id,
        ship_id,
        protection_events,
    );
}

/// Atomically validates warp/cloak/cap/tackle preconditions inside the system actor, commits `cycle`,
/// breaks ship protection if `break_protection`, and finalizes `location_change_invuln` plus
/// protection-break notifications on commit (`#1486`).
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_module_cycle_atomic_with_notifications(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    cycle: crate::system_actor::ActiveModuleCycle,
    allow_while_warping: bool,
    allow_while_cloaked: bool,
    break_protection: bool,
    break_reason: crate::system_actor::ProtectionBreakReason,
) -> Result<crate::system_actor::AtomicActivateCycleOutcome, String> {
    commit_module_cycle_atomic_with_replace_notifications(
        ctx,
        sys,
        char_id,
        ship_id,
        cycle,
        allow_while_warping,
        allow_while_cloaked,
        false,
        break_protection,
        break_reason,
    )
    .await
}

/// Variant of [`commit_module_cycle_atomic_with_notifications`] allowing atomic replacement of an
/// active cycle on `(ship_id, module_id)` inside the system actor (`#1486`).
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_module_cycle_atomic_with_replace_notifications(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    cycle: crate::system_actor::ActiveModuleCycle,
    allow_while_warping: bool,
    allow_while_cloaked: bool,
    replace_existing_cycle: bool,
    break_protection: bool,
    break_reason: crate::system_actor::ProtectionBreakReason,
) -> Result<crate::system_actor::AtomicActivateCycleOutcome, String> {
    let real_now_unix = ctx.now_unix();
    let location_change_invuln = (break_protection && ship_id > 0)
        .then(|| crate::services::space_destiny::peek_active_location_change_invuln(ctx, char_id))
        .flatten();
    let mut outcome = sys
        .activate_module_cycle_atomic(crate::system_actor::AtomicActivateCycleRequest {
            char_id,
            cycle,
            allow_while_warping,
            allow_while_cloaked,
            replace_existing_cycle,
            break_protection,
            break_reason,
            location_change_invuln,
            real_now_unix,
        })
        .await?;
    if break_protection {
        finalize_atomic_protection_break(
            ctx,
            char_id,
            ship_id,
            location_change_invuln,
            real_now_unix,
            &mut outcome.protection_events,
        );
    }
    Ok(outcome)
}

/// Atomically verifies `ship_id` is in space, no active lance cycle exists on `(ship_id, module_id)`,
/// non-warp, non-cloak, and sufficient capacitor, deducts `cap_cost`, breaks ship protection, and
/// finalizes `location_change_invuln` plus notifications on commit (`#1486`).
#[inline(never)]
pub(crate) async fn commit_lancer_activation_atomic_with_notifications(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    cap_cost: f64,
) -> Result<crate::system_actor::AtomicActivateCycleOutcome, String> {
    let real_now_unix = ctx.now_unix();
    let location_change_invuln = (ship_id > 0)
        .then(|| crate::services::space_destiny::peek_active_location_change_invuln(ctx, char_id))
        .flatten();
    let mut outcome = sys
        .commit_lancer_activation_atomic(crate::system_actor::AtomicLancerActivationRequest {
            char_id,
            ship_id,
            module_id,
            cap_cost,
            location_change_invuln,
            real_now_unix,
        })
        .await?;
    finalize_atomic_protection_break(
        ctx,
        char_id,
        ship_id,
        location_change_invuln,
        real_now_unix,
        &mut outcome.protection_events,
    );
    Ok(outcome)
}

/// Breaks any active gate cloak, undock invulnerability, and post-jump
/// `location_change_invuln` when a space action commits, emitting
/// `OnInvulnCancelled` at most once (`#494`, `#1351`, `#1403`, `#1486`).
#[inline(never)]
pub(crate) async fn break_gate_cloak_on_space_action(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
) {
    commit_space_action_cloak_and_invuln_break(ctx, sys, char_id, ship_id, 0).await;
}

/// Breaks remaining gate cloak, undock invulnerability, and `location_change_invuln`
/// on commit, ensuring `OnInvulnCancelled` is emitted at most once even when
/// `add_module_cycle` cleared `undock_invulnerable_remaining_ticks` prior to commit (`#1403`).
pub(crate) async fn commit_space_action_cloak_and_invuln_break(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    pre_undock_invuln_ticks: u32,
) {
    let events = crate::services::space_destiny::break_ship_protection_with_location_invuln(
        ctx,
        sys,
        char_id,
        ship_id,
        crate::system_actor::ProtectionBreakReason::ModuleActivation,
    )
    .await;
    if !events.invuln_cancelled && pre_undock_invuln_ticks > 0 && ship_id > 0 {
        crate::services::notify_emitters_space_dungeons::emit_warp_cloak_or_invuln_cancelled(
            ctx, char_id, ship_id, false, true,
        );
    }
}

/// Enforces active-warp (`CantActivateInWarp`), gate-cloak break (`gate_cloak_remaining_ticks > 0`),
/// and non-gate cloak (`CannotActivateCloaked`) checks before activating a ship module in space (`#494`).
/// Kept under `#[cfg(test)]` for existing guard unit tests while production callers use
/// `check_space_module_cloak_and_warp_readonly` + deferred `break_gate_cloak_on_space_action`.
#[cfg(test)]
pub(crate) async fn enforce_space_module_cloak_and_warp_guard(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
) -> Result<(), BridgeRpcError> {
    check_space_module_cloak_and_warp_readonly(ctx, sys, ship_id, mod_type).await?;
    break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
    Ok(())
}

/// Validates active-warp (`CantActivateInWarp`) and non-gate cloak (`CannotActivateCloaked`)
/// read-only for non-module space actions (`superWeaponMgr`, `#1403`), leaving gate cloak intact
/// until the action commits.
pub(crate) async fn check_space_action_cloak_and_warp_readonly(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
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
                client_error: None,
            });
        }
    }

    if is_ship_actively_cloaked(ctx, sys, ship_id).await {
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
    check_space_action_cloak_and_warp_readonly(ctx, sys, ship_id, action_label).await?;
    break_gate_cloak_on_space_action(ctx, sys, char_id, ship_id).await;
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

fn is_slot_loaded_charge_item_inner(
    ctx: &ServerContext,
    mod_type: &eve_sde::InvType,
    item: &eve_db::InventoryItemRecord,
    ship_id: i64,
    slot_flag_u16: u16,
    module_item_id: i64,
    allow_scripts: bool,
) -> bool {
    if item.location_id != ship_id
        || (item.flag as u16) != slot_flag_u16
        || item.item_id == module_item_id
        || item.quantity <= 0
    {
        return false;
    }
    let Some(ct) = ctx.sde.types.get(&item.type_id) else {
        return false;
    };
    if ct.category_id != 8 || (!allow_scripts && ct.group_id == 908) {
        return false;
    }
    let has_charge_groups = [604_u16, 605, 606, 609, 610]
        .iter()
        .any(|attr| mod_type.default_attributes.contains_key(attr));
    if has_charge_groups
        || mod_type.type_id == 28_748
        || ct.default_attributes.contains_key(&137)
    {
        return ServerContext::is_charge_compatible_with_weapon(mod_type, ct);
    }
    true
}

/// Returns `true` iff `item` is a non-script charge (`category_id == 8`, excluding scripts
/// `group_id == 908`) loaded in `(ship_id, slot_flag_u16)` for `mod_type`.
///
/// Rejects sibling modules (`category_id != 8`), excludes scripts (`group_id == 908`), and enforces
/// `ServerContext::is_charge_compatible_with_weapon` whenever `mod_type` declares `chargeGroup*`
/// or `ct` declares `launcherGroup` (`137`).
pub(crate) fn is_slot_loaded_charge_item(
    ctx: &ServerContext,
    mod_type: &eve_sde::InvType,
    item: &eve_db::InventoryItemRecord,
    ship_id: i64,
    slot_flag_u16: u16,
    module_item_id: i64,
) -> bool {
    is_slot_loaded_charge_item_inner(ctx, mod_type, item, ship_id, slot_flag_u16, module_item_id, false)
}

/// Returns `true` iff `item` is a compatible charge or script (`category_id == 8`) loaded in
/// `(ship_id, slot_flag_u16)` for `mod_type`.
pub(crate) fn is_slot_loaded_charge_or_script_item(
    ctx: &ServerContext,
    mod_type: &eve_sde::InvType,
    item: &eve_db::InventoryItemRecord,
    ship_id: i64,
    slot_flag_u16: u16,
    module_item_id: i64,
) -> bool {
    is_slot_loaded_charge_item_inner(ctx, mod_type, item, ship_id, slot_flag_u16, module_item_id, true)
}

/// Maps `SystemActor` atomic activation rejection messages to structured `BridgeRpcError`s (`#1486`).
pub(crate) fn map_atomic_cycle_activation_error(
    ctx: &ServerContext,
    module_type_id: u32,
    err: String,
) -> BridgeRpcError {
    if err.contains("ModuleAlreadyActive") {
        return super::module_already_active_rpc_error(ctx, Some(module_type_id), err);
    }
    if err.contains("not in space") {
        let custom = format!("ShipNotInSpace: {err}");
        return BridgeRpcError::with_client_error(
            "ShipNotInSpace",
            err,
            eve_proto::ClientError::CustomInfo(custom),
        );
    }
    if err.contains("CannotActivateCloaked") {
        return BridgeRpcError::with_client_error(
            "CannotActivateCloaked",
            "CannotActivateCloaked",
            eve_proto::ClientError::CustomInfo("CannotActivateCloaked: CannotActivateCloaked".into()),
        );
    }
    if err.contains("WarpScrambled") {
        return BridgeRpcError::with_client_error(
            "WarpScrambled",
            err,
            eve_proto::ClientError::user(eve_proto::UserErrorKey::WarpScrambled),
        );
    }
    if err.contains("CantActivateInWarp") {
        let custom = format!("CantActivateInWarp: {err}");
        return BridgeRpcError::with_client_error(
            "CantActivateInWarp",
            err,
            eve_proto::ClientError::CustomInfo(custom),
        );
    }
    if err.contains("capacitor") || err.contains("Capacitor") {
        let custom = format!("NotEnoughCapacitor: {err}");
        return BridgeRpcError::with_client_error(
            "NotEnoughCapacitor",
            err,
            eve_proto::ClientError::CustomInfo(custom),
        );
    }
    let custom = format!("ModuleActivationFailed: {err}");
    BridgeRpcError::with_client_error("ModuleActivationFailed", err, eve_proto::ClientError::CustomInfo(custom))
}

#[cfg(test)]
#[path = "fitting_module_cycles_guards_edges_tests.rs"]
mod fitting_module_cycles_guards_edges_tests;
