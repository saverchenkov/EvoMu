//! `tactical_module_mechanics` — Fitting group limits, Interdiction Probe variants & lifetime,
//! Incremental Nanite Repair Paste consumption, and Thermodynamics rack heat & damage propagation.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_module_cycles::set_modules_reload_until;
use crate::services::tactical_modules::{self, TacticalModulesState};
use crate::system_actor::post_unlock::{
    execute_post_unlock_ops_for_standalone, PostUnlockIntent, PostUnlockOps,
};
use crate::system_actor::{ActiveModuleCycle, SystemHandle};
use eve_db::lock_rank::AnyRwLock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository, WorldStateSnapshot};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ShipFittingEngine};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

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
                client_error: None,
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
            let msg = format!(
                "UserError: MaxGroupFittedExceeded — maximum of {} {} can be fitted per ship",
                max_count, mod_t.name
            );
            return Err(if mod_t.group_id == 1672 {
                BridgeRpcError {
                    code: "MaxGroupFitted".into(),
                    message: msg,
                    client_error: None,
                }
            } else {
                BridgeRpcError::fitting_validation_failed(
                    Some(ship_id),
                    Some(item_id),
                    Some(mod_t.type_id as i64),
                    &mod_t.name,
                    msg,
                )
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
            client_error: None,
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
            client_error: None,
        });
    }

    let Some((current_tick, ship_ball)) = sys.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError::ship_not_in_space(
            ship_id,
            Some(char_id),
            Some(sys_id),
            "ActivateInterdictionSphereLauncher",
            format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
        ));
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
                    client_error: None,
                });
            }
        }
    }

    let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) == m.flag_id
                    && i.item_id != module_id
                    && i.quantity > 0
                    && crate::services::fitting_cycles_overheat::is_valid_loaded_assembled_charge(
                        snap,
                        Some(char_id),
                        Some(ship_id),
                        i,
                    )
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| matches!(t.group_id, 548 | 4088))
            })
            .min_by_key(|i| i.item_id)
            .map(|i| (i.item_id, i.type_id))
    });
    let Some((charge_item_id, probe_type_id)) = loaded_charge_opt else {
        return Err(BridgeRpcError {
            code: "NoProbesLoaded".into(),
            message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded".into(),
            client_error: None,
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
                        client_error: None,
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
                    let req_vol = (excess as f64) * probe_vol;
                    return Err(BridgeRpcError::with_client_error(
                        "NotEnoughCargoSpace",
                        format!(
                            "Not enough cargo space to return {excess} excess probe(s) (free: {free_m3:.2} m3)"
                        ),
                        eve_proto::ClientError::hold_capacity_error(
                            eve_proto::UserErrorKey::NotEnoughCargoSpace,
                            req_vol,
                            free_m3.max(0.0),
                        ),
                    ));
                }
                if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                    ci.quantity = max_probes_in_launcher;
                    ci.version += 1;
                }
                let merge_opts = eve_db::ConsumeExactOptions {
                    active_ship_id: Some(ship_id),
                    ..eve_db::ConsumeExactOptions::for_owner_exact(
                        Some(char_id),
                        char_id,
                        eve_db::ItemClassPolicy::StackableCommodity,
                    )
                };
                let existing_cargo_id = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.type_id == cur_tid
                            && i.quantity > 0
                            && i.quantity.checked_add(excess).is_some()
                            && snap.validate_item_for_consumption(i, &merge_opts).is_ok()
                    })
                    .min_by_key(|i| i.item_id)
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
                let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
                    Some(char_id),
                    c_owner,
                    eve_db::ItemClassPolicy::ExactAssembledItem,
                );
                opts.reject_fitted = false;
                opts.active_ship_id = Some(ship_id);
                match snap.consume_exact_item_quantity(charge_item_id, 1, opts) {
                    Ok(Some(it)) => it.quantity,
                    Ok(None) => 0,
                    Err(e) => {
                        return Err(BridgeRpcError {
                            code: "InventoryError".into(),
                            message: e.to_string(),
                            client_error: None,
                        });
                    }
                }
            } else {
                return Err(BridgeRpcError {
                    code: "NoProbesLoaded".into(),
                    message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded.".into(),
                    client_error: None,
                });
            };
            let mut reloaded_qty = 0;
            if rem == 0 {
                let mut cargo_probe_opts = eve_db::ConsumeExactOptions::for_owner_exact(
                    Some(char_id),
                    char_id,
                    eve_db::ItemClassPolicy::StackableCommodity,
                );
                cargo_probe_opts.active_ship_id = Some(ship_id);
                let is_cargo_probe = |i: &InventoryItemRecord| {
                    i.owner_id == char_id
                        && i.location_id == ship_id
                        && i.flag == InventoryFlag::Cargo
                        && i.quantity > 0
                        && snap.validate_item_for_consumption(i, &cargo_probe_opts).is_ok()
                };
                let cargo_probe_opt = snap
                    .items
                    .values()
                    .filter(|i| is_cargo_probe(i) && i.type_id == probe_type_id)
                    .min_by_key(|i| i.item_id)
                    .or_else(|| {
                        snap.items
                            .values()
                            .filter(|i| {
                                is_cargo_probe(i)
                                    && ctx
                                        .sde
                                        .types
                                        .get(&i.type_id)
                                        .is_some_and(|t| matches!(t.group_id, 548 | 4088))
                            })
                            .min_by_key(|i| i.item_id)
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
                    let take = if let Some(c_avail) = snap.items.get(&cargo_id).map(|c| c.quantity) {
                        let req = c_avail.min(r_cap);
                        if req > 0 && snap.consume_exact_item_quantity(cargo_id, req, cargo_probe_opts).is_ok() {
                            req
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
            client_error: None,
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
                client_error: None,
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

    if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) {
        let _ = ctx.notify_module_effect_started(
            scene,
            char_id,
            ship_id,
            module_id,
            m.type_id,
            Some(effect_name),
            target_id_opt,
        );
    }
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
    tactical_state: &dyn AnyRwLock<TacticalModulesState>,
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
    tactical_state: &dyn AnyRwLock<TacticalModulesState>,
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct RackHeatPersistDbOp {
    pub ship_id: BallId,
    pub heat_snap: [f64; 3],
    pub migrate_legacy_dyn_attrs: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OverheatModuleHit {
    pub module_id: i64,
    pub dmg_delta: f64,
    pub max_hp: f64,
    pub burned: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OverheatModuleDamageDbOp {
    pub ship_id: BallId,
    pub updates: Vec<OverheatModuleHit>,
}

pub(crate) fn apply_rack_heat_persist_in_snap(
    snap: &mut WorldStateSnapshot,
    op: &RackHeatPersistDbOp,
) {
    if op.migrate_legacy_dyn_attrs {
        if let Some(sh) = snap.items.get_mut(&op.ship_id) {
            for &aid in &RACK_HEAT_ATTR_IDS {
                sh.dynamic_attributes.remove(&aid);
            }
        }
    }
    let tr = snap.transient_item_attributes.entry(op.ship_id).or_default();
    for (idx, &aid) in RACK_HEAT_ATTR_IDS.iter().enumerate() {
        let val = op.heat_snap[idx].clamp(0.0, 1.0);
        if val > 0.0 {
            tr.insert(aid, val);
        } else {
            tr.remove(&aid);
        }
    }
    if tr.is_empty() {
        snap.transient_item_attributes.remove(&op.ship_id);
    }
}

pub(crate) fn apply_overheat_module_damage_in_snap(
    snap: &mut WorldStateSnapshot,
    op: &OverheatModuleDamageDbOp,
) {
    for u in &op.updates {
        let mut mod_burned = u.burned;
        if let Some(m_item) = snap.items.get_mut(&u.module_id) {
            let cur_dmg = m_item
                .dynamic_attributes
                .get(&attr_ids::DAMAGE)
                .copied()
                .unwrap_or(0.0);
            let max_hp = m_item
                .dynamic_attributes
                .get(&attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(u.max_hp);
            let next_dmg = (cur_dmg + u.dmg_delta).min(max_hp);
            m_item.dynamic_attributes.insert(attr_ids::DAMAGE, next_dmg);
            if next_dmg >= max_hp - 1e-6 {
                mod_burned = true;
            }
        }
        if mod_burned {
            snap.module_online_states.insert(u.module_id, false);
            snap.module_active_states.remove(&u.module_id);
            crate::router::ServerContext::clear_persisted_module_overload(
                snap,
                op.ship_id,
                u.module_id,
            );
        }
    }
}

fn read_runtime_rack_heat_readonly(
    ship_id: BallId,
    db_opt: &Option<Arc<WorldRepository>>,
) -> ([f64; 3], bool) {
    let Some(db) = db_opt.as_ref() else {
        return ([0.0; 3], false);
    };
    db.with_snapshot(|snap| {
        let legacy_vals = snap.items.get(&ship_id).map(|sh| {
            let has_any = RACK_HEAT_ATTR_IDS
                .iter()
                .any(|aid| sh.dynamic_attributes.contains_key(aid));
            let vals = [
                sh.dynamic_attributes
                    .get(&RACK_HEAT_ATTR_IDS[0])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                sh.dynamic_attributes
                    .get(&RACK_HEAT_ATTR_IDS[1])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                sh.dynamic_attributes
                    .get(&RACK_HEAT_ATTR_IDS[2])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
            ];
            (has_any, vals)
        });
        let (has_legacy_dyn, legacy_heat) = legacy_vals.unwrap_or((false, [0.0; 3]));
        let tr_heat = snap.transient_item_attributes.get(&ship_id).map(|tr| {
            [
                tr.get(&RACK_HEAT_ATTR_IDS[0])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                tr.get(&RACK_HEAT_ATTR_IDS[1])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                tr.get(&RACK_HEAT_ATTR_IDS[2])
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
            ]
        });
        let heat = if let Some(tr) = tr_heat {
            if has_legacy_dyn {
                [
                    if tr[0] > 0.0 { tr[0] } else { legacy_heat[0] },
                    if tr[1] > 0.0 { tr[1] } else { legacy_heat[1] },
                    if tr[2] > 0.0 { tr[2] } else { legacy_heat[2] },
                ]
            } else {
                tr
            }
        } else if has_legacy_dyn {
            legacy_heat
        } else {
            [0.0; 3]
        };
        (heat, has_legacy_dyn)
    })
}

/// Integrates rack heat once per tick (`dt`) across all ships with active overloaded cycles or residual heat,
/// collecting DB rack-heat persistence and legacy dynamic-attribute migration into `post_unlock` (`#1170`).
pub(crate) fn step_rack_heat_per_tick_collecting(
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    active_module_cycles: &[ActiveModuleCycle],
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
    dt: f64,
    mut rack_heat_pending: Option<(&mut HashMap<BallId, ([f64; 3], u64)>, u64)>,
    post_unlock: &mut PostUnlockOps,
) {
    if dt <= 0.0 || !dt.is_finite() {
        return;
    }
    let mut ship_ids: Vec<BallId> = ship_rack_heat.keys().copied().collect();
    if let Some((sh, _)) = rack_heat_pending.as_ref() {
        for &sid in sh.keys() {
            if !ship_ids.contains(&sid) {
                ship_ids.push(sid);
            }
        }
    }
    for c in active_module_cycles.iter().filter(|c| c.is_overloaded) {
        if !ship_ids.contains(&c.ship_id) {
            ship_ids.push(c.ship_id);
        }
    }
    ship_ids.sort_unstable();
    for sid in ship_ids {
        let eval = evaluate_ship_thermodynamics(sid, sde_opt, db_opt);
        let pending_heat = rack_heat_pending
            .as_ref()
            .and_then(|(sh, _)| sh.get(&sid).map(|&(h, _)| h));
        let (runtime_heat, needs_legacy_cleanup) = if let Some(h) = pending_heat {
            let has_legacy = db_opt.as_ref().is_some_and(|db| {
                db.with_snapshot(|snap| {
                    snap.items.get(&sid).is_some_and(|sh| {
                        RACK_HEAT_ATTR_IDS
                            .iter()
                            .any(|aid| sh.dynamic_attributes.contains_key(aid))
                    })
                })
            });
            (h, has_legacy)
        } else if db_opt.is_some() {
            read_runtime_rack_heat_readonly(sid, db_opt)
        } else {
            (ship_rack_heat.get(&sid).copied().unwrap_or([0.0; 3]), false)
        };
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
        if (changed || needs_legacy_cleanup) && db_opt.is_some() {
            if let Some((sh, seq)) = rack_heat_pending.as_mut() {
                sh.insert(sid, (heat_snap, *seq));
            }
            post_unlock.push(PostUnlockIntent::RackHeatPersist(RackHeatPersistDbOp {
                ship_id: sid,
                heat_snap,
                migrate_legacy_dyn_attrs: needs_legacy_cleanup,
            }));
        }
    }
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
    let mut ops = PostUnlockOps::default();
    step_rack_heat_per_tick_collecting(
        ship_rack_heat,
        active_module_cycles,
        sde_opt,
        db_opt,
        dt,
        None,
        &mut ops,
    );
    execute_post_unlock_ops_for_standalone(
        db_opt.as_deref(),
        db_opt.as_ref(),
        sde_opt.as_deref(),
        ops,
        None,
        None,
    );
}

/// Rolls probabilistic heat damage per overloaded cycle using read-only snapshot data and
/// queues DB module damage / burnout state mutations into `post_unlock` (`#1170`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_overheat_rack_and_damage_on_cycle_collecting(
    cycle: &ActiveModuleCycle,
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
    mechanic_rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    burned_out: &mut bool,
    burned_ship_modules: &mut Vec<(BallId, i64)>,
    mut local_heat_dmg: Option<&mut HashMap<i64, (f64, bool)>>,
    rack_heat_pending: Option<&HashMap<BallId, ([f64; 3], u64)>>,
    post_unlock: &mut PostUnlockOps,
) {
    let r_idx = cycle.rack_idx.min(2);
    let eval = evaluate_ship_thermodynamics(cycle.ship_id, sde_opt, db_opt);
    let pending_heat = rack_heat_pending
        .and_then(|sh| sh.get(&cycle.ship_id).map(|&(h, _)| h));
    let (runtime_heat, needs_legacy_cleanup) = if let Some(h) = pending_heat {
        (ship_rack_heat.get(&cycle.ship_id).copied().unwrap_or(h), false)
    } else if db_opt.is_some() {
        let (db_heat, has_legacy) = read_runtime_rack_heat_readonly(cycle.ship_id, db_opt);
        if db_heat.iter().all(|&h| h == 0.0) {
            ship_rack_heat.remove(&cycle.ship_id);
        } else {
            ship_rack_heat.insert(cycle.ship_id, db_heat);
        }
        (db_heat, has_legacy)
    } else {
        (ship_rack_heat.get(&cycle.ship_id).copied().unwrap_or([0.0; 3]), false)
    };
    if needs_legacy_cleanup && db_opt.is_some() {
        post_unlock.push(PostUnlockIntent::RackHeatPersist(RackHeatPersistDbOp {
            ship_id: cycle.ship_id,
            heat_snap: ship_rack_heat.get(&cycle.ship_id).copied().unwrap_or(runtime_heat),
            migrate_legacy_dyn_attrs: true,
        }));
    }
    let current_rack_heat = ship_rack_heat
        .get(&cycle.ship_id)
        .map(|r| r[r_idx])
        .unwrap_or(runtime_heat[r_idx])
        .clamp(0.0, 1.0);
    if current_rack_heat <= 0.0 {
        return;
    }
    let att_base = eval.att_bases[r_idx];
    let slot_ratio = eval.slot_ratio;

    let Some(db_ref) = db_opt.as_ref() else {
        return;
    };
    let (eff_dmg, active_flag, rack_mods) = db_ref.with_snapshot(|snap| {
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
        let active_flag = snap
            .items
            .get(&cycle.module_id)
            .map(|i| i.flag as u16)
            .unwrap_or(0);
        if eff_dmg <= 0.0 || !(11..=34).contains(&active_flag) {
            return (eff_dmg, active_flag, Vec::new());
        }
        let mut mods: Vec<(i64, u16, f64, f64)> = snap
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
            .map(|i| {
                let default_hp = sde_opt
                    .as_ref()
                    .and_then(|sde| sde.types.get(&i.type_id))
                    .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                    .unwrap_or(40.0);
                let cur_dmg = i
                    .dynamic_attributes
                    .get(&attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0);
                let max_hp = i
                    .dynamic_attributes
                    .get(&attr_ids::HP_STRUCT)
                    .copied()
                    .unwrap_or(default_hp);
                (i.item_id, i.flag as u16, cur_dmg, max_hp)
            })
            .collect();
        // Sort deterministically by slot flag so seeded RNG draws are reproducible across runs.
        mods.sort_unstable_by_key(|&(mid, flag, _, _)| (flag, mid));
        (eff_dmg, active_flag, mods)
    });

    if eff_dmg <= 0.0 || !(11..=34).contains(&active_flag) || rack_mods.is_empty() {
        return;
    }

    let mut updates = Vec::new();
    for (mod_id, mod_flag, base_dmg, max_hp) in rack_mods {
        let (pending_dmg, already_burned) = local_heat_dmg
            .as_deref()
            .and_then(|m| m.get(&mod_id).copied())
            .unwrap_or((0.0, false));
        if already_burned || base_dmg + pending_dmg >= max_hp - 1e-6 {
            continue;
        }
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
        let cur_dmg = base_dmg + pending_dmg;
        let next_dmg = (cur_dmg + eff_dmg).min(max_hp);
        let applied_delta = (next_dmg - cur_dmg).max(0.0);
        let mod_burned = next_dmg >= max_hp - 1e-6;
        if let Some(m) = local_heat_dmg.as_deref_mut() {
            let entry = m.entry(mod_id).or_insert((0.0, false));
            entry.0 += applied_delta;
            entry.1 |= mod_burned;
        }
        updates.push(OverheatModuleHit {
            module_id: mod_id,
            dmg_delta: applied_delta,
            max_hp,
            burned: mod_burned,
        });
        if mod_burned {
            if mod_id == cycle.module_id {
                *burned_out = true;
            } else {
                burned_ship_modules.push((cycle.ship_id, mod_id));
            }
        }
    }

    if !updates.is_empty() {
        for u in &updates {
            post_unlock
                .overheat_dmg_deltas
                .push((u.module_id, u.dmg_delta, u.burned));
        }
        post_unlock.push(PostUnlockIntent::OverheatModuleDamage(
            OverheatModuleDamageDbOp {
                ship_id: cycle.ship_id,
                updates,
            },
        ));
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
    let mut ops = PostUnlockOps::default();
    apply_overheat_rack_and_damage_on_cycle_collecting(
        cycle,
        ship_rack_heat,
        sde_opt,
        db_opt,
        mechanic_rng_opt,
        burned_out,
        burned_ship_modules,
        None,
        None,
        &mut ops,
    );
    execute_post_unlock_ops_for_standalone(
        db_opt.as_deref(),
        db_opt.as_ref(),
        sde_opt.as_deref(),
        ops,
        None,
        None,
    );
}

#[cfg(test)]
#[path = "tactical_module_mechanics_tests.rs"]
mod tactical_module_mechanics_tests;

#[cfg(test)]
mod r6_probe_policy_tests {
    use super::*;
    use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
    use eve_dogma::{FittedModule, ModuleState};
    use eve_sde::SdeRegistry;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    fn test_sde() -> Arc<SdeRegistry> {
        static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
        Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interdiction_launcher_skips_bpc_courier_loaded_probes_and_blocks_bad_cargo_merge() {
        let mut sde = (*test_sde()).clone();
        let base = sde.types.get(&587).expect("seed type").clone();
        let dictor_ship_tid = 995_601_u32;
        let launcher_tid = 995_602_u32;
        let disrupt_probe_tid = 22_778_u32;

        let mut dictor_t = base.clone();
        dictor_t.type_id = dictor_ship_tid;
        dictor_t.group_id = 541;
        dictor_t.category_id = 6;
        dictor_t.capacity = 30.0;
        dictor_t.default_attributes.clear();
        dictor_t.effect_ids.clear();
        dictor_t.default_attributes.insert(38, 30.0);
        sde.types.insert(dictor_ship_tid, dictor_t);

        let mut launcher_t = base.clone();
        launcher_t.type_id = launcher_tid;
        launcher_t.group_id = 589;
        launcher_t.category_id = 7;
        launcher_t.capacity = 15.0;
        launcher_t.default_attributes.clear();
        launcher_t.effect_ids.clear();
        launcher_t.default_attributes.insert(51, 5_000.0);
        launcher_t.default_attributes.insert(5561, 1.0);
        sde.types.insert(launcher_tid, launcher_t.clone());

        let mut probe_t = base;
        probe_t.type_id = disrupt_probe_tid;
        probe_t.group_id = 548;
        probe_t.category_id = 8;
        probe_t.volume = 5.0;
        probe_t.default_attributes.clear();
        probe_t.effect_ids.clear();
        probe_t.default_attributes.insert(103, 20_000.0);
        probe_t.default_attributes.insert(281, 120_000.0);
        probe_t.default_attributes.insert(9, 1_000.0);
        sde.types.insert(disrupt_probe_tid, probe_t);

        let ctx = ServerContext::with_sde_and_repository(
            Arc::new(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        );
        let null_sys = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
            .min_by_key(|s| s.solar_system_id)
            .map(|s| s.solar_system_id)
            .unwrap_or(30_004_759);

        let char_id = 90_000_001_i64;
        let ship_id = 995_601_001_i64;
        let mod_id = 995_601_002_i64;
        let bpc_loaded_id = 995_601_010_i64;
        let courier_loaded_id = 995_601_011_i64;
        let valid_loaded_id = 995_601_012_i64;
        let bpc_cargo_id = 995_601_020_i64;
        let courier_cargo_id = 995_601_021_i64;

        let fitted_mod = FittedModule {
            item_id: mod_id,
            type_id: launcher_tid,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: ModuleState::Online,
            charge_type_id: Some(disrupt_probe_tid),
            charge_quantity: 4,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        };

        ctx.db.with_mut_snapshot(|snap| {
            let seed_ch = snap.characters[&char_id].clone();
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    station_id: None,
                    structure_id: None,
                    solar_system_id: null_sys,
                    active_ship_id: ship_id,
                    ..seed_ch
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: dictor_ship_tid,
                    owner_id: char_id,
                    location_id: null_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: launcher_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                bpc_loaded_id,
                InventoryItemRecord {
                    item_id: bpc_loaded_id,
                    type_id: disrupt_probe_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    is_blueprint_copy: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                courier_loaded_id,
                InventoryItemRecord {
                    item_id: courier_loaded_id,
                    type_id: disrupt_probe_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 2,
                    ..Default::default()
                },
            );
            snap.item_custom_names
                .insert(courier_loaded_id, "courier_cargo:10".into());
            snap.items.insert(
                valid_loaded_id,
                InventoryItemRecord {
                    item_id: valid_loaded_id,
                    type_id: disrupt_probe_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 4,
                    ..Default::default()
                },
            );
            snap.items.insert(
                bpc_cargo_id,
                InventoryItemRecord {
                    item_id: bpc_cargo_id,
                    type_id: disrupt_probe_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    is_blueprint_copy: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                courier_cargo_id,
                InventoryItemRecord {
                    item_id: courier_cargo_id,
                    type_id: disrupt_probe_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 2,
                    ..Default::default()
                },
            );
            snap.item_custom_names
                .insert(courier_cargo_id, "courier_cargo:11".into());
        });

        let sys_null = ctx.systems.get_or_spawn(null_sys);
        sys_null
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: dictor_ship_tid,
                    position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_500_000.0,
                    radius: 60.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                SlimItemMeta::default(),
            )
            .await
            .unwrap();

        activate_interdiction_sphere_launcher(
            &ctx,
            &sys_null,
            null_sys,
            char_id,
            ship_id,
            dictor_ship_tid,
            mod_id,
            &fitted_mod,
            &launcher_t,
            "useMissiles",
            None,
        )
        .await
        .expect("valid loaded probe fires cleanly despite lower-ID BPC/courier shadows");

        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&valid_loaded_id].quantity, 2);
            assert_eq!(snap.items[&bpc_loaded_id].quantity, 1);
            assert_eq!(snap.items[&courier_loaded_id].quantity, 2);
            assert_eq!(snap.items[&bpc_cargo_id].quantity, 1);
            assert_eq!(snap.items[&courier_cargo_id].quantity, 2);
            assert!(snap.items.values().any(|i| {
                i.location_id == ship_id
                    && i.flag == InventoryFlag::Cargo
                    && i.type_id == disrupt_probe_tid
                    && i.item_id != bpc_cargo_id
                    && i.item_id != courier_cargo_id
                    && i.quantity == 1
            }));
        });

        // Removing the valid probe leaves only BPC + courier shadows -> NoProbesLoaded.
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&valid_loaded_id);
        });
        let db_key = tactical_modules::db_ptr_key(&ctx.db);
        ctx.tactical_state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .launcher_last_fire
            .remove(&(db_key, mod_id));
        let err = activate_interdiction_sphere_launcher(
            &ctx,
            &sys_null,
            null_sys,
            char_id,
            ship_id,
            dictor_ship_tid,
            mod_id,
            &fitted_mod,
            &launcher_t,
            "useMissiles",
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "NoProbesLoaded");
    }
}

