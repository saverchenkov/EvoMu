//! `space_entity_mining` — `entity.CmdMineRepeatedly` / `beyonce.CmdMineRepeatedly` / `dogmaIM.Mine` asteroid mining
//! for mining modules and launched mining drones (`MN-09`, `feat:mining.*`).
//!
//! Arguments are positional only: `(droneIDs, targetID)`, `(droneID, targetID)` or `(targetID[, moduleID])`.

#![forbid(unsafe_code)]

use crate::formulas::compute_reprocessing_yield_rate;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::mining_advanced::mining_compression::structure_has_online_reprocessing_service;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_sde::{InvType, SdeRegistry, TypeId, TypeMaterialEntry};
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

    let mut ast_state_opt = handle.get_asteroid_state(asteroid_id).await.ok().flatten();
    if ast_state_opt.is_none() {
        if let Some(db_ast) = ctx.db.get_asteroid(asteroid_id) {
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
    if let Some((_, ast_ball)) = handle.get_ball_snapshot(asteroid_id).await.ok().flatten() {
        let surface_dist = (ship_ball.position.distance(ast_ball.position) - ship_ball.radius - ast_ball.radius).max(0.0);
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
            let d_waste = drone_t
                .and_then(|t| t.default_attributes.get(&3154).copied())
                .unwrap_or(0.0);
            max_waste_prob = max_waste_prob.max(d_waste);
            let is_ice_drone = drone_t
                .and_then(|t| t.default_attributes.get(&182).copied())
                == Some(43702.0);
            let drone_mult = if is_ice_drone {
                let _ = (ice_op_lvl, ice_spec_lvl);
                ind_core_ice_rate_mult
            } else {
                let spec_factor = if matches!(d_item.type_id, 10250 | 41030 | 90621) {
                    1.0 + 0.02 * (drone_spec_lvl as f64)
                } else {
                    1.0
                };
                (1.0 + 0.05 * (drone_op_lvl as f64))
                    * (1.0 + 0.10 * (drone_int_lvl as f64))
                    * spec_factor
                    * rig_mult
                    * rorq_ore_bonus_mult
                    * ind_core_ore_mult
            };
            let stack_qty = (d_item.quantity.max(1)) as f64;
            total_drone_yield += d_yield * drone_mult * stack_qty;
        }
        (total_drone_yield, max_waste_prob)
    } else if let (Some(fit), Some(mod_id)) = (&fit, primary_id) {
        let y = fit.get_module_effective_attribute(mod_id, 77, &ctx.sde);
        let w = fit.get_module_effective_attribute(mod_id, 3154, &ctx.sde);
        (y, w)
    } else if let Some(fit) = &fit {
        let m = fit.modules.values().find(|m| {
            ctx.sde.types.get(&m.type_id).map(|t| matches!(t.group_id, 54 | 464 | 483 | 737)).unwrap_or(false)
        });
        if let Some(m) = m {
            let y = fit.get_module_effective_attribute(m.item_id, 77, &ctx.sde);
            let w = fit.get_module_effective_attribute(m.item_id, 3154, &ctx.sde);
            (y, w)
        } else {
            (40.0, 0.0)
        }
    } else {
        (40.0, 0.0)
    };

    let unit_vol = ast_state.unit_volume.max(0.001);
    let requested_units = ((base_yield_m3 / unit_vol).floor() as u64).max(1);

    let extraction = match handle.mine_asteroid(asteroid_id, requested_units, waste_prob).await {
        Ok(e) => e,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "MiningFailed".into(),
                message: e,
            });
        }
    };

    let ast_pos_opt = ctx.db.with_mut_snapshot(|snap| {
        let mut pos_opt = None;
        if let Some(ast) = snap.asteroids.get_mut(&asteroid_id) {
            ast.quantity = extraction.remaining_units as i64;
            pos_opt = Some(DVec3::new(ast.position.0, ast.position.1, ast.position.2));
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

    // Determine target hold: SpecializedOreHold (134), SpecializedGasHold (135), SpecializedIceHold (181), or Cargo (5)
    let ore_type_rec = ctx.sde.types.get(&extraction.ore_type_id);
    let ore_group_id = ore_type_rec.map(|t| t.group_id).unwrap_or(0);
    let ore_category_id = ore_type_rec.map(|t| t.category_id).unwrap_or(0);

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

    if extraction.mined_units > 0 {
        let _ = ctx.db.add_or_stack_item(
            char_id,
            ship_id,
            target_flag,
            extraction.ore_type_id,
            extraction.mined_units as i64,
        );

        let now_unix = ctx.now_unix_secs();
        let _ = ctx.db.record_mining_ledger_entry(
            char_id,
            sys_id,
            extraction.ore_type_id,
            extraction.mined_units as i64,
            now_unix,
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

/// Computes the Upwell structure base reprocessing yield (`0.51` for Athanor `35835` via SDE
/// `35899` attribute `717` = `0.50` and `35835` attribute `2722` `strRefiningYieldBonus` = `+2.0%`;
/// `0.52` for Tatara `35836`; `0.50` default).
pub fn compute_structure_reprocessing_base_yield(sde: &SdeRegistry, st_type_id: TypeId) -> f64 {
    let service_base = sde
        .types
        .get(&35899)
        .and_then(|t| t.default_attributes.get(&717).copied())
        .unwrap_or(0.50);
    if st_type_id == 35836 {
        0.52
    } else if st_type_id == 35835 {
        let bonus_pct = sde
            .types
            .get(&st_type_id)
            .and_then(|t| t.default_attributes.get(&2722).copied())
            .unwrap_or(2.0);
        ((service_base * (1.0 + bonus_pct / 100.0)) * 10_000.0).round() / 10_000.0
    } else {
        service_base
    }
}

pub fn compute_structure_rigged_base_yield(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    inv_type: Option<&InvType>,
) -> f64 {
    let Some(st) = snap.structures.get(&structure_id) else {
        return 0.50;
    };
    let unrigged = compute_structure_reprocessing_base_yield(sde, st.type_id);
    let hull_mult = unrigged / 0.50;
    let st_rig_size = sde
        .types
        .get(&st.type_id)
        .and_then(|t| t.default_attributes.get(&1547).copied())
        .unwrap_or(if st.type_id == 35836 { 3.0 } else { 2.0 });
    let sec_attr_id: u16 = if st.solar_system_id >= 31_000_000 {
        2357
    } else {
        let sec = sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|s| crate::services::mining_sites::displayed_security(s.security_status))
            .unwrap_or(1.0);
        if sec <= 0.0 {
            2357
        } else if sec < 0.5 {
            2356
        } else {
            2355
        }
    };
    let mut best_rig_yield: Option<f64> = None;
    for it in snap.items.values() {
        if it.location_id != structure_id
            || !matches!(
                it.flag,
                InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
            )
        {
            continue;
        }
        let Some(rig_t) = sde.types.get(&it.type_id) else {
            continue;
        };
        if !matches!(rig_t.group_id, 1941..=1945) {
            continue;
        }
        let rig_size = rig_t.default_attributes.get(&1547).copied().unwrap_or(0.0);
        if (rig_size - st_rig_size).abs() > 0.1 {
            continue;
        }
        let matches_item = match inv_type {
            None => true,
            Some(ty) => {
                let is_ice = ty.group_id == 465 || ty.group_id == 4033;
                let is_moon = matches!(ty.group_id, 1884 | 1920..=1923);
                let is_ast =
                    (ty.category_id == 25 || ty.group_id == 4932) && !is_ice && !is_moon;
                match rig_t.group_id {
                    1941 => is_ast,
                    1942 => is_ice,
                    1943 => is_moon,
                    1944 | 1945 => is_ast || is_ice || is_moon,
                    _ => false,
                }
            }
        };
        if !matches_item {
            continue;
        }
        let Some(&rig_base) = rig_t.default_attributes.get(&717) else {
            continue;
        };
        let sec_mult = rig_t
            .default_attributes
            .get(&sec_attr_id)
            .copied()
            .unwrap_or(1.0);
        let candidate = rig_base * sec_mult * hull_mult;
        if best_rig_yield.is_none_or(|b| candidate > b) {
            best_rig_yield = Some(candidate);
        }
    }
    best_rig_yield.unwrap_or(unrigged)
}

/// Resolves reprocessing output materials for `inv_type` (`sde.type_materials`), including
/// Equinox `Prismaticite` (`90041`) / `Compressed Prismaticite` (`90307`) (`group_id == 4915`,
/// randomly collapsing into 1 of the 8 standard minerals weighted by equal ISK value from `sde.types`)
/// and `Unrefined <Mineral>` (`90283..=90298`, `group_id == 4932`, deterministically yielding the
/// target mineral from `sde.types`).
/// DERIVED: CCP stores Prismaticite & Unrefined Mineral outputs in `randomizedTypeMaterials` rather than
/// `sde_type_materials`; equal-ISK batch quantities (`524,288 ISK / base_price` per 100 units) are
/// derived from each SDE mineral's `base_price` (`34..=40`, `11399`).
fn resolve_reprocessable_materials(
    sde: &SdeRegistry,
    inv_type: &InvType,
    item_id: i64,
    next_entity_id: i64,
    rng_bits: u64,
    deterministic_quote: bool,
) -> Option<Vec<TypeMaterialEntry>> {
    if let Some(mats) = sde.type_materials.get(&inv_type.type_id).filter(|m| !m.is_empty()) {
        return Some(mats.clone());
    }
    let mut standard_minerals: Vec<&InvType> = sde
        .types
        .values()
        .filter(|t| t.group_id == 18 && t.published && t.type_id <= 11399 && t.base_price > 0.0)
        .collect();
    standard_minerals.sort_by_key(|t| t.type_id);
    if standard_minerals.is_empty() {
        return None;
    }
    if inv_type.group_id == 4932 {
        let mineral_name = inv_type.name.strip_prefix("Unrefined ")?;
        let min_t = standard_minerals.into_iter().find(|t| t.name == mineral_name)?;
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![TypeMaterialEntry {
            material_type_id: min_t.type_id,
            quantity: qty,
        }]);
    }
    if inv_type.group_id == 4915 || matches!(inv_type.type_id, 90041 | 90307) {
        let idx = if deterministic_quote {
            (item_id.unsigned_abs() as usize) % standard_minerals.len()
        } else {
            let seed = (item_id as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(next_entity_id as u64)
                ^ rng_bits;
            (seed as usize) % standard_minerals.len()
        };
        let min_t = standard_minerals[idx];
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![TypeMaterialEntry {
            material_type_id: min_t.type_id,
            quantity: qty,
        }]);
    }
    None
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
        snap.structures
            .get(&station_id)
            .and_then(|st| snap.corporations.get(&st.owner_corp_id))
            .map(|c| c.tax_rate.clamp(0.0, 1.0))
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
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Reprocess requires an explicit integer itemID".into(),
                }));
            };

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
                }

                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} does not exist"),
                    });
                };
                if item.owner_id != char_id || item.location_id != station_id || item.quantity <= 0
                {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .or(session.ship_id);
                if active_ship_id == Some(item_id) {
                    return Err(BridgeRpcError {
                        code: "CannotReprocessActiveShip".into(),
                        message: format!(
                            "Item {item_id} is your currently active ship and cannot be reprocessed"
                        ),
                    });
                }
                let Some(inv_type) = ctx.sde.types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotReprocessable".into(),
                        message: format!("Unknown item type {}", item.type_id),
                    });
                };
                let rng_bits = (ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5) * (u64::MAX as f64)) as u64;
                let Some(materials) = resolve_reprocessable_materials(
                    &ctx.sde,
                    inv_type,
                    item_id,
                    snap.next_entity_id,
                    rng_bits,
                    false,
                ) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotReprocessable".into(),
                        message: format!(
                            "Item type {} ({}) has no SDE typeMaterials and cannot be reprocessed",
                            item.type_id, inv_type.name
                        ),
                    });
                };
                if item.singleton || snap.items.values().any(|child| child.location_id == item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemMustBeRepackaged".into(),
                        message: format!(
                            "Item {item_id} must be emptied and repackaged before reprocessing"
                        ),
                    });
                }
                if item.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked and cannot be reprocessed"),
                    });
                }

                let portion_size = inv_type.portion_size.max(1) as i32;
                if item.quantity < portion_size {
                    return Err(BridgeRpcError {
                        code: "InsufficientBatchPortion".into(),
                        message: format!(
                            "Need at least {portion_size} units to reprocess type {} (have {})",
                            item.type_id, item.quantity
                        ),
                    });
                }

                let (station_base_yield, yield_rate, station_tax_rate) =
                    compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
                let net_yield_rate = if station_id >= 1_000_000_000_000 {
                    yield_rate
                } else {
                    yield_rate * (1.0 - station_tax_rate)
                };

                let portions = item.quantity / portion_size;
                let remainder = item.quantity % portion_size;
                if remainder > 0 {
                    if let Some(im) = snap.items.get_mut(&item_id) {
                        im.quantity = remainder;
                        im.version += 1;
                    }
                } else {
                    snap.items.remove(&item_id);
                }

                let mut outputs: Vec<(i64, TypeId, i32)> = Vec::with_capacity(materials.len());
                let mut gross_value_isk = 0.0_f64;
                for mat in &materials {
                    let unit_val = ctx
                        .sde
                        .types
                        .get(&mat.material_type_id)
                        .map(|t| t.base_price.max(4.0))
                        .unwrap_or(4.0);
                    gross_value_isk +=
                        (portions as f64) * (mat.quantity as f64) * yield_rate * unit_val;
                    let out_qty =
                        ((portions as f64) * (mat.quantity as f64) * net_yield_rate).floor() as i32;
                    if out_qty <= 0 {
                        continue;
                    }
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.items.insert(
                        mid,
                        InventoryItemRecord {
                            item_id: mid,
                            type_id: mat.material_type_id,
                            owner_id: char_id,
                            location_id: station_id,
                            flag: InventoryFlag::Hangar,
                            quantity: out_qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    outputs.push((mid, mat.material_type_id, out_qty));
                }

                let tax_isk = if station_tax_rate > 0.0 {
                    (gross_value_isk * station_tax_rate).max(1.0)
                } else {
                    0.0
                };
                if tax_isk > 0.0 {
                    let tax_cents = IskCents::from_f64_isk(tax_isk);
                    let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let deduct = tax_cents.min(ch.balance);
                        ch.balance = ch.balance.checked_sub(deduct).unwrap_or(IskCents(0));
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

                Ok((outputs, station_base_yield, yield_rate, station_tax_rate, tax_isk, remainder))
            });

            match res {
                Ok((outputs, station_base_yield, yield_rate, station_tax_rate, tax_isk, remainder)) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnReprocessingMaterialsChanged",
                        "character",
                        vec![PyRep::Integer(char_id), PyRep::Integer(station_id)],
                    );
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
                    Some(Ok(PyRep::keyval([
                        ("stationBaseYield", PyRep::Real(station_base_yield)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("reprocessingTaxISK", PyRep::Real(tax_isk)),
                        ("remainderQuantity", PyRep::Integer(remainder as i64)),
                        ("tritaniumItemID", PyRep::Integer(trit_id)),
                        ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
                        ("outputs", PyRep::List(output_entries)),
                    ])))
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
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) => list.iter().filter_map(|a| a.as_i64()).collect(),
                Some(first) => first.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            if item_ids.is_empty() {
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
                return Some(Ok(PyRep::keyval([
                    ("stationBaseYield", PyRep::Real(station_base_yield)),
                    ("stationTaxRate", PyRep::Real(station_tax_rate)),
                    ("reprocessingSkillLevel", PyRep::Integer(reproc_lvl as i64)),
                    ("reprocessingEfficiencyLevel", PyRep::Integer(reproc_eff_lvl as i64)),
                ])));
            }
            let res = ctx.db.with_snapshot(|snap| {
                let mut quotes = Vec::new();
                for item_id in item_ids {
                    let Some(item) = snap.items.get(&item_id) else { continue };
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else { continue };
                    let Some(materials) = resolve_reprocessable_materials(
                        &ctx.sde,
                        inv_type,
                        item_id,
                        snap.next_entity_id,
                        0,
                        true,
                    ) else {
                        continue;
                    };
                    let (station_base_yield, yield_rate, station_tax_rate) =
                        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
                    let net_yield_rate = if station_id >= 1_000_000_000_000 {
                        yield_rate
                    } else {
                        yield_rate * (1.0 - station_tax_rate)
                    };
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let mut est_outputs = Vec::new();
                    for mat in &materials {
                        let out_qty = ((portions as f64) * (mat.quantity as f64) * net_yield_rate).floor() as i32;
                        if out_qty > 0 {
                            est_outputs.push(PyRep::keyval([
                                ("typeID", PyRep::Integer(mat.material_type_id as i64)),
                                ("quantity", PyRep::Integer(out_qty as i64)),
                            ]));
                        }
                    }
                    quotes.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("stationBaseYield", PyRep::Real(station_base_yield)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("netYieldRate", PyRep::Real(net_yield_rate)),
                        ("portionSize", PyRep::Integer(portion_size as i64)),
                        ("outputs", PyRep::List(est_outputs)),
                    ]));
                }
                quotes
            });
            Some(Ok(PyRep::List(res)))
        }


        _ => None,
    }
}
