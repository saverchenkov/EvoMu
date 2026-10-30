//! `space_entity_mining` — `entity.CmdMineRepeatedly` / `beyonce.CmdMineRepeatedly` / `dogmaIM.Mine` asteroid mining
//! for mining modules and launched mining drones (`MN-09`, `feat:mining.*`).
//!
//! Arguments are positional only: `(droneIDs, targetID)`, `(droneID, targetID)` or `(targetID[, moduleID])`.

#![forbid(unsafe_code)]

use crate::formulas::compute_reprocessing_yield_rate;
use crate::router::{BoundObjectEntry, ServerContext};
pub use crate::services::mining_advanced::mining_compression::{
    caller_can_take_corp_hangar_item, compute_structure_reprocessing_base_yield,
    compute_structure_rigged_base_yield, resolve_reprocessable_materials,
    resolve_reprocessable_materials_for_portions, structure_has_online_reprocessing_service,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_sde::{InvType, SdeRegistry, TypeId};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

const BEYONCE_SVC: &str = "beyonce";

fn no_solar_system() -> BridgeRpcError {
    BridgeRpcError {
        // rpc-lint: allow "NoSolarSystem" is an error code, not a beyonce method
        code: "NoSolarSystem".into(),
        message: "Session has no active solarSystemID".into(),
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
        });
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "CannotMineInWarp".into(),
            message: "Cannot activate mining lasers or drones while in warp".into(),
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
                });
            }
            if db_ast.quantity <= 0 {
                return Err(BridgeRpcError {
                    code: "AsteroidDepleted".into(),
                    message: format!("Asteroid {asteroid_id} is depleted"),
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
        });
    };

    if ast_state.remaining_units == 0 || ast_state.depleted {
        return Err(BridgeRpcError {
            code: "AsteroidDepleted".into(),
            message: format!("Asteroid {asteroid_id} is depleted"),
        });
    }

    // Target range validation
    let mut ast_surface_dist_m = 0.0_f64;
    if let Some((_, ast_ball)) = handle.get_ball_snapshot(asteroid_id).await.ok().flatten() {
        let surface_dist = (ship_ball.position.distance(ast_ball.position) - ship_ball.radius - ast_ball.radius).max(0.0);
        ast_surface_dist_m = surface_dist;
        let max_range = 35_000.0;
        if surface_dist > max_range {
            return Err(BridgeRpcError {
                code: "OutOfRange".into(),
                message: format!("Asteroid {asteroid_id} is {surface_dist:.1} m away (max range {max_range} m)"),
            });
        }
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
    let already_mining = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        drone_ids.iter().any(|did| {
            let ball_id = rt.active_drones_in_space.values()
                .find(|d| d.drone_item_id == *did || d.ball_id == *did)
                .map(|d| d.ball_id)
                .unwrap_or(*did);
            rt.active_drone_miners.get(&ball_id).is_some_and(|m| m.ship_id == ship_id && m.asteroid_id == asteroid_id)
        })
    });
    if already_mining {
        return Err(BridgeRpcError {
            code: "DroneAlreadyMining".into(),
            message: "One or more drones are already mining this asteroid".into(),
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
    let (base_yield_m3, waste_prob) = if is_drone {
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
                });
            }
            if !is_ice_drone && ast_group_id == 465 {
                return Err(BridgeRpcError {
                    code: "InvalidTargetGroup".into(),
                    message: "Ore mining drones cannot harvest Ice asteroids (group 465)".into(),
                });
            }
            // 'Excavator' Mining Drone (41030) / 'Excavator' Ice Harvesting Drone (43681) requires Rorqual (28352) + active Industrial Core (group 515)
            if matches!(d_item.type_id, 41030 | 43681 | 41031 | 90621 | 90622) {
                if ship_type_id != 28352 {
                    return Err(BridgeRpcError {
                        code: "InvalidShipForExcavator".into(),
                        message: "Excavator mining drones can only be operated by a Rorqual (28352)".into(),
                    });
                }
                if active_ind_core_type.is_none() {
                    return Err(BridgeRpcError {
                        code: "IndustrialCoreRequired".into(),
                        message: "Excavator mining drones require an active Capital Industrial Core on the Rorqual".into(),
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
                let int = sk.and_then(|m| m.get(&3436)).map(|s| s.level).unwrap_or(0);
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

        let rorq_ore_bonus_mult = if ship_type_id == 28352 && cap_ind_lvl > 0 {
            let per_lvl = ctx
                .sde
                .types
                .get(&28352)
                .and_then(|t| t.default_attributes.get(&3223).copied())
                .unwrap_or(10.0);
            1.0 + (per_lvl / 100.0) * (cap_ind_lvl.min(5) as f64)
        } else {
            1.0
        };
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
            let is_ice_drone = drone_t
                .and_then(|t| t.default_attributes.get(&182).copied())
                == Some(43702.0);
            let base_dur_s = drone_t
                .and_then(|t| t.default_attributes.get(&73).copied())
                .map(|dur_ms| if dur_ms >= 1000.0 { dur_ms / 1000.0 } else { dur_ms })
                .unwrap_or(60.0)
                .max(1.0);
            let (drone_mult, mine_dur_s) = if is_ice_drone {
                let ice_spec_factor = if matches!(d_item.type_id, 43681 | 43682) {
                    1.0 - 0.02 * (ice_spec_lvl.min(5) as f64)
                } else {
                    1.0
                };
                let rorq_ice_dur_factor = if ship_type_id == 28352 {
                    1.0 - 0.05 * (cap_ind_lvl.min(5) as f64)
                } else {
                    1.0
                };
                let core_ice_dur_factor = (1.0 / ind_core_ice_rate_mult).clamp(0.1, 1.0);
                let dur_s = base_dur_s
                    * (1.0 - 0.05 * (ice_op_lvl.min(5) as f64))
                    * ice_spec_factor
                    * rorq_ice_dur_factor
                    * core_ice_dur_factor;
                (ind_core_ice_rate_mult, dur_s.max(1.0))
            } else {
                let spec_factor = if matches!(d_item.type_id, 10250 | 41030 | 90621) {
                    1.0 + 0.02 * (drone_spec_lvl as f64)
                } else {
                    1.0
                };
                let mult = (1.0 + 0.05 * (drone_op_lvl as f64))
                    * (1.0 + 0.10 * (drone_int_lvl as f64))
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
        (total_drone_yield, max_waste_prob)
    } else if let (Some(fit), Some(mod_id)) = (&fit, primary_id) {
        let Some(m) = fit.modules.get(&mod_id) else {
            return Err(BridgeRpcError { code: "InvalidMiningModule".into(), message: "Mining module is not fitted to active ship".into() });
        };
        if m.state == eve_dogma::ModuleState::Offline || !mod_compatible(m.type_id) {
            return Err(BridgeRpcError { code: "InvalidMiningModule".into(), message: "Specified module is offline or incompatible with target resource".into() });
        }
        let y = fit.get_module_effective_attribute(mod_id, 77, &ctx.sde);
        let raw_w = fit.get_module_effective_attribute(mod_id, 3154, &ctx.sde);
        (y, if raw_w > 1.0 { raw_w / 100.0 } else { raw_w })
    } else if let Some(fit) = &fit {
        let m = fit.modules.values().find(|m| m.state != eve_dogma::ModuleState::Offline && mod_compatible(m.type_id));
        if let Some(m) = m {
            let y = fit.get_module_effective_attribute(m.item_id, 77, &ctx.sde);
            let raw_w = fit.get_module_effective_attribute(m.item_id, 3154, &ctx.sde);
            (y, if raw_w > 1.0 { raw_w / 100.0 } else { raw_w })
        } else {
            let has_any_mining_mod = fit.modules.values().any(|m| {
                ctx.sde.types.get(&m.type_id).is_some_and(|t| matches!(t.group_id, 54 | 464 | 483 | 737))
            });
            if has_any_mining_mod || ore_group_id == 711 || ore_group_id == 465 || !matches!(ship_id, 100_000_001 | 100_000_002) {
                return Err(BridgeRpcError { code: "NoMiningModuleFitted".into(), message: "Ship has no online compatible mining/harvesting module fitted".into() });
            }
            (40.0, 0.0)
        }
    } else {
        if ore_group_id == 711 || ore_group_id == 465 || !matches!(ship_id, 100_000_001 | 100_000_002) {
            return Err(BridgeRpcError { code: "NoMiningModuleFitted".into(), message: "Ship has no online compatible mining/harvesting module fitted".into() });
        }
        (40.0, 0.0)
    };

    let ship_attrs = ctx.sde.types.get(&ship_type_id).map(|t| &t.default_attributes);
    let has_ore_hold = ship_attrs.is_some_and(|a| a.get(&1556).copied().unwrap_or(0.0) > 0.0 || a.get(&3227).copied().unwrap_or(0.0) > 0.0);
    let has_gas_hold = ship_attrs.is_some_and(|a| a.get(&1557).copied().unwrap_or(0.0) > 0.0);
    let has_ice_hold = ship_attrs.is_some_and(|a| a.get(&3136).copied().unwrap_or(0.0) > 0.0);

    let target_flag = if ore_group_id == 465 && has_ice_hold {
        InventoryFlag::SpecializedIceHold
    } else if ore_group_id == 711 && has_gas_hold {
        InventoryFlag::SpecializedGasHold
    } else if ore_category_id == 25 && has_ore_hold {
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
                rt.active_drone_miners.insert(
                    drone_ball_id,
                    crate::services::combat_crimewatch::DroneMiningState {
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
                    },
                );
            });
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
        });
    }
    let requested_units = (((base_yield_m3 / unit_vol).floor() as u64).max(1)).min(max_fit_units);

    let extraction = match handle.mine_asteroid(asteroid_id, requested_units, waste_prob).await {
        Ok(e) => e,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "MiningFailed".into(),
                message: e,
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

    if extraction.mined_units > 0 {
        crate::services::space_mining_harvest::deposit_mined_ore_and_record_ledger_with_waste(
            ctx,
            char_id,
            ship_id,
            sys_id,
            target_flag,
            extraction.ore_type_id,
            extraction.mined_units,
            extraction.wasted_units,
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

pub fn compute_reprocessing_rates_for_item(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    char_id: i64,
    station_id: i64,
    inv_type: &InvType,
) -> (f64, f64, f64) {
    let char_skills = snap.skills.get(&char_id);
    let reproc_lvl = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
    let reproc_eff_lvl = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
    let ore_proc_lvl = inv_type
        .default_attributes
        .get(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE)
        .and_then(|&skill_id_f| char_skills.and_then(|m| m.get(&(skill_id_f as TypeId))).map(|s| s.level))
        .unwrap_or(0);
    let scrap_lvl = char_skills.and_then(|m| m.get(&12196)).map(|s| s.level).unwrap_or(0);
    let is_ore_or_ice = inv_type.category_id == 25
        || inv_type.group_id == 465
        || inv_type.group_id == 4932
        || inv_type.default_attributes.contains_key(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE);
    let station_base_yield = if station_id >= 1_000_000_000_000 {
        if is_ore_or_ice {
            compute_structure_rigged_base_yield(sde, snap, station_id, Some(inv_type))
        } else {
            0.50
        }
    } else {
        sde.solar_systems
            .values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.station_id == station_id)
            .map(|st| st.reprocessing_efficiency.clamp(0.30, 0.50))
            .unwrap_or(0.50)
    };
    let implant_bonus = snap
        .active_implants
        .get(&char_id)
        .map(|implants| {
            implants
                .iter()
                .filter_map(|&imp_id| sde.types.get(&imp_id)?.default_attributes.get(&379))
                .map(|&bonus| bonus / 100.0)
                .sum::<f64>()
        })
        .unwrap_or(0.0);
    let yield_rate = if is_ore_or_ice {
        compute_reprocessing_yield_rate(
            station_base_yield,
            reproc_lvl,
            reproc_eff_lvl,
            ore_proc_lvl,
            implant_bonus,
        )
    } else {
        (station_base_yield * (1.0 + 0.02 * (scrap_lvl.min(5) as f64))).clamp(0.0, 0.55)
    };
    let station_tax_rate = if station_id >= 1_000_000_000_000 {
        crate::services::station_structure::structure_upwell_ops::load_ext_state(snap)
            .access_profiles
            .get(&station_id)
            .and_then(|p| {
                p.service_settings
                    .get(&3)
                    .or_else(|| p.service_settings.get(&35899))
                    .copied()
            })
            .map(|bps| (bps as f64 / 10_000.0).clamp(0.0, 1.0))
            .unwrap_or(0.0)
    } else {
        let owner_corp_id = sde
            .solar_systems
            .values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.station_id == station_id)
            .map(|st| st.owner_corporation_id as i64);
        let effective_standing: f64 = owner_corp_id
            .map(|cid| {
                let ch_opt = snap.characters.get(&char_id);
                let personal_raw = ch_opt
                    .and_then(|ch| ch.standings.get(&cid).copied())
                    .unwrap_or(0.0);
                let conn_lvl = char_skills
                    .and_then(|m| m.get(&3359))
                    .map(|s| s.level.min(5))
                    .unwrap_or(0);
                let has_completed_mission_for_corp = snap.active_missions.values().any(|m| {
                    m.character_id == char_id
                        && m.state == "Completed"
                        && sde.agents.get(&m.agent_id).is_some_and(|a| a.corporation_id == cid)
                }) || snap
                    .standing_transactions
                    .get(&char_id)
                    .is_some_and(|txs| {
                        txs.iter()
                            .any(|tx| tx.from_id == cid && tx.event_type_id == 85 && tx.modification > 0.0)
                    });
                let personal_eff = if has_completed_mission_for_corp && personal_raw >= 0.0 && conn_lvl > 0 {
                    (personal_raw + (10.0 - personal_raw) * 0.04 * (conn_lvl as f64)).clamp(-10.0, 10.0)
                } else {
                    personal_raw
                };
                let player_corp_id = ch_opt.map(|ch| ch.corporation_id).unwrap_or(0);
                let corp_standing = if player_corp_id > 0 {
                    let direct_corp_st = snap
                        .characters
                        .get(&player_corp_id)
                        .and_then(|c| c.standings.get(&cid).copied());
                    direct_corp_st.unwrap_or_else(|| {
                        let mut sum = 0.0;
                        let mut cnt = 0usize;
                        for member in snap.characters.values().filter(|m| m.corporation_id == player_corp_id) {
                            if let Some(&st) = member.standings.get(&cid) {
                                sum += st;
                                cnt += 1;
                            }
                        }
                        if cnt > 0 { sum / (cnt as f64) } else { 0.0 }
                    })
                } else {
                    0.0
                };
                personal_eff.max(corp_standing).max(0.0)
            })
            .unwrap_or(0.0);
        (0.05_f64 - 0.0075_f64 * effective_standing).clamp(0.0, 0.05)
    };
    (station_base_yield, yield_rate, station_tax_rate)
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

        // parity: plan:S-7
        ("reprocessingSvc", "SetSessionCheck") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ok = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(ok)))
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
                }));
            };
            let is_batch_call =
                args.len() >= 2 || matches!(args.first(), Some(PyRep::List(_)));
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) => list.iter().filter_map(|a| a.as_i64()).collect(),
                Some(first) => first.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Reprocess requires at least one integer itemID".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if station_id >= 1_000_000_000_000 {
                    let Some(st) = snap.structures.get(&station_id) else {
                        return Err(BridgeRpcError {
                            code: "StructureNotFound".into(),
                            message: format!("Structure {station_id} not found"),
                        });
                    };
                    if st.fuel_blocks == 0
                        || !structure_has_online_reprocessing_service(
                            &ctx.sde,
                            snap,
                            station_id,
                            st.type_id,
                        )
                    {
                        return Err(BridgeRpcError {
                            code: "ReprocessingServiceOffline".into(),
                            message: format!(
                                "Structure {station_id} does not have an online Reprocessing service"
                            ),
                        });
                    }
                    if !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                        snap, station_id, char_id,
                    ) {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: format!(
                                "Character {char_id} is denied access to structure {station_id} by its access profile"
                            ),
                        });
                    }
                } else if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 5) {
                    return Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {station_id} does not offer reprocessing facilities"),
                    });
                }

                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let mut batch_results = Vec::with_capacity(item_ids.len());
                for item_id in item_ids {
                    let Some(item) = snap.items.get(&item_id).cloned() else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {item_id} does not exist") });
                    };
                    let valid_owner_and_flag = (item.owner_id == char_id && item.flag == InventoryFlag::Hangar)
                        || (caller_corp > 0 && item.owner_id == caller_corp && caller_can_take_corp_hangar_item(snap, char_id, caller_corp, item.flag, session.corp_role));
                    if !valid_owner_and_flag || item.location_id != station_id || item.quantity <= 0 {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!("Item {item_id} must be owned by character {char_id} or an authorized corporate division at station {station_id}"),
                        });
                    }
                    let active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).or(session.ship_id);
                    if active_ship_id == Some(item_id) {
                        return Err(BridgeRpcError { code: "CannotReprocessActiveShip".into(), message: format!("Item {item_id} is your currently active ship and cannot be reprocessed") });
                    }
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else {
                        return Err(BridgeRpcError { code: "ItemNotReprocessable".into(), message: format!("Unknown item type {}", item.type_id) });
                    };
                    if item.singleton || snap.items.values().any(|child| child.location_id == item_id) {
                        return Err(BridgeRpcError { code: "ItemMustBeRepackaged".into(), message: format!("Item {item_id} must be emptied and repackaged before reprocessing") });
                    }
                    if item.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(item_id) {
                        return Err(BridgeRpcError { code: "ItemLocked".into(), message: format!("Item {item_id} is locked and cannot be reprocessed") });
                    }
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let remainder = item.quantity % portion_size;
                    let rng_bits = ctx.mechanic_rng.try_next_u64().unwrap_or(0);
                    let Some(materials) = resolve_reprocessable_materials_for_portions(
                        &ctx.sde,
                        inv_type,
                        item_id,
                        snap.next_entity_id,
                        rng_bits,
                        portions.max(1),
                        false,
                    ) else {
                        return Err(BridgeRpcError { code: "ItemNotReprocessable".into(), message: format!("Item type {} ({}) has no SDE typeMaterials and cannot be reprocessed", item.type_id, inv_type.name) });
                    };
                    if item.quantity < portion_size {
                        return Err(BridgeRpcError { code: "InsufficientBatchPortion".into(), message: format!("Need at least {portion_size} units to reprocess type {} (have {})", item.type_id, item.quantity) });
                    }
                    let (station_base_yield, yield_rate, station_tax_rate) =
                        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
                    let mut gross_value_cents = 0_i64;
                    for mat in &materials {
                        let unit_val = ctx.sde.types.get(&mat.material_type_id).map(|t| t.base_price.max(4.0)).unwrap_or(4.0);
                        let unit_cents = IskCents::from_f64_isk(unit_val).0;
                        let est_qty = ((mat.quantity as f64) * yield_rate).floor() as i64;
                        gross_value_cents = gross_value_cents.saturating_add(unit_cents.saturating_mul(est_qty.max(0)));
                    }
                    let tax_cents = if station_tax_rate > 0.0 && gross_value_cents > 0 {
                        IskCents(((gross_value_cents as f64) * station_tax_rate).round().max(100.0) as i64)
                    } else {
                        IskCents(0)
                    };
                    let tax_isk = (tax_cents.0 as f64) / 100.0;
                    if tax_cents.0 > 0 {
                        let char_balance = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                        if char_balance < tax_cents {
                            return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: format!("Insufficient ISK to pay reprocessing tax ({:.2} ISK required)", tax_isk) });
                        }
                    }
                    if remainder > 0 {
                        if let Some(im) = snap.items.get_mut(&item_id) {
                            im.quantity = remainder;
                            im.version += 1;
                        }
                    } else {
                        snap.items.remove(&item_id);
                    }
                    let mut outputs: Vec<(i64, TypeId, i32)> = Vec::with_capacity(materials.len());
                    for mat in &materials {
                        let out_qty = ((mat.quantity as f64) * yield_rate).floor() as i32;
                        if out_qty <= 0 { continue; }
                        snap.next_entity_id += 1;
                        let mid = snap.next_entity_id;
                        snap.items.insert(
                            mid,
                            InventoryItemRecord {
                                item_id: mid,
                                type_id: mat.material_type_id,
                                owner_id: item.owner_id,
                                location_id: station_id,
                                flag: item.flag,
                                quantity: out_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                        outputs.push((mid, mat.material_type_id, out_qty));
                    }

                    if tax_cents.0 > 0 {
                        let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
                            ch.balance
                        } else {
                            IskCents(0)
                        };
                        if station_id >= 1_000_000_000_000 {
                            if let Some(owner_corp_id) = snap.structures.get(&station_id).map(|s| s.owner_corp_id) {
                                if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
                                    corp.master_wallet.0 = corp.master_wallet.0.saturating_add(tax_cents.0);
                                }
                            }
                        }
                        snap.append_journal(
                            char_id,
                            "ReprocessingTax",
                            tax_cents,
                            bal_after,
                            format!(
                                "Station reprocessing tax ({:.2}%) at station {station_id}",
                                station_tax_rate * 100.0
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
                        ("stationBaseYield", PyRep::Real(station_base_yield)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("reprocessingTaxISK", PyRep::Real(tax_isk)),
                        ("remainderQuantity", PyRep::Integer(remainder as i64)),
                        ("tritaniumItemID", PyRep::Integer(trit_id)),
                        ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
                        ("outputs", PyRep::List(output_entries)),
                    ]));
                }

                Ok(batch_results)
            });

            match res {
                Ok(mut batch_results) => {
                    ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
                        char_id as u32,
                        &["refineOreCount", "finishedReprocessingCount", "reprocessingInput", "reprocessingOutput"],
                        batch_results.len().max(1) as u64,
                        ctx.now_unix(),
                    );
                    // parity: notify:OnReprocessingMaterialsChanged
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnReprocessingMaterialsChanged",
                        "charid",
                        vec![],
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
                }));
            };
            if station_id < 1_000_000_000_000
                && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 5)
            {
                return Some(Err(BridgeRpcError {
                    code: "StationServiceUnavailable".into(),
                    message: format!("Station {station_id} does not offer reprocessing facilities"),
                }));
            }
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) => list.iter().filter_map(|a| a.as_i64()).collect(),
                Some(first) => first.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let (station_base_yield, station_tax_rate, reproc_lvl, reproc_eff_lvl) =
                ctx.db.with_snapshot(|snap| {
                    let dummy_type = ctx.sde.types.get(&1230);
                    let (mut base, _, tax) = dummy_type
                        .map(|t| compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, t))
                        .unwrap_or((0.50, 0.50, 0.0));
                    if station_id >= 1_000_000_000_000 {
                        base = compute_structure_rigged_base_yield(&ctx.sde, snap, station_id, None);
                    }
                    let char_skills = snap.skills.get(&char_id);
                    let r = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
                    let re = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
                    (base, tax, r, re)
                });
            if item_ids.is_empty() && args.len() < 2 {
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
                        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
                    let net_yield_rate = yield_rate;
                    let mut est_outputs = Vec::new();
                    for mat in &materials {
                        let out_qty = ((mat.quantity as f64) * yield_rate).floor() as i32;
                        if out_qty > 0 {
                            est_outputs.push(PyRep::keyval([
                                ("typeID", PyRep::Integer(mat.material_type_id as i64)),
                                ("quantity", PyRep::Integer(out_qty as i64)),
                            ]));
                        }
                    }
                    quotes.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
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
            if args.len() >= 2 {
                Some(Ok(PyRep::Tuple(vec![
                    PyRep::Real(station_tax_rate),
                    PyRep::Real(station_base_yield),
                    PyRep::List(quotes),
                ])))
            } else {
                Some(Ok(PyRep::List(quotes)))
            }
        }


        _ => None,
    }
}
