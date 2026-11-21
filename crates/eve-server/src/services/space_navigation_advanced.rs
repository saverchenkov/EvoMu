//! `space_navigation_advanced` — Space Navigation, Jump Drives, and Cloak Mechanics helpers.
//!
//! Provides validation and calculation routines for:
//! - Bastion / Siege / Industrial Core gate and docking blocks (`feat:navigation.bastion-siege-style-gate-blocks`)
//! - Cloak targeting delay & Stealth Bomber / Covert Ops / T3C / Black Ops modifiers (`feat:navigation.cloak-targeting-delay`)
//! - Supercapital NPC station docking refusal (`feat:navigation.npc-station-supercap-docking`)

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct NavDogmaAttrIds {
    disallow_docking: Option<u16>,
    disallow_tethering: Option<u16>,
    siege_warp_status: Option<u16>,
    disallow_drive_jump: Option<u16>,
    cloaking_targeting_delay: Option<u16>,
    covert_ops_stealth_bomber_delay: Option<u16>,
    max_velocity_modifier: Option<u16>,
}

fn nav_attr_ids(sde: &eve_sde::SdeRegistry) -> NavDogmaAttrIds {
    NavDogmaAttrIds {
        disallow_docking: find_attr_id_by_name(sde, "disallowDocking"),
        disallow_tethering: find_attr_id_by_name(sde, "disallowTethering"),
        siege_warp_status: find_attr_id_by_name(sde, "siegeModeWarpStatus"),
        disallow_drive_jump: find_attr_id_by_name(sde, "disallowDriveJumping"),
        cloaking_targeting_delay: find_attr_id_by_name(sde, "cloakingTargetingDelay")
            .or_else(|| find_attr_id_by_name(sde, "decloakTargetingDelay")),
        covert_ops_stealth_bomber_delay: find_attr_id_by_name(
            sde,
            "covertOpsStealthBomberTargettingDelay",
        ),
        max_velocity_modifier: find_attr_id_by_name(sde, "maxVelocityModifier")
            .or_else(|| find_attr_id_by_name(sde, "maxVelocityCloaked")),
    }
}

/// Cached lookup for cloak velocity multiplier attribute ID.
pub fn max_velocity_cloak_attr_id(sde: &eve_sde::SdeRegistry) -> Option<u16> {
    nav_attr_ids(sde).max_velocity_modifier
}

/// Deterministic lookup of a single SDE dogma attribute ID by exact attribute name.
pub fn find_attr_id_by_name(sde: &eve_sde::SdeRegistry, name: &str) -> Option<u16> {
    sde.attributes
        .values()
        .find(|a| a.name == name)
        .map(|a| a.attribute_id)
}

/// Checks if an immobilizing module (Bastion, Siege, Industrial Core, Triage) is actively cycling
/// on the ship, blocking docking, tethering, stargate/Ansiblex/jump-drive jumps, and warping
/// (`feat:navigation.bastion-siege-style-gate-blocks`).
///
/// Returns the live V24.01 `UserError` keys defined in `res:/staticdata/dialogs.static`:
/// - `"dock"` -> `"CantDockWhileModuleActive"` (`dialog_id = 2094`, `dialog_type = 3`)
/// - `"jump"` -> `"CantJumpWhileModuleActive"` (`dialog_id = 2095`, `dialog_type = 3`)
/// - `"warp"` -> `"WarpDisrupted"` (`dialog_id = 1680`, `dialog_type = 3`)
/// - `"tether"` -> `"DeniedTethered"` (`dialog_id = 4899`, `dialog_type = 3`)
pub fn check_immobilizing_module_active(
    ctx: &ServerContext,
    ship_id: i64,
    action: &str,
) -> Result<(), BridgeRpcError> {
    let ids = nav_attr_ids(&ctx.sde);

    let immobilizing_mod_type_id = ctx.db.with_snapshot(|snap| {
        snap.items.values().find_map(|it| {
            let is_match = it.location_id == ship_id
                && matches!(it.flag as u16, 11..=34 | 125..=132)
                && snap.module_active_states.contains(&it.item_id)
                && ctx
                    .sde
                    .types
                    .get(&it.type_id)
                    .map(|t| {
                        let has_attr = |aid_opt: Option<u16>| {
                            aid_opt
                                .and_then(|aid| t.default_attributes.get(&aid).copied())
                                .unwrap_or(0.0)
                                != 0.0
                        };
                        let is_siege_immobilized = has_attr(ids.siege_warp_status);
                        match action {
                            "dock" => is_siege_immobilized || has_attr(ids.disallow_docking),
                            "tether" => is_siege_immobilized || has_attr(ids.disallow_tethering),
                            "warp" => is_siege_immobilized,
                            "jump" => {
                                is_siege_immobilized
                                    || has_attr(ids.disallow_docking)
                                    || has_attr(ids.disallow_drive_jump)
                            }
                            _ => is_siege_immobilized,
                        }
                    })
                    .unwrap_or(false);
            is_match.then_some(it.type_id as i64)
        })
    });

    if let Some(mod_type_id) = immobilizing_mod_type_id {
        let (code, client_err) = match action {
            "dock" => (
                eve_proto::UserErrorKey::CantDockWhileModuleActive.as_str(),
                eve_proto::ClientError::cant_dock_while_module_active(mod_type_id),
            ),
            "jump" => (
                eve_proto::UserErrorKey::CantJumpWhileModuleActive.as_str(),
                eve_proto::ClientError::cant_jump_while_module_active(mod_type_id),
            ),
            "warp" => (
                eve_proto::UserErrorKey::WarpDisrupted.as_str(),
                eve_proto::ClientError::user(eve_proto::UserErrorKey::WarpDisrupted),
            ),
            "tether" => (
                eve_proto::UserErrorKey::DeniedTethered.as_str(),
                eve_proto::ClientError::user(eve_proto::UserErrorKey::DeniedTethered),
            ),
            _ => (
                eve_proto::UserErrorKey::CantJumpWhileModuleActive.as_str(),
                eve_proto::ClientError::cant_jump_while_module_active(mod_type_id),
            ),
        };
        return Err(BridgeRpcError::with_client_error(
            code,
            format!("Cannot perform {action} while an immobilizing module (Bastion/Siege/Core) is active"),
            client_err,
        ));
    }
    Ok(())
}

/// Cloak deactivation sensor recalibration targeting delay calculation
/// (`feat:navigation.cloak-targeting-delay`).
/// Evaluates ship decloak-delay role bonus attributes (including non-zero overrides) and Cloaking skill reduction.
pub fn compute_decloak_targeting_delay_s(
    sde: &eve_sde::SdeRegistry,
    ship_type_id: u32,
    cloaking_skill_level: u8,
    base_delay_s: f64,
) -> f64 {
    let ids = nav_attr_ids(sde);
    let hull = sde.types.get(&ship_type_id);
    if let Some(aid) = ids.covert_ops_stealth_bomber_delay {
        if let Some(override_ms) = hull.and_then(|t| t.default_attributes.get(&aid)).copied() {
            return (override_ms / 1000.0).max(0.0);
        }
    }
    let skill_reduction = 0.10 * (cloaking_skill_level.min(5) as f64);
    (base_delay_s * (1.0 - skill_reduction)).max(0.0)
}

/// Evaluates module `cloakingTargetingDelay` (`560`) and hull/subsystem modifiers
/// (`covertOpsStealthBomberTargettingDelay` `1035` including non-zero Black Ops / Expedition Frigate overrides,
/// T3 Strategic Cruiser Covert Reconfiguration subsystem modifiers, Covert Ops frigate role bonuses,
/// and `Cloaking` skill `11579`) through `ShipFittingEngine`.
pub fn compute_decloak_targeting_delay_for_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    module_item_id: Option<i64>,
    module_type_id: u32,
    cloaking_skill_level: u8,
) -> f64 {
    let ids = nav_attr_ids(&ctx.sde);
    let hull = ctx.sde.types.get(&ship_type_id);
    let mod_type = ctx.sde.types.get(&module_type_id);

    let is_covert_cloak = mod_type
        .map(|t| {
            t.effect_ids.iter().any(|&eid| {
                ctx.sde
                    .effects
                    .get(&eid)
                    .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
            })
        })
        .unwrap_or(false);

    // Covert Ops frigate role bonus effect (`covertOpsCloakCpuPercentBonus1` in SDE) with covert cloak
    let has_covops_cloak_role_effect = hull.is_some_and(|t| {
        t.effect_ids.iter().any(|&eid| {
            ctx.sde
                .effects
                .get(&eid)
                .is_some_and(|e| e.name == "covertOpsCloakCpuPercentBonus1")
        })
    });
    if is_covert_cloak && has_covops_cloak_role_effect {
        return 0.0;
    }

    // Evaluate live ShipFittingEngine (including T3C subsystems, dynamic ship/module overrides,
    // Cloaking skill PostPercent, and hull/subsystem PostAssignment on 560 via 1035).
    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let eval_ctx = fit.evaluated_context(&ctx.sde);

        // Check if ship or online T3C subsystem explicitly sets `covertOpsStealthBomberTargettingDelay` (`1035`)
        // via dynamic ship attribute, hull default attribute, or online subsystem attribute.
        if let Some(sb_aid) = ids.covert_ops_stealth_bomber_delay {
            let ship_has_explicit_1035 = fit.ship.base_attributes.contains_key(&sb_aid)
                || ctx
                    .sde
                    .types
                    .get(&eval_ctx.ship.type_id)
                    .is_some_and(|t| t.default_attributes.contains_key(&sb_aid))
                || fit.modules.values().any(|m| {
                    (125..=132).contains(&m.flag_id)
                        && m.state != eve_dogma::ModuleState::Offline
                        && (m.dynamic_attribute_overrides.contains_key(&sb_aid)
                            || ctx
                                .sde
                                .types
                                .get(&m.type_id)
                                .is_some_and(|t| t.default_attributes.contains_key(&sb_aid)))
                });
            if ship_has_explicit_1035 {
                let evaluated_override_ms = if fit.ship.base_attributes.contains_key(&sb_aid)
                    || ctx
                        .sde
                        .types
                        .get(&eval_ctx.ship.type_id)
                        .is_some_and(|t| t.default_attributes.contains_key(&sb_aid))
                {
                    eval_ctx.ship.get_attribute(sb_aid, &ctx.sde)
                } else {
                    eval_ctx
                        .ship_located_items
                        .iter()
                        .find(|it| {
                            fit.modules.get(&it.item_id).is_some_and(|m| {
                                (125..=132).contains(&m.flag_id)
                                    && m.state != eve_dogma::ModuleState::Offline
                                    && (m.dynamic_attribute_overrides.contains_key(&sb_aid)
                                        || ctx.sde.types.get(&m.type_id).is_some_and(|t| {
                                            t.default_attributes.contains_key(&sb_aid)
                                        }))
                            })
                        })
                        .map(|it| it.get_attribute(sb_aid, &ctx.sde))
                        .unwrap_or_else(|| eval_ctx.ship.get_attribute(sb_aid, &ctx.sde))
                };
                return (evaluated_override_ms / 1000.0).max(0.0);
            }
        }

        // Check T3 Strategic Cruiser online Covert Reconfiguration subsystem (`covertOpsCpuBonus1` + `subsystemBonusBlackOpsJumpPassenger`)
        let has_t3c_covert_subsystem = fit.modules.values().any(|m| {
            (125..=132).contains(&m.flag_id)
                && m.state != eve_dogma::ModuleState::Offline
                && ctx.sde.types.get(&m.type_id).is_some_and(|t| {
                    let has_cov_cpu = t.effect_ids.iter().any(|&eid| {
                        ctx.sde
                            .effects
                            .get(&eid)
                            .is_some_and(|e| e.name == "covertOpsCpuBonus1")
                    });
                    let has_blops_pass = t.effect_ids.iter().any(|&eid| {
                        ctx.sde
                            .effects
                            .get(&eid)
                            .is_some_and(|e| e.name == "subsystemBonusBlackOpsJumpPassenger")
                    });
                    has_cov_cpu && has_blops_pass
                })
        });

        if let Some(delay_aid) = ids.cloaking_targeting_delay {
            let evaluated_mod = module_item_id
                .and_then(|mid| {
                    eval_ctx
                        .ship_located_items
                        .iter()
                        .find(|m| m.item_id == mid)
                })
                .or_else(|| {
                    eval_ctx
                        .ship_located_items
                        .iter()
                        .find(|m| m.type_id == module_type_id)
                });
            if let Some(m_state) = evaluated_mod {
                let eval_delay_ms = m_state.get_attribute(delay_aid, &ctx.sde);
                // On a T3 Strategic Cruiser with Covert Reconfiguration subsystem, Covert Ops Cloaking Device II
                // evaluates its 10s base delay through Cloaking skill (`5.0s` at Cloaking V), and if a non-covert
                // cloak is fitted it evaluates its `560` attribute through `ShipFittingEngine` as well.
                let _ = has_t3c_covert_subsystem;
                return (eval_delay_ms / 1000.0).max(0.0);
            }
        }
    }

    let raw_delay_ms = ids
        .cloaking_targeting_delay
        .and_then(|aid| mod_type.and_then(|t| t.default_attributes.get(&aid)).copied())
        .unwrap_or(30000.0);
    let base_delay_s = raw_delay_ms / 1000.0;
    compute_decloak_targeting_delay_s(&ctx.sde, ship_type_id, cloaking_skill_level, base_delay_s)
}

/// Supercapital NPC station docking refusal (`feat:navigation.npc-station-supercap-docking`).
/// Returns the live V24.01 `UserError` key `"DockingRequestDenied"` (`res:/staticdata/dialogs.static` `dialog_id = 808`).
pub fn validate_npc_station_docking(
    ship_group_id: u32,
    is_npc_station: bool,
) -> Result<(), BridgeRpcError> {
    // Supercarriers (659) and Titans (30) cannot dock at NPC stations
    if is_npc_station && matches!(ship_group_id, 30 | 659) {
        return Err(BridgeRpcError::docking_request_denied(
            eve_proto::UI_LABEL_SHIP_TOO_LARGE,
            "Supercarriers and Titans cannot dock at NPC stations",
        ));
    }
    Ok(())
}

pub fn deactivate_ship_modules_on_leave_space(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship(ctx, ship_id);
    for (_, handle) in ctx.systems.active_system_handles() {
        handle.cancel_ship_nanite_repairs_sync(ship_id, true);
    }
    let (solar_system_id, active_mods, cleared_overloads): (Option<u32>, Vec<i64>, bool) =
        ctx.db.with_mut_snapshot(|snap| {
            let sys_id = snap.characters.get(&char_id).map(|c| c.solar_system_id);
            let mods: Vec<i64> = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == ship_id && snap.module_active_states.contains(&it.item_id)
                })
                .map(|it| it.item_id)
                .collect();
            for mid in &mods {
                snap.module_active_states.remove(mid);
                snap.module_active_charges.remove(mid);
            }
            let had_overloads =
                crate::router::ServerContext::clear_all_persisted_ship_overloads(snap, ship_id);
            (sys_id, mods, had_overloads)
        });
    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        ctx,
        char_id,
        solar_system_id,
        ship_id,
    );
    if !active_mods.is_empty() || cleared_overloads {
        ctx.invalidate_ship_fitting(ship_id);
    }
    if !active_mods.is_empty() {
        for mid in active_mods {
            let is_phenomena = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&mid)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .is_some_and(|t| t.group_id == 1815)
            });
            if is_phenomena {
                let cleared =
                    crate::services::inventory_fitting::clear_ship_dbuffs_by_source_module(ctx, mid);
                if !cleared.is_empty() {
                    crate::services::inventory_fitting::sync_ship_dbuffs_and_destiny_sync(
                        ctx,
                        &cleared,
                        ctx.now_unix_secs(),
                    );
                }
            }
            let _ = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
                ctx,
                char_id,
                solar_system_id,
                ship_id,
                mid,
            );
            crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, mid);
            ctx.notify_godma_ship_effect(
                crate::router::NotificationTarget::Character(char_id),
                mid,
                10,
                false,
                0,
                0,
            );
        }
    }
}

fn is_zarzakh_toll_exempt_pirate(ctx: &ServerContext, char_id: i64) -> bool {
    let char_war_faction = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id);
        s.fw_runtime
            .direct_enlisted_chars
            .get(&char_id)
            .copied()
            .or_else(|| ch.and_then(|c| c.war_faction_id))
            .or_else(|| {
                ch.and_then(|c| {
                    s.fw_runtime
                        .corp_war_factions
                        .get(&c.corporation_id)
                        .copied()
                })
            })
    });
    matches!(char_war_faction, Some(500_010 | 500_011))
}

fn zarzakh_gate_toll_declined(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> bool {
    kwargs.get("confirmToll") == Some(&PyRep::Bool(false))
        || kwargs.get("confirm_toll") == Some(&PyRep::Bool(false))
        || kwargs.get("declineToll") == Some(&PyRep::Bool(true))
        || kwargs.get("decline_toll") == Some(&PyRep::Bool(true))
        || args.get(1) == Some(&PyRep::Bool(false))
        || args.get(3) == Some(&PyRep::Bool(false))
}

fn compute_zarzakh_gate_toll(
    ctx: &ServerContext,
    ship_id: i64,
    current_ball: &eve_destiny::Ball,
) -> (f64, i64, eve_db::IskCents) {
    // DERIVED: Zarzakh stargate transit toll scales with effective ship jump mass (`auditor_rows.json:6954-6960`, `Zarzakh.md:30-31`):
    // small hulls (<= 1,500,000 kg: capsules, shuttles, corvettes, frigates) pay the 10,000 ISK floor, scaling linearly up to
    // the 100,000,000 ISK cap at 150,000,000 kg (Orca `type_id 28606`, the heaviest Zarzakh-eligible SDE hull).
    let mass = crate::services::wormhole_lifecycle_ops::compute_effective_ship_jump_mass(
        ctx,
        ship_id,
        current_ball.mass,
    )
    .0
    .max(1_000.0);
    let toll = if mass <= 1_500_000.0 {
        10_000
    } else {
        ((mass / 150_000_000.0) * 100_000_000.0)
            .clamp(10_000.0, 100_000_000.0)
            .round() as i64
    };
    let toll_cents = eve_db::IskCents::from_isk(toll);
    (mass, toll, toll_cents)
}

fn zarzakh_gate_toll_explicit_confirm(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> bool {
    let is_three_arg_v24_call = args.len() >= 3 && args.get(1).and_then(|v| v.as_i64()).is_some();
    kwargs.get("confirmToll") == Some(&PyRep::Bool(true))
        || kwargs.get("confirm_toll") == Some(&PyRep::Bool(true))
        || args.get(1) == Some(&PyRep::Bool(true))
        || args.get(3) == Some(&PyRep::Bool(true))
        || (!is_three_arg_v24_call
            && kwargs.get("requireConfirm") != Some(&PyRep::Bool(true))
            && kwargs.get("promptToll") != Some(&PyRep::Bool(true)))
}

fn ask_confirm_zarzakh_gate_toll_error(ship_id: i64, toll: i64, mass: f64) -> BridgeRpcError {
    let message = format!(
        "Confirm Zarzakh stargate transit toll of {toll} ISK for ship {ship_id} (effective mass {mass:.0} kg)"
    );
    BridgeRpcError::with_client_error(
        "AskConfirmZarzakhGateToll",
        message.clone(),
        eve_proto::ClientError::CustomInfo(message),
    )
}

fn zarzakh_toll_declined_error() -> BridgeRpcError {
    let message = "Zarzakh stargate transit toll was declined";
    BridgeRpcError::with_client_error(
        "ZarzakhGateTollDeclined",
        message,
        eve_proto::ClientError::CustomInfo(message.into()),
    )
}

fn zarzakh_toll_character_not_found_error(char_id: i64) -> BridgeRpcError {
    let message = format!("Character {char_id} not found for Zarzakh stargate transit toll");
    BridgeRpcError::with_client_error(
        "CharacterNotFound",
        message.clone(),
        eve_proto::ClientError::CustomInfo(message),
    )
}

fn zarzakh_toll_insufficient_funds_error(toll_cents: eve_db::IskCents) -> BridgeRpcError {
    BridgeRpcError::with_client_error(
        "InsufficientFundsForZarzakhGateToll",
        format!(
            "Insufficient ISK for Zarzakh stargate transit toll ({:.2} ISK required)",
            toll_cents.to_wire_f64()
        ),
        eve_proto::ClientError::user(eve_proto::UserErrorKey::NotEnoughMoney),
    )
}

/// Read-only pre-validation of Zarzakh entry gate toll before lazy stargate activation or ship
/// ball removal (`#1429`).
///
/// Mirrors [`process_zarzakh_gate_toll_with_amount`]'s exact pirate exemption, decline check
/// (`ZarzakhGateTollDeclined`), V24 3-arg quote/confirmation challenge (`AskConfirmZarzakhGateToll`),
/// and wallet balance sufficiency check (`InsufficientFundsForZarzakhGateToll`) **without** debiting
/// ISK or mutating `EveDb` / `state_epoch`.
///
/// On decline, quote prompt, or insufficient funds, updates only the in-memory `zarzakh_toll_quotes`
/// map (without calling `persist_misc_state`) so two-step V24 confirmation flows remain intact; on
/// success, keeps any matching pending quote in place for the post-`take_ball` debit call
/// (`process_zarzakh_gate_toll_with_amount`) to consume.
#[allow(clippy::too_many_arguments)]
pub fn prevalidate_zarzakh_gate_toll(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    from_gate_id: i64,
    to_gate_id: i64,
    current_ball: &eve_destiny::Ball,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<Option<eve_db::IskCents>, BridgeRpcError> {
    if is_zarzakh_toll_exempt_pirate(ctx, char_id) {
        return Ok(None);
    }

    if zarzakh_gate_toll_declined(args, kwargs) {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        return Err(zarzakh_toll_declined_error());
    }

    let (mass, toll, toll_cents) = compute_zarzakh_gate_toll(ctx, ship_id, current_ball);
    let now = ctx.now_unix();
    let explicit_confirm = zarzakh_gate_toll_explicit_confirm(args, kwargs);

    let confirmed_quote = crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
        if let Some(q) = st.zarzakh_toll_quotes.get(&char_id) {
            if q.from_gate_id == from_gate_id && q.ship_id == ship_id && now <= q.expires_unix {
                return Some(q.quoted_toll_cents);
            }
        }
        if explicit_confirm {
            return Some(toll_cents);
        }
        st.zarzakh_toll_quotes.insert(
            char_id,
            crate::services::cluster_macho_misc_services::PendingZarzakhTollQuote {
                from_gate_id,
                to_gate_id,
                ship_id,
                quoted_toll_cents: toll_cents,
                expires_unix: now + 60,
            },
        );
        None
    });

    let Some(effective_toll_cents) = confirmed_quote else {
        return Err(ask_confirm_zarzakh_gate_toll_error(ship_id, toll, mass));
    };

    let balance_check = ctx.db.with_snapshot(|s| {
        let c = s
            .characters
            .get(&char_id)
            .ok_or_else(|| zarzakh_toll_character_not_found_error(char_id))?;
        if c.balance < effective_toll_cents
            || c.balance.checked_sub(effective_toll_cents).is_none()
        {
            return Err(zarzakh_toll_insufficient_funds_error(effective_toll_cents));
        }
        Ok(())
    });
    if let Err(err) = balance_check {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        return Err(err);
    }

    Ok((effective_toll_cents.0 > 0).then_some(effective_toll_cents))
}

/// Validates and debits any Zarzakh gate toll, returning the debited ISK cents (`None` for pirate free pass).
#[allow(clippy::too_many_arguments)]
pub fn process_zarzakh_gate_toll_with_amount(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    from_gate_id: i64,
    to_gate_id: i64,
    current_ball: &eve_destiny::Ball,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<Option<eve_db::IskCents>, BridgeRpcError> {
    if is_zarzakh_toll_exempt_pirate(ctx, char_id) {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        return Ok(None);
    }

    if zarzakh_gate_toll_declined(args, kwargs) {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        crate::services::cluster_macho_misc_services::persist_misc_state(ctx);
        return Err(zarzakh_toll_declined_error());
    }

    let (mass, toll, toll_cents) = compute_zarzakh_gate_toll(ctx, ship_id, current_ball);
    let now = ctx.now_unix();
    let explicit_confirm = zarzakh_gate_toll_explicit_confirm(args, kwargs);

    let confirmed_quote = crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
        if let Some(q) = st.zarzakh_toll_quotes.get(&char_id).cloned() {
            if q.from_gate_id == from_gate_id && q.ship_id == ship_id && now <= q.expires_unix {
                st.zarzakh_toll_quotes.remove(&char_id);
                return Some(q.quoted_toll_cents);
            }
        }
        if explicit_confirm {
            st.zarzakh_toll_quotes.remove(&char_id);
            return Some(toll_cents);
        }
        st.zarzakh_toll_quotes.insert(
            char_id,
            crate::services::cluster_macho_misc_services::PendingZarzakhTollQuote {
                from_gate_id,
                to_gate_id,
                ship_id,
                quoted_toll_cents: toll_cents,
                expires_unix: now + 60,
            },
        );
        None
    });
    crate::services::cluster_macho_misc_services::persist_misc_state(ctx);

    let Some(effective_toll_cents) = confirmed_quote else {
        return Err(ask_confirm_zarzakh_gate_toll_error(ship_id, toll, mass));
    };

    let bal_after = ctx.db.try_with_mut_snapshot(|s| {
        let c = s
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| zarzakh_toll_character_not_found_error(char_id))?;
        if c.balance < effective_toll_cents {
            return Err(zarzakh_toll_insufficient_funds_error(effective_toll_cents));
        }
        c.balance = c
            .balance
            .checked_sub(effective_toll_cents)
            .ok_or_else(|| zarzakh_toll_insufficient_funds_error(effective_toll_cents))?;
        let bal_after = c.balance;
        s.next_entity_id += 1;
        let j_id = s.next_entity_id;
        s.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: j_id,
            character_id: char_id,
            entry_type: "zarzakh_gate_toll".into(),
            amount: effective_toll_cents,
            balance_after: bal_after,
            reason: format!("Zarzakh stargate transit toll for ship mass {mass:.0} kg"),
        });
        Ok::<eve_db::IskCents, BridgeRpcError>(bal_after)
    })?;
    let _ = bal_after;
    Ok((effective_toll_cents.0 > 0).then_some(effective_toll_cents))
}

pub fn process_zarzakh_gate_toll(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    from_gate_id: i64,
    to_gate_id: i64,
    current_ball: &eve_destiny::Ball,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(), BridgeRpcError> {
    process_zarzakh_gate_toll_with_amount(
        ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        current_ball,
        args,
        kwargs,
    )
    .map(|_| ())
}

fn zarzakh_safe_zones(
    ballpark: &eve_destiny::Ballpark,
    sde: Option<&eve_sde::SdeRegistry>,
    cfg: &eve_sde::ZarzakhDeathzoneConfig,
) -> Vec<(glam::DVec3, f64)> {
    let mut zones = Vec::new();
    let default_r = cfg.default_safe_zone_radius_m;

    // 1. Explicit `deathzoneImmunityGenerator` beacons (78485: 150km, 78486: 250km, 78487: 125km)
    // and any SDE type carrying the `deathzoneImmunityGenerator` component.
    for b in ballpark.balls.values() {
        if let Some(&r) = cfg.immunity_ranges_m.get(&b.type_id) {
            zones.push((b.position, r));
        } else if sde
            .and_then(|s| s.space_components_by_type.get(&b.type_id))
            .is_some_and(|c| c.iter().any(|name| name == "deathzoneImmunityGenerator"))
        {
            zones.push((b.position, default_r));
        }
    }

    // 2. Canonical Zarzakh SDE safe-zone anchors (stargates in 30_100_000, The Fulcrum 60_015_187).
    // NOTE: `spacecomponentsbytype.fsdbinary` provides exact immunity radii (`78485..=78487`), while
    // `solarsystemcontent` omits beacon scene coordinates; anchor 150 km (`78485`) on gates/Fulcrum/Shipcaster
    // unless an explicit immunity beacon ball supplies another radius at that site.
    if let Some(sde) = sde {
        for sg in sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == 30_100_000)
        {
            zones.push((sg.position, default_r));
        }
        if let Some(st) = sde.stations.get(&60_015_187) {
            zones.push((st.position, default_r));
        }
    }

    // 3. In-ballpark safe-zone anchors resolved strictly from SDE IDs, `space_components_by_type`
    // (`shipcasterLauncher`), and entity group IDs (10 = Stargate, 15 = Station, 4549 = Interstellar
    // Shipcaster) without matching English display-name substrings.
    for b in ballpark.balls.values() {
        let has_shipcaster_component = sde
            .and_then(|s| s.space_components_by_type.get(&b.type_id))
            .is_some_and(|c| c.iter().any(|name| name == "shipcasterLauncher"));
        let group_id = sde
            .and_then(|s| s.types.get(&b.type_id).map(|t| t.group_id))
            .or_else(|| ballpark.ball_slim_meta.get(&b.id).map(|m| m.group_id))
            .unwrap_or(0);
        if b.id == 60_015_187
            || matches!(b.type_id, 78_334 | 79_018)
            || has_shipcaster_component
            || matches!(group_id, 10 | 15 | 4549)
        {
            zones.push((b.position, default_r));
        }
    }
    zones
}

fn is_zarzakh_hazard_eligible(
    ball: &eve_destiny::Ball,
    ballpark: &eve_destiny::Ballpark,
    sde: Option<&eve_sde::SdeRegistry>,
) -> bool {
    if let Some(sde) = sde {
        let Some(ty) = sde.types.get(&ball.type_id) else {
            return false;
        };
        if let Some(list) = sde.type_lists.get(&573) {
            return list.contains(ty);
        }
        return matches!(ty.category_id, 6 | 18) || ty.group_id == 12;
    }
    ballpark
        .ball_slim_meta
        .get(&ball.id)
        .is_some_and(|m| matches!(m.category_id, 6 | 18) || m.group_id == 12)
}

#[inline]
fn is_ball_committed_warp(ball: &eve_destiny::Ball) -> bool {
    matches!(
        ball.mode,
        eve_destiny::MovementMode::Warp {
            phase: eve_destiny::WarpPhase::Accelerating
                | eve_destiny::WarpPhase::Cruising
                | eve_destiny::WarpPhase::Decelerating,
            ..
        }
    )
}

#[inline]
fn is_ball_hazard_invulnerable(ballpark: &eve_destiny::Ballpark, ball_id: i64) -> bool {
    ballpark.combat_states.get(&ball_id).is_some_and(|cs| {
        cs.is_tethered
            || cs.gate_cloak_remaining_ticks > 0
            || cs.undock_invulnerable_remaining_ticks > 0
            || cs.warp_exit_invulnerable_remaining_ticks > 0
    })
}

/// Zarzakh Jovian gravity hazard (`feat:navigation.gravity-hazard-safe-zones`):
/// Eligible entities (`sde_type_lists[573]`: Ships `6`, Drones `18`, Cargo Containers `group 12`) in
/// Zarzakh (`30_100_000`) outside `deathzoneImmunityGenerator` safe zones receive a `10s` safety
/// grace period (`safetyDuration`) with warning updates at `warningDuration` (`5s` remaining),
/// followed by `2s` (`damageTickPeriod`) ramping hull damage ticks of `+0.5%` `max_structure_hp`
/// per tick (`hullDamageFractionIncreasePerTick = 0.005`, capped at `5.0`). Ships in committed warp
/// (`Accelerating | Cruising | Decelerating`) and invulnerable ships (`is_tethered`, gate cloak,
/// undock/warp-exit invuln) are paused/exempt; entering a safe zone resets the timer.
pub fn step_zarzakh_gravity_hazard_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    sde: Option<&eve_sde::SdeRegistry>,
    sys_id: u32,
) -> Vec<i64> {
    step_zarzakh_gravity_hazard_with_notifications(ballpark, sde, None, sys_id, None, None)
}

pub(crate) fn step_zarzakh_gravity_hazard_with_notifications(
    ballpark: &mut eve_destiny::Ballpark,
    sde: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    sys_id: u32,
    pending_notifications_opt: Option<
        &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>>,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
) -> Vec<i64> {
    if sys_id != 30_100_000 {
        return Vec::new();
    }
    let _tick_scope = crate::router::delta_item_compare::enter_compare_tick_sync_scope(db_opt.map(|d| &**d));
    let cfg = sde
        .map(eve_sde::SdeRegistry::zarzakh_deathzone_config)
        .unwrap_or_default();
    let safe_zones = zarzakh_safe_zones(ballpark, sde, &cfg);
    let now_tick = ballpark.current_tick;
    let safety_secs = u64::from(cfg.damage_generator.safety_duration_secs);
    let warn_secs = u64::from(cfg.damage_generator.warning_duration_secs);
    let period_secs = u64::from(cfg.damage_generator.damage_tick_period_secs.max(1));
    let inc_frac = cfg.damage_generator.hull_damage_fraction_increase_per_tick;
    let peak_frac = cfg.damage_generator.peak_hull_damage_fraction_per_tick;

    let eligible_ids: Vec<i64> = ballpark
        .balls
        .values()
        .filter(|b| is_zarzakh_hazard_eligible(b, ballpark, sde))
        .map(|b| b.id)
        .collect();
    let eligible_set: std::collections::HashSet<i64> = eligible_ids.iter().copied().collect();
    ballpark
        .zarzakh_tracking
        .retain(|bid, _| eligible_set.contains(bid));

    let mut pending_damage: Vec<(i64, f64)> = Vec::new();
    // (ball_id, ui_state: 0 = safe/clear, 1 = warning, 2 = danger, remaining_to_dmg_secs)
    let mut state_transitions: Vec<(i64, u8, u64)> = Vec::new();

    for &bid in &eligible_ids {
        let Some(ball_snap) = ballpark.balls.get(&bid).cloned() else {
            continue;
        };
        let in_safe_zone = safe_zones
            .iter()
            .any(|&(center, radius)| ball_snap.position.distance(center) <= radius);
        if in_safe_zone {
            if let Some(prev) = ballpark.zarzakh_tracking.remove(&bid) {
                if prev.ui_state != 0 {
                    state_transitions.push((bid, 0, 0));
                }
            }
            continue;
        }

        let in_committed_warp = is_ball_committed_warp(&ball_snap);
        if in_committed_warp || is_ball_hazard_invulnerable(ballpark, bid) {
            if let Some(entry) = ballpark.zarzakh_tracking.get_mut(&bid) {
                if now_tick < entry.last_seen_tick {
                    entry.next_damage_tick =
                        now_tick.saturating_add(safety_secs).saturating_add(period_secs);
                    entry.ticks_applied = 0;
                    if entry.ui_state != 0 {
                        entry.ui_state = 0;
                        state_transitions.push((bid, 0, 0));
                    }
                } else {
                    let paused_ticks = now_tick.saturating_sub(entry.last_seen_tick);
                    entry.next_damage_tick = entry.next_damage_tick.saturating_add(paused_ticks);
                    if in_committed_warp && entry.ui_state != 0 {
                        entry.ui_state = 0;
                        state_transitions.push((bid, 0, 0));
                    }
                }
                entry.last_seen_tick = now_tick;
            }
            continue;
        }

        let entry = ballpark.zarzakh_tracking.entry(bid).or_insert(
            eve_destiny::ZarzakhExposureState {
                next_damage_tick: now_tick.saturating_add(safety_secs).saturating_add(period_secs),
                ticks_applied: 0,
                last_seen_tick: now_tick,
                ui_state: 0,
            },
        );
        if now_tick < entry.last_seen_tick {
            *entry = eve_destiny::ZarzakhExposureState {
                next_damage_tick: now_tick.saturating_add(safety_secs).saturating_add(period_secs),
                ticks_applied: 0,
                last_seen_tick: now_tick,
                ui_state: 0,
            };
        }
        entry.last_seen_tick = now_tick;

        let Some(cs) = ballpark.combat_states.get(&bid) else {
            continue;
        };
        let max_hull = cs.max_structure_hp.max(cs.structure_hp);
        if max_hull <= 0.0 {
            continue;
        }

        let mut total_frac = 0.0;
        while entry.next_damage_tick <= now_tick {
            entry.ticks_applied = entry.ticks_applied.saturating_add(1);
            let tick_frac = (f64::from(entry.ticks_applied) * inc_frac).min(peak_frac);
            total_frac += tick_frac;
            entry.next_damage_tick = entry.next_damage_tick.saturating_add(period_secs);
        }
        let remaining_to_dmg = entry.next_damage_tick.saturating_sub(now_tick);
        let desired_ui_state = if entry.ticks_applied > 0 {
            2
        } else if warn_secs > 0 && remaining_to_dmg <= warn_secs.saturating_add(period_secs) {
            1
        } else {
            0
        };
        if desired_ui_state != entry.ui_state {
            entry.ui_state = desired_ui_state;
            state_transitions.push((bid, desired_ui_state, remaining_to_dmg));
        }
        if total_frac > 0.0 {
            pending_damage.push((bid, total_frac * max_hull));
        }
    }

    let mut destroyed = Vec::new();
    for (bid, raw_hull_dmg) in pending_damage {
        if let Some((_, is_destroyed, _)) =
            ballpark.apply_direct_structure_damage_to_ball(bid, raw_hull_dmg)
        {
            if is_destroyed {
                ballpark.zarzakh_tracking.remove(&bid);
                destroyed.push(bid);
            }
        }
    }

    if (pending_notifications_opt.is_some() || notify_tx_opt.is_some())
        && !state_transitions.is_empty()
    {
        let destroyed_set: std::collections::HashSet<i64> = destroyed.iter().copied().collect();
        let now_ft = db_opt
            .map(|db| db.clock().now_blueos_filetime_ticks())
            .unwrap_or_else(|| {
                eve_destiny::unix_to_filetime(if ballpark.sim_unix_secs > 0 {
                    ballpark.sim_unix_secs
                } else {
                    eve_destiny::current_system_unix_secs()
                })
            });
        let ramp_ticks = (peak_frac / inc_frac.max(1e-6)).ceil() as i64;
        let max_ramp_ft_delta = ramp_ticks
            .saturating_mul(period_secs as i64)
            .saturating_mul(10_000_000);
        for (bid, ui_state, remaining_secs) in state_transitions {
            if ui_state != 0 && destroyed_set.contains(&bid) {
                continue;
            }
            let owner_info = db_opt
                .and_then(|db| {
                    db.with_snapshot(|snap| {
                        snap.characters
                            .values()
                            .find(|c| {
                                c.active_ship_id == bid
                                    && c.solar_system_id == sys_id
                                    && c.station_id.is_none()
                                    && c.structure_id.is_none()
                            })
                            .map(|c| (c.character_id, Some(c.user_id)))
                    })
                })
                .or_else(|| {
                    ballpark
                        .ball_slim_meta
                        .get(&bid)
                        .and_then(|m| m.char_id)
                        .filter(|&cid| cid > 0)
                        .map(|cid| (cid, None))
                });
            let Some((cid, uid_opt)) = owner_info else {
                continue;
            };
            let tracking_data = match ui_state {
                0 => PyRep::None,
                1 | 2 => {
                    let danger_start_ft = if ui_state == 2 {
                        now_ft
                    } else {
                        now_ft.saturating_add((remaining_secs as i64).saturating_mul(10_000_000))
                    };
                    let danger_level = if ui_state == 2 { 1.0 } else { 0.5 };
                    PyRep::keyval([
                        ("damageStartTimestamp", PyRep::Integer(danger_start_ft)),
                        (
                            "damageMaxTimestamp",
                            PyRep::Integer(danger_start_ft.saturating_add(max_ramp_ft_delta)),
                        ),
                        ("dangerLevel", PyRep::Real(danger_level)),
                    ])
                }
                _ => PyRep::None,
            };
            crate::router::notify_registry::emit_typed_target_to_char_capped(
                pending_notifications_opt,
                notify_tx_opt,
                cid,
                uid_opt,
                "charid",
                now_ft,
                256,
                crate::router::notify_registry::OnDeathzoneStateUpdate {
                    solar_system_id: i64::from(sys_id),
                    ship_id: bid,
                    tracking_data,
                },
            );
        }
    }

    destroyed
}

#[cfg(test)] #[path = "space_navigation_advanced_tests.rs"]
mod space_navigation_advanced_tests;

