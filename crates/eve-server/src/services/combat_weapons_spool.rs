//! `combat_weapons_spool` — Triglavian Entropic Disintegrator spooling mechanics (`feat:modules.turret-precursor`).
//!
//! Entropic Disintegrators spool up damage multiplier per server cycle tick while firing
//! continuously at the same target, up to a maximum spool cap.
//! The spool multiplier resets to 1.0 if the target is switched or the module is deactivated.
//! Activating an already-active module on the same target returns an `AlreadyActive` error.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use glam::DVec3;
use crate::router::notify_registry::{
    OnGodmaShipEffect, OnModulesDeactivating, OnWeaponBanksChanged, OnWeaponGroupDestroyed,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, EntropicSpoolState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::InvType;

/// Gets the current spool multiplier for an entropic module on a ship (defaults to 1.0).
// parity: feat:modules.entropic-disintegrator-damage-spool
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

/// Fully stops an entropic spool and deactivates the module:
/// clears spool runtime state, removes from DB active modules/charges, clears active flag on fitting,
/// and emits OnModulesDeactivating and OnGodmaShipEffect deactivation notifications.
pub fn stop_entropic_spool_and_deactivate(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
) {
    clear_entropic_spool(ctx, ship_id, module_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&module_id);
        snap.module_active_charges.remove(&module_id);
        crate::router::ServerContext::clear_persisted_module_overload(snap, ship_id, module_id);
    });
    if let Ok(mut fittings) = ctx.ship_fittings.write() {
        if let Some(fit) = fittings.get_mut(&ship_id) {
            let _ = fit.deactivate_module(module_id, &ctx.sde);
        }
    }
    let _ = ctx.invalidate_and_push_fit(ship_id);
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        OnModulesDeactivating {
            module_ids: vec![module_id],
        },
    );
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        OnGodmaShipEffect {
            item_id: module_id,
            effect: PyRep::String("online".into()),
            start_or_time: PyRep::Bool(false),
            start: None,
            active: None,
            environment: None,
            start_time: None,
            duration: None,
            repeat: None,
            error: None,
            actual_stop_time: None,
        },
    );
}

/// Clears active entropic disintegrator spools and deactivates modules when target locks are lost or cleared.
/// If `target_id_opt` is Some(tid), only spools targeting `tid` are stopped; if None, all spools for `ship_id` are stopped.
pub fn clear_entropic_spools_on_target_unlock(
    ctx: &ServerContext,
    ship_id: i64,
    char_id: i64,
    target_id_opt: Option<i64>,
) {
    let to_stop: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.entropic_spools
            .iter()
            .filter(|((s, _), st)| *s == ship_id && target_id_opt.map_or(true, |tid| st.target_id == tid))
            .map(|((_, m), _)| *m)
            .collect()
    });
    for mod_id in to_stop {
        stop_entropic_spool_and_deactivate(ctx, char_id, ship_id, mod_id);
    }
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

    ctx.db.with_snapshot(|snap| -> Result<(), BridgeRpcError> {
        if let Some(cid) = charge_item_id {
            if let Some(ci) = snap.items.get(&cid) {
                ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ci.type_id)?;
            }
        } else if let Some(&ctid) = snap.module_active_charges.get(&module_id) {
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid)?;
        } else if let Some(mod_flag) = snap.items.get(&module_id).map(|m| m.flag) {
            if let Some(ci) = snap.items.values().find(|i| {
                i.location_id == ship_id
                    && i.flag == mod_flag
                    && i.item_id != module_id
                    && i.quantity > 0
                    && ctx.sde.types.get(&i.type_id).is_some_and(|ct| {
                        ServerContext::is_charge_compatible_with_weapon(mod_type, ct)
                    })
            }) {
                ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ci.type_id)?;
            }
        }
        Ok(())
    })?;

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
                    client_error: None,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShipOrNpcDestructionResult {
    pub kill_id: i64,
    pub wreck_id: i64,
    pub corpse_id: i64,
    pub killmail_hash: String,
}

/// Handles ship or NPC destruction: wreck creation, Loot Fairy, pod/capsule ejection,
/// assist log pruning, killmail generation, and bounties/insurance.
pub async fn handle_ship_or_npc_destruction(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_char_id: i64,
    _attacker_ship_id: i64,
    _attacker_ship_type_id: u32,
    _weapon_type_id: u32,
    target_ball_id: i64,
    target_ball: &eve_destiny::Ball,
    player_target_info: Option<(i64, i64)>,
    sys_id: u32,
    applied_dmg: f64,
    now_unix: u64,
) -> ShipOrNpcDestructionResult {
    let mut generated_kill_id: i64 = 0;
    let mut generated_wreck_id: i64 = 0;
    let generated_km_hash;

    let is_active_fighter = ctx.active_fighters.read().is_ok_and(|fighters| {
        fighters
            .values()
            .any(|sq| sq.ball_id == target_ball_id || sq.item_id == target_ball_id)
    });
    let ball_cat_id = ctx
        .sde
        .types
        .get(&target_ball.type_id)
        .map(|t| t.category_id)
        .unwrap_or(6);
    let (target_item_cat_id_opt, target_item_owner_opt, target_item_corp_all_opt) =
        ctx.db.with_snapshot(|snap| {
            let Some(it) = snap.items.get(&target_ball_id) else {
                return (None, None, None);
            };
            let cat_id = ctx
                .sde
                .types
                .get(&it.type_id)
                .map(|t| t.category_id)
                .unwrap_or(6);
            let corp_all = snap
                .characters
                .get(&it.owner_id)
                .map(|c| (c.corporation_id, c.alliance_id))
                .or_else(|| {
                    snap.corporations
                        .get(&it.owner_id)
                        .map(|corp| (corp.corporation_id, corp.alliance_id))
                });
            (Some(cat_id), Some(it.owner_id), corp_all)
        });
    if is_active_fighter
        || target_item_cat_id_opt.is_some_and(|cat_id| matches!(cat_id, 2 | 8 | 18 | 65))
    {
        if is_active_fighter {
            crate::combat::death_pipeline::cleanup_destroyed_non_ship_ball_sync(
                ctx,
                sys_id,
                target_ball_id,
            );
            let _ = sys_handle.remove_ball(target_ball_id).await;
        } else {
            crate::services::combat_crimewatch::combat_missiles_aoe::cleanup_destroyed_non_ship_ball(
                ctx,
                sys_id,
                target_ball_id,
            )
            .await;
        }
        return ShipOrNpcDestructionResult::default();
    }

    crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship(ctx, target_ball_id);
    let already_settled_by_death_pipeline = ctx.db.with_snapshot(|snap| {
        snap.committed_idempotency_keys
            .contains(&(target_ball_id, "ship_insurance_payout".to_string()))
    });
    if already_settled_by_death_pipeline {
        return ShipOrNpcDestructionResult::default();
    }
    let active_ship_vcid =
        crate::services::combat_crimewatch::combat_missiles_aoe::active_ship_victim_char_id(
            ctx,
            target_ball_id,
        );
    let is_corp_ship =
        crate::services::combat_crimewatch::combat_missiles_aoe::is_corp_owned_ship_ball(
            ctx,
            target_ball_id,
        );
    if target_item_cat_id_opt == Some(6) {
        let victim_char_opt = if is_corp_ship {
            Some(0)
        } else {
            active_ship_vcid.or_else(|| player_target_info.map(|(vcid, _)| vcid))
        };
        if let Some(vcid) = victim_char_opt {
            let is_pod_target = target_ball.type_id == 670
                || target_ball.type_id == 33328
                || ctx
                    .sde
                    .types
                    .get(&target_ball.type_id)
                    .is_some_and(|t| t.group_id == 29);
            return match crate::combat::death_pipeline::execute_death_pipeline(
                ctx,
                sys_id,
                vcid,
                target_ball_id,
                attacker_char_id,
                target_ball.position,
                0,
            )
            .await
            {
                Ok(dp) => {
                    let killmail_hash = with_combat_runtime(ctx, |rt| {
                        rt.detailed_killmails
                            .get(&dp.killmail_id)
                            .map(|k| k.killmail_hash.clone())
                    })
                    .unwrap_or_default();
                    ShipOrNpcDestructionResult {
                        kill_id: dp.killmail_id,
                        wreck_id: if is_pod_target { 0 } else { dp.wreck_item_id },
                        corpse_id: if is_pod_target { dp.wreck_item_id } else { 0 },
                        killmail_hash,
                    }
                }
                Err(_) => ShipOrNpcDestructionResult::default(),
            };
        }
    }

    let already_cleaned_actor_deployable = target_item_cat_id_opt.is_none()
        && ball_cat_id == 22
        && !with_combat_runtime(ctx, |rt| rt.is_recently_destroyed_item_id(target_ball_id));
    if target_item_cat_id_opt.is_none()
        && !already_cleaned_actor_deployable
        && (player_target_info.is_some() || active_ship_vcid.is_some())
    {
        return ShipOrNpcDestructionResult::default();
    }

    let is_non_ship_structure_or_deployable = already_cleaned_actor_deployable
        || target_item_cat_id_opt.is_some_and(|cat_id| cat_id != 6);
    if is_non_ship_structure_or_deployable {
        crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(
            ctx,
            target_ball_id,
        );
        crate::services::notify_emitters_seasons_tactical::on_deployable_deactivated(
            ctx,
            None,
            target_ball_id,
        );
    }
    let is_abyssal_npc_or_cache =
        !is_non_ship_structure_or_deployable && (32_000_000..=32_999_999).contains(&sys_id);
    let victim_id = if is_non_ship_structure_or_deployable {
        target_item_owner_opt
            .or_else(|| player_target_info.map(|(vcid, _)| vcid))
            .filter(|&id| id > 0)
            .unwrap_or(target_ball_id)
    } else {
        target_ball_id
    };
    let wreck_owner_id = attacker_char_id;
    let wreck_type_id = ctx.sde.resolve_wreck_type_id(target_ball.type_id, None).unwrap_or_else(|| {
        match crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(ctx, target_ball.type_id) {
            "Drifter" => 37531,
            "Sleeper" => 30495,
            _ => 26557,
        }
    });
    let wreck_name = ctx
        .sde
        .types
        .get(&wreck_type_id)
        .map(|t| t.name.clone())
        .filter(|n| !n.is_empty());
    let fitted_mods: Vec<(i64, u32, u16)> = ctx
        .ship_fittings
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&target_ball_id)
        .map(|f| f.modules.values().map(|m| (m.item_id, m.type_id, m.flag_id)).collect())
        .unwrap_or_default();
    let nested_fitted_mods =
        crate::combat::death_pipeline::drain_nested_ship_fittings(ctx, target_ball_id, true);
    let (vic_corp, vic_all) = if let Some((c, a)) = target_item_corp_all_opt {
        (c, a)
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&victim_id)
                .map(|ch| (ch.corporation_id, ch.alliance_id))
                .or_else(|| {
                    snap.corporations
                        .get(&victim_id)
                        .map(|corp| (corp.corporation_id, corp.alliance_id))
                })
                .unwrap_or((1_000_125, None))
        })
    };

    let drop_threshold = crate::services::security_mgr::insurgency_pvp_loot_drop_threshold(
        ctx,
        sys_id,
        attacker_char_id,
        if is_non_ship_structure_or_deployable {
            player_target_info.map(|(vcid, _)| vcid).unwrap_or(0)
        } else {
            0
        },
        false,
    );
    let mut dropped_entries = Vec::new();
    let mut destroyed_entries = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        if !is_abyssal_npc_or_cache && !already_cleaned_actor_deployable {
            snap.next_entity_id += 1;
            generated_wreck_id = snap.next_entity_id;
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap,
                generated_wreck_id,
                now_unix,
                true,
            );
            snap.items.insert(
                generated_wreck_id,
                eve_db::InventoryItemRecord {
                    item_id: generated_wreck_id,
                    type_id: wreck_type_id,
                    owner_id: wreck_owner_id,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.item_positions.insert(
                generated_wreck_id,
                [
                    target_ball.position.x,
                    target_ball.position.y,
                    target_ball.position.z,
                ],
            );
        }
        for &(mid, mtype, mflag) in &fitted_mods {
            snap.items.entry(mid).or_insert_with(|| eve_db::InventoryItemRecord {
                item_id: mid, type_id: mtype, owner_id: wreck_owner_id, location_id: target_ball_id,
                flag: eve_db::InventoryFlag::from_u16(mflag), quantity: 1, singleton: true, version: 1,
                ..Default::default()
            });
        }
        for &(sid, mid, mtype, mflag) in &nested_fitted_mods {
            snap.items.entry(mid).or_insert_with(|| eve_db::InventoryItemRecord {
                item_id: mid, type_id: mtype, owner_id: wreck_owner_id, location_id: sid,
                flag: eve_db::InventoryFlag::from_u16(mflag), quantity: 1, singleton: true, version: 1,
                ..Default::default()
            });
        }

        let courier_cids: Vec<i64> = snap
            .contracts
            .values()
            .filter(|c| {
                c.contract_type == "Courier"
                    && c.status == "InProgress"
                    && c.item_ids.iter().any(|iid| {
                        crate::combat::death_pipeline::is_item_descendant_of_in_snap(snap, *iid, target_ball_id)
                    })
            })
            .map(|c| c.contract_id)
            .collect();
        for cid in courier_cids {
            snap.fail_courier_contract(cid, now_unix);
        }
        let wrap_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == target_ball_id && i.type_id == 3468)
            .map(|i| i.item_id)
            .collect();
        for wid in wrap_ids {
            let nested_ids: Vec<i64> = snap.items.values().filter(|i| i.location_id == wid).map(|i| i.item_id).collect();
            for nid in nested_ids {
                if let Some(nit) = snap.items.get_mut(&nid) {
                    nit.location_id = target_ball_id;
                    nit.owner_id = wreck_owner_id;
                    nit.flag = eve_db::InventoryFlag::Cargo;
                }
            }
            snap.items.remove(&wid);
        }

        let hull_isk = ctx.sde.types.get(&target_ball.type_id).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(250_000.0);
        let items_isk = if already_cleaned_actor_deployable {
            0.0
        } else {
            let loot_out = crate::combat::death_pipeline::resolve_destroyed_ship_contents_in_snap(
                snap,
                &ctx.sde,
                target_ball_id,
                generated_wreck_id,
                Some(wreck_owner_id),
                None,
                false,
                drop_threshold,
                |_| true,
                |iid| {
                    (((now_unix ^ (target_ball_id as u64)).wrapping_add((iid as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as f64 / 4_294_967_296.0
                },
            );
            dropped_entries.extend(loot_out.dropped_km);
            destroyed_entries.extend(loot_out.destroyed_km);
            loot_out.items_isk
        };

        snap.next_entity_id += 1;
        generated_kill_id = snap.next_entity_id;
        snap.record_killmail(
            eve_db::KillmailRecord {
                kill_id: generated_kill_id, victim_character_id: victim_id, attacker_character_id: attacker_char_id,
                solar_system_id: sys_id, ship_type_id: target_ball.type_id, total_value_isk: hull_isk + items_isk,
                ..Default::default()
            },
            ctx.now_unix(),
        );
        let remove_target_item = snap.items.get(&target_ball_id).is_some_and(|it| {
            ctx.sde
                .types
                .get(&it.type_id)
                .is_some_and(|t| t.category_id == 6)
        }) || is_non_ship_structure_or_deployable;
        if remove_target_item {
            snap.items.remove(&target_ball_id);
            snap.item_positions.remove(&target_ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&target_ball_id);
            snap.sp6_lifecycle.ship_combat_hp.remove(&target_ball_id);
            crate::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
                st.deployables.remove(&target_ball_id);
            });
            for offices in snap.corp_customs_offices.values_mut() {
                offices.retain(|o| o.office_id != target_ball_id);
            }
        }
    });

    let _ = sys_handle.remove_ball(target_ball_id).await;
    if !is_abyssal_npc_or_cache && generated_wreck_id > 0 {
        let _ = sys_handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: generated_wreck_id, type_id: wreck_type_id, position: target_ball.position, velocity: DVec3::ZERO,
                    max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 30.0, speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_type_id),
                eve_destiny::SlimItemMeta {
                    group_id: 186,
                    category_id: 2,
                    owner_id: wreck_owner_id,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: wreck_name,
                    ..Default::default()
                },
            )
            .await;
        let is_crim = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(wreck_owner_id).is_criminal(now_unix);
        crate::services::combat_crimewatch::register_container_lifetime(ctx, generated_wreck_id, sys_id, wreck_type_id, wreck_owner_id, now_unix, is_crim);
    }

    {
        if (32_000_000..33_000_000).contains(&sys_id) {
            crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(
                &ctx.systems,
                &ctx.db,
                sys_id,
                target_ball_id,
                (attacker_char_id > 0).then_some(attacker_char_id),
            )
            .await;
            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(
                &ctx.systems,
                &ctx.db,
                sys_id,
                target_ball_id,
            )
            .await;
        }
        let npc_arch = if !is_non_ship_structure_or_deployable && generated_wreck_id > 0 {
            crate::services::combat_crimewatch::combat_npc_encounters::populate_npc_wreck_on_turret_kill(
                ctx,
                sys_id,
                target_ball_id,
                target_ball.type_id,
                target_ball.position,
                generated_wreck_id,
                attacker_char_id,
                &mut dropped_entries,
            )
            .await
        } else {
            String::new()
        };
        crate::services::sov_gameplay::on_space_entity_destroyed(
            ctx,
            sys_id,
            (attacker_char_id > 0).then_some(attacker_char_id),
            target_ball_id,
            target_ball.type_id,
        );
        crate::services::fw_gameplay::on_space_entity_destroyed(
            ctx,
            sys_id,
            (attacker_char_id > 0).then_some(attacker_char_id),
            target_ball_id,
            target_ball.type_id,
        )
        .await;
        if target_ball.type_id == crate::services::pve_fob_observatory::WORLD_ARK_BOSS_TYPE_ID {
            crate::services::pve_fob_observatory::on_world_ark_boss_killed(
                ctx,
                sys_id,
                attacker_char_id,
            )
            .await;
        }
        crate::services::agents_missions::on_mission_npc_destroyed_core(
            &ctx.db,
            sys_id,
            target_ball_id,
            (attacker_char_id > 0).then_some(attacker_char_id),
        );
        let bounty = if sys_id >= 31_000_000 || npc_arch == "Sleeper" {
            0.0
        } else {
            ctx.sde
                .types
                .get(&target_ball.type_id)
                .and_then(|t| {
                    t.default_attributes
                        .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                        .copied()
                })
                .unwrap_or(0.0)
        };
        if bounty > 0.0 {
            let supp_mult = crate::services::security_mgr::insurgency_suppression_bounty_and_sec_multiplier(ctx, sys_id);
            let eff_bounty = bounty * supp_mult;
            let bounty_newly_credited = with_mut_combat_runtime(ctx, |rt| {
                let _ = rt.destroyed_npc_cache.remove(&target_ball_id);
                if attacker_char_id > 0 && rt.bounty_credited_balls.insert(target_ball_id) {
                    rt.pending_bounty_tick_started_unix
                        .entry((sys_id, attacker_char_id))
                        .or_insert(now_unix);
                    *rt.pending_bounty_ticks
                        .entry((sys_id, attacker_char_id))
                        .or_insert(0.0) += eff_bounty;
                    rt.bounty_tick_next_flush_unix
                        .entry((sys_id, attacker_char_id))
                        .or_insert_with(|| now_unix.saturating_add(1_200));
                    true
                } else {
                    false
                }
            });
            let old_sec = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&attacker_char_id)
                    .map(|c| c.security_status)
                    .unwrap_or(0.0)
            });
            if bounty_newly_credited && old_sec < 5.0 {
                let gain = crate::combat::npc_ai::compute_sec_status_gain(old_sec, bounty) * supp_mult;
                let new_sec = (old_sec + gain).min(5.0).clamp(-10.0, 10.0);
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&attacker_char_id) {
                        ch.security_status = new_sec;
                    }
                });
                with_mut_combat_runtime(ctx, |rt| {
                    rt.sec_status_transactions.entry(attacker_char_id).or_default().push(
                        crate::services::combat_crimewatch::SecStatusTransaction {
                            timestamp_unix: now_unix, old_sec, new_sec, delta: new_sec - old_sec,
                            reason: "NPC Pirate Bounty".into(), solar_system_id: sys_id,
                            event_type_id: crate::services::combat_crimewatch::crimewatch_timers::EVENT_SEC_STATUS_KILL_PIRATE_NPC, other_owner_id: None, other_type_id: Some(target_ball.type_id),
                        },
                    );
                });
            }
        }
    }
    if let Some((sh_info, res_opt)) = crate::services::insurgency_services::on_insurgency_stronghold_destroyed_with_sde(&ctx.systems, &ctx.db, Some(&ctx.sde), sys_id, target_ball_id, Some(attacker_char_id)).await {
        if let Some(res) = res_opt {
            crate::services::fw_gameplay::fw_gameplay_insurgency::emit_campaign_resolution_effects_core(
                &ctx.db,
                Some(&ctx.quasar_gateway),
                Some(ctx.pending_notifications.as_ref()),
                Some(&ctx.notify_tx),
                &res,
            );
        }
        crate::services::insurgency_services::notify_stronghold_destruction(&ctx.db, Some(&ctx.quasar_gateway), Some(&ctx.pending_notifications), Some(&ctx.notify_tx), &sh_info);
    }

    let valid_assists: Vec<crate::services::combat_crimewatch::AssistLogEntry> = with_mut_combat_runtime(ctx, |rt| {
        if is_non_ship_structure_or_deployable {
            rt.container_lifetimes.remove(&target_ball_id);
            rt.record_destroyed_item_id(target_ball_id);
        }
        let assist_keys = if is_non_ship_structure_or_deployable || victim_id == target_ball_id {
            [target_ball_id, target_ball_id]
        } else {
            [target_ball_id, victim_id]
        };
        let mut out = Vec::new();
        for key in assist_keys {
            if let Some(list) = rt.aggression_assist_log.remove(&key) {
                for e in list {
                    if now_unix.saturating_sub(e.last_action_unix) <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && !out.iter().any(|x: &crate::services::combat_crimewatch::AssistLogEntry| x.attacker_char_id == e.attacker_char_id)
                    {
                        out.push(e);
                    }
                }
            }
        }
        out
    });

    let km_attackers: Vec<eve_db::KillmailAttackerEntry> = valid_assists
        .iter()
        .map(|a| eve_db::KillmailAttackerEntry {
            character_id: a.attacker_char_id,
            corporation_id: a.corporation_id,
            alliance_id: a.alliance_id,
            ship_type_id: a.ship_type_id,
            weapon_type_id: a.weapon_type_id,
            damage_done: a.damage_done,
            final_blow: a.attacker_char_id == attacker_char_id,
            security_status: 0.0,
        })
        .collect();
    let total_dmg_taken: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
    let km_total_isk = ctx
        .db
        .with_snapshot(|s| s.killmails.get(&generated_kill_id).map(|k| k.total_value_isk))
        .unwrap_or(250_000.0);
    generated_km_hash = crate::services::combat_crimewatch::compute_killmail_esi_hash(victim_id, attacker_char_id, target_ball.type_id, now_unix);
    with_mut_combat_runtime(ctx, |rt| {
        rt.detailed_killmails.insert(
            generated_kill_id,
            eve_db::DetailedKillmail {
                kill_id: generated_kill_id,
                kill_time_unix: now_unix,
                solar_system_id: sys_id,
                victim_character_id: victim_id,
                victim_corporation_id: vic_corp,
                victim_alliance_id: vic_all,
                victim_ship_type_id: target_ball.type_id,
                victim_damage_taken: total_dmg_taken.max(applied_dmg),
                victim_position: [target_ball.position.x, target_ball.position.y, target_ball.position.z],
                attackers: km_attackers,
                dropped_items: dropped_entries,
                destroyed_items: destroyed_entries,
                total_value_isk: km_total_isk,
                killmail_hash: generated_km_hash.clone(),
            },
        );
    });
    ShipOrNpcDestructionResult {
        kill_id: generated_kill_id,
        wreck_id: generated_wreck_id,
        corpse_id: 0,
        killmail_hash: generated_km_hash,
    }
}

/// Steps all active entropic disintegrator spools in a solar system by `dt` seconds.
/// - Validates target lock, distance, ammo, capacitor
/// - Applies continuous cycle damage scaling upward with each consecutive cycle
/// - Renews CrimeWatch weapon and PvP/aggression timers
/// - On destruction: invokes canonical death pipeline (handle_ship_or_npc_destruction)
/// - On cycle stop or destruction: stop_entropic_spool_and_deactivate
pub async fn step_system_entropic_spools(
    ctx: &ServerContext,
    solar_system_id: u32,
    dt: f64,
) {
    crate::services::combat_crimewatch::combat_npc_encounters::finalize_cycle_destroyed_npcs(ctx, solar_system_id).await;
    let active_spool_keys: Vec<(i64, i64)> =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |guard| {
            guard
                .entropic_spools
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
        let spool_state = with_combat_runtime(ctx, |guard| {
            guard.entropic_spools.get(&(ship_id, module_id)).cloned()
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

        // Verify attacker and target balls exist in system
        let attacker_ball_opt = sys_handle.get_ball_snapshot(state.attacker_ship_id).await.ok().flatten();
        let target_ball_opt = sys_handle.get_ball_snapshot(state.target_id).await.ok().flatten();
        let (Some((_, attacker_ball)), Some((_, target_ball))) = (attacker_ball_opt, target_ball_opt) else {
            stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
            continue;
        };

        // Verify target remains locked on the ship
        let locks = sys_handle.get_target_locks(state.attacker_ship_id).await.unwrap_or_default();
        if !locks.iter().any(|l| l.target_id == state.target_id) {
            stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
            continue;
        }

        // Verify target is alive via structure_hp > 0.0
        let target_alive = sys_handle
            .get_combat_state(state.target_id)
            .await
            .ok()
            .flatten()
            .map(|cs| cs.structure_hp > 0.0)
            .unwrap_or(false);

        if !target_alive {
            stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
            continue;
        }

        let mod_type_opt = ctx.sde.types.get(&state.module_type_id);
        let (range_mult, tracking_mult) = {
            let active_charge_type_id = ctx.db.with_snapshot(|snap| {
                if let Some(cid) = state.charge_item_id {
                    if let Some(ci) = snap.items.get(&cid).filter(|i| i.quantity > 0) {
                        return Some(ci.type_id);
                    }
                }
                let mod_flag = snap.items.get(&module_id).map(|m| m.flag);
                if let (Some(flag), Some(mod_type)) = (mod_flag, mod_type_opt) {
                    snap.items.values().find(|i| {
                        i.location_id == ship_id
                            && i.flag == flag
                            && i.item_id != module_id
                            && i.quantity > 0
                            && ctx.sde.types.get(&i.type_id).is_some_and(|ct| ServerContext::is_charge_compatible_with_weapon(mod_type, ct))
                    }).map(|i| i.type_id)
                } else {
                    None
                }
            });
            if let Some(ct_id) = active_charge_type_id {
                ctx.sde.types.get(&ct_id).map(|ct| {
                    let rm = ct.default_attributes.get(&120).copied().unwrap_or(1.0);
                    let tm = ct.default_attributes.get(&244).copied().unwrap_or(1.0);
                    (rm, tm)
                }).unwrap_or((1.0, 1.0))
            } else {
                (1.0, 1.0)
            }
        };
        let optimal_range = mod_type_opt.and_then(|t| t.default_attributes.get(&54).copied()).unwrap_or(5_000.0) * range_mult;
        let falloff_range = mod_type_opt.and_then(|t| t.default_attributes.get(&158).copied()).unwrap_or(5_000.0);
        let tracking_speed = mod_type_opt.and_then(|t| t.default_attributes.get(&160).copied()).unwrap_or(100.0) * tracking_mult;
        let turret_sig_res = mod_type_opt.and_then(|t| t.default_attributes.get(&620).copied()).unwrap_or(40_000.0);

        state.elapsed_in_cycle += dt;
        let mut stopped = false;

        while state.elapsed_in_cycle >= state.duration_s - 1e-6 {
            state.elapsed_in_cycle -= state.duration_s;
            if state.elapsed_in_cycle < 0.0 {
                state.elapsed_in_cycle = 0.0;
            }

            let now_unix = ctx.now_unix();
            let is_upwell = match crate::services::station_structure::structure_lifecycle_rpc::validate_upwell_structure_attack_precheck(
                ctx, solar_system_id, state.attacker_char_id, state.target_id, now_unix,
            ) {
                Ok(v) => v,
                Err(_) => {
                    let _ = sys_handle.remove_target_lock(ship_id, state.target_id).await;
                    stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                    stopped = true;
                    break;
                }
            };
            // Pre-validate range using surface-to-surface distance for all hulls (#1247).
            let dist = attacker_ball.surface_distance(&target_ball);
            if dist > optimal_range + 3.0 * falloff_range {
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            }

            // Resolve loaded charge
            let ammo_cid = ctx.db.with_snapshot(|snap| {
                let cand_id = state
                    .charge_item_id
                    .filter(|cid| snap.items.get(cid).is_some_and(|i| i.quantity > 0))
                    .or_else(|| {
                        let mod_flag = snap.items.get(&module_id).map(|m| m.flag)?;
                        let mod_type = mod_type_opt?;
                        snap.items.values().find(|i| {
                            i.location_id == ship_id
                                && i.flag == mod_flag
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|ct| ServerContext::is_charge_compatible_with_weapon(mod_type, ct))
                        }).map(|i| i.item_id)
                    })?;
                let ci = snap.items.get(&cand_id)?;
                ctx.check_type_skill_prerequisites_in_snapshot(snap, state.attacker_char_id, ci.type_id)
                    .ok()?;
                Some(cand_id)
            });
            let Some(ammo_cid) = ammo_cid else {
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            };

            // Deduct capacitor
            if state.cap_need > 0.0 {
                if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
                    if cs.capacitor + 1e-6 < state.cap_need {
                        stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                        stopped = true;
                        break;
                    }
                    cs.capacitor = (cs.capacitor - state.cap_need).max(0.0);
                    let _ = sys_handle.set_combat_state(ship_id, cs).await;
                }
            }

            // Consume one round
            let (next_charge_id, ammo_diff) = ctx.db.with_mut_snapshot(|snap| {
                let Some(ci) = snap.items.get_mut(&ammo_cid) else {
                    snap.module_active_charges.remove(&module_id);
                    return (None, None);
                };
                let pre_rec = ci.clone();
                if ci.quantity > 1 {
                    ci.quantity -= 1;
                    ci.version += 1;
                    let post_rec = ci.clone();
                    (Some(ammo_cid), Some((Some(pre_rec), Some(post_rec))))
                } else {
                    snap.items.remove(&ammo_cid);
                    snap.module_active_charges.remove(&module_id);
                    (None, Some((Some(pre_rec), None)))
                }
            });
            state.charge_item_id = next_charge_id;
            if let Some((pre_rec, post_rec)) = ammo_diff {
                let depleted = post_rec.is_none();
                crate::services::inventory_fitting::fitting_module_cycles::emit_ship_charge_item_diffs(
                    ctx,
                    state.attacker_char_id,
                    ship_id,
                    &[(pre_rec, post_rec)],
                );
                if depleted {
                    ctx.invalidate_and_push_fit(ship_id);
                }
            }

            // Target signature with player MWD bloom
            let player_target_info = ctx.db.with_snapshot(|snap| {
                snap.characters.iter().find(|(_, ch)| ch.active_ship_id == state.target_id).map(|(&cid, _)| (cid, state.target_id))
            });
            let target_sig = if let Some((vcid, _)) = player_target_info {
                crate::services::combat_crimewatch::evaluate_effective_signature_radius(ctx, vcid, state.target_id, solar_system_id).await
            } else if let Ok(Some(t_cs)) = sys_handle.get_combat_state(state.target_id).await {
                if t_cs.signature_radius > 0.0 {
                    t_cs.signature_radius
                } else {
                    target_ball.radius.max(35.0)
                }
            } else {
                target_ball.radius.max(35.0)
            };

            let angular_vel = eve_destiny::combat::compute_angular_velocity(
                attacker_ball.position,
                attacker_ball.velocity,
                target_ball.position,
                target_ball.velocity,
            );
            let p_hit = crate::formulas::turret_chance_to_hit_with_res(
                angular_vel,
                tracking_speed,
                turret_sig_res,
                target_sig,
                dist,
                optimal_range,
                falloff_range,
            );
            let hit_roll = ctx.next_invention_roll(state.attacker_ship_id as u64, state.target_id as u64, state.cycles as u64);
            let (shot_dmg, is_wrecking) = crate::formulas::turret_shot_damage(state.base_damage, p_hit, hit_roll);

            state.cycles = state.cycles.saturating_add(1);
            let max_mult = 1.0 + state.max_bonus;
            state.current_multiplier = (1.0f64 + (state.cycles as f64) * state.per_cycle_bonus).min(max_mult);

            let scaled_dmg = shot_dmg.scaled(state.current_multiplier);
            let (applied_dmg, destroyed, became_invuln) = if scaled_dmg.total() > 0.0 {
                let is_invuln = crate::services::insurgency_services::is_insurgency_stronghold_invulnerable(ctx, solar_system_id, state.target_id);
                if is_invuln {
                    (0.0, false, false)
                } else if let Ok(Some((app, dest, inv))) =
                    crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                        ctx,
                        &sys_handle,
                        solar_system_id,
                        state.attacker_char_id,
                        state.target_id,
                        scaled_dmg,
                        state.duration_s,
                        now_unix,
                    )
                    .await
                {
                    (app, dest, inv)
                } else {
                    (0.0, false, false)
                }
            } else {
                (0.0, false, false)
            };

            // Emit combat log and damage state
            let (sr, ar, hr) = if let Ok(Some(t_cs)) = sys_handle.get_combat_state(state.target_id).await {
                (
                    (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            let victim_char_opt = player_target_info.map(|(vcid, _)| vcid);
            crate::services::combat_crimewatch::emit_combat_log_and_damage_state(
                ctx,
                state.attacker_char_id,
                victim_char_opt,
                state.attacker_ship_id,
                state.target_id,
                state.module_type_id,
                applied_dmg,
                is_wrecking,
                sr,
                ar,
                hr,
            );

            // Record attacker damage in assist log
            if applied_dmg > 0.0 {
                let now_unix = ctx.now_unix();
                crate::services::combat_crimewatch::combat_wrecks_killmails::record_killmail_assist_action(
                    ctx,
                    state.target_id,
                    state.attacker_char_id,
                    attacker_ball.type_id,
                    state.module_type_id,
                    applied_dmg,
                    false,
                    now_unix,
                );
                if let Some(vcid) = victim_char_opt {
                    if vcid != state.target_id {
                        crate::services::combat_crimewatch::combat_wrecks_killmails::record_killmail_assist_action(
                            ctx,
                            vcid,
                            state.attacker_char_id,
                            attacker_ball.type_id,
                            state.module_type_id,
                            applied_dmg,
                            false,
                            now_unix,
                        );
                    }
                }
            }

            // Renew CrimeWatch weapon timer and PvP/logoff timers
            let now_unix = ctx.now_unix();
            ctx.crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .trigger_weapon_timer(state.attacker_char_id, now_unix);

            if let Some(vcid) = victim_char_opt {
                {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    cw.get_or_create(state.attacker_char_id).pvp_timer_until_unix =
                        now_unix.saturating_add(900);
                    cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
                }
                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                    ctx,
                    state.attacker_char_id,
                    vcid,
                    now_unix,
                );
            } else {
                crate::sp6_lifecycle::record_npc_aggression_timer(ctx, state.attacker_char_id, now_unix);
            }

            if destroyed && !is_upwell {
                handle_ship_or_npc_destruction(
                    ctx,
                    &sys_handle,
                    state.attacker_char_id,
                    state.attacker_ship_id,
                    attacker_ball.type_id,
                    state.module_type_id,
                    state.target_id,
                    &target_ball,
                    player_target_info,
                    solar_system_id,
                    applied_dmg,
                    ctx.now_unix(),
                ).await;
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            } else if became_invuln {
                let _ = sys_handle.remove_target_lock(ship_id, state.target_id).await;
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            }
        }

        if !stopped {
            with_mut_combat_runtime(ctx, |rt| {
                rt.entropic_spools.insert((ship_id, module_id), state);
            });
        }
    }
}

/// Builds the `{master_id: [master_id, ...]}` PyRep::Dict for a ship's weapon banks.
pub fn build_weapon_banks_dict(snap: &eve_db::WorldStateSnapshot, ship_id: i64) -> PyRep {
    let mut entries: Vec<(PyRep, PyRep)> = snap
        .weapon_banks
        .get(&ship_id)
        .into_iter()
        .flatten()
        .map(|(&master, mods)| {
            let mod_list = mods.iter().copied().map(PyRep::Integer).collect();
            (PyRep::Integer(master), PyRep::List(mod_list))
        })
        .collect();
    entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
    PyRep::Dict(entries)
}

/// Emits `OnWeaponGroupDestroyed` for any dissolved weapon groups and `OnWeaponBanksChanged`.
pub fn notify_weapon_banks_updated(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    destroyed_masters: &[i64],
    info: &PyRep,
) {
    for &master_id in destroyed_masters {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            OnWeaponGroupDestroyed {
                ship_id,
                item_id: master_id,
            },
        );
    }
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        OnWeaponBanksChanged {
            ship_id,
            info: info.clone(),
        },
    );
}

/// Links all compatible weapon modules into banks (`charFittingMgr.LinkAllWeapons` / `dogmaIM.LinkAllWeapons`).
pub fn handle_link_all_weapons(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let info = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Ship {ship_id} is not owned by {char_id}"),
                client_error: None,
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
        for (_type_id, mut mods) in by_type {
            if mods.len() >= 2 {
                mods.sort();
                let master = mods[0];
                banks.insert(master, mods);
            }
        }
        Ok(build_weapon_banks_dict(snap, ship_id))
    })?;
    notify_weapon_banks_updated(ctx, char_id, ship_id, &[], &info);
    Ok(info)
}

#[cfg(test)]
#[path = "combat_weapons_spool_tests.rs"]
mod combat_weapons_spool_tests;
