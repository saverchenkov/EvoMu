//! `space_entity_mining` — `entity.CmdMineRepeatedly` / `beyonce.CmdMineRepeatedly` / `dogmaIM.Mine` asteroid mining
//! for mining modules and launched mining drones (`MN-09`, `feat:mining.*`).
//!
//! Arguments are positional only: `(droneIDs, targetID)`, `(droneID, targetID)` or `(targetID[, moduleID])`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
pub use crate::services::mining_advanced::mining_compression::{
    caller_can_take_corp_hangar_item, caller_can_take_corp_hangar_item_at,
    compute_structure_reprocessing_base_yield,
    compute_structure_rigged_base_yield, resolve_reprocessable_materials,
    resolve_reprocessable_materials_for_portions, structure_has_online_reprocessing_service,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_sde::TypeId;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ClientError, PyRep};
use glam::DVec3;

const BEYONCE_SVC: &str = "beyonce";

fn no_solar_system() -> BridgeRpcError {
    BridgeRpcError {
        // rpc-lint: allow "NoSolarSystem" is an error code, not a beyonce method
        code: "NoSolarSystem".into(),
        message: "Session has no active solarSystemID".into(),
        client_error: None,
    }
}

/// Mines `targetID` with the ship's mining module or the listed launched mining drones.
pub(crate) async fn handle_mine(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)
        .or_else(|_| session.solar_system_id.ok_or_else(no_solar_system))?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };

    // Parse canonical `(droneIDs, targetID)` or `(targetID, droneID)` arguments
    let mut drone_ids: Vec<i64> = Vec::new();
    let mut asteroid_id: i64 = 0;

    if let Some(first) = args.first() {
        match first {
            PyRep::List(list) | PyRep::Tuple(list) => {
                for el in list {
                    if let Some(id) = el.as_i64() {
                        drone_ids.push(id);
                    }
                }
                asteroid_id = args
                    .get(1)
                    .and_then(|a| a.as_i64())
                    .unwrap_or(0);
            }
            _ => {
                if let Some(first_id) = first.as_i64() {
                    let second_id = args.get(1).and_then(|a| a.as_i64());
                    let first_is_drone = ctx.db.with_snapshot(|snap| {
                        snap.items
                            .get(&first_id)
                            .and_then(|i| ctx.sde.types.get(&i.type_id))
                            .is_some_and(|t| t.group_id == 101)
                    });
                    if first_is_drone && second_id.is_some() {
                        drone_ids.push(first_id);
                        asteroid_id = second_id.unwrap_or(0);
                    } else {
                        asteroid_id = first_id;
                        if let Some(sec) = second_id {
                            drone_ids.push(sec);
                        }
                    }
                }
            }
        }
    }
    if asteroid_id == 0 {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CmdMineRepeatedly requires target asteroidID".into(),
            client_error: None,
        });
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
            client_error: None,
        });
    };
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "CannotMineInWarp".into(),
            message: "Cannot activate mining lasers or drones while in warp".into(),
            client_error: None,
        });
    }
    if handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_cloaked)
    {
        return Err(BridgeRpcError {
            code: "CannotMineWhileCloaked".into(),
            message: "Cannot activate mining lasers or drones while cloaked".into(),
            client_error: None,
        });
    }
    let ship_hull_type = ctx
        .db
        .get_item(ship_id)
        .map(|i| i.type_id)
        .unwrap_or(ship_ball.type_id);
    if ship_hull_type == 670
        || ctx
            .sde
            .types
            .get(&ship_hull_type)
            .is_some_and(|t| t.group_id == 29)
    {
        return Err(BridgeRpcError {
            code: "InvalidShipForMining".into(),
            message: "Capsules cannot mine asteroids".into(),
            client_error: None,
        });
    }

    let mut ast_state_opt = handle.get_asteroid_state(asteroid_id).await.ok().flatten();
    if ast_state_opt.is_none() {
        if let Some(db_ast) = ctx.db.get_asteroid(asteroid_id) {
            if db_ast.solar_system_id != 0 && db_ast.solar_system_id != sys_id {
                return Err(BridgeRpcError {
                    code: "TargetNotInSystem".into(),
                    message: format!(
                        "Asteroid {asteroid_id} belongs to solar system {}, not {sys_id}",
                        db_ast.solar_system_id
                    ),
                    client_error: None,
                });
            }
            if db_ast.quantity <= 0 {
                return Err(BridgeRpcError {
                    code: "AsteroidDepleted".into(),
                    message: format!("Asteroid {asteroid_id} is depleted"),
                    client_error: None,
                });
            }
            let unit_vol = ctx
                .sde
                .types
                .get(&db_ast.type_id)
                .map(|t| t.volume.max(0.001))
                .unwrap_or(0.1);
            let pos = glam::DVec3::new(db_ast.position.0, db_ast.position.1, db_ast.position.2);
            let _ = handle
                .spawn_asteroid(
                    crate::system_actor::AsteroidState {
                        asteroid_id,
                        belt_id: db_ast.belt_id,
                        type_id: db_ast.type_id,
                        initial_units: db_ast.initial_quantity.max(0) as u64,
                        remaining_units: db_ast.quantity.max(0) as u64,
                        unit_volume: unit_vol,
                        depleted: false,
                        respawn_at_tick: None,
                    },
                    eve_destiny::Ball {
                        id: asteroid_id,
                        type_id: db_ast.type_id,
                        position: pos,
                        velocity: glam::DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass: 1.0e10,
                        radius: db_ast.radius,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                )
                .await;
            ast_state_opt = handle.get_asteroid_state(asteroid_id).await.ok().flatten();
        }
    }
    let Some(ast_state) = ast_state_opt else {
        return Err(BridgeRpcError {
            code: "AsteroidNotFound".into(),
            message: format!("Asteroid {asteroid_id} not found in system {sys_id}"),
            client_error: None,
        });
    };

    if ast_state.remaining_units == 0 || ast_state.depleted {
        return Err(BridgeRpcError {
            code: "AsteroidDepleted".into(),
            message: format!("Asteroid {asteroid_id} is depleted"),
            client_error: None,
        });
    }

    // Target distance measurement (validated once drone vs module mode is resolved)
    let mut ast_surface_dist_m = 0.0_f64;
    let mut has_ast_ball = false;
    if let Some((_, ast_ball)) = handle.get_ball_snapshot(asteroid_id).await.ok().flatten() {
        ast_surface_dist_m = (ship_ball.position.distance(ast_ball.position) - ship_ball.radius - ast_ball.radius).max(0.0);
        has_ast_ball = true;
    }

    // If no explicit drone/module ID was passed, check if the character has launched mining drones in space
    if drone_ids.is_empty() {
        drone_ids = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| {
                    i.owner_id == char_id
                        && i.location_id == sys_id as i64
                        && i.flag == InventoryFlag::None
                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 101)
                })
                .map(|i| i.item_id)
                .collect()
        });
    }

    drone_ids.sort_unstable();
    drone_ids.dedup();

    // Reject drones already present in active_drone_miners for this ship/target (`NF-1`)
    let drone_ball_ids: Vec<i64> = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        drone_ids
            .iter()
            .map(|did| {
                rt.active_drones_in_space
                    .values()
                    .find(|d| d.drone_item_id == *did || d.ball_id == *did)
                    .map(|d| d.ball_id)
                    .unwrap_or(*did)
            })
            .collect()
    });
    let already_mining = {
        let view = handle.view();
        drone_ball_ids.iter().any(|bid| {
            view.active_drone_miners
                .get(bid)
                .is_some_and(|m| m.ship_id == ship_id && m.asteroid_id == asteroid_id)
        })
    };
    if already_mining {
        return Err(BridgeRpcError {
            code: "DroneAlreadyMining".into(),
            message: "One or more drones are already mining this asteroid".into(),
            client_error: None,
        });
    }

    // Reject drone IDs not owned by caller (`rpc:entity.CmdMineRepeatedly`)
    for &did in &drone_ids {
        let is_owned = ctx.db.with_snapshot(|snap| {
            snap.items.get(&did).is_some_and(|i| i.owner_id == char_id)
        }) || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.get(&did).is_some_and(|d| d.owner_char_id == char_id)
                || rt.active_drones_in_space.values().any(|d| d.drone_item_id == did && d.owner_char_id == char_id)
        });
        if !is_owned {
            return Err(BridgeRpcError {
                code: "InvalidDroneOwner".into(),
                message: format!("Drone {did} is not owned by character {char_id}"),
                client_error: None,
            });
        }
    }

    let primary_id = drone_ids.first().copied();
    let drone_items: Vec<eve_db::InventoryItemRecord> = drone_ids
        .iter()
        .filter_map(|&did| ctx.db.get_item(did))
        .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 101))
        .collect();
    let is_drone = !drone_items.is_empty();

    let ship_type_id = ctx
        .db
        .get_item(ship_id)
        .map(|i| i.type_id)
        .unwrap_or(ship_ball.type_id);

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let unit_vol = ast_state.unit_volume.max(0.001);
    let ore_type_rec = ctx.sde.types.get(&ast_state.type_id);
    let ore_group_id = ore_type_rec.map(|t| t.group_id).unwrap_or(0);
    let ore_category_id = ore_type_rec.map(|t| t.category_id).unwrap_or(0);
    let mod_compatible = |type_id: TypeId| -> bool {
        let Some(t) = ctx.sde.types.get(&type_id) else { return false };
        if ore_group_id == 711 {
            t.group_id == 737
        } else if ore_group_id == 465 {
            matches!(t.group_id, 54 | 464 | 483)
                && (t.name.to_ascii_lowercase().contains("ice") || t.default_attributes.contains_key(&780))
        } else {
            matches!(t.group_id, 54 | 464 | 483) && !t.name.to_ascii_lowercase().contains("ice harvester")
        }
    };

    let mut per_drone_yield_m3: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    let mut per_drone_dur_secs: std::collections::HashMap<i64, u64> = std::collections::HashMap::new();
    let mut per_drone_waste_prob: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    let (base_yield_m3, waste_prob, max_range) = if is_drone {
        let ast_group_id = ctx
            .sde
            .types
            .get(&ast_state.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        let active_ind_core_type: Option<TypeId> = ctx
            .db
            .with_snapshot(|snap| {
                snap.items
                    .values()
                    .find(|it| {
                        it.location_id == ship_id
                            && (27..=34).contains(&(it.flag as u16))
                            && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 515)
                            && snap.module_active_states.contains(&it.item_id)
                    })
                    .map(|it| it.type_id)
            })
            .or_else(|| {
                fit.as_ref().and_then(|f| {
                    f.modules
                        .values()
                        .find(|m| {
                            ctx.sde.types.get(&m.type_id).is_some_and(|t| t.group_id == 515)
                                && m.state == eve_dogma::ModuleState::Active
                        })
                        .map(|m| m.type_id)
                })
            });

        for d_item in &drone_items {
            if d_item.location_id != sys_id as i64 || d_item.flag != InventoryFlag::None {
                return Err(BridgeRpcError {
                    code: "DroneNotInSpace".into(),
                    message: format!(
                        "Mining drone {} must be launched into space before mining",
                        d_item.item_id
                    ),
                    client_error: None,
                });
            }
            let is_ice_drone = ctx
                .sde
                .types
                .get(&d_item.type_id)
                .and_then(|t| t.default_attributes.get(&182).copied())
                == Some(43702.0);
            if is_ice_drone && ast_group_id != 465 {
                return Err(BridgeRpcError {
                    code: "InvalidTargetGroup".into(),
                    message: "Ice harvesting drones can only harvest Ice asteroids (group 465)".into(),
                    client_error: None,
                });
            }
            if !is_ice_drone && ast_group_id == 465 {
                return Err(BridgeRpcError {
                    code: "InvalidTargetGroup".into(),
                    message: "Ore mining drones cannot harvest Ice asteroids (group 465)".into(),
                    client_error: None,
                });
            }
            // Excavator mining/ice drones (SDE group 101, 25 Mbit bandwidth attr 1272) require Capital Industrial Ship (attrs 3223/3224) + active Industrial Core (group 515)
            let is_excavator = ctx
                .sde
                .types
                .get(&d_item.type_id)
                .is_some_and(|t| t.group_id == 101 && t.default_attributes.get(&1272).copied() == Some(25.0));
            if is_excavator {
                let can_operate_excavator = ctx.sde.types.get(&ship_type_id).is_some_and(|t| {
                    t.default_attributes.contains_key(&3223) || t.default_attributes.contains_key(&3224)
                });
                if !can_operate_excavator {
                    return Err(BridgeRpcError {
                        code: "InvalidShipForExcavator".into(),
                        message: "Excavator mining drones can only be operated by a Capital Industrial Ship".into(),
                        client_error: None,
                    });
                }
                if active_ind_core_type.is_none() {
                    return Err(BridgeRpcError {
                        code: "IndustrialCoreRequired".into(),
                        message: "Excavator mining drones require an active Capital Industrial Core on the Rorqual".into(),
                        client_error: None,
                    });
                }
            }
        }

        let (drone_op_lvl, ice_op_lvl, drone_int_lvl, drone_spec_lvl, ice_spec_lvl, cap_ind_lvl, rig_mult) =
            ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                let op3438 = sk.and_then(|m| m.get(&3438)).map(|s| s.level).unwrap_or(0);
                let op3439 = sk.and_then(|m| m.get(&3439)).map(|s| s.level).unwrap_or(0);
                let op = op3438.max(op3439);
                let ice_op = sk.and_then(|m| m.get(&43702)).map(|s| s.level).unwrap_or(0);
                let int3442 = sk.and_then(|m| m.get(&3442)).map(|s| s.level).unwrap_or(0);
                let int3436 = sk.and_then(|m| m.get(&3436)).map(|s| s.level).unwrap_or(0);
                let int = if sk.is_some_and(|m| m.contains_key(&3442)) {
                    int3442
                } else {
                    int3436
                };
                let spec = sk.and_then(|m| m.get(&22541)).map(|s| s.level).unwrap_or(0);
                let ice_spec = sk.and_then(|m| m.get(&43703)).map(|s| s.level).unwrap_or(0);
                let cap_ind = sk.and_then(|m| m.get(&28374)).map(|s| s.level).unwrap_or(0);

                // Drone Mining Augmentor rigs (`group_id == 778`, attr `434` miningUpgradeAmount)
                let mut rig_bonuses: Vec<f64> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == ship_id && (92..=94).contains(&(i.flag as u16)))
                    .filter_map(|i| ctx.sde.types.get(&i.type_id))
                    .filter(|t| t.group_id == 778)
                    .filter_map(|t| t.default_attributes.get(&434).copied())
                    .map(|b| b / 100.0)
                    .collect();
                rig_bonuses.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                let mut r_mult = 1.0;
                for (idx, &b) in rig_bonuses.iter().enumerate() {
                    r_mult *= 1.0 + b * eve_dogma::stacking_penalty_factor(idx);
                }
                (op, ice_op, int, spec, ice_spec, cap_ind, r_mult)
            });

        let sde_skill_pct = |skill_id: u32, attr_id: u16| -> f64 {
            ctx.sde
                .types
                .get(&skill_id)
                .and_then(|t| t.default_attributes.get(&attr_id).copied())
                .unwrap_or(0.0)
                / 100.0
        };
        let drone_requires_skill = |drone_tid: u32, req_skill_id: u32| -> bool {
            let target = f64::from(req_skill_id);
            ctx.sde.types.get(&drone_tid).is_some_and(|t| {
                [182_u16, 183, 184, 1285, 1289, 1290]
                    .into_iter()
                    .any(|attr| t.default_attributes.get(&attr).copied() == Some(target))
            })
        };
        let ship_t = ctx.sde.types.get(&ship_type_id);
        let rorq_ore_bonus_mult = ship_t
            .and_then(|t| t.default_attributes.get(&3223).copied())
            .map(|per_lvl| 1.0 + (per_lvl / 100.0) * (cap_ind_lvl.min(5) as f64))
            .unwrap_or(1.0);
        let rorq_ice_dur_factor = ship_t
            .and_then(|t| t.default_attributes.get(&3224).copied())
            .map(|per_lvl| 1.0 + (per_lvl / 100.0) * (cap_ind_lvl.min(5) as f64))
            .unwrap_or(1.0);
        let ind_core_ore_mult = active_ind_core_type
            .and_then(|cid| ctx.sde.types.get(&cid)?.default_attributes.get(&2585).copied())
            .map(|pct| 1.0 + pct / 100.0)
            .unwrap_or(1.0);
        let ind_core_ice_rate_mult = active_ind_core_type
            .and_then(|cid| ctx.sde.types.get(&cid)?.default_attributes.get(&2586).copied())
            .map(|cyc_pct| 1.0 / (1.0 + cyc_pct / 100.0).max(0.1))
            .unwrap_or(1.0);

        let mut total_drone_yield = 0.0;
        let mut max_waste_prob = 0.0_f64;
        for d_item in &drone_items {
            let drone_t = ctx.sde.types.get(&d_item.type_id);
            let d_yield = drone_t
                .and_then(|t| t.default_attributes.get(&77).copied())
                .unwrap_or(25.0);
            let raw_waste = drone_t
                .and_then(|t| t.default_attributes.get(&3154).copied())
                .unwrap_or(0.0);
            let d_waste = if raw_waste > 1.0 { raw_waste / 100.0 } else { raw_waste };
            per_drone_waste_prob.insert(d_item.item_id, d_waste);
            max_waste_prob = max_waste_prob.max(d_waste);
            let is_ice_drone = drone_requires_skill(d_item.type_id, 43702);
            let base_dur_s = drone_t
                .and_then(|t| t.default_attributes.get(&73).copied())
                .map(|dur_ms| if dur_ms >= 1000.0 { dur_ms / 1000.0 } else { dur_ms })
                .unwrap_or(60.0)
                .max(1.0);
            let (drone_mult, mine_dur_s) = if is_ice_drone {
                let ice_spec_factor = if drone_requires_skill(d_item.type_id, 43703) {
                    1.0 + sde_skill_pct(43703, 293) * (ice_spec_lvl.min(5) as f64)
                } else {
                    1.0
                };
                let core_ice_dur_factor = (1.0 / ind_core_ice_rate_mult).clamp(0.1, 1.0);
                let dur_s = base_dur_s
                    * (1.0 + sde_skill_pct(43702, 293) * (ice_op_lvl.min(5) as f64))
                    * ice_spec_factor
                    * rorq_ice_dur_factor
                    * core_ice_dur_factor;
                (1.0, dur_s.max(1.0))
            } else {
                let spec_factor = if drone_requires_skill(d_item.type_id, 22541) {
                    1.0 + sde_skill_pct(22541, 434) * (drone_spec_lvl.min(5) as f64)
                } else {
                    1.0
                };
                let mult = (1.0 + sde_skill_pct(3438, 434) * (drone_op_lvl.min(5) as f64))
                    * (1.0 + sde_skill_pct(3442, 434) * (drone_int_lvl.min(5) as f64))
                    * spec_factor
                    * rig_mult
                    * rorq_ore_bonus_mult
                    * ind_core_ore_mult;
                (mult, base_dur_s)
            };
            let max_vel = drone_t.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(250.0).max(1.0);
            let eff_dur = (mine_dur_s + (2.0 * ast_surface_dist_m) / max_vel).round().max(1.0) as u64;
            per_drone_dur_secs.insert(d_item.item_id, eff_dur);
            let single_drone_yield = d_yield * drone_mult * ((d_item.quantity.max(1)) as f64);
            per_drone_yield_m3.insert(d_item.item_id, single_drone_yield);
            total_drone_yield += single_drone_yield;
        }
        let drone_range = crate::services::combat_crimewatch::compute_drone_control_range(ctx, char_id, ship_id);
        (total_drone_yield, max_waste_prob, drone_range)
    } else if let (Some(fit), Some(mod_id)) = (&fit, primary_id) {
        let Some(m) = fit.modules.get(&mod_id) else {
            return Err(BridgeRpcError { code: "InvalidMiningModule".into(), message: "Mining module is not fitted to active ship".into(), client_error: None });
        };
        if m.state == eve_dogma::ModuleState::Offline || !mod_compatible(m.type_id) {
            return Err(BridgeRpcError { code: "InvalidMiningModule".into(), message: "Specified module is offline or incompatible with target resource".into(), client_error: None });
        }
        let y = fit.get_module_effective_attribute(mod_id, 77, &ctx.sde);
        let raw_w = fit.get_module_effective_attribute(mod_id, 3154, &ctx.sde);
        let r = fit.get_module_effective_attribute(mod_id, 54, &ctx.sde);
        (y, if raw_w > 1.0 { raw_w / 100.0 } else { raw_w }, if r > 0.0 { r } else { 35_000.0 })
    } else if let Some(fit) = &fit {
        let m = fit.modules.values().find(|m| m.state != eve_dogma::ModuleState::Offline && mod_compatible(m.type_id));
        if let Some(m) = m {
            let y = fit.get_module_effective_attribute(m.item_id, 77, &ctx.sde);
            let raw_w = fit.get_module_effective_attribute(m.item_id, 3154, &ctx.sde);
            let r = fit.get_module_effective_attribute(m.item_id, 54, &ctx.sde);
            (y, if raw_w > 1.0 { raw_w / 100.0 } else { raw_w }, if r > 0.0 { r } else { 35_000.0 })
        } else {
            return Err(BridgeRpcError {
                code: "NoMiningModuleFitted".into(),
                message: "Ship has no online compatible mining/harvesting module fitted".into(),
                client_error: None,
            });
        }
    } else {
        return Err(BridgeRpcError {
            code: "NoMiningModuleFitted".into(),
            message: "Ship has no online compatible mining/harvesting module fitted".into(),
            client_error: None,
        });
    };

    if has_ast_ball && ast_surface_dist_m > max_range {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: format!("Asteroid {asteroid_id} is {ast_surface_dist_m:.1} m away (max range {max_range} m)"),
            client_error: None,
        });
    }

    let ship_attrs = ctx.sde.types.get(&ship_type_id).map(|t| &t.default_attributes);
    let has_ore_hold = ship_attrs.is_some_and(|a| a.get(&1556).copied().unwrap_or(0.0) > 0.0 || a.get(&3227).copied().unwrap_or(0.0) > 0.0);
    let has_gas_hold = ship_attrs.is_some_and(|a| a.get(&1557).copied().unwrap_or(0.0) > 0.0);
    let has_ice_hold = ship_attrs.is_some_and(|a| a.get(&3136).copied().unwrap_or(0.0) > 0.0);

    let target_flag = if ore_group_id == 465 && has_ice_hold {
        InventoryFlag::SpecializedIceHold
    } else if ore_group_id == 711 && has_gas_hold {
        InventoryFlag::SpecializedGasHold
    } else if (ore_category_id == 25
        || crate::services::mining_phased_autotarget::is_prismaticite_ore(
            &ctx.sde,
            ast_state.type_id,
        ))
        && has_ore_hold
    {
        InventoryFlag::SpecializedOreHold
    } else {
        InventoryFlag::Cargo
    };

    if is_drone {
        let now_unix = ctx.now_unix();
        for d_item in &drone_items {
            let dur = per_drone_dur_secs.get(&d_item.item_id).copied().unwrap_or(60);
            let d_yield_m3 = per_drone_yield_m3
                .get(&d_item.item_id)
                .copied()
                .unwrap_or(base_yield_m3);
            let d_units = ((d_yield_m3 / unit_vol).floor() as u64).max(1);

            let drone_ball_id = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .find(|d| d.drone_item_id == d_item.item_id || d.ball_id == d_item.item_id)
                    .map(|d| d.ball_id)
            })
            .unwrap_or(d_item.item_id);

            let _ = handle
                .emit_destiny_event(
                    drone_ball_id,
                    eve_destiny::DestinyEvent::OnDroneStateChange {
                        drone_id: drone_ball_id,
                        owner_id: char_id,
                        controller_id: ship_id,
                        activity_state: 2, // Mining (entityMining = 2)
                        type_id: d_item.type_id,
                        controller_owner_id: char_id,
                        target_id: Some(asteroid_id),
                    },
                )
                .await;

            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                if let Some(d) = rt.active_drones_in_space.get_mut(&drone_ball_id) {
                    d.activity_state = 2;
                    d.target_id = Some(asteroid_id);
                }
            });
            let _ = handle
                .upsert_drone_miner(crate::system_actor::DroneMiningState {
                    drone_id: drone_ball_id,
                    asteroid_id,
                    ship_id,
                    owner_char_id: char_id,
                    solar_system_id: sys_id,
                    yield_units: d_units,
                    duration_secs: dur,
                    next_cycle_unix: now_unix.saturating_add(dur),
                    waste_prob: per_drone_waste_prob.get(&d_item.item_id).copied().unwrap_or(0.0),
                    ore_type_id: ast_state.type_id,
                    destination_flag: target_flag,
                })
                .await;
            crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(ctx, char_id, drone_ball_id, 2, Some("mining"));
        }
        return Ok(PyRep::Dict(Vec::new()));
    }

    let (hold_cap, used_vol) = ctx.db.with_snapshot(|snap| {
        let st = ctx.sde.types.get(&ship_type_id);
        let cap = match target_flag as u16 {
            134 => st.and_then(|t| t.default_attributes.get(&1556).or_else(|| t.default_attributes.get(&1770)).or_else(|| t.default_attributes.get(&3227))).copied().unwrap_or(0.0),
            135 => st.and_then(|t| t.default_attributes.get(&1557)).copied().unwrap_or(0.0),
            181 => st.and_then(|t| t.default_attributes.get(&3136)).copied().unwrap_or(0.0),
            _ => st.map(|t| t.capacity).unwrap_or(1000.0),
        };
        let used: f64 = snap.items.values()
            .filter(|i| i.location_id == ship_id && i.flag == target_flag)
            .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1) * (i.quantity as f64))
            .sum();
        (cap, used)
    });
    let max_fit_units = if matches!(target_flag as u16, 5 | 134 | 135 | 181) && hold_cap > 0.0 {
        ((hold_cap - used_vol).max(0.0) / unit_vol).floor() as u64
    } else {
        u64::MAX
    };
    if max_fit_units == 0 {
        return Err(BridgeRpcError {
            code: "CargoFull".into(),
            message: "Target mining hold is full".into(),
            client_error: None,
        });
    }
    let phase_eff =
        crate::services::mining_phased_autotarget::phased_field_efficiency_for_asteroid(
            ctx,
            sys_id,
            asteroid_id,
            ast_state.type_id,
        );
    let effective_yield_m3 = base_yield_m3 * phase_eff * ctx.mining_yield_multiplier();
    let requested_units = if effective_yield_m3 <= 0.0 {
        0
    } else {
        (((effective_yield_m3 / unit_vol).floor() as u64).max(1)).min(max_fit_units)
    };

    let extraction = match handle.mine_asteroid(asteroid_id, requested_units, waste_prob).await {
        Ok(e) => e,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "MiningFailed".into(),
                message: e,
                client_error: None,
            });
        }
    };

    let now_secs = ctx.now_unix_secs();
    let ast_pos_opt = ctx.db.with_mut_snapshot(|snap| {
        let mut pos_opt = None;
        if let Some(ast) = snap.asteroids.get_mut(&asteroid_id) {
            ast.quantity = extraction.remaining_units as i64;
            if ast.belt_id == 0 && ast_state.belt_id != 0 {
                ast.belt_id = ast_state.belt_id;
            }
            pos_opt = Some(DVec3::new(ast.position.0, ast.position.1, ast.position.2));
        } else if ast_state.belt_id != 0 {
            snap.asteroids.insert(asteroid_id, eve_db::AsteroidRecord {
                asteroid_id,
                type_id: ast_state.type_id,
                solar_system_id: sys_id,
                belt_id: ast_state.belt_id,
                quantity: extraction.remaining_units as i64,
                initial_quantity: ast_state.initial_units as i64,
                position: (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z),
                radius: 60.0,
            });
            pos_opt = Some(ship_ball.position);
        }
        if extraction.depleted {
            if let Some(ast_pos) = pos_opt {
                let mut newly_cleared_ids = Vec::new();
                if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
                    for anom in anoms.iter_mut() {
                        if !anom.cleared {
                            let anom_pos = DVec3::from_array(anom.position);
                            if anom_pos.distance(ast_pos) <= 50_000.0 {
                                let any_remaining = snap.asteroids.values().any(|a| {
                                    a.solar_system_id == sys_id
                                        && a.quantity > 0
                                        && DVec3::new(
                                            a.position.0,
                                            a.position.1,
                                            a.position.2,
                                        )
                                        .distance(anom_pos)
                                            <= 50_000.0
                                });
                                if !any_remaining {
                                    anom.cleared = true;
                                    anom.remaining_volume = 0.0;
                                    anom.created_unix = now_secs;
                                    newly_cleared_ids.push(anom.anomaly_id.clone());
                                }
                            }
                        }
                    }
                }
                if !newly_cleared_ids.is_empty() {
                    if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                        for s in sigs.iter_mut() {
                            if newly_cleared_ids.contains(&s.signature_id) {
                                s.collapsed = true;
                                s.signal_strength = 0.0;
                            }
                        }
                    }
                }
            }
        }
        pos_opt
    });

    if extraction.mined_units > 0 || extraction.wasted_units > 0 {
        crate::services::pve_homefront_sites::on_homefront_ore_mined(
            ctx,
            char_id,
            ship_id,
            sys_id,
            asteroid_id,
            extraction.ore_type_id,
            extraction.mined_units,
            extraction.wasted_units,
            extraction.remaining_units,
        )
        .await;
    }

    if extraction.mined_units > 0 {
        let ast_pos_arr = ast_pos_opt.map(|p| [p.x, p.y, p.z]);
        crate::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
            ctx,
            char_id,
            ship_id,
            sys_id,
            target_flag,
            extraction.ore_type_id,
            extraction.mined_units,
            extraction.wasted_units,
            Some(asteroid_id),
            ast_pos_arr,
        );

        // In low-sec (< 0.45) or wormhole space, harvesting a gas cloud (group 711) triggers an SDE NPC ambush wave
        if ore_group_id == 711 {
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(0.5);
            let is_wh_or_lowsec =
                (31_000_000..=31_002_605).contains(&sys_id) || sys_sec < 0.45;
            if is_wh_or_lowsec {
                let gas_pos = ast_pos_opt.unwrap_or(DVec3::ZERO);
                let existing_balls = handle.list_balls().await.unwrap_or_default();
                let npc_already_present = existing_balls.iter().any(|b| {
                    ctx.sde
                        .types
                        .get(&b.type_id)
                        .is_some_and(|t| t.category_id == 11)
                        && b.position.distance(gas_pos) <= 50_000.0
                });
                if !npc_already_present {
                    let npc_tid = ctx
                        .sde
                        .npc_spawn_groups
                        .values()
                        .flat_map(|g| g.npc_type_ids.iter().copied())
                        .find(|tid| {
                            ctx.sde.types.get(tid).is_some_and(|t| {
                                t.category_id == 11
                                    && t.default_attributes.contains_key(
                                        &eve_sde::attr_ids::ENTITY_KILL_BOUNTY,
                                    )
                                    && t.radius >= 25.0
                            })
                        })
                        .or_else(|| {
                            ctx.sde
                                .types
                                .values()
                                .filter(|t| {
                                    t.category_id == 11
                                        && t.default_attributes.contains_key(
                                            &eve_sde::attr_ids::ENTITY_KILL_BOUNTY,
                                        )
                                        && t.radius >= 25.0
                                })
                                .map(|t| t.type_id)
                                .min()
                        });
                    if let Some(npc_type_id) = npc_tid {
                        let npc_ball_id = ctx.db.with_mut_snapshot(|snap| {
                            snap.next_entity_id += 1;
                            snap.next_entity_id
                        });
                        let npc_rec = ctx.sde.types.get(&npc_type_id);
                        let npc_radius = npc_rec.map(|t| t.radius.max(25.0)).unwrap_or(45.0);
                        let npc_mass =
                            npc_rec.map(|t| t.mass.max(10_000.0)).unwrap_or(1_200_000.0);
                        let _ = handle
                            .spawn_ball_with_combat(
                                Ball {
                                    id: npc_ball_id,
                                    type_id: npc_type_id,
                                    position: DVec3::new(
                                        gas_pos.x + 4_500.0,
                                        gas_pos.y,
                                        gas_pos.z,
                                    ),
                                    velocity: DVec3::ZERO,
                                    max_velocity: 320.0,
                                    inertia: 2.0,
                                    mass: npc_mass,
                                    radius: npc_radius,
                                    speed_fraction: 0.0,
                                    mode: MovementMode::Stop,
                                },
                                ctx.build_type_combat_state(npc_type_id),
                            )
                            .await;
                    }
                }
            }
        }
    }



    Ok(PyRep::keyval([
        ("asteroidID", PyRep::Integer(asteroid_id)),
        ("typeID", PyRep::Integer(extraction.ore_type_id as i64)),
        ("minedUnits", PyRep::Integer(extraction.mined_units as i64)),
        ("yieldUnits", PyRep::Integer(extraction.mined_units as i64)),
        ("yieldVolume", PyRep::Real(extraction.mined_units as f64 * unit_vol)),
        ("wastedUnits", PyRep::Integer(extraction.wasted_units as i64)),
        ("remainingUnits", PyRep::Integer(extraction.remaining_units as i64)),
        ("flagID", PyRep::Integer(target_flag as i64)),
        ("depleted", PyRep::Integer(if extraction.depleted { 1 } else { 0 })),
        ("isDrone", PyRep::Bool(is_drone)),
    ]))
}

pub use crate::services::mining_cycle::{
    compute_reprocessing_rates_for_item, compute_reprocessing_rates_for_item_at,
};

struct ParsedReprocessArgs {
    item_ids: Vec<i64>,
    is_multi_arg: bool,
    is_batch_call: bool,
    from_loc: Option<i64>,
    explicit_out_owner: Option<i64>,
    explicit_out_loc: Option<i64>,
    explicit_out_flag: Option<InventoryFlag>,
}

enum ReprocessDestConstraint {
    StationOrOfficeHangar,
    ShipHold {
        ship_id: i64,
        ship_type_id: TypeId,
        flag: InventoryFlag,
        cap_m3: f64,
    },
    Container {
        cont_id: i64,
        cap_m3: f64,
    },
}

struct ResolvedReprocessDest {
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    constraint: ReprocessDestConstraint,
}

struct PlannedOutputMat {
    type_id: TypeId,
    quantity: i32,
}

struct PlannedReprocessItem {
    source_item: InventoryItemRecord,
    remainder: i32,
    dest: ResolvedReprocessDest,
    outputs: Vec<PlannedOutputMat>,
    station_base_yield: f64,
    yield_rate: f64,
    station_tax_rate: f64,
    tax_cents: IskCents,
}

fn is_corp_sag_flag(flag: InventoryFlag) -> bool {
    matches!(
        flag,
        InventoryFlag::CorpSAG1
            | InventoryFlag::CorpSAG2
            | InventoryFlag::CorpSAG3
            | InventoryFlag::CorpSAG4
            | InventoryFlag::CorpSAG5
            | InventoryFlag::CorpSAG6
            | InventoryFlag::CorpSAG7
    )
}

fn is_station_hangar_alias_flag(flag: InventoryFlag) -> bool {
    matches!(
        flag,
        InventoryFlag::Cargo
            | InventoryFlag::SpecializedAsteroidHold
            | InventoryFlag::SpecializedOreHold
            | InventoryFlag::SpecializedIceHold
            | InventoryFlag::SpecializedGasHold
    )
}

fn is_valid_reprocess_container_type(inv_type: &eve_sde::InvType) -> bool {
    inv_type.category_id == 2
        && matches!(inv_type.group_id, 12 | 340 | 448 | 649)
        && inv_type.type_id != 3468
}

fn container_capacity_m3(item: &InventoryItemRecord, inv_type: &eve_sde::InvType) -> f64 {
    item.dynamic_attributes
        .get(&eve_sde::attr_ids::CAPACITY)
        .copied()
        .filter(|&c| c > 0.0)
        .unwrap_or_else(|| {
            if inv_type.capacity > 0.0 {
                inv_type.capacity
            } else {
                inv_type
                    .default_attributes
                    .get(&eve_sde::attr_ids::CAPACITY)
                    .copied()
                    .unwrap_or(0.0)
            }
        })
}

/// Parses 1-arg, 2-arg, legacy Crucible, and V24 5-arg `reprocessingSvc.Reprocess` call shapes.
fn parse_reprocess_args(args: &[PyRep]) -> Result<ParsedReprocessArgs, BridgeRpcError> {
    let is_multi_arg = args.len() >= 2;
    let is_batch_call = is_multi_arg
        || matches!(
            args.first(),
            Some(PyRep::List(_) | PyRep::Dict(_) | PyRep::Tuple(_))
        );
    let item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
            list.iter().filter_map(PyRep::as_i64).collect()
        }
        Some(PyRep::Dict(entries)) => entries.iter().filter_map(|(k, _)| k.as_i64()).collect(),
        Some(first) => first.as_i64().into_iter().collect(),
        None => Vec::new(),
    };
    if item_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Reprocess requires at least one integer itemID".into(),
            client_error: None,
        });
    }
    let mut seen_ids = std::collections::HashSet::with_capacity(item_ids.len());
    for &id in &item_ids {
        if !seen_ids.insert(id) {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Duplicate itemID {id} in Reprocess batch"),
                client_error: None,
            });
        }
    }

    let from_loc = is_multi_arg
        .then(|| args.get(1).and_then(PyRep::as_i64).filter(|&id| id > 0))
        .flatten();
    if !is_multi_arg {
        return Ok(ParsedReprocessArgs {
            item_ids,
            is_multi_arg,
            is_batch_call,
            from_loc,
            explicit_out_owner: None,
            explicit_out_loc: None,
            explicit_out_flag: None,
        });
    }

    // Legacy Crucible call: `(itemIDs, stationID, 4 /* Hangar */, false, [])`.
    let is_legacy_crucible = args.len() >= 5
        && args.get(2).and_then(PyRep::as_i64) == Some(InventoryFlag::Hangar as i64)
        && matches!(args.get(3), Some(PyRep::Bool(_)))
        && matches!(args.get(4), Some(PyRep::List(v)) if v.is_empty());
    if is_legacy_crucible {
        return Ok(ParsedReprocessArgs {
            item_ids,
            is_multi_arg,
            is_batch_call,
            from_loc,
            explicit_out_owner: None,
            explicit_out_loc: None,
            explicit_out_flag: Some(InventoryFlag::Hangar),
        });
    }

    let explicit_out_owner = match args.get(2) {
        None | Some(PyRep::None) => None,
        Some(rep) => match rep.as_i64() {
            Some(0) => None,
            Some(id) if id > 0 => Some(id),
            _ => {
                return Err(BridgeRpcError {
                    code: "InvalidOutputLocation".into(),
                    message: "Reprocess outputOwner must be a positive owner ID or None".into(),
                    client_error: None,
                });
            }
        },
    };

    let arg4_absent = matches!(args.get(4), None | Some(PyRep::None))
        || matches!(args.get(4), Some(PyRep::List(v)) if v.is_empty());
    let legacy_arg3_flag = arg4_absent
        .then(|| args.get(3).and_then(PyRep::as_i64))
        .flatten()
        .filter(|&f| (1..=10_000).contains(&f));

    let (explicit_out_loc, explicit_out_flag) = if let Some(flag_val) = legacy_arg3_flag {
        (None, Some(InventoryFlag::from_u16(flag_val as u16)))
    } else {
        let out_loc = match args.get(3) {
            None | Some(PyRep::None) => None,
            Some(rep) => match rep.as_i64() {
                Some(0) => None,
                Some(id) if id > 0 => Some(id),
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidOutputLocation".into(),
                        message: "Reprocess outputLocation must be a valid location ID or None"
                            .into(),
                        client_error: None,
                    });
                }
            },
        };
        let out_flag = match args.get(4) {
            None | Some(PyRep::None) => None,
            Some(PyRep::List(v)) if v.is_empty() => None,
            Some(rep) => match rep.as_i64() {
                Some(0) => None,
                Some(f) if (1..=u16::MAX as i64).contains(&f) => {
                    Some(InventoryFlag::from_u16(f as u16))
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidOutputLocation".into(),
                        message: "Reprocess outputFlag must be a valid inventory flag ID".into(),
                        client_error: None,
                    });
                }
            },
        };
        (out_loc, out_flag)
    };

    Ok(ParsedReprocessArgs {
        item_ids,
        is_multi_arg,
        is_batch_call,
        from_loc,
        explicit_out_owner,
        explicit_out_loc,
        explicit_out_flag,
    })
}

fn find_corp_office_at_station(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    station_id: i64,
) -> Option<i64> {
    if corp_id <= 0 {
        return None;
    }
    snap.items
        .values()
        .find(|it| it.type_id == 27 && it.owner_id == corp_id && it.location_id == station_id)
        .map(|it| it.item_id)
        .or_else(|| {
            snap.corp_governance
                .get(&corp_id)
                .and_then(|g| g.offices.get(&station_id).copied())
        })
}

fn check_corp_division_deposit_access(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    station_id: i64,
    sag_flag: InventoryFlag,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if corp_id <= 0 || find_corp_office_at_station(snap, corp_id, station_id).is_none() {
        return Err(BridgeRpcError::from_user_key(
            eve_proto::UserErrorKey::NoOfficeAtStation,
        ));
    }
    let flag_num = sag_flag as i64;
    if !(116..=122).contains(&flag_num) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!("Flag {flag_num} is not a corporate hangar division"),
            client_error: None,
        });
    }
    let div_idx = (flag_num - 116) as usize;
    let (is_ceo, role_mask) =
        crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
            snap,
            corp_id,
            char_id,
            Some(station_id),
            now_unix,
        );
    let div_access_roles = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[div_idx]
        | eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx];
    if !is_ceo && (role_mask & div_access_roles) == 0 {
        return Err(BridgeRpcError {
            code: "CrpAccessDenied".into(),
            message: format!(
                "Missing hangar query/take access to corporate division {} at station {station_id}",
                div_idx + 1
            ),
            client_error: None,
        });
    }
    Ok(())
}

fn check_container_lock_and_password(
    snap: &eve_db::WorldStateSnapshot,
    cont: &InventoryItemRecord,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    crate::services::inventory_fitting::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
        snap, cont,
    )?;
    if cont.flag == InventoryFlag::Locked || snap.is_item_in_alsc_lock(cont.item_id) {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: format!("Container {} is locked", cont.item_id),
            client_error: None,
        });
    }
    let cfg =
        crate::services::inventory_fitting::fitting_containers_ships::container_config_snap(
            snap,
            cont.item_id,
        );
    let unlocked =
        crate::services::inventory_fitting::fitting_containers_ships::is_container_unlocked_snap(
            snap,
            cont.item_id,
            char_id,
        );
    let need_pw_check = cfg.password_per_action || !unlocked;
    if need_pw_check
        && crate::services::inventory_fitting::fitting_containers_ships::container_password_snap(
            snap,
            cont.item_id,
            1,
        )
        .is_some()
    {
        return Err(BridgeRpcError {
            code: "ALSCPasswordRequired".into(),
            message: format!("Password required to deposit into container {}", cont.item_id),
            client_error: None,
        });
    }
    Ok(())
}

fn sum_items_volume_m3<'a>(
    sde: &eve_sde::SdeRegistry,
    items: impl Iterator<Item = &'a InventoryItemRecord>,
) -> f64 {
    items
        .map(|it| {
            crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(
                sde, it,
            ) * (it.quantity.max(0) as f64)
        })
        .sum()
}

#[allow(clippy::too_many_arguments)]
fn resolve_station_or_office_dest(
    snap: &eve_db::WorldStateSnapshot,
    parsed: &ParsedReprocessArgs,
    station_id: i64,
    target_loc: i64,
    is_own_corp_office: bool,
    char_id: i64,
    caller_corp: i64,
    default_station_flag: InventoryFlag,
    now_unix: u64,
) -> Result<ResolvedReprocessDest, BridgeRpcError> {
    let requested_flag = match parsed.explicit_out_flag {
        Some(f)
            if target_loc == station_id
                && is_station_hangar_alias_flag(f)
                && parsed.explicit_out_owner != Some(caller_corp) =>
        {
            if parsed.explicit_out_owner.is_some() {
                InventoryFlag::Hangar
            } else {
                default_station_flag
            }
        }
        Some(f) => f,
        None => {
            if is_own_corp_office || parsed.explicit_out_owner == Some(caller_corp) {
                default_station_flag
            } else if parsed.explicit_out_owner == Some(char_id) {
                InventoryFlag::Hangar
            } else {
                default_station_flag
            }
        }
    };

    if is_corp_sag_flag(requested_flag) {
        if parsed.explicit_out_owner == Some(char_id) {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: "Personal character ownership cannot target a corporate CorpSAG division"
                    .into(),
                client_error: None,
            });
        }
        check_corp_division_deposit_access(
            snap,
            caller_corp,
            char_id,
            station_id,
            requested_flag,
            now_unix,
        )?;
        return Ok(ResolvedReprocessDest {
            owner_id: caller_corp,
            location_id: target_loc,
            flag: requested_flag,
            constraint: ReprocessDestConstraint::StationOrOfficeHangar,
        });
    }

    if is_own_corp_office || parsed.explicit_out_owner == Some(caller_corp) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Corporate reprocessing output at station {station_id} requires a CorpSAG1..7 division flag (got {})",
                requested_flag as u16
            ),
            client_error: None,
        });
    }

    if requested_flag != InventoryFlag::Hangar {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Output flag {} is not valid in station hangar {target_loc}",
                requested_flag as u16
            ),
            client_error: None,
        });
    }

    Ok(ResolvedReprocessDest {
        owner_id: char_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        constraint: ReprocessDestConstraint::StationOrOfficeHangar,
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_ship_hold_dest(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    char_skills: &std::collections::HashMap<TypeId, u8>,
    parsed: &ParsedReprocessArgs,
    dest_item: &InventoryItemRecord,
    dest_type: &eve_sde::InvType,
    station_id: i64,
    char_id: i64,
    caller_corp: i64,
) -> Result<ResolvedReprocessDest, BridgeRpcError> {
    let target_loc = dest_item.item_id;
    if !dest_item.singleton
        || dest_item.quantity != 1
        || dest_item.location_id != station_id
        || dest_item.flag != InventoryFlag::Hangar
        || dest_item.owner_id != char_id
        || parsed.explicit_out_owner == Some(caller_corp)
    {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Ship {target_loc} must be an assembled personal ship docked in hangar at station {station_id}"
            ),
            client_error: None,
        });
    }
    let hold_flag = parsed.explicit_out_flag.unwrap_or(InventoryFlag::Cargo);
    if !matches!(
        hold_flag,
        InventoryFlag::Cargo | InventoryFlag::SpecializedMineralHold
    ) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Output flag {} is not a valid ship reprocessing destination (only Cargo or SpecializedMineralHold allowed)",
                hold_flag as u16
            ),
            client_error: None,
        });
    }
    let eval_cargo_cap = if hold_flag == InventoryFlag::Cargo {
        let mut engine = eve_dogma::ShipFittingEngine::from_sde(
            dest_item.item_id,
            dest_item.type_id,
            sde,
        )
        .with_character_skills(char_skills.clone());
        for it in snap.items.values() {
            if it.location_id == dest_item.item_id
                && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132)
                && sde.types.get(&it.type_id).is_none_or(|t| t.category_id != 8)
            {
                let online = snap
                    .module_online_states
                    .get(&it.item_id)
                    .copied()
                    .unwrap_or(true);
                let _ = engine.fit_module_with_state(
                    it.item_id,
                    it.type_id,
                    it.flag as u16,
                    online,
                    sde,
                );
            }
        }
        let eval_ctx = engine.evaluated_context(sde);
        Some(
            eval_ctx
                .ship
                .get_attribute(eve_sde::attr_ids::CAPACITY, sde)
                .max(0.0),
        )
    } else {
        None
    };
    let cap_m3 = crate::services::fitting_saved_apply::get_ship_hold_capacity(
        hold_flag as u16,
        dest_type,
        eval_cargo_cap,
    );
    Ok(ResolvedReprocessDest {
        owner_id: char_id,
        location_id: target_loc,
        flag: hold_flag,
        constraint: ReprocessDestConstraint::ShipHold {
            ship_id: target_loc,
            ship_type_id: dest_item.type_id,
            flag: hold_flag,
            cap_m3,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_station_container_dest(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    parsed: &ParsedReprocessArgs,
    dest_item: &InventoryItemRecord,
    dest_type: &eve_sde::InvType,
    station_id: i64,
    char_id: i64,
    caller_corp: i64,
    now_unix: u64,
) -> Result<ResolvedReprocessDest, BridgeRpcError> {
    let target_loc = dest_item.item_id;
    if !is_valid_reprocess_container_type(dest_type)
        || !dest_item.singleton
        || dest_item.quantity != 1
    {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Output location {target_loc} (type {}) is not an assembled station container or docked ship",
                dest_item.type_id
            ),
            client_error: None,
        });
    }
    let cap_m3 = container_capacity_m3(dest_item, dest_type);
    if cap_m3 <= 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!("Container {target_loc} has zero m³ storage capacity"),
            client_error: None,
        });
    }

    let is_corp_cont = caller_corp > 0 && dest_item.owner_id == caller_corp;
    let is_personal_cont = dest_item.owner_id == char_id;
    if !is_personal_cont && !is_corp_cont {
        return Err(BridgeRpcError {
            code: "OwnershipOrLocationViolation".into(),
            message: format!(
                "Container {target_loc} is not owned by character {char_id} or their corporation"
            ),
            client_error: None,
        });
    }
    if is_personal_cont && parsed.explicit_out_owner == Some(caller_corp) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: "Cannot assign corporate output owner inside a personal container".into(),
            client_error: None,
        });
    }
    if is_corp_cont && parsed.explicit_out_owner == Some(char_id) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: "Cannot assign personal output owner inside a corporate container".into(),
            client_error: None,
        });
    }

    // Walk container ancestry to verify it sits inside an accessible personal hangar or corp
    // division at `station_id` and that every container along the chain is unlocked and valid.
    let mut visited = std::collections::HashSet::new();
    let mut cur = dest_item;
    let mut root_sag_flag: Option<InventoryFlag> = None;
    for hop in 0..8 {
        if !visited.insert(cur.item_id) {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: "Cyclic container hierarchy".into(),
                client_error: None,
            });
        }
        check_container_lock_and_password(snap, cur, char_id)?;
        if cur.owner_id != dest_item.owner_id {
            return Err(BridgeRpcError {
                code: "OwnershipOrLocationViolation".into(),
                message: format!("Container {target_loc} hierarchy has mismatched ownership"),
                client_error: None,
            });
        }
        if cur.location_id == station_id {
            if is_personal_cont {
                if cur.flag != InventoryFlag::Hangar {
                    return Err(BridgeRpcError {
                        code: "InvalidOutputLocation".into(),
                        message: format!(
                            "Container {target_loc} is not inside station hangar at {station_id}"
                        ),
                        client_error: None,
                    });
                }
            } else {
                if !is_corp_sag_flag(cur.flag) {
                    return Err(BridgeRpcError {
                        code: "InvalidOutputLocation".into(),
                        message: format!(
                            "Corporate container {target_loc} is not inside a CorpSAG division"
                        ),
                        client_error: None,
                    });
                }
                check_corp_division_deposit_access(
                    snap,
                    caller_corp,
                    char_id,
                    station_id,
                    cur.flag,
                    now_unix,
                )?;
                root_sag_flag = Some(cur.flag);
            }
            break;
        }
        let Some(parent) = snap.items.get(&cur.location_id) else {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: format!("Container {target_loc} is not anchored at station {station_id}"),
                client_error: None,
            });
        };
        if parent.type_id == 27 {
            if !is_corp_cont
                || parent.owner_id != caller_corp
                || parent.location_id != station_id
                || !is_corp_sag_flag(cur.flag)
            {
                return Err(BridgeRpcError {
                    code: "InvalidOutputLocation".into(),
                    message: format!(
                        "Container {target_loc} is not anchored in an authorized corporate office at {station_id}"
                    ),
                    client_error: None,
                });
            }
            check_corp_division_deposit_access(
                snap,
                caller_corp,
                char_id,
                station_id,
                cur.flag,
                now_unix,
            )?;
            root_sag_flag = Some(cur.flag);
            break;
        }
        let Some(parent_type) = sde.types.get(&parent.type_id) else {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: format!("Parent container {} has unknown type", parent.item_id),
                client_error: None,
            });
        };
        if !is_valid_reprocess_container_type(parent_type)
            || !parent.singleton
            || parent.quantity != 1
        {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: format!(
                    "Container {target_loc} cannot reside inside non-container item {}",
                    parent.item_id
                ),
                client_error: None,
            });
        }
        let child_flag_ok = matches!(cur.flag, InventoryFlag::Hangar | InventoryFlag::Unlocked)
            || (is_corp_cont && is_corp_sag_flag(cur.flag));
        if !child_flag_ok || hop == 7 {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: format!(
                    "Container {target_loc} exceeds nesting depth or has invalid parent flag"
                ),
                client_error: None,
            });
        }
        cur = parent;
    }

    let out_flag = match parsed.explicit_out_flag {
        None | Some(InventoryFlag::Hangar) => InventoryFlag::Hangar,
        Some(InventoryFlag::Unlocked) => InventoryFlag::Unlocked,
        Some(f) if is_corp_cont && root_sag_flag == Some(f) => InventoryFlag::Hangar,
        Some(bad) => {
            return Err(BridgeRpcError {
                code: "InvalidOutputLocation".into(),
                message: format!(
                    "Output flag {} is not valid inside station container {target_loc}",
                    bad as u16
                ),
                client_error: None,
            });
        }
    };

    Ok(ResolvedReprocessDest {
        owner_id: dest_item.owner_id,
        location_id: target_loc,
        flag: out_flag,
        constraint: ReprocessDestConstraint::Container {
            cont_id: target_loc,
            cap_m3,
        },
    })
}

/// Validates and resolves the destination `(owner_id, location_id, flag)` and capacity bounds
/// for a single reprocessed input item (`#1183`).
#[allow(clippy::too_many_arguments)]
fn resolve_reprocess_output_dest(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    char_skills: &std::collections::HashMap<TypeId, u8>,
    parsed: &ParsedReprocessArgs,
    source_item: &InventoryItemRecord,
    station_id: i64,
    char_id: i64,
    caller_corp: i64,
    now_unix: u64,
) -> Result<ResolvedReprocessDest, BridgeRpcError> {
    let default_station_flag = if is_corp_sag_flag(source_item.flag) {
        source_item.flag
    } else {
        InventoryFlag::Hangar
    };

    if !parsed.is_multi_arg
        || (parsed.explicit_out_owner.is_none()
            && parsed.explicit_out_loc.is_none()
            && parsed.explicit_out_flag.is_none())
    {
        return Ok(ResolvedReprocessDest {
            owner_id: source_item.owner_id,
            location_id: station_id,
            flag: default_station_flag,
            constraint: ReprocessDestConstraint::StationOrOfficeHangar,
        });
    }

    if let Some(out_owner) = parsed.explicit_out_owner {
        if out_owner != char_id && !(caller_corp > 0 && out_owner == caller_corp) {
            return Err(BridgeRpcError {
                code: "OwnershipOrLocationViolation".into(),
                message: format!(
                    "Output owner {out_owner} is neither character {char_id} nor their corporation"
                ),
                client_error: None,
            });
        }
    }

    let target_loc = parsed.explicit_out_loc.unwrap_or(station_id);
    let office_id_opt = find_corp_office_at_station(snap, caller_corp, station_id);
    let is_own_corp_office = caller_corp > 0 && office_id_opt == Some(target_loc);

    if snap.items.get(&target_loc).is_some_and(|i| i.type_id == 27 && !is_own_corp_office) {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Location {target_loc} is not a rented corporate office for character {char_id} at station {station_id}"
            ),
            client_error: None,
        });
    }

    // ReprocessingWnd keeps stale (prev_station_or_structure, Hangar) across undock/redock and sends owner=None (#1563).
    let stale_hangar_redirect = target_loc != station_id
        && !is_own_corp_office
        && parsed.explicit_out_owner.is_none()
        && parsed.explicit_out_flag == Some(InventoryFlag::Hangar)
        && (sde.stations.contains_key(&target_loc) || snap.structures.contains_key(&target_loc))
        && !snap.items.get(&target_loc).is_some_and(|i| i.location_id == station_id);
    let target_loc = if stale_hangar_redirect { station_id } else { target_loc };

    if target_loc == station_id || is_own_corp_office {
        return resolve_station_or_office_dest(
            snap, parsed, station_id, target_loc, is_own_corp_office,
            char_id, caller_corp, default_station_flag, now_unix,
        );
    }

    let Some(dest_item) = snap.items.get(&target_loc) else {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!("Output location {target_loc} does not exist"),
            client_error: None,
        });
    };
    let Some(dest_type) = sde.types.get(&dest_item.type_id) else {
        return Err(BridgeRpcError {
            code: "InvalidOutputLocation".into(),
            message: format!(
                "Output location {target_loc} has unknown SDE type {}",
                dest_item.type_id
            ),
            client_error: None,
        });
    };

    crate::services::inventory_fitting::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
        snap, dest_item,
    )?;
    if dest_item.flag == InventoryFlag::Locked || snap.is_item_in_alsc_lock(target_loc) {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: format!("Output destination {target_loc} is locked"),
            client_error: None,
        });
    }

    if dest_type.category_id == 6 {
        return resolve_ship_hold_dest(
            sde,
            snap,
            char_skills,
            parsed,
            dest_item,
            dest_type,
            station_id,
            char_id,
            caller_corp,
        );
    }

    resolve_station_container_dest(
        sde,
        snap,
        parsed,
        dest_item,
        dest_type,
        station_id,
        char_id,
        caller_corp,
        now_unix,
    )
}

/// Validates hold type rules and cumulative `m³` capacity across the whole planned batch before
/// any snapshot items are mutated (`#1183`).
fn validate_planned_reprocess_capacities(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    planned: &[PlannedReprocessItem],
) -> Result<(), BridgeRpcError> {
    use std::collections::HashMap;
    struct DestVolAccum {
        cap_m3: f64,
        added_m3: f64,
        freed_m3: f64,
    }
    let mut ship_holds: HashMap<(i64, InventoryFlag), DestVolAccum> = HashMap::new();
    let mut containers: HashMap<i64, DestVolAccum> = HashMap::new();

    for p in planned {
        let consumed_qty = (p.source_item.quantity - p.remainder).max(0) as f64;
        let src_freed_m3 = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(
            sde,
            &p.source_item,
        ) * consumed_qty;
        let out_vol: f64 = p
            .outputs
            .iter()
            .map(|mat| {
                sde.types.get(&mat.type_id).map(|t| t.volume).unwrap_or(0.0)
                    * (mat.quantity as f64)
            })
            .sum();
        match p.dest.constraint {
            ReprocessDestConstraint::StationOrOfficeHangar => {}
            ReprocessDestConstraint::ShipHold {
                ship_id,
                ship_type_id,
                flag,
                cap_m3,
            } => {
                if let Some(ship_type) = sde.types.get(&ship_type_id) {
                    if let Some(invalid_mat) = p.outputs.iter().find(|mat| {
                        !sde.types.get(&mat.type_id).is_some_and(|mat_type| {
                            crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(
                                flag as u16,
                                mat_type,
                                ship_type,
                            )
                        })
                    }) {
                        let msg = format!(
                            "Reprocessing output type {} cannot be stored in ship hold flag {flag:?}",
                            invalid_mat.type_id
                        );
                        return Err(BridgeRpcError::with_client_error(
                            "InvalidItemTypeForHold",
                            msg.clone(),
                            ClientError::CustomInfo(msg),
                        ));
                    }
                }
                let entry = ship_holds.entry((ship_id, flag)).or_insert(DestVolAccum {
                    cap_m3,
                    added_m3: 0.0,
                    freed_m3: 0.0,
                });
                entry.added_m3 += out_vol;
                if p.source_item.location_id == ship_id && p.source_item.flag == flag {
                    entry.freed_m3 += src_freed_m3;
                }
            }
            ReprocessDestConstraint::Container { cont_id, cap_m3 } => {
                let entry = containers.entry(cont_id).or_insert(DestVolAccum {
                    cap_m3,
                    added_m3: 0.0,
                    freed_m3: 0.0,
                });
                entry.added_m3 += out_vol;
                if p.source_item.location_id == cont_id {
                    entry.freed_m3 += src_freed_m3;
                }
            }
        }
    }

    for ((ship_id, flag), acc) in ship_holds {
        let existing_used = sum_items_volume_m3(
            sde,
            snap.items
                .values()
                .filter(|it| it.location_id == ship_id && it.flag == flag),
        );
        let net_used = (existing_used - acc.freed_m3).max(0.0);
        if acc.cap_m3 <= 0.0 || net_used + acc.added_m3 > acc.cap_m3 + 1e-4 {
            return Err(crate::services::inventory_fitting::ship_hold_capacity_error(
                flag as u16,
                acc.cap_m3,
                net_used,
                acc.added_m3,
            ));
        }
    }

    for (cont_id, acc) in containers {
        let existing_used = sum_items_volume_m3(
            sde,
            snap.items.values().filter(|it| it.location_id == cont_id),
        );
        let net_used = (existing_used - acc.freed_m3).max(0.0);
        if acc.cap_m3 <= 0.0 || net_used + acc.added_m3 > acc.cap_m3 + 1e-4 {
            return Err(BridgeRpcError {
                code: "CapacityExceeded".into(),
                message: format!(
                    "Container {cont_id} capacity of {:.2} m³ exceeded (used {:.2} m³, required {:.2} m³)",
                    acc.cap_m3, net_used, acc.added_m3
                ),
                client_error: None,
            });
        }
    }

    Ok(())
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("entity" | "beyonce", "CmdMineRepeatedly") | ("dogmaIM", "Mine") => {
            Some(handle_mine(ctx, session, bound_entry, args).await)
        }

        // parity: plan:S-1
        ("reprocessingSvc", "Reprocess") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing requires being docked at a station or structure".into(),
                    client_error: None,
                }));
            };
            let parsed = match parse_reprocess_args(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            enum ReprocessItemDelta {
                Updated(InventoryItemRecord, Vec<(i32, PyRep)>),
                Deleted(InventoryItemRecord),
            }
            let mut item_deltas: Vec<ReprocessItemDelta> = Vec::new();

            let now_unix = ctx.now_unix();
            if let Some(cid) = session.corporation_id.filter(|&id| id > 0) {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
                    ctx, cid,
                );
            }
            let char_skills = ctx.character_skill_levels(char_id);
            let is_batch_call = parsed.is_batch_call;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if station_id >= 1_000_000_000_000 {
                    crate::services::structure_profile_services::require_structure_service_access_in_snap(
                        Some(ctx),
                        snap,
                        station_id,
                        char_id,
                        crate::services::structure_profile_services::StructureServiceKind::Reprocessing,
                        now_unix,
                    )?;
                } else if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 5) {
                    return Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {station_id} does not offer reprocessing facilities"),
                        client_error: None,
                    });
                }

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);

                // Phase 1: Validate every input item, output destination, cumulative ISK tax,
                // and cumulative m³ capacity before mutating snapshot items or balances.
                let mut planned: Vec<PlannedReprocessItem> =
                    Vec::with_capacity(parsed.item_ids.len());
                let mut skipped_insufficient_err: Option<BridgeRpcError> = None;
                let mut sim_next_entity_id = snap.next_entity_id;

                for &item_id in &parsed.item_ids {
                    let Some(item) = snap.items.get(&item_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} does not exist"),
                            client_error: None,
                        });
                    };
                    let personal_ok = item.owner_id == char_id
                        && matches!(
                            item.flag,
                            InventoryFlag::Hangar
                                | InventoryFlag::Cargo
                                | InventoryFlag::SpecializedAsteroidHold
                                | InventoryFlag::SpecializedOreHold
                                | InventoryFlag::SpecializedIceHold
                                | InventoryFlag::SpecializedGasHold
                        );
                    let corp_ok = caller_corp > 0
                        && item.owner_id == caller_corp
                        && is_corp_sag_flag(item.flag)
                        && caller_can_take_corp_hangar_item_at(
                            snap,
                            char_id,
                            caller_corp,
                            item.flag,
                            Some(station_id),
                            now_unix,
                        );
                    let in_station_hangar = item.location_id == station_id;
                    let in_docked_parent = personal_ok
                        && matches!(
                            item.flag,
                            InventoryFlag::Cargo
                                | InventoryFlag::SpecializedAsteroidHold
                                | InventoryFlag::SpecializedOreHold
                                | InventoryFlag::SpecializedIceHold
                                | InventoryFlag::SpecializedGasHold
                        )
                        && snap.items.get(&item.location_id).is_some_and(|parent| {
                            parent.location_id == station_id
                                && parent.flag == InventoryFlag::Hangar
                                && parent.owner_id == char_id
                                && !snap.is_item_or_ancestor_locked_or_impounded(item.location_id)
                        });
                    let from_loc_ok = parsed.from_loc.is_none_or(|fl| fl == item.location_id);
                    let valid_location = (in_station_hangar || in_docked_parent) && from_loc_ok;
                    if (!personal_ok && !corp_ok) || !valid_location || item.quantity <= 0 {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!(
                                "Item {item_id} must be owned by character {char_id} or an authorized corporate division at station {station_id}"
                            ),
                            client_error: None,
                        });
                    }
                    let active_ship_id = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.active_ship_id)
                        .or(session.ship_id);
                    if active_ship_id == Some(item_id)
                        || snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "CannotReprocessActiveShip".into(),
                            message: format!(
                                "Item {item_id} is your currently active ship and cannot be reprocessed"
                            ),
                            client_error: None,
                        });
                    }
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotReprocessable".into(),
                            message: format!("Unknown item type {}", item.type_id),
                            client_error: None,
                        });
                    };
                    if !ctx
                        .sde
                        .type_materials
                        .get(&inv_type.type_id)
                        .is_some_and(|m| !m.is_empty())
                        && ctx.sde.randomized_type_materials_for(inv_type.type_id).is_empty()
                    {
                        return Err(BridgeRpcError {
                            code: "ItemNotReprocessable".into(),
                            message: format!(
                                "Item type {} ({}) has no SDE typeMaterials and cannot be reprocessed",
                                item.type_id, inv_type.name
                            ),
                            client_error: None,
                        });
                    }
                    if crate::services::inventory_fitting::fitting_containers_ships::container_has_active_audit_log_snap(
                        snap,
                        item_id,
                        now_unix as u64,
                    ) {
                        return Err(BridgeRpcError {
                            code: "ContainerHasActiveAuditLog".into(),
                            message: format!(
                                "Container {item_id} has audit log entries younger than 3 weeks and cannot be reprocessed"
                            ),
                            client_error: None,
                        });
                    }
                    if item.singleton
                        || snap.items.values().any(|child| child.location_id == item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemMustBeRepackaged".into(),
                            message: format!(
                                "Item {item_id} must be emptied and repackaged before reprocessing"
                            ),
                            client_error: None,
                        });
                    }
                    if item.flag == InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!("Item {item_id} is locked and cannot be reprocessed"),
                            client_error: None,
                        });
                    }
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let remainder = item.quantity % portion_size;
                    let rng_bits = ctx.mechanic_rng.try_next_u64().unwrap_or(0);
                    let Some(materials) = resolve_reprocessable_materials_for_portions(
                        &ctx.sde,
                        inv_type,
                        item_id,
                        sim_next_entity_id,
                        rng_bits,
                        portions.max(1),
                        false,
                    ) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotReprocessable".into(),
                            message: format!(
                                "Item type {} ({}) has no SDE typeMaterials and cannot be reprocessed",
                                item.type_id, inv_type.name
                            ),
                            client_error: None,
                        });
                    };
                    if item.quantity < portion_size {
                        let err = BridgeRpcError {
                            code: "InsufficientBatchPortion".into(),
                            message: format!(
                                "Need at least {portion_size} units to reprocess type {} (have {})",
                                item.type_id, item.quantity
                            ),
                            client_error: None,
                        };
                        if parsed.item_ids.len() > 1 {
                            skipped_insufficient_err.get_or_insert(err);
                            continue;
                        }
                        return Err(err);
                    }

                    let dest = resolve_reprocess_output_dest(
                        &ctx.sde,
                        snap,
                        &char_skills,
                        &parsed,
                        &item,
                        station_id,
                        char_id,
                        caller_corp,
                        now_unix,
                    )?;

                    let (station_base_yield, yield_rate, station_tax_rate) =
                        compute_reprocessing_rates_for_item_at(
                            &ctx.sde,
                            snap,
                            char_id,
                            station_id,
                            inv_type,
                            Some(now_unix),
                        );
                    let mut gross_value_cents = 0_i64;
                    let mut outputs = Vec::with_capacity(materials.len());
                    for mat in &materials {
                        let unit_val = ctx
                            .sde
                            .types
                            .get(&mat.material_type_id)
                            .map(|t| t.base_price.max(4.0))
                            .unwrap_or(4.0);
                        let unit_cents = IskCents::from_f64_isk(unit_val).0;
                        let out_qty = ((mat.quantity as f64) * yield_rate).floor() as i32;
                        gross_value_cents = gross_value_cents
                            .saturating_add(unit_cents.saturating_mul(out_qty.max(0) as i64));
                        if out_qty > 0 {
                            sim_next_entity_id += 1;
                            outputs.push(PlannedOutputMat {
                                type_id: mat.material_type_id,
                                quantity: out_qty,
                            });
                        }
                    }
                    let tax_cents = if station_tax_rate > 0.0 && gross_value_cents > 0 {
                        IskCents(
                            ((gross_value_cents as f64) * station_tax_rate)
                                .round()
                                .max(100.0) as i64,
                        )
                    } else {
                        IskCents(0)
                    };
                    planned.push(PlannedReprocessItem {
                        source_item: item,
                        remainder,
                        dest,
                        outputs,
                        station_base_yield,
                        yield_rate,
                        station_tax_rate,
                        tax_cents,
                    });
                }
                if planned.is_empty() {
                    if let Some(err) = skipped_insufficient_err {
                        return Err(err);
                    }
                }

                let total_tax_cents = planned
                    .iter()
                    .fold(0_i64, |acc, p| acc.saturating_add(p.tax_cents.0));
                if total_tax_cents > 0 {
                    let char_balance = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.balance)
                        .unwrap_or(IskCents(0));
                    if char_balance.0 < total_tax_cents {
                        return Err(BridgeRpcError::from_user_key(
                            eve_proto::UserErrorKey::NotEnoughMoney,
                        ));
                    }
                }

                validate_planned_reprocess_capacities(&ctx.sde, snap, &planned)?;

                // Phase 2: Commit validated input consumption, mineral minting, and tax deductions.
                let mut batch_results = Vec::with_capacity(planned.len());
                let mut reprocessed_inputs: Vec<(TypeId, u64)> = Vec::with_capacity(planned.len());
                for p in planned {
                    let item_id = p.source_item.item_id;
                    let old_qty = p.source_item.quantity;
                    let consumed_qty = u64::try_from(old_qty.saturating_sub(p.remainder)).unwrap_or(0);
                    if consumed_qty > 0 {
                        reprocessed_inputs.push((p.source_item.type_id, consumed_qty));
                    }
                    if p.remainder > 0 {
                        if let Some(im) = snap.items.get_mut(&item_id) {
                            im.quantity = p.remainder;
                            im.version += 1;
                            item_deltas.push(ReprocessItemDelta::Updated(
                                im.clone(),
                                vec![(9, PyRep::Integer(old_qty as i64))],
                            ));
                        }
                    } else {
                        snap.items.remove(&item_id);
                        item_deltas.push(ReprocessItemDelta::Deleted(p.source_item));
                    }

                    let mut outputs: Vec<(i64, TypeId, i32)> = Vec::with_capacity(p.outputs.len());
                    for mat in p.outputs {
                        snap.next_entity_id += 1;
                        let mid = snap.next_entity_id;
                        let new_item = InventoryItemRecord {
                            item_id: mid,
                            type_id: mat.type_id,
                            owner_id: p.dest.owner_id,
                            location_id: p.dest.location_id,
                            flag: p.dest.flag,
                            quantity: mat.quantity,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        };
                        snap.items.insert(mid, new_item.clone());
                        item_deltas.push(ReprocessItemDelta::Updated(
                            new_item,
                            vec![(3, PyRep::Integer(0))],
                        ));
                        outputs.push((mid, mat.type_id, mat.quantity));
                    }

                    let tax_isk = (p.tax_cents.0 as f64) / 100.0;
                    if p.tax_cents.0 > 0 {
                        let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch
                                .balance
                                .checked_sub(p.tax_cents)
                                .unwrap_or(IskCents(0));
                            ch.balance
                        } else {
                            IskCents(0)
                        };
                        if station_id >= 1_000_000_000_000 {
                            if let Some(owner_corp_id) =
                                snap.structures.get(&station_id).map(|s| s.owner_corp_id)
                            {
                                crate::services::station_structure::structure_upwell_ops::credit_structure_owner_corp_div1(
                                    snap,
                                    owner_corp_id,
                                    p.tax_cents,
                                    eve_db::ledger::ref_types::TRANSACTION_TAX,
                                    "ReprocessingTax",
                                    char_id,
                                    format!(
                                        "Structure reprocessing tax ({:.2}%) at structure {station_id}",
                                        p.station_tax_rate * 100.0
                                    ),
                                    now_unix,
                                );
                            }
                        }
                        snap.append_journal(
                            char_id,
                            "ReprocessingTax",
                            p.tax_cents,
                            bal_after,
                            format!(
                                "Station reprocessing tax ({:.2}%) at station {station_id}",
                                p.station_tax_rate * 100.0
                            ),
                        );
                    }

                    let (trit_id, _, trit_qty) = outputs
                        .iter()
                        .copied()
                        .find(|&(_, tid, _)| tid == 34)
                        .or_else(|| outputs.first().copied())
                        .unwrap_or((0, 0, 0));
                    let output_entries: Vec<PyRep> = outputs
                        .into_iter()
                        .map(|(iid, tid, qty)| {
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(iid)),
                                ("typeID", PyRep::Integer(tid as i64)),
                                ("quantity", PyRep::Integer(qty as i64)),
                            ])
                        })
                        .collect();
                    batch_results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("stationBaseYield", PyRep::Real(p.station_base_yield)),
                        ("yieldRate", PyRep::Real(p.yield_rate)),
                        ("stationTaxRate", PyRep::Real(p.station_tax_rate)),
                        ("reprocessingTaxISK", PyRep::Real(tax_isk)),
                        ("remainderQuantity", PyRep::Integer(p.remainder as i64)),
                        ("tritaniumItemID", PyRep::Integer(trit_id)),
                        ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
                        ("outputs", PyRep::List(output_entries)),
                    ]));
                }

                Ok((batch_results, reprocessed_inputs))
            });

            match res {
                Ok((mut batch_results, reprocessed_inputs)) => {
                    for delta in item_deltas {
                        match delta {
                            ReprocessItemDelta::Updated(rec, changes) => {
                                ctx.emit_item_change_routed(char_id, &rec, changes);
                            }
                            ReprocessItemDelta::Deleted(rec) => {
                                ctx.emit_item_deleted_routed(char_id, &rec);
                            }
                        }
                    }
                    crate::quasar_gateway::isolate_side_effect("space_entity_mining.reprocess_achievements", || {
                        for (tid, qty) in reprocessed_inputs {
                            let group_id = ctx.sde.types.get(&tid).map(|t| t.group_id);
                            ctx.quasar_gateway.progression.career_goals.on_career_gameplay_event(
                                char_id as u32,
                                crate::services::quasar_career_goals::CareerGameplayEvent {
                                    activity_kind: "refining",
                                    type_id: Some(tid),
                                    group_id,
                                    ..Default::default()
                                },
                                qty,
                                &ctx.quasar_gateway,
                            );
                        }
                        ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
                            char_id as u32,
                            &["refineOreCount", "finishedReprocessingCount", "reprocessingInput", "reprocessingOutput"],
                            batch_results.len().max(1) as u64,
                            ctx.now_unix(),
                            &ctx.quasar_gateway,
                        );
                    });
                    // parity: notify:OnReprocessingMaterialsChanged
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnReprocessingMaterialsChanged,
                    );
                    if is_batch_call {
                        Some(Ok(PyRep::List(batch_results)))
                    } else {
                        Some(Ok(batch_results.pop().unwrap_or(PyRep::None)))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("reprocessingSvc", "GetQuotes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing quote requires being docked at a station or structure".into(),
                    client_error: None,
                }));
            };
            if station_id < 1_000_000_000_000
                && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 5)
            {
                return Some(Err(BridgeRpcError {
                    code: "StationServiceUnavailable".into(),
                    message: format!("Station {station_id} does not offer reprocessing facilities"),
                    client_error: None,
                }));
            }
            let now_unix = ctx.now_unix();
            if station_id >= 1_000_000_000_000 {
                let acl_err = ctx.db.with_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap_at(
                        snap, station_id, char_id, Some(now_unix),
                    ) {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: format!("Character {char_id} is denied access to structure {station_id}"),
                            client_error: None,
                        });
                    }
                    let _ = crate::services::structure_profile_services::check_service_domain_acl_and_tax(
                        snap, station_id, char_id, &[3, 35_899], false, Some(now_unix), "AccessDenied", "reprocessing",
                    )?;
                    Ok(())
                });
                if let Err(e) = acl_err {
                    return Some(Err(e));
                }
            }
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) => list.iter().filter_map(|a| a.as_i64()).collect(),
                Some(PyRep::Tuple(tuple)) => tuple.iter().filter_map(|a| a.as_i64()).collect(),
                Some(PyRep::Dict(entries)) => entries
                    .iter()
                    .filter_map(|(k, v)| {
                        k.as_i64()
                            .or_else(|| v.dict_get("itemID").and_then(PyRep::as_i64))
                            .or_else(|| v.as_i64())
                    })
                    .collect(),
                Some(first) => first.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let (station_base_yield, station_tax_rate, reproc_lvl, reproc_eff_lvl) =
                ctx.db.with_snapshot(|snap| {
                    let dummy_type = ctx.sde.types.get(&1230);
                    let (mut base, _, tax) = dummy_type
                        .map(|t| compute_reprocessing_rates_for_item_at(&ctx.sde, snap, char_id, station_id, t, Some(now_unix)))
                        .unwrap_or((0.50, 0.50, 0.0));
                    if station_id >= 1_000_000_000_000 {
                        base = compute_structure_rigged_base_yield(&ctx.sde, snap, station_id, None);
                    }
                    let char_skills = snap.skills.get(&char_id);
                    let r = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
                    let re = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
                    (base, tax, r, re)
                });
            if item_ids.is_empty() && args.is_empty() {
                return Some(Ok(PyRep::keyval([
                    ("stationBaseYield", PyRep::Real(station_base_yield)),
                    ("stationTaxRate", PyRep::Real(station_tax_rate)),
                    ("reprocessingSkillLevel", PyRep::Integer(reproc_lvl as i64)),
                    ("reprocessingEfficiencyLevel", PyRep::Integer(reproc_eff_lvl as i64)),
                ])));
            }
            let quotes = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let mut quotes = Vec::new();
                for item_id in item_ids {
                    let Some(item) = snap.items.get(&item_id) else { continue };
                    if item.owner_id != char_id && !(caller_corp > 0 && item.owner_id == caller_corp) {
                        continue;
                    }
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else { continue };
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let Some(materials) = resolve_reprocessable_materials_for_portions(
                        &ctx.sde,
                        inv_type,
                        item_id,
                        snap.next_entity_id,
                        0,
                        portions,
                        true,
                    ) else {
                        continue;
                    };
                    let (item_base_yield, yield_rate, item_tax_rate) =
                        compute_reprocessing_rates_for_item_at(&ctx.sde, snap, char_id, station_id, inv_type, Some(now_unix));
                    let net_yield_rate = yield_rate;
                    let quantity_to_process = portions * portion_size;
                    let left_overs = (item.quantity - quantity_to_process).max(0);
                    let mut est_outputs = Vec::new();
                    for mat in &materials {
                        let out_qty = ((mat.quantity as f64) * yield_rate).floor() as i32;
                        let unrecoverable = (mat.quantity as i64 - out_qty as i64).max(0);
                        if out_qty > 0 {
                            est_outputs.push(PyRep::keyval([
                                ("typeID", PyRep::Integer(mat.material_type_id as i64)),
                                ("quantity", PyRep::Integer(out_qty as i64)),
                                ("unrecoverable", PyRep::Integer(unrecoverable)),
                                ("station", PyRep::Integer(0)),
                                ("iskCost", PyRep::Real(0.0)),
                            ]));
                        }
                    }
                    quotes.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("typeID", PyRep::Integer(item.type_id as i64)),
                        ("quantityToProcess", PyRep::Integer(quantity_to_process as i64)),
                        ("leftOvers", PyRep::Integer(left_overs as i64)),
                        ("iskCost", PyRep::Real(0.0)),
                        ("totalISKCost", PyRep::Real(0.0)),
                        ("stationBaseYield", PyRep::Real(item_base_yield)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(item_tax_rate)),
                        ("netYieldRate", PyRep::Real(net_yield_rate)),
                        ("portionSize", PyRep::Integer(portion_size as i64)),
                        ("outputs", PyRep::List(est_outputs)),
                    ]));
                }
                quotes
            });
            if args.len() < 2 {
                // V24.01 `Quotes._GetQuotes` (`reprocessing/ui/quotes.pyj:14-19`) passes `{itemID: DBRow}`
                // or a list/tuple/itemID; whenever `activeShipID` is omitted (`args.len() < 2`), always
                // return the V24.01 3-tuple `(stationTax, stationEfficiencyDict, {itemID: KeyVal(...)})` (`#723`).
                Some(Ok(
                    crate::router::client_compat::build_v24_reprocessing_quotes_tuple(
                        PyRep::Real(station_tax_rate),
                        PyRep::Real(station_base_yield),
                        quotes,
                    ),
                ))
            } else {
                Some(Ok(PyRep::Tuple(vec![
                    PyRep::Real(station_tax_rate),
                    PyRep::Real(station_base_yield),
                    PyRep::List(quotes),
                ])))
            }
        }


        _ => None,
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    _kwargs: &'a std::collections::HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle(ctx, session, service, method, args, bound_entry))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("entity", "CmdMineRepeatedly", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("beyonce", "CmdMineRepeatedly", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "Mine", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("reprocessingSvc", "Reprocess", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("reprocessingSvc", "GetQuotes", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

#[cfg(test)]
#[path = "space_entity_mining_tests.rs"]
mod space_entity_mining_tests;

#[path = "space_entity_mining_cov_unit_tests.rs"]
#[cfg(test)]
mod space_entity_mining_cov_unit_tests;
