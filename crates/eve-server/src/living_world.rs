//! `living_world` — 3-Tier LOD Living World NPC Ecology & Dynamic Supply Chain Director (`§3.10`, `E2E-17`).
//!
//! Simulates:
//! - **LOD 0 (Macro System Economy):** Regional mineral supply/demand pressure & arbitrage triggers.
//! - **LOD 1 (Route & System Presence):** NPC Hauler Convoys, Belt Mining Operations, Pirate Roamers,
//!   and Empire Navy Patrols traversing stargate routes (`Jita <-> Perimeter <-> Tama`).
//! - **LOD 2 (Tactical Grid Materialization):** Real `Ball` entities spawned into solar-system
//!   `Ballpark`s that can be scanned, engaged, or interdicted (`DestroyConvoy`), dropping physical
//!   cargo loot and spiking destination trade-hub ask prices when destroyed.

#![forbid(unsafe_code)]

use crate::system_actor::SystemRegistry;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NpcArchetype {
    HaulerConvoy,
    MiningOp,
    MarketArbitrageur,
    PirateRoamer,
    EmpireNavyPatrol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivingWorldConvoy {
    pub convoy_id: i64,
    pub archetype: NpcArchetype,
    pub ship_type_id: TypeId,
    pub origin_system_id: SolarSystemId,
    pub current_system_id: SolarSystemId,
    pub destination_system_id: SolarSystemId,
    pub destination_station_id: i64,
    pub cargo_type_id: TypeId,
    pub cargo_quantity: i32,
    pub lod_level: u8, // 0, 1, or 2 (spawned on grid)
    pub destroyed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvoyRouteState {
    pub route: Vec<SolarSystemId>,
    pub route_index: usize,
    pub docked: bool,
    pub last_hop_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConvoyStepStatus {
    TraversingSystem { system_id: SolarSystemId },
    JumpedGate { from_system: SolarSystemId, to_system: SolarSystemId },
    DockedAtDestination { station_id: i64, system_id: SolarSystemId },
    AlreadyDocked,
    Destroyed,
}

pub struct LivingWorldDirector {
    convoys: RwLock<HashMap<i64, LivingWorldConvoy>>,
    routes: RwLock<HashMap<i64, ConvoyRouteState>>,
    hauler_cooldowns: RwLock<HashMap<SolarSystemId, std::time::Instant>>,
    mining_last_step: RwLock<HashMap<i64, u64>>,
}

impl Default for LivingWorldDirector {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a `Ball` and `CombatState` from SDE attributes for `type_id`.
pub fn sde_ball_and_combat(
    sde: &eve_sde::SdeRegistry,
    id: i64,
    type_id: TypeId,
    position: DVec3,
) -> (Ball, eve_destiny::CombatState) {
    let (mass, radius, max_vel, inertia, sh, ar, st) = sde
        .types
        .get(&type_id)
        .map(|t| {
            let a = &t.default_attributes;
            let mv = a.get(&37).copied().unwrap_or(250.0).max(1.0);
            let ag = a.get(&70).copied().unwrap_or(3.5).max(0.05);
            let sh = a.get(&263).copied().unwrap_or(1_200.0).max(1.0);
            let ar = a.get(&265).copied().unwrap_or(1_200.0).max(1.0);
            let st = a.get(&9).copied().unwrap_or(1_500.0).max(1.0);
            (t.mass.max(1_000.0), t.radius.max(20.0), mv, ag, sh, ar, st)
        })
        .unwrap_or((1_200_000.0, 40.0, 250.0, 3.5, 1_500.0, 1_200.0, 1_500.0));
    (
        Ball {
            id,
            type_id,
            position,
            velocity: DVec3::ZERO,
            max_velocity: max_vel,
            inertia,
            mass,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            shield_hp: sh,
            max_shield_hp: sh,
            armor_hp: ar,
            max_armor_hp: ar,
            structure_hp: st,
            max_structure_hp: st,
            ..eve_destiny::CombatState::default()
        },
    )
}

/// Resolves the empire/faction industrial hauler `TypeId` for `solar_system_id` from SDE (`group_id == 28`).
pub fn hauler_type_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> TypeId {
    let faction_id = sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            sys.stations
                .first()
                .and_then(|st| st.owner_faction_id.map(|f| f as u32))
                .or_else(|| sde.constellations.get(&sys.constellation_id).and_then(|c| c.faction_id))
                .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.faction_id))
        })
        .unwrap_or(500_001);
    let target_race: u32 = match faction_id {
        500_001 => 1, // Caldari
        500_002 => 2, // Minmatar
        500_003 => 4, // Amarr
        500_004 => 8, // Gallente
        _ => 1,
    };
    sde.types
        .values()
        .filter(|t| t.group_id == 28 && t.published && t.race_id == Some(target_race))
        .map(|t| t.type_id)
        .min()
        .unwrap_or(648)
}

impl LivingWorldDirector {
    /// Initializes an empty `LivingWorldDirector` with `0` pre-seeded convoys (`S-5`).
    pub fn new() -> Self {
        Self {
            convoys: RwLock::new(HashMap::new()),
            routes: RwLock::new(HashMap::new()),
            hauler_cooldowns: RwLock::new(HashMap::new()),
            mining_last_step: RwLock::new(HashMap::new()),
        }
    }

    /// Explicitly seeds demo convoys `800_000_001..=800_000_003` for tests or `--seed-demo` (`S-5`).
    pub fn seed_default_convoys(&self) {
        let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        // Convoy 800_000_001: Hauler Convoy carrying 50,000 Tritanium from Perimeter (30000144) to Jita 4-4 (60003760)
        convoys.insert(
            800_000_001,
            LivingWorldConvoy {
                convoy_id: 800_000_001,
                archetype: NpcArchetype::HaulerConvoy,
                ship_type_id: 49_793, // Frostline Transporter (SDE NPC hauler)
                origin_system_id: 30_000_144,
                current_system_id: 30_000_142,
                destination_system_id: 30_000_142,
                destination_station_id: 60_003_760,
                cargo_type_id: 34, // Tritanium
                cargo_quantity: 50_000,
                lod_level: 1,
                destroyed: false,
            },
        );
        // Convoy 800_000_002: Belt Mining Op in Perimeter (30000144) mining Veldspar (1230)
        convoys.insert(
            800_000_002,
            LivingWorldConvoy {
                convoy_id: 800_000_002,
                archetype: NpcArchetype::MiningOp,
                ship_type_id: 32880, // Venture
                origin_system_id: 30_000_144,
                current_system_id: 30_000_144,
                destination_system_id: 30_000_144,
                destination_station_id: 60_003_766,
                cargo_type_id: 1230, // Veldspar
                cargo_quantity: 5_000,
                lod_level: 1,
                destroyed: false,
            },
        );
        // Convoy 800_000_003: Pirate Roamer Gang in Tama (30002813)
        convoys.insert(
            800_000_003,
            LivingWorldConvoy {
                convoy_id: 800_000_003,
                archetype: NpcArchetype::PirateRoamer,
                ship_type_id: 16997, // Guristas Pithi Invader
                origin_system_id: 30_002_813,
                current_system_id: 30_002_813,
                destination_system_id: 30_000_144,
                destination_station_id: 60_003_766,
                cargo_type_id: 21924,
                cargo_quantity: 200,
                lod_level: 1,
                destroyed: false,
            },
        );
    }

    pub fn new_seeded() -> Self {
        let dir = Self::new();
        dir.seed_default_convoys();
        dir
    }

    pub fn list_convoys(&self) -> Vec<LivingWorldConvoy> {
        let mut list: Vec<LivingWorldConvoy> = self
            .convoys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        list.sort_by_key(|c| c.convoy_id);
        list
    }

    /// Promotes all active (`lod_level < 2`, non-destroyed) convoys in `solar_system_id` to `LOD 2` (full tactical grid `Ball`s).
    pub async fn materialize_system_lod2(
        &self,
        solar_system_id: SolarSystemId,
        systems: &SystemRegistry,
        sde: &eve_sde::SdeRegistry,
    ) -> usize {
        let to_spawn: Vec<LivingWorldConvoy> = {
            let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for c in guard.values_mut() {
                if c.current_system_id == solar_system_id && !c.destroyed && c.lod_level < 2 {
                    c.lod_level = 2;
                    out.push(c.clone());
                }
            }
            out
        };
        if to_spawn.is_empty() {
            return 0;
        }

        let handle = systems.get_or_spawn(solar_system_id);
        let mut count = 0;
        for c in to_spawn {
            let station_pos = sde
                .solar_systems
                .get(&solar_system_id)
                .and_then(|sys| {
                    sys.stations
                        .iter()
                        .find(|st| st.station_id == c.destination_station_id)
                        .or_else(|| sys.stations.first())
                        .map(|st| st.position)
                })
                .unwrap_or(DVec3::ZERO);
            let grid_origin = if c.archetype == NpcArchetype::MiningOp {
                sde.celestials_in_system(solar_system_id)
                    .into_iter()
                    .find(|cel| cel.group_id == 9)
                    .map(|cel| cel.position)
                    .unwrap_or(station_pos + DVec3::new(1_500.0, 0.0, 0.0))
            } else {
                station_pos + DVec3::new(1_500.0, 0.0, 0.0)
            };
            let (ball, combat) = sde_ball_and_combat(
                sde,
                c.convoy_id,
                c.ship_type_id,
                grid_origin + DVec3::new(1_500.0, 0.0, 0.0),
            );
            let _ = handle.spawn_ball_with_combat(ball, combat).await;
            if c.archetype == NpcArchetype::MiningOp {
                let hauler_tid = hauler_type_for_system(sde, solar_system_id);
                let (h_ball, h_combat) = sde_ball_and_combat(
                    sde,
                    c.convoy_id + 1_000_000,
                    hauler_tid,
                    grid_origin + DVec3::new(2_100.0, 0.0, 250.0),
                );
                let _ = handle.spawn_ball_with_combat(h_ball, h_combat).await;
            }
            count += 1;
        }
        count
    }

    pub fn mark_convoy_destroyed(&self, convoy_id: i64) {
        if let Some(c) = self
            .convoys
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&convoy_id)
        {
            c.destroyed = true;
            if c.archetype == NpcArchetype::HaulerConvoy {
                let mut cd = self.hauler_cooldowns.write().unwrap_or_else(|e| e.into_inner());
                cd.insert(c.origin_system_id, std::time::Instant::now() + std::time::Duration::from_secs(1800));
            }
        }
    }

    pub fn mark_convoy_destroyed_with_supply_shock(&self, convoy_id: i64, db: &WorldRepository) {
        let convoy_opt = {
            let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            guard.get_mut(&convoy_id).and_then(|c| {
                if c.destroyed {
                    None
                } else {
                    c.destroyed = true;
                    if c.archetype == NpcArchetype::HaulerConvoy {
                        let mut cd = self.hauler_cooldowns.write().unwrap_or_else(|e| e.into_inner());
                        cd.insert(c.origin_system_id, std::time::Instant::now() + std::time::Duration::from_secs(1800));
                    }
                    Some(c.clone())
                }
            })
        };
        if let Some(convoy) = convoy_opt {
            db.with_mut_snapshot(|snap| {
                for order in snap.market_orders.values_mut() {
                    if !order.is_bid
                        && order.is_npc
                        && order.station_id == convoy.destination_station_id
                        && order.type_id == convoy.cargo_type_id
                    {
                        let spiked = ((order.price.0 as f64) * 1.15).ceil();
                        order.price = IskCents(spiked.clamp(1.0, i64::MAX as f64) as i64);
                    }
                }
            });
        }
    }

    pub fn register_convoy(&self, convoy: LivingWorldConvoy) {
        let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        guard.insert(convoy.convoy_id, convoy);
    }

    pub fn get_convoy(&self, convoy_id: i64) -> Option<LivingWorldConvoy> {
        self.convoys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&convoy_id)
            .cloned()
    }

    /// Interdicts and destroys a Living World convoy (`E2E-17`), dropping its physical cargo into a
    /// space wreck (`type_id: 26557`) in `convoy.current_system_id` and increasing NPC destination
    /// station ask prices by `+15%` due to supply loss.
    pub fn interdict_convoy(
        &self,
        convoy_id: i64,
        attacker_char_id: i64,
        db: &WorldRepository,
    ) -> Result<(i64, i64, IskCents), String> {
        let convoy = {
            let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            let c = guard
                .get_mut(&convoy_id)
                .ok_or_else(|| format!("Convoy {convoy_id} not found"))?;
            if c.destroyed {
                return Err(format!("Convoy {convoy_id} is already destroyed"));
            }
            if c.lod_level < 2 {
                return Err(format!(
                    "Convoy {convoy_id} is at LOD {} (must be materialized at LOD 2 on grid before interdiction)",
                    c.lod_level
                ));
            }
            c.destroyed = true;
            if c.archetype == NpcArchetype::HaulerConvoy {
                let mut cd = self.hauler_cooldowns.write().unwrap_or_else(|e| e.into_inner());
                cd.insert(c.origin_system_id, std::time::Instant::now() + std::time::Duration::from_secs(1800));
            }
            c.clone()
        };

        let now_unix = db.clock().now_unix_secs();
        db.try_with_mut_snapshot(|snap| -> Result<(i64, i64, IskCents), String> {
            snap.next_entity_id += 1;
            let wreck_item_id = snap.next_entity_id;
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap,
                wreck_item_id,
                now_unix,
                true,
            );
            snap.items.insert(
                wreck_item_id,
                InventoryItemRecord {
                    item_id: wreck_item_id,
                    type_id: 26557,
                    owner_id: attacker_char_id,
                    location_id: convoy.current_system_id as i64,
                    flag: InventoryFlag::None,
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

            snap.next_entity_id += 1;
            let loot_item_id = snap.next_entity_id;
            snap.items.insert(
                loot_item_id,
                InventoryItemRecord {
                    item_id: loot_item_id,
                    type_id: convoy.cargo_type_id,
                    owner_id: attacker_char_id,
                    location_id: wreck_item_id,
                    flag: InventoryFlag::Cargo,
                    quantity: convoy.cargo_quantity,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );

            // Spike NPC destination station ask orders for `cargo_type_id` by +15% (supply shock)
            let mut new_ask_price = IskCents::from_isk(6);
            for order in snap.market_orders.values_mut() {
                if !order.is_bid
                    && order.is_npc
                    && order.station_id == convoy.destination_station_id
                    && order.type_id == convoy.cargo_type_id
                {
                    let spiked = ((order.price.0 as f64) * 1.15).ceil();
                    order.price = IskCents(spiked.clamp(1.0, i64::MAX as f64) as i64);
                    new_ask_price = order.price;
                }
            }
            Ok((wreck_item_id, loot_item_id, new_ask_price))
        })
    }

    /// Advances an NPC mining operation cycle (`MN-17`).
    /// Mines asteroids in the current solar system using MN-04 laser cycle mechanics,
    /// depleting asteroid volume, accumulating ore in the mining op's cargo, and hauling
    /// ore to the NPC destination station to replenish regional market supply.
    pub async fn step_npc_mining_op(
        &self,
        convoy_id: i64,
        systems: &SystemRegistry,
        db: &WorldRepository,
        sde: &eve_sde::SdeRegistry,
    ) -> Result<NpcMiningStepResult, String> {
        let (sys_id, ship_type_id, cargo_type, current_cargo, dest_station_id) = {
            let guard = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            let c = guard
                .get(&convoy_id)
                .ok_or_else(|| format!("Convoy {convoy_id} not found"))?;
            if c.destroyed {
                return Err(format!("Convoy {convoy_id} is destroyed"));
            }
            if c.archetype != NpcArchetype::MiningOp {
                return Err(format!("Convoy {convoy_id} is not a MiningOp"));
            }
            (
                c.current_system_id,
                c.ship_type_id,
                c.cargo_type_id,
                c.cargo_quantity,
                c.destination_station_id,
            )
        };

        let handle = systems.get_or_spawn(sys_id);
        let asteroids = handle
            .populate_asteroid_belt_entities()
            .await
            .unwrap_or_default();

        let target_ast = asteroids
            .iter()
            .find(|a| a.type_id == cargo_type && !a.depleted && a.remaining_units > 0)
            .or_else(|| asteroids.iter().find(|a| !a.depleted && a.remaining_units > 0));

        let Some(target) = target_ast else {
            return Err(format!("No minable asteroids remaining in system {sys_id}"));
        };

        let target_id = target.asteroid_id;
        let ore_type_id = target.type_id;
        let ast_pos = handle
            .get_ball_snapshot(target_id as i64)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::new(50_000.0, 0.0, 0.0));

        // Ensure miner and companion hauler balls are spawned at the asteroid belt (`feat:npc.mining-op-npc`)
        if handle
            .get_ball_snapshot(convoy_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            let (m_ball, m_combat) = sde_ball_and_combat(
                sde,
                convoy_id,
                ship_type_id,
                ast_pos + DVec3::new(500.0, 0.0, 0.0),
            );
            let _ = handle.spawn_ball_with_combat(m_ball, m_combat).await;
        } else {
            let _ = handle.set_ball_position(convoy_id, ast_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        }
        let hauler_ball_id = convoy_id + 1_000_000;
        if handle
            .get_ball_snapshot(hauler_ball_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            let hauler_tid = hauler_type_for_system(sde, sys_id);
            let (h_ball, h_combat) = sde_ball_and_combat(
                sde,
                hauler_ball_id,
                hauler_tid,
                ast_pos + DVec3::new(1_200.0, 0.0, 200.0),
            );
            let _ = handle.spawn_ball_with_combat(h_ball, h_combat).await;
        } else {
            let _ = handle.set_ball_position(hauler_ball_id, ast_pos + DVec3::new(1_200.0, 0.0, 200.0)).await;
        }

        // Compute SDE mining laser cycle yield & Venture ore hold capacity (`483` Miner I + `ship_type_id` hull attrs)
        let ship_type = sde.types.get(&ship_type_id);
        let turrets = ship_type
            .and_then(|t| t.default_attributes.get(&102).copied())
            .unwrap_or(2.0)
            .max(1.0);
        let role_bonus_pct = ship_type
            .and_then(|t| t.default_attributes.get(&3239).copied())
            .unwrap_or(0.0);
        let laser_sde = sde.types.get(&483);
        let laser_m3 = laser_sde
            .and_then(|t| t.default_attributes.get(&77).copied())
            .unwrap_or(10.0)
            .max(1.0);
        let laser_dur_s = (laser_sde
            .and_then(|t| t.default_attributes.get(&73).copied())
            .unwrap_or(15_000.0)
            / 1_000.0)
            .max(1.0);
        // Yield scaled by elapsed cycle duration over laser cycle time (R-LW-RATE-01)
        let now_unix = db.clock().now_unix_secs();
        let elapsed_tick_secs: f64 = {
            let mut guard = self.mining_last_step.write().unwrap_or_else(|e| e.into_inner());
            let prev = guard.insert(convoy_id, now_unix);
            match prev {
                Some(p) => (now_unix.saturating_sub(p).max(1)) as f64,
                None => 0.0,
            }
        };
        let tick_m3 = laser_m3 * turrets * (1.0 + role_bonus_pct / 100.0) * (elapsed_tick_secs / laser_dur_s);
        let ore_unit_vol = sde
            .types
            .get(&ore_type_id)
            .map(|t| t.volume)
            .unwrap_or(0.1)
            .max(0.01);
        let units_per_tick = if elapsed_tick_secs > 0.0 {
            ((tick_m3 / ore_unit_vol).floor() as u64).max(1)
        } else {
            0
        };
        let ore_hold_m3 = ship_type
            .and_then(|t| t.default_attributes.get(&1556).copied().or(Some(t.capacity)))
            .unwrap_or(5_000.0)
            .max(500.0);

        let extraction = handle.mine_asteroid(target_id, units_per_tick, 0.0).await?;

        let mut hauled = false;
        let mut final_cargo = current_cargo + extraction.mined_units as i32;

        // When SDE ore hold capacity (`specialOreHoldCapacity` attr 1556) is filled, companion hauler delivers to NPC station
        if (final_cargo as f64) * ore_unit_vol >= ore_hold_m3 - 1e-6 {
            hauled = true;
            // Replenish destination station NPC market orders
            let added_units = final_cargo;
            let (sys_region_id, owner_corp_id) = sde
                .solar_systems
                .get(&sys_id)
                .map(|sys| {
                    let corp = sys
                        .stations
                        .iter()
                        .find(|st| st.station_id == dest_station_id)
                        .or_else(|| sys.stations.first())
                        .map(|st| st.owner_corporation_id)
                        .unwrap_or(1_000_035);
                    (sys.region_id, corp)
                })
                .unwrap_or((10_000_002, 1_000_035));
            db.with_mut_snapshot(|snap| {
                // Determine regional market price for ore from active sell orders, or fall back to SDE base price
                let ore_price = snap
                    .market_orders
                    .values()
                    .filter(|o| !o.is_bid && o.region_id == sys_region_id && o.type_id == ore_type_id && o.price.0 > 0)
                    .map(|o| o.price)
                    .min()
                    .unwrap_or_else(|| {
                        sde.types
                            .get(&ore_type_id)
                            .map(|t| IskCents::from_f64_isk(t.base_price.max(5.0)))
                            .unwrap_or_else(|| IskCents::from_isk(5))
                    });
                let mut found_order = false;
                for order in snap.market_orders.values_mut() {
                    if !order.is_bid
                        && order.is_npc
                        && order.station_id == dest_station_id
                        && order.type_id == ore_type_id
                    {
                        order.vol_remaining += added_units;
                        order.vol_entered += added_units;
                        found_order = true;
                        break;
                    }
                }
                if !found_order {
                    snap.next_entity_id += 1;
                    let oid = snap.next_entity_id;
                    snap.market_orders.insert(
                        oid,
                        eve_db::MarketOrderRecord {
                            order_id: oid,
                            owner_id: owner_corp_id,
                            station_id: dest_station_id,
                            region_id: sys_region_id,
                            type_id: ore_type_id,
                            is_bid: false,
                            price: ore_price,
                            vol_entered: added_units,
                            vol_remaining: added_units,
                            min_volume: 1,
                            is_npc: true,
                            escrow: IskCents(0),
                        },
                    );
                }
            });
            final_cargo = 0;
        }

        // Update convoy state
        {
            let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = guard.get_mut(&convoy_id) {
                c.cargo_quantity = final_cargo;
                c.lod_level = 2;
            }
        }

        Ok(NpcMiningStepResult {
            convoy_id,
            system_id: sys_id,
            asteroid_id: target_id as i64,
            ore_type_id,
            mined_units: extraction.mined_units,
            remaining_asteroid_units: extraction.remaining_units,
            convoy_cargo_units: final_cargo as u64,
            hauled_to_station: hauled,
        })
    }

    /// Sets or updates the multi-system stargate route for a living-world convoy (`feat:npc.npc-hauler`).
    pub fn set_convoy_route(&self, convoy_id: i64, route: Vec<SolarSystemId>) {
        let mut routes = self.routes.write().unwrap_or_else(|e| e.into_inner());
        routes.insert(
            convoy_id,
            ConvoyRouteState {
                route,
                route_index: 0,
                docked: false,
                last_hop_unix: 0,
            },
        );
    }

    /// Retrieves the current route state of a living-world convoy.
    pub fn get_convoy_route(&self, convoy_id: i64) -> Option<ConvoyRouteState> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&convoy_id)
            .cloned()
    }

    /// Checks if a living-world hauler convoy has arrived at destination and docked.
    pub fn is_convoy_docked(&self, convoy_id: i64) -> bool {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&convoy_id)
            .map(|r| r.docked)
            .unwrap_or(false)
    }

    /// DERIVED: hauler in-system transit duration
    ///
    /// Computes the in-system traversal duration for NPC haulers based on canonical EVE physics:
    /// 1. Align time derived from hull mass and inertia:
    ///    `tau = (inertia * mass) / 1_000_000.0`, `align_secs = ceil(ln(4) * tau).max(2.0)`.
    /// 2. Gate-to-gate / station distance calculated using SDE celestial positions:
    ///    if origin and destination celestials exist, distance is `pos1.distance(pos2)`;
    ///    otherwise default to 15 AU (`15.0 * 149_597_870_700.0`).
    /// 3. Warp speed in m/s derived from `base_warp_speed * warp_speed_mult` and transit time
    ///    calculated using the standard acceleration profile (`compute_warp_duration_seconds`).
    /// 4. Stargate jump transit delay (5.0s).
    pub fn compute_hauler_transit_duration_secs(
        sde: &eve_sde::SdeRegistry,
        ship_type_id: u32,
        current_system_id: SolarSystemId,
        prev_system_id: Option<SolarSystemId>,
        next_system_id: Option<SolarSystemId>,
        destination_station_id: i64,
    ) -> u64 {
        const AU_METERS: f64 = 149_597_870_700.0;

        let (mass, inertia, max_vel, warp_speed_au_s) = sde
            .types
            .get(&ship_type_id)
            .map(|t| {
                let m = t.default_attributes.get(&eve_sde::attr_ids::MASS).copied().unwrap_or(t.mass).max(1_000.0);
                let ag = t.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied().unwrap_or(0.5).max(0.01);
                let mv = t.default_attributes.get(&37).copied().unwrap_or(150.0).max(10.0);
                let base_warp = t.default_attributes.get(&1281).copied().unwrap_or(1.0).max(0.5);
                let warp_mult = t.default_attributes.get(&600).copied().unwrap_or(3.0).max(1.0);
                let ws = (base_warp * warp_mult).clamp(1.5, 12.0);
                (m, ag, mv, ws)
            })
            .unwrap_or((10_000_000.0, 0.5, 150.0, 3.0));

        let tau = (inertia * mass) / 1_000_000.0;
        let align_secs = (4.0_f64.ln() * tau).ceil().max(2.0);

        // Calculate gate-to-gate / station distance using SDE celestial positions:
        // if origin and destination celestials exist, distance is pos1.distance(pos2);
        // otherwise default to 15 AU (15.0 * 149_597_870_700.0).
        let origin_pos = prev_system_id
            .and_then(|prev| {
                sde.stargates()
                    .values()
                    .find(|g| g.from_solar_system_id == current_system_id && g.to_solar_system_id == prev)
                    .map(|g| g.position)
            })
            .or_else(|| {
                sde.stations
                    .values()
                    .find(|sta| sta.solar_system_id == current_system_id)
                    .map(|sta| sta.position)
            })
            .or_else(|| {
                sde.stargates()
                    .values()
                    .find(|g| g.from_solar_system_id == current_system_id)
                    .map(|g| g.position)
            });

        let dest_pos = next_system_id
            .and_then(|next| {
                sde.stargates()
                    .values()
                    .find(|g| g.from_solar_system_id == current_system_id && g.to_solar_system_id == next)
                    .map(|g| g.position)
            })
            .or_else(|| {
                sde.stations
                    .get(&destination_station_id)
                    .filter(|sta| sta.solar_system_id == current_system_id)
                    .map(|sta| sta.position)
            })
            .or_else(|| {
                sde.stations
                    .values()
                    .find(|sta| sta.solar_system_id == current_system_id)
                    .map(|sta| sta.position)
            });

        let distance = match (origin_pos, dest_pos) {
            (Some(pos1), Some(pos2)) => pos1.distance(pos2),
            _ => 15.0 * 149_597_870_700.0,
        };

        let _warp_speed_mps = warp_speed_au_s * AU_METERS;
        let warp_transit_secs = crate::formulas::compute_warp_duration_seconds(distance, warp_speed_au_s, max_vel);
        let gate_jump_secs = 5.0;

        (align_secs + warp_transit_secs + gate_jump_secs).round() as u64
    }

    /// Advances a living-world hauler convoy along its multi-system stargate route (`feat:npc.npc-hauler`).
    /// The hauler jumps stargates across systems on ticks and docks/despawns at destination station.
    pub async fn step_hauler_convoy(
        &self,
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
            let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            let c = convoys
                .get_mut(&convoy_id)
                .ok_or_else(|| format!("Convoy {convoy_id} not found"))?;
            if c.destroyed {
                StepAction::Destroyed
            } else {
                let mut routes = self.routes.write().unwrap_or_else(|e| e.into_inner());
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
                    // DERIVED: hauler in-system transit duration
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
                    let transit_duration_secs = Self::compute_hauler_transit_duration_secs(
                        sde,
                        c.ship_type_id,
                        c.current_system_id,
                        prev_sys,
                        next_sys,
                        c.destination_station_id,
                    );

                    if r.last_hop_unix == 0 {
                        r.last_hop_unix = now_unix;
                        StepAction::Traverse {
                            system_id: c.current_system_id,
                        }
                    } else if now_unix.saturating_sub(r.last_hop_unix) < transit_duration_secs {
                        StepAction::Traverse {
                            system_id: c.current_system_id,
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
            StepAction::Traverse { system_id } => {
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
                        .find(|g| g.from_solar_system_id == next_sys && g.to_solar_system_id == prev_sys)
                        .map(|g| g.position)
                        .or_else(|| {
                            sde.stargates()
                                .values()
                                .find(|g| g.from_solar_system_id == next_sys)
                                .map(|g| g.position)
                        })
                        .unwrap_or(DVec3::new(10_000.0, 0.0, 0.0));

                    let (ball, combat) = sde_ball_and_combat(
                        sde,
                        convoy_id,
                        ship_type_id,
                        gate_pos + DVec3::new(1_000.0, 0.0, 0.0),
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

    /// Ensures at least one `MiningOp` exists in `solar_system_id` (`feat:npc.mining-op-npc`).
    pub fn ensure_system_mining_op(
        &self,
        solar_system_id: SolarSystemId,
        sde: &eve_sde::SdeRegistry,
    ) -> i64 {
        {
            let convoys = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = convoys
                .values()
                .find(|c| c.archetype == NpcArchetype::MiningOp && c.current_system_id == solar_system_id)
            {
                return existing.convoy_id;
            }
        }
        let dest_station_id = sde
            .solar_systems
            .get(&solar_system_id)
            .and_then(|sys| sys.stations.first().map(|st| st.station_id))
            .unwrap_or(60_003_760);
        let convoy_id = 800_100_000 + (solar_system_id as i64 % 100_000);
        let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        convoys.entry(convoy_id).or_insert(LivingWorldConvoy {
            convoy_id,
            archetype: NpcArchetype::MiningOp,
            ship_type_id: 32880, // Venture
            origin_system_id: solar_system_id,
            current_system_id: solar_system_id,
            destination_system_id: solar_system_id,
            destination_station_id: dest_station_id,
            cargo_type_id: 1230, // Veldspar
            cargo_quantity: 0,
            lod_level: 1,
            destroyed: false,
        });
        convoy_id
    }

    /// Ensures a stargate-routed `HaulerConvoy` exists originating in `solar_system_id` (`feat:npc.npc-hauler`).
    pub fn ensure_system_hauler_convoy(
        &self,
        solar_system_id: SolarSystemId,
        sde: &eve_sde::SdeRegistry,
    ) -> i64 {
        let convoy_id = 800_200_000 + (solar_system_id as i64 % 100_000);
        {
            let convoys = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = convoys.get(&convoy_id) {
                if !existing.destroyed {
                    return existing.convoy_id;
                }
                // Hauler respawn cooldown: if destroyed, enforce respawn delay before recreation
                let cooldowns = self.hauler_cooldowns.read().unwrap_or_else(|e| e.into_inner());
                if let Some(&respawn_at) = cooldowns.get(&solar_system_id) {
                    if std::time::Instant::now() < respawn_at {
                        return existing.convoy_id;
                    }
                }
            }
        }
        let next_sys = sde
            .stargates()
            .values()
            .find(|g| g.from_solar_system_id == solar_system_id)
            .map(|g| g.to_solar_system_id)
            .unwrap_or(30_000_144);
        let dest_station_id = sde
            .solar_systems
            .get(&next_sys)
            .and_then(|sys| sys.stations.first().map(|st| st.station_id))
            .unwrap_or(60_003_760);
        let hauler_tid = hauler_type_for_system(sde, solar_system_id);
        // DERIVED: Primary regional mineral cargo (Tritanium type_id 34, SDE volume 0.01 m3) scaled to hauler cargo hold
        let cargo_type_id = sde
            .types
            .get(&34)
            .map(|t| t.type_id)
            .unwrap_or(34);
        let cargo_m3 = sde
            .types
            .get(&hauler_tid)
            .map(|t| t.capacity)
            .unwrap_or(5_000.0);
        let unit_vol = sde
            .types
            .get(&cargo_type_id)
            .map(|t| t.volume)
            .unwrap_or(0.01)
            .max(0.01);
        let cargo_quantity = ((cargo_m3 * 0.1 / unit_vol) as i32).max(25_000);
        {
            let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
            convoys.insert(
                convoy_id,
                LivingWorldConvoy {
                    convoy_id,
                    archetype: NpcArchetype::HaulerConvoy,
                    ship_type_id: hauler_tid,
                    origin_system_id: solar_system_id,
                    current_system_id: solar_system_id,
                    destination_system_id: next_sys,
                    destination_station_id: dest_station_id,
                    cargo_type_id,
                    cargo_quantity,
                    lod_level: 1,
                    destroyed: false,
                },
            );
        }
        self.set_convoy_route(convoy_id, vec![solar_system_id, next_sys]);
        convoy_id
    }

    /// Advances all active NPC mining operations on living-world ticks (`feat:npc.mining-op-npc`, `feat:mining.npc-mining-ops`).
    pub async fn step_all_mining_ops(
        &self,
        systems: &SystemRegistry,
        db: &WorldRepository,
        sde: &eve_sde::SdeRegistry,
    ) -> Vec<(i64, Result<NpcMiningStepResult, String>)> {
        let active_ids: Vec<i64> = {
            let convoys = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            convoys
                .values()
                .filter(|c| !c.destroyed && c.archetype == NpcArchetype::MiningOp)
                .map(|c| c.convoy_id)
                .collect()
        };
        let mut results = Vec::new();
        for cid in active_ids {
            let res = self.step_npc_mining_op(cid, systems, db, sde).await;
            results.push((cid, res));
        }
        results
    }

    /// Advances all active hauler convoys on living-world ticks (`feat:npc.npc-hauler`).
    pub async fn step_all_hauler_convoys(
        &self,
        systems: &SystemRegistry,
        db: &WorldRepository,
        sde: &eve_sde::SdeRegistry,
    ) -> Vec<(i64, Result<ConvoyStepStatus, String>)> {
        let active_ids: Vec<i64> = {
            let routes = self.routes.read().unwrap_or_else(|e| e.into_inner());
            let convoys = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            routes
                .iter()
                .filter_map(|(&cid, r)| {
                    if !r.docked {
                        convoys.get(&cid).and_then(|c| {
                            if !c.destroyed && c.archetype == NpcArchetype::HaulerConvoy {
                                Some(cid)
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut results = Vec::new();
        for cid in active_ids {
            let res = self.step_hauler_convoy(cid, systems, db, sde).await;
            results.push((cid, res));
        }
        results
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcMiningStepResult {
    pub convoy_id: i64,
    pub system_id: SolarSystemId,
    pub asteroid_id: i64,
    pub ore_type_id: TypeId,
    pub mined_units: u64,
    pub remaining_asteroid_units: u64,
    pub convoy_cargo_units: u64,
    pub hauled_to_station: bool,
}
