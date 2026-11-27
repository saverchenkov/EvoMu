//! Protocol-agnostic combat health restoration, module damage clearing, and entity destruction
//! domain operations shared across gameplay services and GM `/heal` + `/kill` commands (`#1193`).
//!
//! All solar-system lookups use non-waking [`crate::system_actor::SolarSystemRegistry::get_handle`]
//! (`#1542`) and delegate to canonical SP6, ballpark, NPC AI, and death-pipeline primitives.

#![forbid(unsafe_code)]

use crate::combat::npc_ai::{
    spawn_npc_wreck_and_loot_in_snap, DestroyedNpcOutcome, NpcDestroyedOptions,
};
use crate::domain::DomainError;
use crate::router::notify_registry::BallparkScene;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{KillmailRecord, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use glam::DVec3;
use std::collections::HashMap;

/// Target parameters for [`restore_ship_vital_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreShipVitalsRequest {
    pub target_char_id: i64,
    pub target_ship_id: i64,
    pub ship_type_id: u32,
    pub sys_id: u32,
}

/// Restored ship vitals returned by [`restore_ship_vital_state`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RestoreShipVitalsOutcome {
    pub character_id: i64,
    pub ship_id: i64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub capacitor: f64,
}

/// Target specification for [`destroy_entity_in_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestroyEntityRequest {
    pub target_id: i64,
    pub victim_char_id: i64,
    pub victim_ship_id: i64,
    pub sys_id: u32,
    pub killer_char_id: i64,
    pub in_db_items: bool,
}

/// Entity destruction outcome returned by [`destroy_entity_in_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestroyEntityOutcome {
    pub target_id: i64,
    pub wreck_id: i64,
    pub killmail_id: i64,
    pub ejected_capsule_id: Option<i64>,
}

/// Clears persisted ship hull/armor/shield/capacitor damage (`clear_ship_combat_damage_in_snap`)
/// and, when `clear_children` is true, also clears module/heat damage (`DAMAGE`, `ARMOR_DAMAGE`,
/// heat `1211`) on the ship and its fitted child items in `snap`.
pub fn clear_ship_and_fitted_damage_in_snap(
    snap: &mut WorldStateSnapshot,
    ship_id: i64,
    clear_children: bool,
) {
    if ship_id <= 0 {
        return;
    }
    crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, ship_id);
    if clear_children {
        for item in snap.items.values_mut() {
            if item.item_id == ship_id || item.location_id == ship_id {
                let r1 = item
                    .dynamic_attributes
                    .remove(&eve_sde::attr_ids::DAMAGE)
                    .is_some();
                let r2 = item
                    .dynamic_attributes
                    .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
                    .is_some();
                let r3 = item.dynamic_attributes.remove(&1211u16).is_some();
                if (r1 || r2 || r3) && item.item_id != ship_id {
                    item.version += 1;
                }
            }
        }
    }
}

/// Clears cached module `damage_hp` entries inside `ctx.ship_fittings` for `item_ids`.
pub fn clear_fitted_module_damage_in_cache(ctx: &ServerContext, item_ids: &[i64]) {
    if item_ids.is_empty() {
        return;
    }
    if let Ok(mut fits) = ctx.ship_fittings.write() {
        for fit in fits.values_mut() {
            for m in fit.modules.values_mut() {
                if item_ids.contains(&m.item_id) {
                    m.damage_hp = 0.0;
                }
            }
        }
    }
}

fn resolve_ship_vital_capacities(
    ctx: &ServerContext,
    ship_id: i64,
    ship_type_id: u32,
    fit_opt: Option<&eve_dogma::ShipFittingEngine>,
) -> (f64, f64, f64, f64, f64, f64) {
    let pick = |attrs: &HashMap<u16, f64>| {
        let g = |aid| attrs.get(&aid).copied().unwrap_or(0.0);
        (
            g(eve_sde::attr_ids::HP_STRUCT),
            g(eve_sde::attr_ids::ARMOR_HP),
            g(eve_sde::attr_ids::SHIELD_CAPACITY),
            g(eve_sde::attr_ids::CAPACITOR_CAPACITY),
            g(eve_sde::attr_ids::RECHARGE_RATE),
            g(eve_sde::attr_ids::SHIELD_RECHARGE_RATE),
        )
    };
    if let Some(fit) = fit_opt {
        let mut eff_attrs: HashMap<u16, f64> = [
            eve_sde::attr_ids::HP_STRUCT,
            eve_sde::attr_ids::ARMOR_HP,
            eve_sde::attr_ids::SHIELD_CAPACITY,
            eve_sde::attr_ids::CAPACITOR_CAPACITY,
            eve_sde::attr_ids::RECHARGE_RATE,
            eve_sde::attr_ids::SHIELD_RECHARGE_RATE,
        ]
        .into_iter()
        .map(|aid| (aid, fit.ship.get_attribute(aid, &ctx.sde)))
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            ctx,
            ship_id,
            &mut eff_attrs,
        );
        pick(&eff_attrs)
    } else if let Some(t) = ctx.sde.types.get(&ship_type_id) {
        pick(&t.default_attributes)
    } else {
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    }
}

/// Restores ship shield, armor, hull, capacitor, module damage, and rack heat across live ballpark
/// state (only when the solar system is already loaded — never waking cold systems `#1542`),
/// SP6/DB damage attributes, fitting cache, and Destiny/Godma notifications.
pub async fn restore_ship_vital_state(
    ctx: &ServerContext,
    req: RestoreShipVitalsRequest,
) -> Result<RestoreShipVitalsOutcome, DomainError> {
    let (
        fit_max_hull,
        fit_max_armor,
        fit_max_shield,
        fit_max_cap,
        fit_cap_recharge_ms,
        fit_shield_recharge_ms,
        has_fit,
        pre_rack_heat,
        cached_damaged_mods,
    ) = {
        let fit_opt = (req.target_ship_id > 0 && req.target_char_id > 0)
            .then(|| ctx.fit(req.target_char_id, req.target_ship_id).ok())
            .flatten();
        let caps =
            resolve_ship_vital_capacities(ctx, req.target_ship_id, req.ship_type_id, fit_opt.as_ref());
        let has_fit = fit_opt.is_some();
        let pre_rack_heat = fit_opt
            .as_ref()
            .map(|f| f.rack_heat)
            .unwrap_or([0.0, 0.0, 0.0]);
        let cached_damaged_mods: Vec<(i64, f64)> = fit_opt
            .as_ref()
            .map(|f| {
                f.modules
                    .values()
                    .filter(|m| m.damage_hp > 0.0)
                    .map(|m| (m.item_id, m.damage_hp))
                    .collect()
            })
            .unwrap_or_default();
        (caps.0, caps.1, caps.2, caps.3, caps.4, caps.5, has_fit, pre_rack_heat, cached_damaged_mods)
    };

    let handle_opt = (req.target_ship_id > 0 && req.sys_id > 0)
        .then(|| ctx.systems.get_handle(req.sys_id))
        .flatten();
    let pre_cs = if let Some(ref handle) = handle_opt {
        handle
            .get_combat_state(req.target_ship_id)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let pick_max = |live: f64, fallback: f64| if live > 0.0 { live } else { fallback };
    let unified_fallback_maxes = if let Some(ref cs) = pre_cs {
        (
            pick_max(cs.max_structure_hp, fit_max_hull),
            pick_max(cs.max_armor_hp, fit_max_armor),
            pick_max(cs.max_shield_hp, fit_max_shield),
        )
    } else {
        (fit_max_hull, fit_max_armor, fit_max_shield)
    };

    let (pre_unified, pre_dyn_shield_charge, pre_dyn_cap, mut repaired_child_attrs, pre_ship_heat) =
        ctx.db.with_snapshot(|snap| {
            let unified = (req.target_ship_id > 0).then(|| {
                crate::sp6_lifecycle::read_unified_ship_damage_state(
                    ctx,
                    snap,
                    req.target_ship_id,
                    req.ship_type_id,
                    Some(unified_fallback_maxes),
                )
            });
            let ship_it = snap.items.get(&req.target_ship_id);
            let dyn_shield = ship_it
                .and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::SHIELD_CHARGE).copied());
            let dyn_cap = ship_it.and_then(|it| it.dynamic_attributes.get(&18u16).copied());
            let ship_heat = ship_it
                .and_then(|it| it.dynamic_attributes.get(&1211u16).copied())
                .unwrap_or(0.0);
            let mut child_changes: Vec<(i64, u16, f64)> = Vec::new();
            if req.target_ship_id > 0 {
                for item in snap.items.values() {
                    if item.location_id == req.target_ship_id {
                        if let Some(&old_d) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE) {
                            if old_d > 0.0 {
                                child_changes.push((item.item_id, eve_sde::attr_ids::DAMAGE, old_d));
                            }
                        }
                        if let Some(&old_h) = item.dynamic_attributes.get(&1211u16) {
                            if old_h > 0.0 {
                                child_changes.push((item.item_id, 1211u16, old_h));
                            }
                        }
                    }
                }
            }
            (unified, dyn_shield, dyn_cap, child_changes, ship_heat)
        });

    let mut combat_healed = eve_destiny::CombatState::default();
    if let Some(ref handle) = handle_opt {
        if let Some(ref cs) = pre_cs {
            let needs_cap_sync = fit_max_cap > 0.0 && cs.max_capacitor + 1e-6 < fit_max_cap;
            let needs_recharge_sync = (fit_cap_recharge_ms > 0.0
                && (cs.capacitor_recharge_time_ms <= 0.0
                    || (needs_cap_sync
                        && (cs.capacitor_recharge_time_ms - fit_cap_recharge_ms).abs() > 1e-3)))
                || (fit_shield_recharge_ms > 0.0 && cs.shield_recharge_time_ms <= 0.0);
            let needs_zero_hp_sync = (cs.max_shield_hp <= 0.0 && fit_max_shield > 0.0)
                || (cs.max_armor_hp <= 0.0 && fit_max_armor > 0.0)
                || (cs.max_structure_hp <= 0.0 && fit_max_hull > 0.0);
            if needs_cap_sync || needs_recharge_sync || needs_zero_hp_sync {
                let _ = handle
                    .modify_combat_state(req.target_ship_id, move |st| {
                        if needs_cap_sync {
                            st.max_capacitor = fit_max_cap;
                            st.base_max_capacitor = Some(fit_max_cap);
                        }
                        if fit_cap_recharge_ms > 0.0
                            && (st.capacitor_recharge_time_ms <= 0.0 || needs_cap_sync)
                        {
                            st.capacitor_recharge_time_ms = fit_cap_recharge_ms;
                            st.base_capacitor_recharge_time_ms = Some(fit_cap_recharge_ms);
                        }
                        if fit_shield_recharge_ms > 0.0 && st.shield_recharge_time_ms <= 0.0 {
                            st.shield_recharge_time_ms = fit_shield_recharge_ms;
                        }
                        if st.max_shield_hp <= 0.0 && fit_max_shield > 0.0 {
                            st.max_shield_hp = fit_max_shield;
                            st.base_max_shield_hp = Some(fit_max_shield);
                        }
                        if st.max_armor_hp <= 0.0 && fit_max_armor > 0.0 {
                            st.max_armor_hp = fit_max_armor;
                            st.base_max_armor_hp = Some(fit_max_armor);
                        }
                        if st.max_structure_hp <= 0.0 && fit_max_hull > 0.0 {
                            st.max_structure_hp = fit_max_hull;
                            st.base_max_structure_hp = Some(fit_max_hull);
                        }
                    })
                    .await;
            }
        }
        if let Ok(cs) = handle.heal_combat_state(req.target_ship_id).await {
            combat_healed = cs;
        }
        let _ = handle.flush_peer_observer_updates(None).await;
    }

    if req.target_ship_id > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            clear_ship_and_fitted_damage_in_snap(snap, req.target_ship_id, true);
        });
    }
    if has_fit {
        let mut cached_damaged_ids = Vec::with_capacity(cached_damaged_mods.len());
        for (mod_id, dmg_hp) in cached_damaged_mods {
            cached_damaged_ids.push(mod_id);
            if !repaired_child_attrs
                .iter()
                .any(|&(id, aid, _)| id == mod_id && aid == eve_sde::attr_ids::DAMAGE)
            {
                repaired_child_attrs.push((mod_id, eve_sde::attr_ids::DAMAGE, dmg_hp));
            }
        }
        clear_fitted_module_damage_in_cache(ctx, &cached_damaged_ids);
        ctx.update_ship_rack_heat(req.target_char_id, req.target_ship_id, [0.0, 0.0, 0.0], true);
    }

    let eff_max_shield = combat_healed.max_shield_hp.max(fit_max_shield);
    let eff_max_armor = combat_healed.max_armor_hp.max(fit_max_armor);
    let eff_max_hull = combat_healed.max_structure_hp.max(fit_max_hull);
    let eff_max_cap = combat_healed.max_capacitor.max(fit_max_cap);
    let final_shield = pick_max(
        if combat_healed.max_shield_hp > 0.0 { combat_healed.shield_hp } else { 0.0 },
        eff_max_shield,
    );
    let final_armor = pick_max(
        if combat_healed.max_armor_hp > 0.0 { combat_healed.armor_hp } else { 0.0 },
        eff_max_armor,
    );
    let final_hull = pick_max(
        if combat_healed.max_structure_hp > 0.0 { combat_healed.structure_hp } else { 0.0 },
        eff_max_hull,
    );
    let final_cap = pick_max(
        if combat_healed.max_capacitor > 0.0 { combat_healed.capacitor } else { 0.0 },
        eff_max_cap,
    );

    if req.target_char_id > 0 && req.target_ship_id > 0 {
        let (old_shield, old_armor_dmg, old_hull_dmg) = if let Some(u) = pre_unified {
            let s = match pre_dyn_shield_charge {
                Some(dc) if pre_cs.as_ref().is_none_or(|c| c.max_shield_hp <= 0.0) => {
                    dc.clamp(0.0, final_shield.max(dc))
                }
                Some(dc) => u.shield_hp.min(dc).max(0.0),
                None => u.shield_hp,
            };
            (s, u.armor_damage, u.hull_damage)
        } else {
            (pre_dyn_shield_charge.unwrap_or(final_shield), 0.0, 0.0)
        };
        let old_cap = match (pre_cs.as_ref().filter(|c| c.max_capacitor > 0.0), pre_dyn_cap) {
            (Some(cs), Some(dc)) => cs.capacitor.min(dc).max(0.0),
            (Some(cs), None) => cs.capacitor.max(0.0),
            (None, Some(dc)) => dc.max(0.0),
            (None, None) => final_cap,
        };

        let mut attr_changes: Vec<(i64, u16, f64, f64)> = Vec::new();
        if final_shield > 0.0 || pre_dyn_shield_charge.is_some() {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::SHIELD_CHARGE, final_shield, old_shield));
        }
        if final_armor > 0.0 || old_armor_dmg > 0.0 {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::ARMOR_DAMAGE, 0.0, old_armor_dmg));
        }
        if final_hull > 0.0 || old_hull_dmg > 0.0 {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::DAMAGE, 0.0, old_hull_dmg));
        }
        let emit_cap_sync = (final_cap > 0.0 || pre_dyn_cap.is_some()) && eff_max_cap > 0.0;
        if !emit_cap_sync && (final_cap > 0.0 || pre_dyn_cap.is_some()) {
            attr_changes.push((req.target_ship_id, 18u16, final_cap, old_cap));
        }
        if pre_ship_heat > 0.0 {
            attr_changes.push((req.target_ship_id, 1211u16, 0.0, pre_ship_heat));
        }
        for (idx, rack_attr) in [1175u16, 1176, 1177].into_iter().enumerate() {
            if pre_rack_heat[idx] > 0.0 {
                attr_changes.push((req.target_ship_id, rack_attr, 0.0, pre_rack_heat[idx]));
            }
        }
        repaired_child_attrs.sort_unstable_by_key(|&(id, aid, _)| (id, aid));
        for (mod_id, aid, old_val) in repaired_child_attrs {
            attr_changes.push((mod_id, aid, 0.0, old_val));
        }
        ctx.notify_module_attribute_changes(req.target_char_id, req.target_ship_id, &attr_changes);
        if emit_cap_sync {
            let _ = ctx.notify_ship_capacitor_change(
                req.target_char_id,
                req.target_ship_id,
                final_cap,
                eff_max_cap,
                Some(old_cap),
            );
        }
        if let Some(scene) = BallparkScene::try_solar_system(req.sys_id) {
            let _ = ctx.notify_damage_state_change(
                NotificationTarget::Character(req.target_char_id),
                scene,
                req.target_ship_id,
                1.0,
                1.0,
                1.0,
            );
        }
    }

    Ok(RestoreShipVitalsOutcome {
        character_id: req.target_char_id,
        ship_id: req.target_ship_id,
        shield_hp: final_shield,
        armor_hp: final_armor,
        hull_hp: final_hull,
        capacitor: final_cap,
    })
}

/// Destroys a player/DB ship (via [`crate::combat::death_pipeline::execute_death_pipeline`],
/// rejecting `sys_id == 0` with `NoSolarSystem`) or a space ball / tracked NPC (via
/// [`crate::combat::npc_ai::NpcSpawnManager::handle_npc_destroyed_detailed_with_options`] or
/// [`spawn_npc_wreck_and_loot_in_snap`]), never waking system `0` or cold solar systems for space balls (`#1542`).
pub async fn destroy_entity_in_system(
    ctx: &ServerContext,
    req: DestroyEntityRequest,
) -> Result<DestroyEntityOutcome, DomainError> {
    let handle_opt = (req.sys_id > 0)
        .then(|| ctx.systems.get_handle(req.sys_id))
        .flatten();
    if req.in_db_items {
        if req.sys_id == 0 {
            return Err(DomainError::precondition_failed(
                "NoSolarSystem",
                format!("Target {} is not in an active solar system", req.victim_ship_id),
            ));
        }
        let victim_pos = if let Some(ref handle) = handle_opt {
            handle
                .get_ball_snapshot(req.victim_ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO)
        } else {
            DVec3::ZERO
        };
        let outcome = Box::pin(crate::combat::death_pipeline::execute_death_pipeline(
            ctx,
            req.sys_id,
            req.victim_char_id,
            req.victim_ship_id,
            req.killer_char_id,
            victim_pos,
            ctx.now_unix(),
        ))
        .await
        .map_err(|e| DomainError::internal("KillFailed", e))?;
        if let Some(ref handle) = handle_opt {
            let _ = handle.flush_peer_observer_updates(None).await;
        }
        return Ok(DestroyEntityOutcome {
            target_id: req.victim_ship_id,
            wreck_id: outcome.wreck_item_id,
            killmail_id: outcome.killmail_id,
            ejected_capsule_id: outcome.ejected_capsule_id,
        });
    }

    let Some(handle) = handle_opt else {
        return Err(DomainError::not_found(
            "TargetNotFound",
            format!("Target entity {} not found", req.target_id),
        ));
    };
    let Some((_, ball)) = handle
        .get_ball_snapshot(req.victim_ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(DomainError::not_found(
            "TargetNotFound",
            format!("Target entity {} not found", req.target_id),
        ));
    };
    let victim_pos = ball.position;
    let _ = handle.remove_ball(req.victim_ship_id).await;
    ctx.systems
        .invoke_npc_death_hook(req.sys_id, req.victim_ship_id, Some(req.killer_char_id))
        .await;
    crate::services::fw_gameplay::on_space_entity_destroyed(
        ctx,
        req.sys_id,
        Some(req.killer_char_id),
        req.victim_ship_id,
        ball.type_id,
    )
    .await;

    let now_unix = ctx.now_unix();
    let tracked_res: Option<Result<DestroyedNpcOutcome, String>> = ctx
        .npc_spawns
        .write()
        .ok()
        .and_then(|mut spawns| {
            let mgr = spawns.get_mut(&req.sys_id)?;
            mgr.entities.contains_key(&req.victim_ship_id).then(|| {
                mgr.handle_npc_destroyed_detailed_with_options(
                    ctx,
                    req.sys_id,
                    req.victim_ship_id,
                    req.killer_char_id,
                    now_unix,
                    NpcDestroyedOptions {
                        skip_death_hook_duplicates: true,
                        live_pos_override: Some(victim_pos),
                    },
                )
            })
        });
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.remove(&req.victim_ship_id);
    });

    let (wreck_id, killmail_id, wreck_type_id, wreck_pos) = match tracked_res {
        Some(Ok(npc_out)) => (
            npc_out.wreck_id,
            npc_out.killmail_id,
            npc_out.wreck_type_id,
            npc_out.wreck_pos,
        ),
        Some(Err(err)) => return Err(DomainError::internal("KillFailed", err)),
        None => {
            let in_abyssal = (32_000_000..=32_999_999).contains(&req.sys_id);
            let wt_id = if in_abyssal {
                0
            } else {
                ctx.sde
                    .resolve_wreck_type_id(ball.type_id, None)
                    .unwrap_or(26557)
            };
            let killmail_value_isk = ctx
                .sde
                .types
                .get(&ball.type_id)
                .map(|t| {
                    let bounty = t
                        .default_attributes
                        .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                        .copied()
                        .unwrap_or(0.0);
                    bounty.max(t.base_price).max(0.0)
                })
                .unwrap_or(0.0);
            let (wid, kid) = ctx.db.with_mut_snapshot(|snap| {
                let wid = if in_abyssal {
                    0
                } else {
                    spawn_npc_wreck_and_loot_in_snap(
                        snap,
                        req.sys_id,
                        req.killer_char_id,
                        victim_pos,
                        now_unix,
                        wt_id,
                        &[],
                    )
                };
                let kid = snap.alloc_entity_id();
                snap.record_killmail(
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: 0,
                        attacker_character_id: req.killer_char_id,
                        solar_system_id: req.sys_id,
                        ship_type_id: ball.type_id,
                        total_value_isk: killmail_value_isk,
                        ..Default::default()
                    },
                    now_unix,
                );
                (wid, kid)
            });
            (wid, kid, wt_id, victim_pos)
        }
    };

    if wreck_id > 0 && wreck_type_id > 0 {
        let wreck_sde = ctx.sde.types.get(&wreck_type_id);
        let _ = handle
            .spawn_ball_with_meta(
                Ball {
                    id: wreck_id,
                    type_id: wreck_type_id,
                    position: wreck_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000.0,
                    radius: 30.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_type_id),
                SlimItemMeta {
                    group_id: wreck_sde.map(|t| t.group_id).unwrap_or(186),
                    category_id: wreck_sde.map(|t| t.category_id).unwrap_or(2),
                    owner_id: req.killer_char_id,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: wreck_sde.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                    ..Default::default()
                },
            )
            .await;
    }
    let _ = handle.flush_peer_observer_updates(None).await;

    Ok(DestroyEntityOutcome {
        target_id: req.victim_ship_id,
        wreck_id,
        killmail_id,
        ejected_capsule_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
    use eve_destiny::DamageVector;

    #[tokio::test]
    async fn heal_and_kill_do_not_wake_zero_or_cold_solar_system() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let (ship_id, ship_type_id) = ctx.db.with_snapshot(|s| {
            let sid = s.characters[&cid].active_ship_id;
            (sid, s.items[&sid].type_id)
        });
        let cold_sys = 30_000_142_u32;
        assert!(ctx.systems.get_handle(0).is_none());
        assert!(ctx.systems.get_handle(cold_sys).is_none());

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&ship_id) {
                it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 33.0);
                it.dynamic_attributes.insert(1211u16, 12.0);
            }
        });

        let healed = restore_ship_vital_state(
            &ctx,
            RestoreShipVitalsRequest {
                target_char_id: cid,
                target_ship_id: ship_id,
                ship_type_id,
                sys_id: cold_sys,
            },
        )
        .await
        .expect("cold-system vital restore clears DB without waking system");
        assert_eq!(healed.ship_id, ship_id);
        assert!(ctx.systems.get_handle(cold_sys).is_none(), "heal must not wake cold system");
        ctx.db.with_snapshot(|snap| {
            let attrs = &snap.items[&ship_id].dynamic_attributes;
            assert!(!attrs.contains_key(&eve_sde::attr_ids::DAMAGE));
            assert!(!attrs.contains_key(&1211u16));
        });

        let _ = restore_ship_vital_state(
            &ctx,
            RestoreShipVitalsRequest {
                target_char_id: cid,
                target_ship_id: ship_id,
                ship_type_id,
                sys_id: 0,
            },
        )
        .await
        .expect("zero-system vital restore does not spawn sys 0");
        assert!(ctx.systems.get_handle(0).is_none());

        let db_zero_err = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: ship_id,
                victim_char_id: cid,
                victim_ship_id: ship_id,
                sys_id: 0,
                killer_char_id: cid,
                in_db_items: true,
            },
        )
        .await
        .expect_err("DB entity kill in sys 0 must fail closed");
        assert_eq!(db_zero_err.code, "NoSolarSystem");
        assert!(ctx.systems.get_handle(0).is_none());

        let ball_cold_err = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: 950_777_001,
                victim_char_id: cid,
                victim_ship_id: 950_777_001,
                sys_id: cold_sys,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect_err("ball kill in cold system must fail closed without spawning system");
        assert_eq!(ball_cold_err.code, "TargetNotFound");
        assert!(ctx.systems.get_handle(cold_sys).is_none());
    }

    #[tokio::test]
    async fn destroy_entity_tracked_npc_uses_unified_spawn_manager_path() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let sys_id = 30_000_142_u32;
        let npc_id = 950_888_001_i64;
        let npc_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 11 && ctx.sde.resolve_wreck_type_id(t.type_id, None).is_some())
            .map(|(&tid, _)| tid)
            .unwrap_or(16_997);
        let npc_pos = DVec3::new(3_000.0, 0.0, 0.0);
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_id,
                    type_id: npc_type_id,
                    position: npc_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(npc_type_id),
            )
            .await
            .unwrap();
        if let Ok(mut spawns) = ctx.npc_spawns.write() {
            spawns
                .entry(sys_id)
                .or_insert_with(NpcSpawnManager::new)
                .entities
                .insert(
                    npc_id,
                    NpcCombatEntity {
                        ball_id: npc_id,
                        type_id: npc_type_id,
                        faction_id: 500_010,
                        state: NpcAiState::Idle,
                        spawn_group_id: 0,
                        spawn_pos: npc_pos,
                        target_id: None,
                        bounty_isk: 12_500.0,
                        optimal_range: 5_000.0,
                        orbit_range: 3_000.0,
                        base_dps: 10.0,
                        damage_vector: DamageVector::default(),
                        last_attack_tick: 0,
                        has_shield_rep: false,
                        has_armor_rep: false,
                    },
                );
        }

        let pre_sec = ctx.db.with_snapshot(|s| s.characters[&cid].security_status);
        let out = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: npc_id,
                victim_char_id: cid,
                victim_ship_id: npc_id,
                sys_id,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect("tracked NPC destroy succeeds");
        assert!(out.wreck_id > 0);
        assert!(out.killmail_id > 0);
        assert!(ctx.npc_spawns.read().unwrap().get(&sys_id).is_none_or(|m| !m.entities.contains_key(&npc_id)));
        assert!(handle.get_ball_snapshot(npc_id).await.unwrap().is_none());
        assert!(handle.get_ball_snapshot(out.wreck_id).await.unwrap().is_some());
        ctx.db.with_snapshot(|snap| {
            assert!(snap.items.contains_key(&out.wreck_id));
            assert!(snap.killmails.contains_key(&out.killmail_id));
            assert!(snap.characters[&cid].security_status >= pre_sec);
        });
    }

    #[tokio::test]
    async fn destroy_entity_tracked_npc_credits_corp_project_payout_and_achievement_once() {
        use crate::services::social_corp_fleet::social_corp_projects::{
            create_canonical_project, CanonicalProjectSpec,
        };
        use eve_db::IskCents;

        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let sys_id = 30_000_142_u32;
        let npc_id = 950_888_002_i64;
        let npc_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 11 && ctx.sde.resolve_wreck_type_id(t.type_id, None).is_some())
            .map(|(&tid, _)| tid)
            .unwrap_or(16_997);
        let spawn_pos = DVec3::new(3_000.0, 0.0, 0.0);
        let live_pos = DVec3::new(4_200.0, 1_100.0, -500.0);

        let corp_id = ctx.db.with_mut_snapshot(|snap| {
            let corp_id = snap.characters[&cid].corporation_id;
            let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
                let mut rec = eve_db::CorporationRecord::default();
                rec.corporation_id = corp_id;
                rec.name = "Test Corp".into();
                rec.ticker = "TSTC".into();
                rec.ceo_id = cid;
                rec
            });
            corp.ensure_wallet_divisions();
            corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            corp_id
        });
        let (pid, _) = create_canonical_project(
            &ctx.db,
            CanonicalProjectSpec {
                corp_id,
                creator_id: cid,
                name: "Single NPC Kill Payout".into(),
                description: "Verify destroy_entity_in_system credits +1 once".into(),
                target_quantity: 5,
                initial_quantity: 0,
                escrow: IskCents::from_isk(50_000),
                isk_per_unit: IskCents::from_isk(10_000),
                wallet_division: 1,
                matcher_type: "DestroyNPCShips".into(),
                is_freelance: false,
                auto_payout_on_contribution: true,
                max_per_character: None,
                expires_unix: None,
                filter_id: None,
                batch_size: 1,
                now_unix: ctx.now_unix(),
                min_project_id: 9_100,
            },
        )
        .expect("create DestroyNPCShips corp project");

        let handle = ctx.systems.get_or_spawn(sys_id);
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_id,
                    type_id: npc_type_id,
                    position: live_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(npc_type_id),
            )
            .await
            .unwrap();
        if let Ok(mut spawns) = ctx.npc_spawns.write() {
            spawns
                .entry(sys_id)
                .or_insert_with(NpcSpawnManager::new)
                .entities
                .insert(
                    npc_id,
                    NpcCombatEntity {
                        ball_id: npc_id,
                        type_id: npc_type_id,
                        faction_id: 500_010,
                        state: NpcAiState::Idle,
                        spawn_group_id: 0,
                        spawn_pos,
                        target_id: None,
                        bounty_isk: 0.0,
                        optimal_range: 5_000.0,
                        orbit_range: 3_000.0,
                        base_dps: 10.0,
                        damage_vector: DamageVector::default(),
                        last_attack_tick: 0,
                        has_shield_rep: false,
                        has_armor_rep: false,
                    },
                );
        }

        let conds_for = |ev: &str| -> Vec<(u32, u32)> {
            ctx.quasar_gateway
                .progression
                .achievements
                .definitions
                .iter()
                .flat_map(|d| {
                    d.conditions
                        .iter()
                        .filter(move |c| c.event_name == ev)
                        .map(move |c| (d.id, c.condition_id))
                })
                .collect()
        };
        let cond_val = |ach_id: u32, cond_id: u32| -> u64 {
            ctx.quasar_gateway
                .progression
                .achievements
                .character_achievements
                .read()
                .unwrap()
                .get(&(cid as u32))
                .and_then(|list| list.iter().find(|a| a.def.id == ach_id))
                .and_then(|a| a.condition_progress.get(&cond_id).copied())
                .unwrap_or(0)
        };
        let hostile_conds = conds_for("destroyHostileNPC");
        assert!(!hostile_conds.is_empty(), "expected destroyHostileNPC achievement condition");
        let group_conds = conds_for("entityGroupDestroyed");
        let hostile_before: Vec<u64> = hostile_conds.iter().map(|&(a, c)| cond_val(a, c)).collect();
        let group_before: Vec<u64> = group_conds.iter().map(|&(a, c)| cond_val(a, c)).collect();
        let bal_before = ctx.db.with_snapshot(|s| s.characters[&cid].balance);

        let out = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: npc_id,
                victim_char_id: cid,
                victim_ship_id: npc_id,
                sys_id,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect("tracked NPC destroy succeeds");

        let wreck_ball = handle
            .get_ball_snapshot(out.wreck_id)
            .await
            .unwrap()
            .expect("wreck ball spawned");
        assert_eq!(wreck_ball.1.position, live_pos, "wreck ball must use live NPC position");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.item_positions.get(&out.wreck_id).copied(),
                Some([live_pos.x, live_pos.y, live_pos.z]),
                "persisted wreck position must use live NPC position"
            );
            assert_eq!(snap.corp_projects[&pid].current_quantity, 1);
            assert_eq!(
                snap.characters[&cid].balance.raw_cents() - bal_before.raw_cents(),
                IskCents::from_isk(10_000).raw_cents(),
                "auto-payout must credit 1x 10,000 ISK per kill, not 2x"
            );
        });
        for (&(ach_id, cond_id), &prev) in hostile_conds.iter().zip(&hostile_before) {
            assert_eq!(
                cond_val(ach_id, cond_id),
                prev + 1,
                "destroyHostileNPC condition ({ach_id}, {cond_id}) must increment by 1"
            );
        }
        for (&(ach_id, cond_id), &prev) in group_conds.iter().zip(&group_before) {
            assert_eq!(
                cond_val(ach_id, cond_id),
                prev + 1,
                "entityGroupDestroyed condition ({ach_id}, {cond_id}) must increment by 1"
            );
        }
    }
}
