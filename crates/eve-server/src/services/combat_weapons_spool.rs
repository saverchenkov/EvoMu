//! `combat_weapons_spool` — Triglavian Entropic Disintegrator spooling mechanics (`feat:modules.turret-precursor`).
//!
//! Entropic Disintegrators spool up damage multiplier per server cycle tick while firing
//! continuously at the same target, up to a maximum spool cap.
//! The spool multiplier resets to 1.0 if the target is switched or the module is deactivated.
//! Activating an already-active module on the same target returns an `AlreadyActive` error.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use glam::DVec3;
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
    });
    if let Ok(mut fittings) = ctx.ship_fittings.write() {
        if let Some(fit) = fittings.get_mut(&ship_id) {
            let _ = fit.deactivate_module(module_id, &ctx.sde);
        }
    }
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnModulesDeactivating",
        "charid",
        vec![PyRep::List(vec![PyRep::Integer(module_id)])],
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![PyRep::Integer(module_id), PyRep::String("online".into()), PyRep::Bool(false)],
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
) -> (i64, i64, i64, String) {
    let mut generated_kill_id: i64 = 0;
    let mut generated_wreck_id: i64 = 0;
    let mut generated_corpse_id: i64 = 0;
    let generated_km_hash;

    let is_non_ship_space_obj = ctx.db.with_snapshot(|snap| {
        snap.items.get(&target_ball_id).is_some_and(|it| {
            let cat_id = ctx
                .sde
                .types
                .get(&it.type_id)
                .map(|t| t.category_id)
                .unwrap_or(6);
            matches!(cat_id, 2 | 18)
        })
    });
    if is_non_ship_space_obj {
        crate::services::combat_crimewatch::combat_missiles_aoe::cleanup_destroyed_non_ship_ball(
            ctx,
            sys_id,
            target_ball_id,
        )
        .await;
        return (0, 0, 0, String::new());
    }

    let victim_char_opt = player_target_info.and_then(|(vcid, _)| {
        crate::services::combat_crimewatch::combat_missiles_aoe::active_ship_victim_char_id(
            ctx,
            target_ball_id,
        )
        .or(Some(vcid).filter(|&id| {
            ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&id)
                    .is_some_and(|c| c.active_ship_id == target_ball_id)
            })
        }))
    });
    let is_pod_target = target_ball.type_id == 670;
    let victim_id = victim_char_opt.unwrap_or(target_ball_id);
    let wreck_owner_id = if victim_char_opt.is_some() { victim_id } else { attacker_char_id };
    let wreck_type_id = ctx.sde.wrecks.get(&target_ball.type_id).copied().unwrap_or_else(|| {
        match crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(ctx, target_ball.type_id) {
            "Drifter" => 37531,
            "Sleeper" => 30495,
            _ => 26557,
        }
    });
    let fitted_mods: Vec<(i64, u32, u16)> = ctx
        .ship_fittings
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&target_ball_id)
        .map(|f| f.modules.values().map(|m| (m.item_id, m.type_id, m.flag_id)).collect())
        .unwrap_or_default();
    let (vic_corp, vic_all, active_implants) = ctx.db.with_snapshot(|snap| {
        let (c, a) = snap
            .characters
            .get(&victim_id)
            .map(|ch| (ch.corporation_id, ch.alliance_id))
            .unwrap_or((1_000_125, None));
        let imps = snap
            .active_implants
            .get(&victim_id)
            .cloned()
            .unwrap_or_default();
        (c, a, imps)
    });

    let mut dropped_entries = Vec::new();
    let mut destroyed_entries = Vec::new();
    let mut escape_frigate_id: Option<i64> = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        generated_wreck_id = snap.next_entity_id;
        if !is_pod_target {
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap,
                generated_wreck_id,
                now_unix,
                victim_char_opt.is_none(),
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
                item_id: mid,
                type_id: mtype,
                owner_id: wreck_owner_id,
                location_id: target_ball_id,
                flag: eve_db::InventoryFlag::from_u16(mflag),
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            });
        }

        let courier_cids: Vec<i64> = snap
            .contracts
            .values()
            .filter(|c| {
                c.contract_type == "Courier"
                    && c.status == "InProgress"
                    && c.item_ids.iter().any(|iid| {
                        let Some(it) = snap.items.get(iid) else { return false; };
                        it.location_id == target_ball_id
                            || snap.items.get(&it.location_id).is_some_and(|p| p.location_id == target_ball_id)
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

        // parity: feat:ships.frigate-escape-bay
        escape_frigate_id = if !is_pod_target {
            let has_feb = ctx
                .sde
                .types
                .get(&target_ball.type_id)
                .and_then(|t| t.default_attributes.get(&3020).copied())
                .unwrap_or(0.0)
                > 0.0;
            if has_feb {
                snap.items
                    .values()
                    .find(|i| {
                        i.location_id == target_ball_id
                            && i.singleton
                            && (i.flag as u16) == 179
                            && ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                                matches!(
                                    t.group_id,
                                    25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001
                                )
                            })
                    })
                    .map(|i| i.item_id)
            } else {
                None
            }
        } else {
            None
        };

        let mut contained: Vec<eve_db::InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| i.location_id == target_ball_id && Some(i.item_id) != escape_frigate_id)
            .cloned()
            .collect();
        contained.sort_by_key(|i| i.item_id);

        let drop_threshold = if ctx.sde.solar_systems.get(&sys_id).is_some_and(|s| s.region_id == 10_000_070) { 0.60 } else { 0.50 };
        let hull_isk = ctx.sde.types.get(&target_ball.type_id).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(if is_pod_target { 1_000.0 } else { 250_000.0 });
        let mut items_isk = 0.0;

        for item in contained {
            let unit_isk = ctx.sde.types.get(&item.type_id).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(100.0);
            items_isk += unit_isk * (item.quantity.max(1) as f64);
            let is_rig_or_sub = matches!(item.flag as i64, 89 | 92..=94 | 125..=132);
            let roll = (((now_unix ^ (target_ball_id as u64)).wrapping_add((item.item_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as f64 / 4_294_967_296.0;
            if !is_pod_target && !is_rig_or_sub && roll < drop_threshold {
                if let Some(it_mut) = snap.items.get_mut(&item.item_id) {
                    it_mut.location_id = generated_wreck_id;
                    it_mut.owner_id = wreck_owner_id;
                    it_mut.flag = eve_db::InventoryFlag::Cargo;
                    it_mut.version += 1;
                }
                dropped_entries.push(eve_db::KillmailItemEntry { type_id: item.type_id, flag: item.flag as u16, qty_dropped: item.quantity, qty_destroyed: 0, singleton: item.singleton });
            } else {
                snap.items.remove(&item.item_id);
                let orphan_ids: Vec<i64> = snap.items.values().filter(|c| c.location_id == item.item_id).map(|c| c.item_id).collect();
                for oid in orphan_ids {
                    if let Some(orph) = snap.items.remove(&oid) {
                        destroyed_entries.push(eve_db::KillmailItemEntry { type_id: orph.type_id, flag: orph.flag as u16, qty_dropped: 0, qty_destroyed: orph.quantity, singleton: orph.singleton });
                    }
                }
                destroyed_entries.push(eve_db::KillmailItemEntry { type_id: item.type_id, flag: item.flag as u16, qty_dropped: 0, qty_destroyed: item.quantity, singleton: item.singleton });
            }
        }
        if let Some(esc_id) = escape_frigate_id {
            if let Some(esc_item) = snap.items.get(&esc_id) {
                // parity: feat:ships.frigate-escape-bay - frigate survives and is recorded on killmail
                dropped_entries.push(eve_db::KillmailItemEntry {
                    type_id: esc_item.type_id,
                    flag: 179,
                    qty_dropped: 0,
                    qty_destroyed: 0,
                    singleton: true,
                });
            }
        }

        if is_pod_target {
            let mut imp_tids = active_implants.clone();
            let char_imp_items: Vec<(i64, u32)> = snap
                .items
                .values()
                .filter(|i| i.location_id == victim_id && i.flag == eve_db::InventoryFlag::Implant)
                .map(|i| (i.item_id, i.type_id))
                .collect();
            for (iid, tid) in char_imp_items {
                snap.items.remove(&iid);
                if !imp_tids.contains(&tid) {
                    imp_tids.push(tid);
                }
            }
            for imp_tid in imp_tids {
                items_isk += ctx.sde.types.get(&imp_tid).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(1_000.0);
                destroyed_entries.push(eve_db::KillmailItemEntry { type_id: imp_tid, flag: eve_db::InventoryFlag::Implant as u16, qty_dropped: 0, qty_destroyed: 1, singleton: true });
            }
            if let Some(imps) = snap.active_implants.get_mut(&victim_id) {
                imps.clear();
            }
            snap.next_entity_id += 1;
            generated_corpse_id = snap.next_entity_id;
            snap.items.insert(
                generated_corpse_id,
                eve_db::InventoryItemRecord {
                    item_id: generated_corpse_id, type_id: 25, owner_id: victim_id, location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, version: 1,
                    dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0,
                    is_blueprint_copy: false, blueprint_runs_remaining: None,
                },
            );
            snap.item_positions.insert(generated_corpse_id, [target_ball.position.x, target_ball.position.y, target_ball.position.z]);
        }

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
    });

    let _ = sys_handle.remove_ball(target_ball_id).await;
    if !is_pod_target && generated_wreck_id > 0 {
        let _ = sys_handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: generated_wreck_id, type_id: wreck_type_id, position: target_ball.position, velocity: DVec3::ZERO,
                    max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 30.0, speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_type_id),
                eve_destiny::SlimItemMeta {
                    group_id: 186, category_id: 2, owner_id: wreck_owner_id, char_id: None,
                    corp_id: -1, alliance_id: -1, security_status: 0.0, is_cloaked: false,
                },
            )
            .await;
        let is_crim = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(wreck_owner_id).is_criminal(now_unix);
        crate::services::combat_crimewatch::register_container_lifetime(ctx, generated_wreck_id, sys_id, wreck_type_id, wreck_owner_id, now_unix, is_crim);
    }

    if is_pod_target {
        if generated_corpse_id > 0 {
            let (corpse_grp, corpse_cat) = ctx
                .sde
                .types
                .get(&25)
                .map(|t| (t.group_id, t.category_id))
                .unwrap_or((14, 2));
            let _ = sys_handle
                .spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: generated_corpse_id, type_id: 25, position: target_ball.position, velocity: DVec3::ZERO,
                        max_velocity: 0.0, inertia: 1.0, mass: 80.0, radius: 2.0, speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(25),
                    eve_destiny::SlimItemMeta {
                        group_id: corpse_grp, category_id: corpse_cat, owner_id: victim_id, char_id: None,
                        corp_id: -1, alliance_id: -1, security_status: 0.0, is_cloaked: false,
                    },
                )
                .await;
        }
        if let Some(vcid) = victim_char_opt {
            let vship_id = target_ball_id;
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
            crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
                st.disconnected_ships.remove(&vcid);
            });
            let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(ctx, attacker_char_id, vcid, 670);
            crate::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(
                ctx,
                attacker_char_id,
                vcid,
                sys_id,
                applied_dmg.max(1000.0), // DERIVED: final-blow damage floor for PG damageDealt stats
            );
            let cleared_zarzakh_gate = ctx.db.with_mut_snapshot(|snap| {
                snap.zarzakh_gate_locks.remove(&vcid).map(|(gid, _)| gid)
            });
            if let Some(locked_gate_id) = cleared_zarzakh_gate {
                crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
                    &ctx.quasar_gateway,
                    vcid,
                    30_100_000,
                    locked_gate_id,
                    now_unix,
                );
            }
            crate::services::station_structure::structure_lifecycle::prepare_home_station_respawn(ctx, vcid);
            let _ = ctx
                .db
                .respawn_character_at_home_station(vcid, vship_id, &ctx.sde);
            let new_sys_id = ctx
                .db
                .with_snapshot(|s| s.characters.get(&vcid).map(|c| c.solar_system_id))
                .unwrap_or(sys_id);
            crate::services::space_destiny::record_session_change_timer(
                ctx,
                vcid,
                now_unix,
                new_sys_id,
                0,
                "podding",
            );
            crate::services::map_service::record_solar_system_visit(ctx, vcid, new_sys_id);
        }
    } else if let Some(vcid) = victim_char_opt {
        let vship_id = target_ball_id;
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
            st.disconnected_ships.remove(&vcid);
        });
        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::process_ship_insurance_payout(ctx, vcid, vship_id, target_ball.type_id, false);
        let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(ctx, attacker_char_id, vcid, target_ball.type_id);
        crate::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(
            ctx,
            attacker_char_id,
            vcid,
            sys_id,
            applied_dmg.max(1000.0), // DERIVED: final-blow damage floor for PG damageDealt stats
        );
        let (cap_id, esc_type_id) = ctx.db.with_mut_snapshot(|snap| {
            let esc_opt = escape_frigate_id.and_then(|esc_id| snap.items.get(&esc_id).map(|i| (i.item_id, i.type_id)));
            snap.items.remove(&vship_id);
            snap.item_positions.remove(&vship_id);
            if let Some((esc_id, esc_tid)) = esc_opt {
                if let Some(esc_item) = snap.items.get_mut(&esc_id) {
                    esc_item.location_id = sys_id as i64;
                    esc_item.flag = eve_db::InventoryFlag::None;
                    esc_item.owner_id = vcid;
                    esc_item.version += 1;
                }
                snap.item_positions.insert(
                    esc_id,
                    [
                        target_ball.position.x,
                        target_ball.position.y,
                        target_ball.position.z,
                    ],
                );
                if let Some(vch) = snap.characters.get_mut(&vcid) {
                    vch.active_ship_id = esc_id;
                    vch.version += 1;
                    vch.system_epoch += 1;
                }
                return (esc_id, esc_tid);
            }
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            snap.items.insert(
                cid,
                eve_db::InventoryItemRecord {
                    item_id: cid,
                    type_id: 670,
                    owner_id: vcid,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
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
                cid,
                [
                    target_ball.position.x,
                    target_ball.position.y,
                    target_ball.position.z,
                ],
            );
            if let Some(vch) = snap.characters.get_mut(&vcid) {
                vch.active_ship_id = cid;
                vch.version += 1;
                vch.system_epoch += 1;
            }
            (cid, 670)
        });
        let esc_sde = ctx.sde.types.get(&esc_type_id);
        let esc_grp = esc_sde.map(|t| t.group_id).unwrap_or(29);
        let _ = sys_handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: cap_id,
                    type_id: esc_type_id,
                    position: target_ball.position,
                    velocity: DVec3::ZERO,
                    max_velocity: esc_sde
                        .and_then(|t| t.default_attributes.get(&37).copied())
                        .unwrap_or(335.0),
                    inertia: esc_sde
                        .and_then(|t| t.default_attributes.get(&70).copied())
                        .unwrap_or(0.25),
                    mass: esc_sde.map(|t| t.mass.max(1_000.0)).unwrap_or(16_000.0),
                    radius: esc_sde.map(|t| t.radius.max(15.0)).unwrap_or(15.0),
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(vcid, cap_id, esc_type_id),
                eve_destiny::SlimItemMeta {
                    group_id: esc_grp,
                    category_id: 6,
                    owner_id: vcid,
                    char_id: Some(vcid),
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                },
            )
            .await;
        crate::services::space_destiny::record_session_change_timer(
            ctx,
            vcid,
            now_unix,
            sys_id,
            0,
            "ship_loss",
        );
        crate::services::fw_gameplay::on_player_ship_destroyed_in_combat(
            ctx,
            sys_id,
            attacker_char_id,
            vcid,
            target_ball.type_id,
        );
    } else {
        if (32_000_000..33_000_000).contains(&sys_id) {
            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(
                &ctx.systems,
                &ctx.db,
                sys_id,
                target_ball_id,
            )
            .await;
        }
        let npc_arch = crate::services::combat_crimewatch::combat_npc_encounters::populate_npc_wreck_on_turret_kill(
            ctx,
            sys_id,
            target_ball_id,
            target_ball.type_id,
            target_ball.position,
            generated_wreck_id,
            attacker_char_id,
            &mut dropped_entries,
        )
        .await;
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
        ctx.db.with_mut_snapshot(|snap| {
            let is_unpiloted_hull = snap.items.get(&target_ball_id).is_some_and(|it| {
                ctx.sde
                    .types
                    .get(&it.type_id)
                    .is_some_and(|t| t.category_id == 6)
            });
            if is_unpiloted_hull {
                snap.items.remove(&target_ball_id);
                snap.item_positions.remove(&target_ball_id);
            }
        });
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
            with_mut_combat_runtime(ctx, |rt| {
                let _ = rt.destroyed_npc_cache.remove(&target_ball_id);
                if attacker_char_id > 0 && rt.bounty_credited_balls.insert(target_ball_id) {
                    *rt.pending_bounty_ticks
                        .entry((sys_id, attacker_char_id))
                        .or_insert(0.0) += eff_bounty;
                }
            });
            let old_sec = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&attacker_char_id)
                    .map(|c| c.security_status)
                    .unwrap_or(0.0)
            });
            if old_sec < 5.0 {
                let gain = crate::combat::npc_ai::compute_sec_status_gain(old_sec, bounty) * supp_mult;
                let new_sec = (old_sec + gain).min(5.0).clamp(-10.0, 10.0);
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&attacker_char_id) {
                        ch.security_status = new_sec;
                    }
                });
                with_mut_combat_runtime(ctx, |rt| {
                    rt.sec_status_transactions
                        .entry(attacker_char_id)
                        .or_default()
                        .push(crate::services::combat_crimewatch::SecStatusTransaction {
                            timestamp_unix: now_unix,
                            old_sec,
                            new_sec,
                            delta: new_sec - old_sec,
                            reason: "NPC Pirate Bounty".into(),
                            solar_system_id: sys_id,
                        });
                });
            }
        }
    }
    if let Some(sh_info) = crate::services::insurgency_services::on_insurgency_stronghold_destroyed_core(&ctx.systems, &ctx.db, sys_id, target_ball_id, Some(attacker_char_id)).await {
        crate::services::insurgency_services::notify_stronghold_destruction(
            &ctx.db,
            Some(&ctx.quasar_gateway),
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
            &sh_info,
        );
    }

    let valid_assists: Vec<crate::services::combat_crimewatch::AssistLogEntry> = with_mut_combat_runtime(ctx, |rt| {
        let mut out = Vec::new();
        for key in [target_ball_id, victim_id] {
            if let Some(list) = rt.aggression_assist_log.remove(&key) {
                for e in list {
                    if now_unix.saturating_sub(e.last_action_unix)
                        <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && !out
                            .iter()
                            .any(|x: &crate::services::combat_crimewatch::AssistLogEntry| x.attacker_char_id == e.attacker_char_id)
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

    (generated_kill_id, generated_wreck_id, generated_corpse_id, generated_km_hash)
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
    let active_spool_keys: Vec<(i64, i64)> = {
        let mut guard = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        ctx.db.with_snapshot(|s| guard.sync_from_persisted(&s.combat_crimewatch_state));
        guard
            .entropic_spools
            .iter()
            .filter(|(_, state)| state.solar_system_id == solar_system_id)
            .map(|(&key, _)| key)
            .collect()
    };

    if active_spool_keys.is_empty() {
        return;
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);

    for (ship_id, module_id) in active_spool_keys {
        let spool_state = ctx
            .combat_ephemeral
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entropic_spools
            .get(&(ship_id, module_id))
            .cloned();
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
            // Pre-validate range
            let dist = if is_upwell {
                (attacker_ball.position.distance(target_ball.position) - target_ball.radius - attacker_ball.radius).max(0.0)
            } else {
                attacker_ball.position.distance(target_ball.position)
            };
            if dist > optimal_range + 3.0 * falloff_range {
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            }

            // Resolve loaded charge
            let ammo_cid = ctx.db.with_snapshot(|snap| {
                state
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
                    })
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
            state.charge_item_id = ctx.db.with_mut_snapshot(|snap| {
                match snap.items.get_mut(&ammo_cid) {
                    Some(ci) if ci.quantity > 1 => {
                        ci.quantity -= 1;
                        ci.version += 1;
                        Some(ammo_cid)
                    }
                    _ => {
                        snap.items.remove(&ammo_cid);
                        None
                    }
                }
            });

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
            ctx.combat_ephemeral
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entropic_spools
                .insert((ship_id, module_id), state);
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
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnWeaponGroupDestroyed",
            "charid",
            vec![PyRep::Integer(ship_id), PyRep::Integer(master_id)],
        );
    }
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnWeaponBanksChanged",
        "charid",
        vec![PyRep::Integer(ship_id), info.clone()],
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
