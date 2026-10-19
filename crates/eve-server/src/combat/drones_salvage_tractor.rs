//! `drones_salvage_tractor` — Salvage mechanics, Tractor Beam, and Drone Bandwidth & Control (`C-7`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// Outcome of attempting to salvage a wreck entity.
#[derive(Debug, Clone, PartialEq)]
pub enum SalvageOutcome {
    Success {
        yielded_salvage_type_ids: Vec<u32>,
        yielded_quantities: Vec<i32>,
    },
    Failed {
        reason: &'static str,
    },
}

/// Executes a salvager cycle against a target wreck:
/// - Evaluates base chance + salvager access difficulty bonus.
/// - On success: removes wreck ball and item, grants salvage items (from SDE salvage materials).
/// - On failure: wreck remains intact for subsequent attempts.
pub async fn attempt_salvage_wreck(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_item_id: i64,
    salvager_access_bonus: f64,
    roll_u: f64,
) -> Result<SalvageOutcome, String> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Verify distance between ship and wreck is within salvager range (5,000 m)
    let ship_ball = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| format!("Query ship failed: {e}"))?
        .map(|(_, b)| b)
        .ok_or_else(|| format!("Ship {ship_id} not found in space"))?;

    let wreck_ball = sys_handle
        .get_ball_snapshot(wreck_item_id)
        .await
        .map_err(|e| format!("Query wreck failed: {e}"))?
        .map(|(_, b)| b)
        .ok_or_else(|| format!("Wreck {wreck_item_id} not found in space"))?;

    let dist = ship_ball.position.distance(wreck_ball.position);
    if dist > 5_000.0 {
        return Err(format!(
            "Wreck is {:.1} m away; salvager maximum range is 5,000 m",
            dist
        ));
    }

    // Base salvage chance: 20% + salvager access bonus (e.g. +30% for Salvager I = 50%)
    let chance = (0.20 + salvager_access_bonus).clamp(0.05, 0.95);

    if roll_u < chance {
        // Success: select canonical SDE salvage materials
        // 25588: Alloyed Tritanium Bar, 25595: Armor Plates, 25597: Burned Logic Circuit
        let salvage_materials = vec![(25588, 4), (25595, 2), (25597, 3)];

        ctx.db.try_with_mut_snapshot(|snap| {
            // Remove wreck item from world repository
            snap.items.remove(&wreck_item_id);

            // Insert salvage items into character's ship cargo
            for &(tid, qty) in &salvage_materials {
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: tid,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::Cargo,
                        quantity: qty,
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
            Ok::<_, String>(())
        })?;

        // Remove physical wreck ball from Ballpark
        let _ = sys_handle.remove_ball(wreck_item_id).await;

        let types = salvage_materials.iter().map(|&(t, _)| t).collect();
        let quantities = salvage_materials.iter().map(|&(_, q)| q).collect();

        Ok(SalvageOutcome::Success {
            yielded_salvage_type_ids: types,
            yielded_quantities: quantities,
        })
    } else {
        Ok(SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials",
        })
    }
}

/// Simulates a tractor beam pull on a wreck towards the player ship:
/// - Maximum tractor range: 20,000 m.
/// - Tractor pull speed: 500 m/s.
/// - Stop distance: 1,200 m (so ship can easily loot/salvage).
/// Returns `(new_wreck_position, has_arrived)`.
pub fn step_tractor_beam(
    wreck_pos: DVec3,
    ship_pos: DVec3,
    tractor_velocity: f64,
    dt_sec: f64,
) -> (DVec3, bool) {
    let delta = ship_pos - wreck_pos;
    let dist = delta.length();
    let stop_distance = 1_200.0;

    if dist <= stop_distance {
        return (wreck_pos, true);
    }

    let dir = delta.normalize_or_zero();
    let travel = (tractor_velocity * dt_sec).min(dist - stop_distance);
    let new_pos = wreck_pos + dir * travel;
    let arrived = (ship_pos - new_pos).length() <= stop_distance + 1.0;
    (new_pos, arrived)
}

/// Validates combat drone bandwidth requirements against a ship's Dogma bandwidth limit.
/// Max 5 active combat drones, total bandwidth <= ship `droneBandwidth` attribute.
pub fn validate_drone_bandwidth(
    ctx: &ServerContext,
    ship_id: i64,
    requested_drone_type_ids: &[u32],
    currently_active_drone_type_ids: &[u32],
) -> Result<f64, String> {
    let total_count = requested_drone_type_ids.len() + currently_active_drone_type_ids.len();
    if total_count > 5 {
        return Err(format!(
            "Cannot control more than 5 active drones simultaneously (requested {total_count})"
        ));
    }

    let ship_type_id = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
        .ok_or_else(|| format!("Ship {ship_id} not found"))?;

    let ship_bandwidth = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_BANDWIDTH).copied())
        .unwrap_or(25.0);

    let mut required_bandwidth = 0.0;
    for &dtid in currently_active_drone_type_ids.iter().chain(requested_drone_type_ids) {
        let drone_bw = ctx
            .sde
            .types
            .get(&dtid)
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_BANDWIDTH_USED).copied())
            .unwrap_or(5.0); // Light combat drones default to 5 Mbit/s
        required_bandwidth += drone_bw;
    }

    if required_bandwidth > ship_bandwidth + 1e-4 {
        return Err(format!(
            "Insufficient drone bandwidth: required {:.1} Mbit/s, ship maximum is {:.1} Mbit/s",
            required_bandwidth, ship_bandwidth
        ));
    }

    Ok(ship_bandwidth - required_bandwidth)
}

/// State of an active combat drone in space.
#[derive(Debug, Clone)]
pub struct ActiveDrone {
    pub ball_id: i64,
    pub drone_item_id: i64,
    pub type_id: u32,
    pub owner_char_id: i64,
    pub parent_ship_id: i64,
    pub target_id: Option<i64>,
    pub dps: f64,
}

/// Drone Controller managing launch, attack orders, and recall into DroneBay.
#[derive(Debug, Default)]
pub struct DroneController {
    pub active_drones: HashMap<i64, ActiveDrone>,
}

impl DroneController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Launches specified drone items from ship's `DroneBay` into space balls.
    pub async fn launch_drones(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        char_id: i64,
        ship_id: i64,
        drone_item_ids: &[i64],
    ) -> Result<Vec<i64>, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let ship_pos = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .map_err(|e| format!("Query ship failed: {e}"))?
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);

        let active_types: Vec<u32> = self.active_drones.values().map(|d| d.type_id).collect();

        let drone_items: Vec<(i64, u32)> = ctx.db.with_snapshot(|snap| {
            let mut list = Vec::new();
            for &did in drone_item_ids {
                let item = snap
                    .items
                    .get(&did)
                    .ok_or_else(|| format!("Drone item {did} not found in inventory"))?;
                if item.owner_id != char_id || item.location_id != ship_id {
                    return Err(format!("Drone item {did} is not in ship's drone bay"));
                }
                list.push((did, item.type_id));
            }
            Ok::<_, String>(list)
        })?;

        let req_types: Vec<u32> = drone_items.iter().map(|&(_, tid)| tid).collect();
        validate_drone_bandwidth(ctx, ship_id, &req_types, &active_types)?;

        let mut spawned_ball_ids = Vec::new();

        for (idx, (item_id, type_id)) in drone_items.into_iter().enumerate() {
            let ball_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let bid = snap.next_entity_id;
                // Move item location to space
                if let Some(i) = snap.items.get_mut(&item_id) {
                    i.location_id = sys_id as i64;
                    i.flag = InventoryFlag::None;
                    i.version += 1;
                }
                bid
            });

            let offset = DVec3::new(
                ((idx as f64) * 100.0).cos() * 50.0,
                0.0,
                ((idx as f64) * 100.0).sin() * 50.0,
            );
            let d_pos = ship_pos + offset;

            let ball = Ball {
                id: ball_id,
                type_id,
                position: d_pos,
                velocity: DVec3::ZERO,
                max_velocity: 800.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            };

            let _ = sys_handle
                .spawn_ball_with_combat(ball, ctx.build_type_combat_state(type_id))
                .await;

            let active = ActiveDrone {
                ball_id,
                drone_item_id: item_id,
                type_id,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                target_id: None,
                dps: 18.0,
            };

            self.active_drones.insert(ball_id, active);
            spawned_ball_ids.push(ball_id);
        }

        Ok(spawned_ball_ids)
    }

    /// Orders active drones to engage a hostile target.
    pub async fn engage_target(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        target_ball_id: i64,
    ) -> Result<usize, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut count = 0;

        for drone in self.active_drones.values_mut() {
            drone.target_id = Some(target_ball_id);
            let _ = sys_handle
                .set_movement_mode(
                    drone.ball_id,
                    MovementMode::Orbit {
                        target_id: target_ball_id,
                        range: 1_000.0,
                    },
                )
                .await;

            // Apply drone damage
            let quarter = drone.dps * 0.25;
            let dmg = DamageVector {
                em: quarter,
                thermal: quarter,
                kinetic: quarter,
                explosive: quarter,
            };
            let _ = sys_handle.apply_damage(target_ball_id, dmg).await;
            count += 1;
        }

        Ok(count)
    }

    /// Recalls active drones into the ship's `DroneBay`.
    pub async fn recall_drones(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        ship_id: i64,
    ) -> Result<Vec<i64>, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut recalled = Vec::new();

        let drone_entries: Vec<ActiveDrone> = self
            .active_drones
            .values()
            .filter(|d| d.parent_ship_id == ship_id)
            .cloned()
            .collect();

        for drone in drone_entries {
            self.active_drones.remove(&drone.ball_id);
            let _ = sys_handle.remove_ball(drone.ball_id).await;

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get_mut(&drone.drone_item_id) {
                    item.location_id = ship_id;
                    item.flag = InventoryFlag::DroneBay;
                    item.version += 1;
                }
            });

            recalled.push(drone.drone_item_id);
        }

        Ok(recalled)
    }

    /// Orders active combat drones to return and orbit their parent ship (`feat:drones.return-orbit`).
    pub async fn return_and_orbit(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        ship_id: i64,
    ) -> Result<usize, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut count = 0;

        for drone in self.active_drones.values_mut() {
            if drone.parent_ship_id == ship_id {
                drone.target_id = None;
                let _ = sys_handle
                    .set_movement_mode(
                        drone.ball_id,
                        MovementMode::Orbit {
                            target_id: ship_id,
                            range: 1_000.0,
                        },
                    )
                    .await;
                count += 1;
            }
        }

        Ok(count)
    }
}
