//! `combat_weapons_spool` — Triglavian Entropic Disintegrator spooling mechanics (`feat:modules.turret-precursor`).
//!
//! Entropic Disintegrators spool up damage multiplier per server cycle tick while firing
//! continuously at the same target, up to a maximum spool cap.
//! The spool multiplier resets to 1.0 if the target is switched or the module is deactivated.
//! Activating an already-active module on the same target returns an `AlreadyActive` error.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, EntropicSpoolState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::InvType;

/// Gets the current spool multiplier for an entropic module on a ship (defaults to 1.0).
pub fn get_entropic_spool_multiplier(ctx: &ServerContext, ship_id: i64, module_id: i64) -> f64 {
    with_combat_runtime(ctx, |rt| {
        rt.entropic_spools
            .get(&(ship_id, module_id))
            .map(|s| s.current_multiplier)
            .unwrap_or(1.0)
    })
}

/// Advances entropic spool for an active module by one server cycle tick.
/// Increases spool multiplier by `damageMultiplierBonusPerCycle` (attr 2733) up to
/// `1.0 + damageMultiplierBonusMax` (attr 2734).
pub fn advance_entropic_spool_cycle(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
) -> (u32, f64) {
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(entry) = rt.entropic_spools.get_mut(&(ship_id, module_id)) {
            entry.cycles = entry.cycles.saturating_add(1);
            let mult = 1.0 + (entry.cycles as f64 * entry.per_cycle_bonus).min(entry.max_bonus);
            entry.current_multiplier = mult;
            (entry.cycles, mult)
        } else {
            (0, 1.0)
        }
    })
}

/// Resets/clears the entropic spool for a module to 1.0 (e.g. on deactivation or target switch).
pub fn clear_entropic_spool(ctx: &ServerContext, ship_id: i64, module_id: i64) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.entropic_spools.remove(&(ship_id, module_id));
    });
}

pub fn reset_entropic_spool(ctx: &ServerContext, ship_id: i64, module_id: i64) {
    clear_entropic_spool(ctx, ship_id, module_id);
}

/// Handles entropic disintegrator activation:
/// - Rejects if already active on the same target (`AlreadyActive`).
/// - If active on a different target, retargets and resets spool to 1.0.
/// - If not active, initializes spool state at 1.0 (cycle 0).
pub fn handle_entropic_activation(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
    module_id: i64,
    target_ball_id: i64,
    mod_type: &InvType,
    charge_item_id: Option<i64>,
    base_damage: eve_destiny::DamageVector,
    cap_need: f64,
    duration_s: f64,
) -> Result<(u32, f64), BridgeRpcError> {
    let per_cycle = mod_type
        .default_attributes
        .get(&2733)
        .copied()
        .unwrap_or(0.07);
    let max_bonus = mod_type
        .default_attributes
        .get(&2734)
        .copied()
        .unwrap_or(2.125);

    let is_active_in_db = ctx.db.with_snapshot(|snap| {
        snap.module_active_states.contains(&module_id)
    });

    with_mut_combat_runtime(ctx, |rt| {
        if let Some(entry) = rt.entropic_spools.get_mut(&(ship_id, module_id)) {
            if entry.target_id == target_ball_id && is_active_in_db {
                // Activating an already-active module on the same target is rejected!
                return Err(BridgeRpcError {
                    code: "AlreadyActive".into(),
                    message: format!("Module {module_id} is already active"),
                });
            } else {
                // Switched target! Reset spool to 1.0
                entry.target_id = target_ball_id;
                entry.solar_system_id = solar_system_id;
                entry.attacker_char_id = char_id;
                entry.attacker_ship_id = ship_id;
                entry.module_id = module_id;
                entry.module_type_id = mod_type.type_id;
                entry.charge_item_id = charge_item_id;
                entry.base_damage = base_damage;
                entry.cap_need = cap_need;
                entry.duration_s = duration_s;
                entry.cycles = 0;
                entry.elapsed_in_cycle = 0.0;
                entry.per_cycle_bonus = per_cycle;
                entry.max_bonus = max_bonus;
                entry.current_multiplier = 1.0;
                return Ok((0, 1.0));
            }
        }

        // Fresh activation (cycle 0)
        rt.entropic_spools.insert(
            (ship_id, module_id),
            EntropicSpoolState {
                target_id: target_ball_id,
                solar_system_id,
                attacker_char_id: char_id,
                attacker_ship_id: ship_id,
                module_id,
                module_type_id: mod_type.type_id,
                charge_item_id,
                base_damage,
                cap_need,
                duration_s,
                cycles: 0,
                elapsed_in_cycle: 0.0,
                per_cycle_bonus: per_cycle,
                max_bonus,
                current_multiplier: 1.0,
            },
        );
        Ok((0, 1.0))
    })
}

/// Steps all active entropic disintegrator spools in a solar system by `dt` seconds.
/// Fires repeated cycles against target_id, deducting capacitor and ammo, and applying damage
/// scaled by the current multiplier.
pub async fn step_system_entropic_spools(
    ctx: &ServerContext,
    solar_system_id: u32,
    dt: f64,
) {
    let active_spool_keys: Vec<(i64, i64)> = with_combat_runtime(ctx, |rt| {
        rt.entropic_spools
            .iter()
            .filter(|(_, state)| state.solar_system_id == solar_system_id)
            .map(|(&key, _)| key)
            .collect()
    });

    if active_spool_keys.is_empty() {
        return;
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);

    for (ship_id, module_id) in active_spool_keys {
        let spool_state = with_combat_runtime(ctx, |rt| {
            rt.entropic_spools.get(&(ship_id, module_id)).cloned()
        });
        let Some(mut state) = spool_state else {
            continue;
        };

        // Verify module is still active in DB
        let is_active = ctx.db.with_snapshot(|snap| {
            snap.module_active_states.contains(&module_id)
        });
        if !is_active {
            clear_entropic_spool(ctx, ship_id, module_id);
            continue;
        }

        // Verify target is alive via map(|cs| cs.structure_hp > 0.0).unwrap_or(false)
        let target_alive = sys_handle
            .get_combat_state(state.target_id)
            .await
            .ok()
            .flatten()
            .map(|cs| cs.structure_hp > 0.0)
            .unwrap_or(false);

        if !target_alive {
            clear_entropic_spool(ctx, ship_id, module_id);
            continue;
        }

        state.elapsed_in_cycle += dt;
        let mut stopped = false;

        while state.elapsed_in_cycle >= state.duration_s - 1e-6 {
            state.elapsed_in_cycle -= state.duration_s;
            if state.elapsed_in_cycle < 0.0 {
                state.elapsed_in_cycle = 0.0;
            }

            // Deduct capacitor
            if state.cap_need > 0.0 {
                if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
                    if cs.capacitor + 1e-6 < state.cap_need {
                        clear_entropic_spool(ctx, ship_id, module_id);
                        stopped = true;
                        break;
                    }
                    cs.capacitor = (cs.capacitor - state.cap_need).max(0.0);
                    let _ = sys_handle.set_combat_state(ship_id, cs).await;
                }
            }

            // Deduct ammo
            if let Some(cid) = state.charge_item_id {
                let has_ammo = ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ci) = snap.items.get_mut(&cid) {
                        if ci.quantity > 1 {
                            ci.quantity -= 1;
                            true
                        } else {
                            snap.items.remove(&cid);
                            true
                        }
                    } else {
                        false
                    }
                });
                if !has_ammo {
                    clear_entropic_spool(ctx, ship_id, module_id);
                    stopped = true;
                    break;
                }
            }

            // Advance cycle count and scale multiplier:
            // current_multiplier = (1.0 + cycles * step).min(max_mult)
            state.cycles = state.cycles.saturating_add(1);
            let max_mult = 1.0 + state.max_bonus;
            state.current_multiplier = (1.0 + (state.cycles as f64) * state.per_cycle_bonus).min(max_mult);

            // Apply scaled damage to target
            let scaled_dmg = state.base_damage.scaled(state.current_multiplier);
            let _ = sys_handle
                .apply_damage_with_attacker(state.target_id, scaled_dmg, Some(state.attacker_char_id))
                .await;

            // Check if target is still alive
            let still_alive = sys_handle
                .get_combat_state(state.target_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.structure_hp > 0.0)
                .unwrap_or(false);

            if !still_alive {
                clear_entropic_spool(ctx, ship_id, module_id);
                stopped = true;
                break;
            }
        }

        if !stopped {
            with_mut_combat_runtime(ctx, |rt| {
                if let Some(entry) = rt.entropic_spools.get_mut(&(ship_id, module_id)) {
                    *entry = state;
                }
            });
        }
    }
}

/// Links all compatible weapon modules into banks (`charFittingMgr.LinkAllWeapons` / `dogmaIM.LinkAllWeapons`).
pub fn handle_link_all_weapons(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Ship {ship_id} is not owned by {char_id}"),
            });
        }
        let is_linkable_weapon = |ty: &eve_sde::InvType| -> bool {
            ty.effect_ids.contains(&42) || ty.effect_ids.contains(&40) || ty.default_attributes.contains_key(&604)
        };
        let mut by_type: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
        for item in snap.items.values() {
            if item.location_id == ship_id && matches!(item.flag as u16, 27..=34) {
                if let Some(ty) = ctx.sde.types.get(&item.type_id) {
                    if is_linkable_weapon(ty) { by_type.entry(item.type_id).or_default().push(item.item_id); }
                }
            }
        }
        let banks = snap.weapon_banks.entry(ship_id).or_default();
        banks.clear();
        let mut result_entries: Vec<(PyRep, PyRep)> = Vec::new();
        for (_type_id, mut mods) in by_type {
            if mods.len() >= 2 {
                mods.sort();
                let master = mods[0];
                banks.insert(master, mods.clone());
                let mod_list: Vec<PyRep> = mods.into_iter().map(PyRep::Integer).collect();
                result_entries.push((PyRep::Integer(master), PyRep::List(mod_list)));
            }
        }
        result_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        Ok(PyRep::Dict(result_entries))
    })
}
