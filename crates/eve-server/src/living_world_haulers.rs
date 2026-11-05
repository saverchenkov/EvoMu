//! `living_world_haulers` — Living World NPC hauler convoys, trade routes, warp/gate transitions, and SDE cargo drops (`feat:npc.npc-hauler`).

#![forbid(unsafe_code)]

use crate::living_world::{
    sde_ball_and_combat, ConvoyRouteState, ConvoyStepStatus, LivingWorldDirector,
};
use crate::system_actor::SystemRegistry;
use eve_db::WorldRepository;
use eve_destiny::MovementMode;
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;

/// Calculates an authentic multi-system trade route originating from `solar_system_id`.
pub fn compute_trade_route(
    sde: &eve_sde::SdeRegistry,
    origin_system_id: SolarSystemId,
) -> Vec<SolarSystemId> {
    let mut route = vec![origin_system_id];
    let mut current = origin_system_id;
    let mut visited = std::collections::HashSet::new();
    visited.insert(current);

    // Build a 2-4 system transit along connected stargates toward regional trade hubs
    for _ in 0..3 {
        let next_opt = sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == current && !visited.contains(&g.to_solar_system_id))
            .min_by_key(|g| g.stargate_id)
            .map(|g| g.to_solar_system_id);
        if let Some(next) = next_opt {
            visited.insert(next);
            route.push(next);
            current = next;
        } else {
            break;
        }
    }
    if route.len() == 1 {
        if let Some(next) = sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == origin_system_id)
            .map(|g| g.to_solar_system_id)
            .next()
        {
            route.push(next);
        }
    }
    route
}

/// Generates authentic SDE mineral / commodity cargo drops based on cargo hold capacity.
pub fn generate_hauler_sde_cargo_loot(
    sde: &eve_sde::SdeRegistry,
    hauler_type_id: TypeId,
) -> Vec<(TypeId, i32)> {
    let capacity = sde
        .types
        .get(&hauler_type_id)
        .map(|t| t.capacity)
        .unwrap_or(5_000.0)
        .max(1_000.0);

    // SDE Mineral & Commodity candidates:
    // Tritanium (34, vol 0.01), Pyerite (35, vol 0.01), Mexallon (36, vol 0.01), Isogen (37, vol 0.01)
    // Construction Blocks (3828, vol 5.0), Coolant (9832, vol 0.38), Consumer Goods (3699, vol 0.2)
    let tritanium_qty = ((capacity * 0.40) / 0.01) as i32;
    let pyerite_qty = ((capacity * 0.25) / 0.01) as i32;
    let mexallon_qty = ((capacity * 0.15) / 0.01) as i32;
    let isogen_qty = ((capacity * 0.08) / 0.01) as i32;
    let commodity_qty = ((capacity * 0.12) / 5.0).max(10.0) as i32;

    vec![
        (34, tritanium_qty.max(10_000)),
        (35, pyerite_qty.max(2_500)),
        (36, mexallon_qty.max(500)),
        (37, isogen_qty.max(100)),
        (3828, commodity_qty.max(20)),
    ]
}

/// Resolves authentic SDE wreck type for a hauler hull.
pub fn hauler_wreck_type_id(sde: &eve_sde::SdeRegistry, ship_type_id: TypeId) -> TypeId {
    sde.wrecks
        .get(&ship_type_id)
        .copied()
        .unwrap_or(26557) // Industrial Wreck
}

/// Advances a living-world hauler convoy along its multi-system stargate route (`feat:npc.npc-hauler`).
pub async fn step_hauler_convoy_internal(
    director: &LivingWorldDirector,
    convoy_id: i64,
    systems: &SystemRegistry,
    db: &WorldRepository,
    sde: &eve_sde::SdeRegistry,
) -> Result<ConvoyStepStatus, String> {
    enum StepAction {
        Destroyed,
        AlreadyDocked,
        Traverse {
            system_id: SolarSystemId,
            target_pos: DVec3,
        },
        Jump {
            prev_sys: SolarSystemId,
            next_sys: SolarSystemId,
            lod_level: u8,
            ship_type_id: u32,
        },
        Dock {
            destination_station_id: i64,
            destination_system_id: SolarSystemId,
            lod_level: u8,
        },
    }

    let action = {
        let mut convoys = director.convoys.write().unwrap_or_else(|e| e.into_inner());
        let c = convoys
            .get_mut(&convoy_id)
            .ok_or_else(|| format!("Convoy {convoy_id} not found"))?;
        if c.destroyed {
            StepAction::Destroyed
        } else {
            let mut routes = director.routes.write().unwrap_or_else(|e| e.into_inner());
            let r = routes.entry(convoy_id).or_insert_with(|| ConvoyRouteState {
                route: vec![c.origin_system_id, c.destination_system_id],
                route_index: 0,
                docked: false,
                last_hop_unix: 0,
            });

            if r.docked {
                StepAction::AlreadyDocked
            } else {
                let now_unix = db.clock().now_unix_secs();
                let prev_sys = if r.route_index > 0 {
                    Some(r.route[r.route_index - 1])
                } else {
                    None
                };
                let next_sys = if r.route_index + 1 < r.route.len() {
                    Some(r.route[r.route_index + 1])
                } else {
                    None
                };
                let transit_duration_secs = LivingWorldDirector::compute_hauler_transit_duration_secs(
                    sde,
                    c.ship_type_id,
                    c.current_system_id,
                    prev_sys,
                    next_sys,
                    c.destination_station_id,
                );

                // Determine target celestial position for warp navigation:
                let target_pos = next_sys
                    .and_then(|nxt| {
                        sde.stargates()
                            .values()
                            .find(|g| g.from_solar_system_id == c.current_system_id && g.to_solar_system_id == nxt)
                            .map(|g| g.position)
                    })
                    .or_else(|| {
                        sde.solar_systems
                            .get(&c.current_system_id)
                            .and_then(|sys| {
                                sys.stations
                                    .iter()
                                    .find(|st| st.station_id == c.destination_station_id)
                                    .or_else(|| sys.stations.first())
                                    .map(|st| st.position)
                            })
                    })
                    .unwrap_or(DVec3::new(20_000.0, 0.0, 0.0));

                if r.last_hop_unix == 0 {
                    r.last_hop_unix = now_unix;
                    StepAction::Traverse {
                        system_id: c.current_system_id,
                        target_pos,
                    }
                } else if now_unix.saturating_sub(r.last_hop_unix) < transit_duration_secs {
                    StepAction::Traverse {
                        system_id: c.current_system_id,
                        target_pos,
                    }
                } else {
                    r.last_hop_unix = now_unix;
                    let prev_sys = c.current_system_id;
                    if r.route_index + 1 < r.route.len() {
                        r.route_index += 1;
                        let next_sys = r.route[r.route_index];
                        c.current_system_id = next_sys;
                        StepAction::Jump {
                            prev_sys,
                            next_sys,
                            lod_level: c.lod_level,
                            ship_type_id: c.ship_type_id,
                        }
                    } else {
                        r.docked = true;
                        let lod = c.lod_level;
                        if lod == 2 {
                            c.lod_level = 1;
                        }
                        StepAction::Dock {
                            destination_station_id: c.destination_station_id,
                            destination_system_id: c.current_system_id,
                            lod_level: lod,
                        }
                    }
                }
            }
        }
    };

    match action {
        StepAction::Destroyed => Ok(ConvoyStepStatus::Destroyed),
        StepAction::AlreadyDocked => Ok(ConvoyStepStatus::AlreadyDocked),
        StepAction::Traverse { system_id, target_pos } => {
            let handle = systems.get_or_spawn(system_id);
            if let Ok(Some((_, ball))) = handle.get_ball_snapshot(convoy_id).await {
                // If hauler is far from target gate/station, set warp movement mode toward destination
                let dist = ball.position.distance(target_pos);
                if dist > 5_000.0 && !matches!(ball.mode, MovementMode::Warp { .. }) {
                    let warp_mode = MovementMode::new_warp(ball.position, target_pos, 3.0);
                    let _ = handle.set_movement_mode(convoy_id, warp_mode).await;
                }
            }
            Ok(ConvoyStepStatus::TraversingSystem { system_id })
        }
        StepAction::Jump {
            prev_sys,
            next_sys,
            lod_level,
            ship_type_id,
        } => {
            if lod_level == 2 {
                let old_handle = systems.get_or_spawn(prev_sys);
                let _ = old_handle.remove_ball(convoy_id).await;

                let new_handle = systems.get_or_spawn(next_sys);
                let gate_pos = sde
                    .stargates()
                    .values()
                    .filter(|g| g.from_solar_system_id == next_sys && g.to_solar_system_id == prev_sys)
                    .min_by_key(|g| g.stargate_id)
                    .map(|g| g.position)
                    .or_else(|| {
                        sde.stargates()
                            .values()
                            .filter(|g| g.from_solar_system_id == next_sys)
                            .min_by_key(|g| g.stargate_id)
                            .map(|g| g.position)
                    })
                    .unwrap_or(DVec3::new(10_000.0, 0.0, 0.0));

                let (ball, combat) = sde_ball_and_combat(
                    sde,
                    convoy_id,
                    ship_type_id,
                    gate_pos + DVec3::new(1_200.0, 0.0, 0.0),
                );
                let _ = new_handle.spawn_ball_with_combat(ball, combat).await;
            }

            Ok(ConvoyStepStatus::JumpedGate {
                from_system: prev_sys,
                to_system: next_sys,
            })
        }
        StepAction::Dock {
            destination_station_id,
            destination_system_id,
            lod_level,
        } => {
            if lod_level == 2 {
                let handle = systems.get_or_spawn(destination_system_id);
                let _ = handle.remove_ball(convoy_id).await;
            }
            Ok(ConvoyStepStatus::DockedAtDestination {
                station_id: destination_station_id,
                system_id: destination_system_id,
            })
        }
    }
}

#[cfg(test)]
#[path = "living_world_haulers_tests.rs"]
mod living_world_haulers_tests;

#[cfg(test)] #[path = "living_world_haulers_extra_tests.rs"]
mod living_world_haulers_extra_tests;

