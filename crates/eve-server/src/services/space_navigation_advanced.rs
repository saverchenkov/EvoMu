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

    let has_immobilizing_module = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.location_id == ship_id
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
                    .unwrap_or(false)
        })
    });

    if has_immobilizing_module {
        let key = match action {
            "dock" => eve_proto::UserErrorKey::CantDockWhileModuleActive,
            "jump" => eve_proto::UserErrorKey::CantJumpWhileModuleActive,
            "warp" => eve_proto::UserErrorKey::WarpDisrupted,
            "tether" => eve_proto::UserErrorKey::DeniedTethered,
            _ => eve_proto::UserErrorKey::CantJumpWhileModuleActive,
        };
        return Err(BridgeRpcError::with_client_error(
            key.as_str(),
            format!("Cannot perform {action} while an immobilizing module (Bastion/Siege/Core) is active"),
            eve_proto::ClientError::user(key),
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
        return Err(BridgeRpcError::with_client_error(
            eve_proto::UserErrorKey::DockingRequestDenied.as_str(),
            "Supercarriers and Titans cannot dock at NPC stations",
            eve_proto::ClientError::user(eve_proto::UserErrorKey::DockingRequestDenied),
        ));
    }
    Ok(())
}

pub fn deactivate_ship_modules_on_leave_space(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship(ctx, ship_id);
    for (_, handle) in ctx.systems.active_system_handles() {
        handle.cancel_ship_nanite_repairs_sync(ship_id, true);
    }
    let (solar_system_id, active_mods): (Option<u32>, Vec<i64>) = ctx.db.with_mut_snapshot(|snap| {
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
        (sys_id, mods)
    });
    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        ctx,
        char_id,
        solar_system_id,
        ship_id,
    );
    if !active_mods.is_empty() {
        ctx.invalidate_ship_fitting(ship_id);
        for mid in active_mods {
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
    let is_pirate = matches!(char_war_faction, Some(500_010 | 500_011));
    if is_pirate {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        return Ok(());
    }

    let declined = kwargs.get("confirmToll") == Some(&PyRep::Bool(false))
        || kwargs.get("confirm_toll") == Some(&PyRep::Bool(false))
        || kwargs.get("declineToll") == Some(&PyRep::Bool(true))
        || kwargs.get("decline_toll") == Some(&PyRep::Bool(true))
        || args.get(1) == Some(&PyRep::Bool(false))
        || args.get(3) == Some(&PyRep::Bool(false));
    if declined {
        crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
            st.zarzakh_toll_quotes.remove(&char_id);
        });
        crate::services::cluster_macho_misc_services::persist_misc_state(ctx);
        return Err(BridgeRpcError::user_error("ZarzakhGateTollDeclined"));
    }

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

    let now = ctx.now_unix();
    let is_three_arg_v24_call = args.len() >= 3 && args.get(1).and_then(|v| v.as_i64()).is_some();
    let explicit_confirm = kwargs.get("confirmToll") == Some(&PyRep::Bool(true))
        || kwargs.get("confirm_toll") == Some(&PyRep::Bool(true))
        || args.get(1) == Some(&PyRep::Bool(true))
        || args.get(3) == Some(&PyRep::Bool(true))
        || (!is_three_arg_v24_call
            && kwargs.get("requireConfirm") != Some(&PyRep::Bool(true))
            && kwargs.get("promptToll") != Some(&PyRep::Bool(true)));

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
        return Err(BridgeRpcError {
            code: "AskConfirmZarzakhGateToll".into(),
            message: format!(
                "Confirm Zarzakh stargate transit toll of {toll} ISK for ship {ship_id} (effective mass {mass:.0} kg)"
            ),
        });
    };

    let bal_after = ctx.db.try_with_mut_snapshot(|s| {
        let c = s
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| BridgeRpcError::user_error("CharacterNotFound"))?;
        if c.balance < effective_toll_cents {
            return Err(BridgeRpcError::user_error(
                "InsufficientFundsForZarzakhGateToll",
            ));
        }
        c.balance = c
            .balance
            .checked_sub(effective_toll_cents)
            .ok_or_else(|| BridgeRpcError::user_error("InsufficientFundsForZarzakhGateToll"))?;
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
    ctx.notify_account_change(char_id, "cash", char_id, bal_after.to_wire_f64());
    Ok(())
}

/// Zarzakh Jovian gravity hazard (`feat:navigation.gravity-hazard-safe-zones`):
/// Ships in Zarzakh (`30_100_000`) outside the Jovian safe zones (stargates, The Fulcrum, Shipcaster)
/// take periodic hull damage; inside a safe zone no hazard damage is applied.
pub fn step_zarzakh_gravity_hazard_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    sde: Option<&eve_sde::SdeRegistry>,
    sys_id: u32,
) -> Vec<i64> {
    if sys_id != 30_100_000 {
        return Vec::new();
    }
    let mut safe_centers = Vec::new();
    if let Some(sde) = sde {
        for sg in sde.stargates().values().filter(|g| g.from_solar_system_id == 30_100_000) {
            safe_centers.push(sg.position);
        }
        if let Some(st) = sde.stations.get(&60015187) {
            safe_centers.push(st.position);
        }
    }
    for b in ballpark.balls.values() {
        let is_safe_anchor = sde
            .and_then(|s| s.types.get(&b.type_id))
            .is_some_and(|t| matches!(t.group_id, 10 | 15 | 4736) || t.name.contains("Shipcaster") || t.name.contains("Fulcrum"));
        if is_safe_anchor {
            safe_centers.push(b.position);
        }
    }
    // DERIVED: 150 km Jovian safe-zone radius around Zarzakh stargates, The Fulcrum, and the Shipcaster.
    let safe_radius = 150_000.0;
    // DERIVED: 50 HP/s unresisted environmental hull/structure bleed outside Jovian safe zones (`apply_direct_structure_damage_to_ball`).
    let hazard_hull_dmg = 50.0;
    let mut destroyed = Vec::new();
    let ship_ids: Vec<i64> = ballpark
        .balls
        .values()
        .filter(|b| {
            sde.and_then(|s| s.types.get(&b.type_id))
                .is_some_and(|t| t.category_id == 6)
        })
        .map(|b| b.id)
        .collect();
    for bid in ship_ids {
        let Some(pos) = ballpark.balls.get(&bid).map(|b| b.position) else { continue };
        if safe_centers.iter().any(|&c| pos.distance(c) <= safe_radius) {
            continue;
        }
        if let Some((_, is_destroyed, _)) = ballpark.apply_direct_structure_damage_to_ball(bid, hazard_hull_dmg) {
            if is_destroyed {
                destroyed.push(bid);
            }
        }
    }
    destroyed
}

#[cfg(test)] #[path = "space_navigation_advanced_tests.rs"]
mod space_navigation_advanced_tests;

