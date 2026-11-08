//! `tactical_module_mechanics` — Fitting group limits, Interdiction Probe variants & lifetime,
//! Incremental Nanite Repair Paste consumption, and Thermodynamics rack heat & damage propagation.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_module_cycles::set_modules_reload_until;
use crate::services::tactical_modules::{self, TacticalModulesState};
use crate::system_actor::{ActiveModuleCycle, SystemHandle};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository, WorldStateSnapshot};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ShipFittingEngine};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveInterdictionProbe {
    pub probe_id: i64,
    pub probe_type_id: u32,
    pub solar_system_id: u32,
    pub remaining_s: f64,
    pub is_wubble: bool,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct LauncherFireStamp {
    pub fired_unix: u64,
    pub fired_tick: u64,
    pub fired_sim_s: f64,
}

/// Validates hull group restrictions and `maxGroupFitted` (`attr 1544`) constraints when fitting modules.
pub fn validate_module_fitting_constraints(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    ship_id: i64,
    item_id: i64,
    mod_t: &InvType,
    ship_t: &InvType,
) -> Result<(), BridgeRpcError> {
    if mod_t.group_id == 1672 {
        let hull_ok = matches!(
            ship_t.group_id,
            27 | 30 | 485 | 513 | 547 | 659 | 883 | 898 | 900 | 902 | 941 | 1538 | 4594 | 5120
        );
        if !hull_ok {
            return Err(BridgeRpcError {
                code: "CanNotFitModuleToShip".into(),
                message: format!(
                    "UserError: Heavy Stasis Grappler cannot be fitted to {} (group {})",
                    ship_t.name, ship_t.group_id
                ),
            });
        }
    }
    let max_group_fitted = mod_t
        .default_attributes
        .get(&1544)
        .copied()
        .map(|v| v.round().max(1.0) as usize)
        .or_else(|| {
            if matches!(mod_t.group_id, 4117 | 589 | 1672 | 2018) {
                Some(1)
            } else {
                None
            }
        });
    if let Some(max_count) = max_group_fitted {
        let fitted_same_group = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.item_id != item_id
                    && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| t.group_id == mod_t.group_id)
            })
            .count();
        if fitted_same_group >= max_count {
            let code = if mod_t.group_id == 1672 {
                "MaxGroupFitted"
            } else {
                "FittingValidationFailed"
            };
            return Err(BridgeRpcError {
                code: code.into(),
                message: format!(
                    "UserError: MaxGroupFittedExceeded — maximum of {} {} can be fitted per ship",
                    max_count, mod_t.name
                ),
            });
        }
    }
    Ok(())
}

/// Validates and launches an Interdiction Sphere Launcher (`group 589`) probe (`22778` Warp Disrupt Probe,
/// `34260` Surgical Warp Disrupt Probe, `41402` Phobetor Warp Disrupt Probe, `56748` Stasis Webification Probe),
/// enforcing firing cadence (`5 s`), launcher capacity (`15 m3`), `60 s` reload when empty, spawning the
/// probe ball in `sys`, and registering its lifetime (`120 s` / `180 s` / `30 s`).
#[allow(clippy::too_many_arguments)]
pub async fn activate_interdiction_sphere_launcher(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    module_id: i64,
    m: &FittedModule,
    mod_type: &InvType,
    effect_name: &str,
    target_id_opt: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    if sys_id == 30_100_000 && mod_type.default_attributes.get(&5561).copied().unwrap_or(0.0) > 0.0 {
        return Err(BridgeRpcError::user_error("ModuleDisallowedInHazardSystem"));
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let corruption_stage5 =
        crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id) >= 5;
    if sys_sec >= 0.45 || (sys_sec > 0.0 && !corruption_stage5) {
        return Err(BridgeRpcError {
            code: "CannotLaunchInterdictionProbeInEmpire".into(),
            message: format!(
                "Interdiction sphere probes can only be launched in null-sec or wormhole space (system {sys_id} security {sys_sec:.2} > 0.0)"
            ),
        });
    }
    let ship_group_id = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    if ship_group_id != 541 {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Interdiction Sphere Launcher can only be activated on Interdictors (group 541)".into(),
        });
    }

    let Some((current_tick, ship_ball)) = sys.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
        });
    };
    let ship_pos = ship_ball.position;
    let now_unix = ctx.now_unix_secs();
    let cadence_s = mod_type
        .default_attributes
        .get(&51)
        .copied()
        .unwrap_or(5_000.0)
        / 1000.0;
    let db_key = tactical_modules::db_ptr_key(&ctx.db);
    {
        let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
        let current_sim_s = st.system_sim_time_s.get(&(db_key, sys_id)).copied().unwrap_or(0.0);
        if let Some(&stamp) = st.launcher_last_fire.get(&(db_key, module_id)) {
            let elapsed_unix = now_unix.saturating_sub(stamp.fired_unix) as f64;
            let elapsed_ticks = current_tick.saturating_sub(stamp.fired_tick) as f64;
            let elapsed_sim = (current_sim_s - stamp.fired_sim_s).max(0.0);
            if elapsed_unix.max(elapsed_ticks).max(elapsed_sim) + 1e-6 < cadence_s {
                return Err(BridgeRpcError {
                    code: "ModuleReactivationDelay".into(),
                    message: format!(
                        "Interdiction Sphere Launcher firing cadence ({cadence_s:.0}s) has not elapsed"
                    ),
                });
            }
        }
    }

    let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) == m.flag_id
                    && i.item_id != module_id
                    && i.quantity > 0
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| matches!(t.group_id, 548 | 4088))
            })
            .map(|i| (i.item_id, i.type_id))
    });
    let Some((charge_item_id, probe_type_id)) = loaded_charge_opt else {
        return Err(BridgeRpcError {
            code: "NoProbesLoaded".into(),
            message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded".into(),
        });
    };

    let probe_sde = ctx.sde.types.get(&probe_type_id);
    let is_wubble_probe =
        probe_type_id == 56748 || probe_sde.is_some_and(|t| t.group_id == 4088);
    let probe_vol = probe_sde
        .map(|t| t.volume.max(0.1))
        .unwrap_or(if is_wubble_probe { 3.0 } else { 5.0 });
    let launcher_cap = mod_type.capacity.max(15.0);
    let max_probes_in_launcher = (launcher_cap / probe_vol).floor().max(1.0) as i32;

    let bubble_radius = probe_sde
        .and_then(|t| {
            t.default_attributes
                .get(&103)
                .copied()
                .or_else(|| t.default_attributes.get(&2279).copied())
                .or_else(|| t.default_attributes.get(&99).copied())
        })
        .unwrap_or(if is_wubble_probe { 15_000.0 } else { 20_000.0 });
    let lifetime_s = probe_sde
        .and_then(|t| {
            t.default_attributes
                .get(&281)
                .copied()
                .or_else(|| t.default_attributes.get(&2280).copied())
        })
        .map(|ms| ms / 1000.0)
        .unwrap_or(if is_wubble_probe { 30.0 } else { 120.0 });
    let probe_hp = probe_sde
        .and_then(|t| t.default_attributes.get(&9).copied())
        .unwrap_or(1_000.0);
    let probe_group_id = probe_sde
        .map(|t| t.group_id)
        .unwrap_or(if is_wubble_probe { 4088 } else { 548 });

    let (probe_id, remaining_in_launcher, auto_reloaded, ship_items_backup) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let (cur_qty, cur_tid) = match snap.items.get(&charge_item_id) {
                Some(ci)
                    if ci.location_id == ship_id
                        && (ci.flag as u16) == m.flag_id
                        && ci.quantity > 0 =>
                {
                    (ci.quantity, ci.type_id)
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "NoProbesLoaded".into(),
                        message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded"
                            .into(),
                    });
                }
            };
            let ship_items_backup: Vec<(i64, InventoryItemRecord)> = snap
                .items
                .iter()
                .filter(|(_, it)| it.location_id == ship_id)
                .map(|(&k, v)| (k, v.clone()))
                .collect();
            // Return any excess charges above launcher capacity back to Cargo instead of destroying them.
            if cur_qty > max_probes_in_launcher {
                let excess = cur_qty - max_probes_in_launcher;
                let probe_vol = ctx
                    .sde
                    .types
                    .get(&cur_tid)
                    .map(|t| t.volume.max(1e-6))
                    .unwrap_or(1.0);
                let free_m3 = ship_free_cargo_m3_in_snap(snap, &ctx.sde, ship_id);
                let max_fit = ((free_m3 + 1e-9) / probe_vol).floor().max(0.0) as i32;
                if max_fit < excess {
                    return Err(BridgeRpcError {
                        code: "NotEnoughCargoSpace".into(),
                        message: format!(
                            "Not enough cargo space to return {excess} excess probe(s) (free: {free_m3:.2} m3)"
                        ),
                    });
                }
                if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                    ci.quantity = max_probes_in_launcher;
                    ci.version += 1;
                }
                let existing_cargo_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.type_id == cur_tid
                            && !i.singleton
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id);
                if let Some(cid) = existing_cargo_id {
                    if let Some(c_item) = snap.items.get_mut(&cid) {
                        c_item.quantity += excess;
                        c_item.version += 1;
                    }
                } else {
                    snap.next_entity_id += 1;
                    let excess_id = snap.next_entity_id;
                    snap.items.insert(
                        excess_id,
                        InventoryItemRecord {
                            item_id: excess_id,
                            type_id: cur_tid,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag: InventoryFlag::Cargo,
                            quantity: excess,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                }
            }
            let mut rem = if let Some(c_owner) = snap.items.get(&charge_item_id).map(|ci| ci.owner_id) {
                let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), c_owner);
                opts.reject_fitted = false;
                opts.active_ship_id = Some(ship_id);
                match snap.consume_exact_item_quantity(charge_item_id, 1, opts) {
                    Ok(Some(it)) => it.quantity,
                    Ok(None) => 0,
                    Err(e) => {
                        return Err(BridgeRpcError {
                            code: "InventoryError".into(),
                            message: e.to_string(),
                        });
                    }
                }
            } else {
                return Err(BridgeRpcError {
                    code: "NoProbesLoaded".into(),
                    message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded.".into(),
                });
            };
            let mut reloaded_qty = 0;
            if rem == 0 {
                let cargo_probe_opt = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.quantity > 0
                            && i.type_id == probe_type_id
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .or_else(|| {
                        snap.items.values().find(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == InventoryFlag::Cargo
                                && i.quantity > 0
                                && !snap.is_item_locked_in_contract_or_job(i.item_id)
                                && ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .is_some_and(|t| matches!(t.group_id, 548 | 4088))
                        })
                    })
                    .map(|i| (i.item_id, i.type_id));
                if let Some((cargo_id, reload_tid)) = cargo_probe_opt {
                    let r_vol = ctx
                        .sde
                        .types
                        .get(&reload_tid)
                        .map(|t| t.volume.max(0.1))
                        .unwrap_or(probe_vol);
                    let r_cap = (launcher_cap / r_vol).floor().max(1.0) as i32;
                    let take = if let Some((c_owner, c_avail)) = snap.items.get(&cargo_id).map(|c| (c.owner_id, c.quantity)) {
                        let req = c_avail.min(r_cap);
                        if req > 0 {
                            let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), c_owner);
                            opts.active_ship_id = Some(ship_id);
                            if snap.consume_exact_item_quantity(cargo_id, req, opts).is_ok() {
                                req
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    if take > 0 {
                        snap.next_entity_id += 1;
                        let slot_stack_id = snap.next_entity_id;
                        snap.items.insert(
                            slot_stack_id,
                            InventoryItemRecord {
                                item_id: slot_stack_id,
                                type_id: reload_tid,
                                owner_id: char_id,
                                location_id: ship_id,
                                flag: InventoryFlag::from_u16(m.flag_id),
                                quantity: take,
                                singleton: false,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                        rem = take;
                        reloaded_qty = take;
                    }
                }
            }
            snap.next_entity_id += 1;
            let probe_id = snap.next_entity_id;
            Ok((probe_id, rem, reloaded_qty, ship_items_backup))
        })?;

    let probe_ball = Ball {
        id: probe_id,
        type_id: probe_type_id,
        position: ship_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 100.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    let probe_cs = CombatState {
        shield_hp: 0.0,
        max_shield_hp: 0.0,
        armor_hp: 0.0,
        max_armor_hp: 0.0,
        structure_hp: probe_hp,
        max_structure_hp: probe_hp,
        ..CombatState::default()
    };
    if let Err(e) = sys
        .spawn_ball_with_meta(
            probe_ball,
            probe_cs,
            SlimItemMeta {
                group_id: probe_group_id,
                category_id: 8,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 0,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                skin_id: None,
                skin_material_set_id: None,
                jump_target: None,
                ..eve_destiny::SlimItemMeta::default()
            },
        )
        .await
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, it| it.location_id != ship_id);
            for (k, v) in ship_items_backup {
                snap.items.insert(k, v);
            }
        });
        return Err(BridgeRpcError {
            code: "SystemActorError".into(),
            message: e,
        });
    }
    if !is_wubble_probe {
        if let Err(e) = sys.add_interdiction_sphere(probe_id, ship_pos, bubble_radius).await {
            let _ = sys.remove_ball(probe_id).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, it| it.location_id != ship_id);
                for (k, v) in ship_items_backup {
                    snap.items.insert(k, v);
                }
            });
            return Err(BridgeRpcError {
                code: "SystemActorError".into(),
                message: e,
            });
        }
    }

    if is_wubble_probe {
        tactical_modules::register_launched_wubble_probe(
            ctx,
            probe_id,
            probe_type_id,
            sys_id,
            ship_pos,
            bubble_radius,
        );
    }

    if auto_reloaded > 0 {
        let reload_time_s = mod_type
            .default_attributes
            .get(&1795)
            .copied()
            .unwrap_or(60_000.0)
            / 1000.0;
        set_modules_reload_until(
            ctx,
            &[module_id],
            now_unix.saturating_add(reload_time_s.round().max(1.0) as u64),
        );
    }
    if remaining_in_launcher == 0 || auto_reloaded > 0 {
        ctx.invalidate_ship_fitting(ship_id);
    }

    {
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        let current_sim_s = st.system_sim_time_s.get(&(db_key, sys_id)).copied().unwrap_or(0.0);
        st.launcher_last_fire.insert(
            (db_key, module_id),
            LauncherFireStamp {
                fired_unix: now_unix,
                fired_tick: current_tick,
                fired_sim_s: current_sim_s,
            },
        );
        st.active_interdiction_probes.insert(
            (db_key, probe_id),
            ActiveInterdictionProbe {
                probe_id,
                probe_type_id,
                solar_system_id: sys_id,
                remaining_s: lifetime_s,
                is_wubble: is_wubble_probe,
            },
        );
    }

    let _ = ctx.notify_module_effect_started(
        char_id,
        ship_id,
        module_id,
        m.type_id,
        Some(effect_name),
        target_id_opt,
    );
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("launcherID", PyRep::Integer(module_id)),
        ("probeID", PyRep::Integer(probe_id)),
        ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
        ("isWubble", PyRep::Bool(is_wubble_probe)),
        ("bubbleRadius", PyRep::Real(bubble_radius)),
        ("lifetimeSeconds", PyRep::Real(lifetime_s)),
        ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
        ("reloading", PyRep::Bool(auto_reloaded > 0)),
    ]))
}

/// Returns `true` if `ball_id` in `sys` is an interdiction probe (`group 548 | 4088`), which cannot
/// be targeted directly by weapons/modules (only AoE Smartbombs/Bombs can damage it).
pub async fn is_untargetable_interdiction_probe(
    ctx: &ServerContext,
    sys: &SystemHandle,
    ball_id: i64,
) -> bool {
    if let Ok(Some((_, b))) = sys.get_ball_snapshot(ball_id).await {
        return ctx
            .sde
            .types
            .get(&b.type_id)
            .is_some_and(|t| matches!(t.group_id, 548 | 4088));
    }
    false
}

/// Cleans up warp bubble and probe tracking when `probe_id` is destroyed or removed from `ballpark`.
pub fn on_probe_ball_removed_in_actor(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<WorldRepository>>,
    tactical_state: &RwLock<TacticalModulesState>,
    probe_id: BallId,
) {
    ballpark.remove_warp_bubble(probe_id);
    if let Some(db_ref) = db_opt {
        let db_key = tactical_modules::db_ptr_key(db_ref);
        let mut st = tactical_state.write().unwrap_or_else(|e| e.into_inner());
        st.active_interdiction_probes.remove(&(db_key, probe_id));
        st.pending_wubble_probes
            .retain(|&(k, ref p)| !(k == db_key && p.probe_id == probe_id));
    }
}

/// Advances lifetimes of active interdiction probes in `ballpark` and despawns expired probes & bubbles.
pub fn step_interdiction_probes_lifetime(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<WorldRepository>>,
    tactical_state: &RwLock<TacticalModulesState>,
    dt: f64,
) {
    let Some(db_ref) = db_opt else {
        return;
    };
    let db_key = tactical_modules::db_ptr_key(db_ref);
    let mut expired_ids = Vec::new();
    {
        let mut st = tactical_state.write().unwrap_or_else(|e| e.into_inner());
        *st.system_sim_time_s
            .entry((db_key, ballpark.solar_system_id))
            .or_insert(0.0) += dt;
        for (&(k, pid), probe) in st.active_interdiction_probes.iter_mut() {
            if k == db_key && probe.solar_system_id == ballpark.solar_system_id {
                probe.remaining_s -= dt;
                if probe.remaining_s <= 1e-6 {
                    expired_ids.push(pid);
                }
            }
        }
        for &pid in &expired_ids {
            st.active_interdiction_probes.remove(&(db_key, pid));
            st.pending_wubble_probes
                .retain(|&(k, ref p)| !(k == db_key && p.probe_id == pid));
        }
    }
    for pid in expired_ids {
        ballpark.remove_warp_bubble(pid);
        let _ = ballpark.remove_ball(pid);
    }
}

/// Deducts up to `amount` unlocked Nanite Repair Paste (`type_id == 28668`) from `InventoryFlag::Cargo`,
/// returning the exact quantity deducted (`0..=amount`) for streaming per-tick module repair.
pub(crate) fn deduct_paste_from_cargo_in_snap(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    amount: i32,
) -> i32 {
    if amount <= 0 {
        return 0;
    }
    let mut stack_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|i| {
            i.owner_id == char_id
                && i.location_id == ship_id
                && i.flag == InventoryFlag::Cargo
                && i.type_id == 28668
                && i.quantity > 0
                && !snap.is_item_locked_in_contract_or_job(i.item_id)
        })
        .map(|i| i.item_id)
        .collect();
    stack_ids.sort_unstable();
    let mut remaining_to_take = amount;
    let mut taken_total = 0;
    for pid in stack_ids {
        if remaining_to_take <= 0 {
            break;
        }
        let mut empty = false;
        if let Some(p_item) = snap.items.get_mut(&pid) {
            let take = p_item.quantity.min(remaining_to_take);
            p_item.quantity -= take;
            p_item.version += 1;
            remaining_to_take -= take;
            taken_total += take;
            empty = p_item.quantity == 0;
        }
        if empty {
            snap.items.remove(&pid);
        }
    }
    taken_total
}

/// Computes remaining free cargo capacity (`m3`) on `ship_id` (`hull_capacity - used_cargo_m3`),
/// accounting for fitted Cargohold Expanders (`attr 38` / `attr 614`) via `ShipFittingEngine`.
pub(crate) fn ship_free_cargo_m3_in_snap(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    ship_id: i64,
) -> f64 {
    let Some(ship_item) = snap.items.get(&ship_id) else {
        return 0.0;
    };
    let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, sde);
    if let Some(sk_map) = snap.skills.get(&ship_item.owner_id) {
        for (&skill_tid, rec) in sk_map {
            fit.character_skills.insert(skill_tid, rec.level);
        }
    }
    for (&aid, &val) in &ship_item.dynamic_attributes {
        fit.ship.base_attributes.insert(aid, val);
    }
    for it in snap.items.values().filter(|i| i.location_id == ship_id) {
        let flag_u16 = it.flag as u16;
        if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132)
            && sde.types.get(&it.type_id).is_some_and(|t| matches!(t.category_id, 7 | 32))
        {
            let online = snap.module_online_states.get(&it.item_id).copied().unwrap_or(true);
            let _ = fit.fit_module_with_state(it.item_id, it.type_id, flag_u16, online, sde);
        }
    }
    fit.recalculate_stats(sde);
    let cap_m3 = fit
        .ship
        .get_attribute(38, sde)
        .max(sde.types.get(&ship_item.type_id).map(|t| t.capacity).unwrap_or(0.0));
    let used_m3: f64 = snap
        .items
        .values()
        .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.quantity > 0)
        .map(|i| {
            let unit_vol = sde.types.get(&i.type_id).map(|t| t.volume.max(0.0)).unwrap_or(0.0);
            unit_vol * (i.quantity as f64)
        })
        .sum();
    (cap_m3 - used_m3).max(0.0)
}

/// Refunds unconsumed Nanite Repair Paste (`type_id == 28668`) back into the ship's cargo hold,
/// capping refunded quantity to remaining ship hull cargo capacity (`m3`) so cargo volume can never overflow.
pub(crate) fn refund_paste_to_cargo_capped_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    ship_id: i64,
    amount: i32,
) -> i32 {
    if amount <= 0 {
        return 0;
    }
    let paste_vol = sde
        .types
        .get(&28668)
        .map(|t| t.volume.max(1e-6))
        .unwrap_or(0.01);
    let free_m3 = ship_free_cargo_m3_in_snap(snap, sde, ship_id);
    let max_fit = ((free_m3 + 1e-9) / paste_vol).floor().max(0.0) as i32;
    let refund_qty = amount.min(max_fit);
    if refund_qty <= 0 {
        return 0;
    }
    let existing_stack_id = snap
        .items
        .values()
        .find(|i| {
            i.owner_id == char_id
                && i.location_id == ship_id
                && i.flag == InventoryFlag::Cargo
                && i.type_id == 28668
                && !i.singleton
                && !snap.is_item_locked_in_contract_or_job(i.item_id)
        })
        .map(|i| i.item_id);
    if let Some(pid) = existing_stack_id {
        if let Some(p_item) = snap.items.get_mut(&pid) {
            p_item.quantity += refund_qty;
            p_item.version += 1;
        }
    } else {
        snap.next_entity_id += 1;
        let new_id = snap.next_entity_id;
        snap.items.insert(
            new_id,
            InventoryItemRecord {
                item_id: new_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: refund_qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    }
    refund_qty
}

/// Canonical ship rack heat Dogma attribute IDs: `[heatHi (1175), heatMed (1176), heatLow (1177)]`.
pub const RACK_HEAT_ATTR_IDS: [u16; 3] = [1175, 1176, 1177];

pub struct EvaluatedShipThermodynamics {
    pub heat_gen_mult: f64,
    pub diss_rates: [f64; 3],
    pub att_bases: [f64; 3],
    pub slot_ratio: f64,
}

pub fn evaluate_ship_thermodynamics(
    ship_id: BallId,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
) -> EvaluatedShipThermodynamics {
    let default_eval = EvaluatedShipThermodynamics {
        heat_gen_mult: 1.0,
        diss_rates: [0.01, 0.01, 0.01],
        att_bases: [0.5, 0.5, 0.5],
        slot_ratio: 1.0,
    };
    let (Some(sde), Some(db)) = (sde_opt.as_ref(), db_opt.as_ref()) else {
        return default_eval;
    };
    db.with_snapshot(|snap| {
        let Some(ship_item) = snap.items.get(&ship_id) else {
            return default_eval;
        };
        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, sde);
        if let Some(sk_map) = snap.skills.get(&ship_item.owner_id) {
            for (&skill_tid, rec) in sk_map {
                fit.character_skills.insert(skill_tid, rec.level);
            }
        }
        for (&aid, &val) in &ship_item.dynamic_attributes {
            fit.ship.base_attributes.insert(aid, val);
        }
        let mut online_hi_mid_low = 0usize;
        for it in snap.items.values().filter(|i| i.location_id == ship_id) {
            let flag_u16 = it.flag as u16;
            if !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                continue;
            }
            if !sde
                .types
                .get(&it.type_id)
                .is_some_and(|t| matches!(t.category_id, 7 | 32))
            {
                continue;
            }
            let online = snap.module_online_states.get(&it.item_id).copied().unwrap_or(true);
            let _ = fit.fit_module_with_state(it.item_id, it.type_id, flag_u16, online, sde);
            if online && (11..=34).contains(&flag_u16) {
                online_hi_mid_low += 1;
            }
        }
        fit.recalculate_stats(sde);
        let heat_gen_mult = fit.ship.get_attribute(1224, sde).max(0.0);
        let diss_rates = [
            fit.ship.get_attribute(1179, sde).max(0.0),
            fit.ship.get_attribute(1196, sde).max(0.0),
            fit.ship.get_attribute(1198, sde).max(0.0),
        ];
        let att_bases = [
            fit.ship.get_attribute(1259, sde).clamp(0.0, 1.0),
            fit.ship.get_attribute(1261, sde).clamp(0.0, 1.0),
            fit.ship.get_attribute(1262, sde).clamp(0.0, 1.0),
        ];
        let total_slots = (fit.ship.get_attribute(14, sde).max(0.0)
            + fit.ship.get_attribute(13, sde).max(0.0)
            + fit.ship.get_attribute(12, sde).max(0.0)
            + fit.ship.get_attribute(1137, sde).max(0.0))
        .max(1.0);
        let slot_ratio = ((online_hi_mid_low as f64) / total_slots).clamp(0.0, 1.0);
        EvaluatedShipThermodynamics {
            heat_gen_mult,
            diss_rates,
            att_bases,
            slot_ratio,
        }
    })
}

fn sync_runtime_rack_heat(ship_id: BallId, db_opt: &Option<Arc<WorldRepository>>) -> [f64; 3] {
    let Some(db) = db_opt.as_ref() else {
        return [0.0; 3];
    };
    let has_legacy_dyn = db.with_snapshot(|snap| {
        snap.items.get(&ship_id).is_some_and(|sh| {
            RACK_HEAT_ATTR_IDS
                .iter()
                .any(|aid| sh.dynamic_attributes.contains_key(aid))
        })
    });
    if has_legacy_dyn && !db.is_write_locked_by_current_thread() {
        db.with_mut_snapshot(|snap| {
            let mut migrated = [None; 3];
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                for (idx, &aid) in RACK_HEAT_ATTR_IDS.iter().enumerate() {
                    migrated[idx] = sh.dynamic_attributes.remove(&aid);
                }
            }
            let tr = snap.transient_item_attributes.entry(ship_id).or_default();
            for (idx, &aid) in RACK_HEAT_ATTR_IDS.iter().enumerate() {
                if let Some(val) = migrated[idx] {
                    let clamped = val.clamp(0.0, 1.0);
                    if clamped > 0.0 {
                        tr.insert(aid, clamped);
                    } else {
                        tr.remove(&aid);
                    }
                }
            }
            if tr.is_empty() {
                snap.transient_item_attributes.remove(&ship_id);
            }
        });
    }
    db.with_snapshot(|snap| {
        snap.transient_item_attributes
            .get(&ship_id)
            .map(|tr| {
                [
                    tr.get(&RACK_HEAT_ATTR_IDS[0]).copied().unwrap_or(0.0).clamp(0.0, 1.0),
                    tr.get(&RACK_HEAT_ATTR_IDS[1]).copied().unwrap_or(0.0).clamp(0.0, 1.0),
                    tr.get(&RACK_HEAT_ATTR_IDS[2]).copied().unwrap_or(0.0).clamp(0.0, 1.0),
                ]
            })
            .unwrap_or([0.0; 3])
    })
}

/// Integrates rack heat once per tick (`dt`) across all ships with active overloaded cycles or residual heat,
/// using `H(t + dt) = 1 - (1 - H(t)) * exp(-dt * heatGenerationMultiplier * sum(heatAbsorbtionRateModifier))`
/// when heating and `H(t) * exp(-dt * heatDissipationRate)` when cooling (`feat:modules.overheat-rack-heat-generation`).
pub fn step_rack_heat_per_tick(
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    active_module_cycles: &[ActiveModuleCycle],
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
    dt: f64,
) {
    if dt <= 0.0 || !dt.is_finite() {
        return;
    }
    let mut ship_ids: Vec<BallId> = ship_rack_heat.keys().copied().collect();
    for c in active_module_cycles.iter().filter(|c| c.is_overloaded) {
        if !ship_ids.contains(&c.ship_id) {
            ship_ids.push(c.ship_id);
        }
    }
    ship_ids.sort_unstable();
    for sid in ship_ids {
        let eval = evaluate_ship_thermodynamics(sid, sde_opt, db_opt);
        let runtime_heat = sync_runtime_rack_heat(sid, db_opt);
        let racks = ship_rack_heat.entry(sid).or_insert(runtime_heat);
        if db_opt.is_some() {
            *racks = runtime_heat;
        }
        let mut rack_absorption_sum = [0.0_f64; 3];
        for c in active_module_cycles
            .iter()
            .filter(|c| c.ship_id == sid && c.is_overloaded)
        {
            let r_idx = c.rack_idx.min(2);
            let mod_abs = sde_opt
                .as_ref()
                .and_then(|sde| sde.types.get(&c.module_type_id))
                .and_then(|t| t.default_attributes.get(&1180).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(0.01);
            rack_absorption_sum[r_idx] += mod_abs;
        }
        let mut changed = false;
        for (r_idx, rack_val) in racks.iter_mut().enumerate() {
            let gen_rate = rack_absorption_sum[r_idx] * eval.heat_gen_mult;
            if gen_rate > 0.0 || *rack_val > 0.0 {
                let next = eve_dogma::thermodynamics::step_rack_heat(
                    *rack_val,
                    gen_rate,
                    eval.diss_rates[r_idx],
                    dt,
                );
                *rack_val = if next < 1e-6 && gen_rate <= 0.0 {
                    0.0
                } else {
                    next
                };
                changed = true;
            }
        }
        let heat_snap = *racks;
        if heat_snap.iter().all(|&h| h == 0.0) {
            ship_rack_heat.remove(&sid);
        }
        if changed {
            if let Some(db_ref) = db_opt.as_ref().filter(|db| !db.is_write_locked_by_current_thread()) {
                db_ref.with_mut_snapshot(|snap| {
                    let tr = snap.transient_item_attributes.entry(sid).or_default();
                    for (r_idx, &aid) in RACK_HEAT_ATTR_IDS.iter().enumerate() {
                        if heat_snap[r_idx] > 0.0 {
                            tr.insert(aid, heat_snap[r_idx]);
                        } else {
                            tr.remove(&aid);
                        }
                    }
                    if tr.is_empty() {
                        snap.transient_item_attributes.remove(&sid);
                    }
                });
            }
        }
    }
}

/// Rolls probabilistic heat damage per overloaded cycle (`P = RackHeat * (OnlineSlots / TotalSlots) * heatAttenuation^SlotDistance`)
/// using `MechanicRng` and applies SDE `heatDamage` (`1211`) mitigated by `Thermodynamics` (`28164`, `-5%/lvl`)
/// across online modules in the same rack without wrapping around rack edges (`feat:modules.overheat-damage-propagation`).
#[allow(clippy::too_many_arguments)]
pub fn apply_overheat_rack_and_damage_on_cycle(
    cycle: &ActiveModuleCycle,
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
    mechanic_rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    burned_out: &mut bool,
    burned_ship_modules: &mut Vec<(BallId, i64)>,
) {
    let r_idx = cycle.rack_idx.min(2);
    let eval = evaluate_ship_thermodynamics(cycle.ship_id, sde_opt, db_opt);
    let runtime_heat = sync_runtime_rack_heat(cycle.ship_id, db_opt);
    let current_rack_heat = ship_rack_heat
        .get(&cycle.ship_id)
        .map(|r| r[r_idx].max(runtime_heat[r_idx]))
        .unwrap_or(runtime_heat[r_idx])
        .clamp(0.0, 1.0);
    if current_rack_heat <= 0.0 {
        return;
    }
    let att_base = eval.att_bases[r_idx];
    let slot_ratio = eval.slot_ratio;

    if let Some(db_ref) = db_opt {
        db_ref.with_mut_snapshot(|snap| {
            let owner_id = snap
                .items
                .get(&cycle.ship_id)
                .map(|i| i.owner_id)
                .unwrap_or(0);
            let thermo_lvl = snap
                .skills
                .get(&owner_id)
                .and_then(|sk| sk.get(&28164))
                .map(|r| r.level)
                .unwrap_or(0);
            let eff_dmg = eve_dogma::thermodynamics::effective_heat_damage(
                cycle.heat_damage_per_cycle,
                thermo_lvl,
            );
            if eff_dmg <= 0.0 {
                return;
            }
            let active_flag = snap
                .items
                .get(&cycle.module_id)
                .map(|i| i.flag as u16)
                .unwrap_or(0);
            if !(11..=34).contains(&active_flag) {
                return;
            }
            let mut rack_mods: Vec<(i64, u16, u32)> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == cycle.ship_id
                        && sde_opt
                            .as_ref()
                            .and_then(|sde| sde.types.get(&i.type_id))
                            .map(|t| t.category_id == 7)
                            .unwrap_or(true)
                        && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                        && match (r_idx, i.flag as u16) {
                            (0, 27..=34) => true,
                            (1, 19..=26) => true,
                            (2, 11..=18) => true,
                            _ => false,
                        }
                })
                .map(|i| (i.item_id, i.flag as u16, i.type_id))
                .collect();
            // Sort deterministically by slot flag so seeded RNG draws are reproducible across runs.
            rack_mods.sort_unstable_by_key(|&(mid, flag, _)| (flag, mid));

            for (mod_id, mod_flag, mod_tid) in rack_mods {
                let slot_dist = (mod_flag as isize - active_flag as isize).unsigned_abs() as i32;
                let damage_chance =
                    (current_rack_heat * slot_ratio * att_base.powi(slot_dist)).clamp(0.0, 1.0);
                let hit = damage_chance > 0.0
                    && mechanic_rng_opt
                        .as_ref()
                        .map(|rng| rng.roll(damage_chance))
                        .unwrap_or(false);
                if !hit {
                    continue;
                }
                let default_hp = sde_opt
                    .as_ref()
                    .and_then(|sde| sde.types.get(&mod_tid))
                    .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                    .unwrap_or(40.0);
                let mut mod_burned = false;
                if let Some(m_item) = snap.items.get_mut(&mod_id) {
                    let cur_dmg = m_item
                        .dynamic_attributes
                        .get(&attr_ids::DAMAGE)
                        .copied()
                        .unwrap_or(0.0);
                    let max_hp = m_item
                        .dynamic_attributes
                        .get(&attr_ids::HP_STRUCT)
                        .copied()
                        .unwrap_or(default_hp);
                    let next_dmg = (cur_dmg + eff_dmg).min(max_hp);
                    m_item
                        .dynamic_attributes
                        .insert(attr_ids::DAMAGE, next_dmg);
                    if next_dmg >= max_hp - 1e-6 {
                        mod_burned = true;
                    }
                }
                if mod_burned {
                    snap.module_online_states.insert(mod_id, false);
                    snap.module_active_states.remove(&mod_id);
                    if mod_id == cycle.module_id {
                        *burned_out = true;
                    } else {
                        burned_ship_modules.push((cycle.ship_id, mod_id));
                    }
                }
            }
        });
    }
}

#[cfg(test)]
#[path = "tactical_module_mechanics_tests.rs"]
mod tactical_module_mechanics_tests;

