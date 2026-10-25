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

    let victim_char_opt = player_target_info.map(|(vcid, _)| vcid);
    let is_pod_target = target_ball.type_id == 670;
    let victim_id = victim_char_opt.unwrap_or(target_ball_id);
    let wreck_owner_id = if victim_char_opt.is_some() { victim_id } else { attacker_char_id };
    let wreck_type_id = ctx.sde.wrecks.get(&target_ball.type_id).copied().unwrap_or(26557);
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

        let mut contained: Vec<eve_db::InventoryItemRecord> = snap.items.values().filter(|i| i.location_id == target_ball_id).cloned().collect();
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
                destroyed_entries.push(eve_db::KillmailItemEntry { type_id: item.type_id, flag: item.flag as u16, qty_dropped: 0, qty_destroyed: item.quantity, singleton: item.singleton });
            }
        }

        if is_pod_target {
            for &imp_tid in &active_implants {
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

    if !is_pod_target && generated_wreck_id > 0 {
        let _ = sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: generated_wreck_id, type_id: wreck_type_id, position: target_ball.position, velocity: DVec3::ZERO,
                    max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 30.0, speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_type_id),
            )
            .await;
        let is_crim = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(wreck_owner_id).is_criminal(now_unix);
        crate::services::combat_crimewatch::register_container_lifetime(ctx, generated_wreck_id, sys_id, wreck_type_id, wreck_owner_id, now_unix, is_crim);
    }

    if is_pod_target {
        if generated_corpse_id > 0 {
            let _ = sys_handle
                .spawn_ball_with_combat(
                    eve_destiny::Ball {
                        id: generated_corpse_id, type_id: 25, position: target_ball.position, velocity: DVec3::ZERO,
                        max_velocity: 0.0, inertia: 1.0, mass: 80.0, radius: 2.0, speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(25),
                )
                .await;
        }
        if let Some((vcid, vship_id)) = player_target_info {
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
            crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
                st.disconnected_ships.remove(&vcid);
            });
            let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(ctx, attacker_char_id, vcid, 670);
            let _ = ctx
                .db
                .respawn_character_at_home_station(vcid, vship_id, &ctx.sde);
        }
    } else if let Some((vcid, vship_id)) = player_target_info {
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, vcid, vship_id, sys_id);
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
            st.disconnected_ships.remove(&vcid);
        });
        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::process_ship_insurance_payout(ctx, vcid, vship_id, target_ball.type_id, false);
        let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(ctx, attacker_char_id, vcid, target_ball.type_id);
        let cap_id = ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&vship_id);
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
            if let Some(vch) = snap.characters.get_mut(&vcid) {
                vch.active_ship_id = cid;
            }
            cid
        });
        let _ = sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: cap_id,
                    type_id: 670,
                    position: target_ball.position,
                    velocity: DVec3::ZERO,
                    max_velocity: 335.0,
                    inertia: 0.25,
                    mass: 16_000.0,
                    radius: 15.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(670),
            )
            .await;
    } else {
        let bounty = ctx
            .sde
            .types
            .get(&target_ball.type_id)
            .and_then(|t| {
                t.default_attributes
                    .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                    .copied()
            })
            .unwrap_or(50_000.0);
        if bounty > 0.0 {
            let old_sec = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&attacker_char_id)
                    .map(|c| c.security_status)
                    .unwrap_or(0.0)
            });
            if old_sec < 5.0 {
                let gain = crate::combat::npc_ai::compute_sec_status_gain(old_sec, bounty);
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
/// Fires repeated cycles against target_id, deducting capacitor and ammo, verifying range/tracking/hit roll,
/// emitting combat logs, updating weapon timers, and applying damage scaled by the current multiplier.
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

        // Verify attacker and target balls exist in system
        let attacker_ball_opt = sys_handle.get_ball_snapshot(state.attacker_ship_id).await.ok().flatten();
        let target_ball_opt = sys_handle.get_ball_snapshot(state.target_id).await.ok().flatten();
        let (Some((_, attacker_ball)), Some((_, target_ball))) = (attacker_ball_opt, target_ball_opt) else {
            stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
            continue;
        };

        // Verify target remains locked if locks exist on the ship
        let locks = sys_handle.get_target_locks(state.attacker_ship_id).await.unwrap_or_default();
        if !locks.is_empty() && !locks.iter().any(|l| l.target_id == state.target_id) {
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
        let optimal_range = mod_type_opt.and_then(|t| t.default_attributes.get(&54).copied()).unwrap_or(20_000.0);
        let falloff_range = mod_type_opt.and_then(|t| t.default_attributes.get(&158).copied()).unwrap_or(5_000.0);
        let tracking_speed = mod_type_opt.and_then(|t| t.default_attributes.get(&160).copied()).unwrap_or(100.0);
        let turret_sig_res = mod_type_opt.and_then(|t| t.default_attributes.get(&620).copied()).unwrap_or(40_000.0);

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
                        stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
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
                    stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                    stopped = true;
                    break;
                }
            }

            // Check optimal + 3*falloff range
            let dist = attacker_ball.position.distance(target_ball.position);
            if dist > optimal_range + 3.0 * falloff_range {
                stop_entropic_spool_and_deactivate(ctx, state.attacker_char_id, ship_id, module_id);
                stopped = true;
                break;
            }

            // Calculate hit probability & damage roll
            let target_sig = sys_handle
                .get_combat_state(state.target_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.signature_radius)
                .unwrap_or(target_ball.radius.max(35.0));
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

            // Advance cycle count and scale multiplier:
            // current_multiplier = (1.0 + cycles * step).min(max_mult)
            state.cycles = state.cycles.saturating_add(1);
            let max_mult = 1.0 + state.max_bonus;
            state.current_multiplier = (1.0 + (state.cycles as f64) * state.per_cycle_bonus).min(max_mult);

            // Apply scaled damage to target
            let scaled_dmg = shot_dmg.scaled(state.current_multiplier);
            let (applied_dmg, destroyed) = if scaled_dmg.total() > 0.0 {
                if let Ok(Some((app, dest, _))) = sys_handle
                    .apply_damage_with_attacker(state.target_id, scaled_dmg, Some(state.attacker_char_id))
                    .await
                {
                    (app, dest)
                } else {
                    (0.0, false)
                }
            } else {
                (0.0, false)
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
            let player_target_info = ctx.db.with_snapshot(|snap| {
                snap.characters.iter().find(|(_, ch)| ch.active_ship_id == state.target_id).map(|(&cid, _)| (cid, state.target_id))
            });
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

            // Trigger weapon timer on attacker
            ctx.crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .trigger_weapon_timer(state.attacker_char_id, ctx.now_unix());

            if destroyed {
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
