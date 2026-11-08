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

#[path = "living_world_haulers.rs"]
pub mod living_world_haulers;
pub use living_world_haulers::*;

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
    pub(crate) convoys: RwLock<HashMap<i64, LivingWorldConvoy>>,
    pub(crate) routes: RwLock<HashMap<i64, ConvoyRouteState>>,
    pub(crate) hauler_cooldowns: RwLock<HashMap<SolarSystemId, std::time::Instant>>,
    pub(crate) mining_last_step: RwLock<HashMap<i64, u64>>,
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

fn system_faction_id(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> u32 {
    sde.solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            sde.constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.faction_id))
                .or_else(|| {
                    sys.stations.iter().find_map(|st| {
                        st.owner_faction_id.map(|f| f as u32).or_else(|| {
                            sde.npc_corporations
                                .get(&st.owner_corporation_id)
                                .and_then(|c| c.faction_id)
                        })
                    })
                })
        })
        .or_else(|| {
            sde.factions
                .values()
                .find(|f| f.solar_system_id == Some(solar_system_id))
                .map(|f| f.faction_id)
        })
        .unwrap_or(500_001)
}

/// Resolves the empire/faction industrial hauler `TypeId` for `solar_system_id` from SDE (`group_id == 28`).
pub fn hauler_type_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> TypeId {
    match system_faction_id(sde, solar_system_id) {
        500_002 => 653, // Minmatar (Wreathe)
        500_003 => 649, // Amarr
        500_004 => 651, // Gallente
        _ => 648,       // Caldari (Badger)
    }
}

pub fn miner_type_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> TypeId {
    match system_faction_id(sde, solar_system_id) {
        500_002 => 20163, // Minmatar Miner
        500_003 => 26750, // Sansha Miner (fallback)
        500_004 => 21840, // Gallente Miner
        _ => 21654,       // Nugoeihuvi Miner
    }
}

/// Resolves the empire/faction frigate/destroyer escort hull `TypeId` protecting an NPC mining fleet (`feat:npc.mining-op-npc`).
pub fn mining_escort_type_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> TypeId {
    match system_faction_id(sde, solar_system_id) {
        500_002 => 16242, // Thrasher (Republic defense escort)
        500_003 => 16236, // Coercer (Imperial defense escort)
        500_004 => 16240, // Catalyst (Federal defense escort)
        _ => 16238,       // Cormorant (Caldari defense escort)
    }
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

    /// Returns the current ore units persisted in `eve-db` on the companion hauler for `convoy_id`.
    pub fn hauler_cargo_units(&self, db: &WorldRepository, convoy_id: i64) -> i32 {
        let hauler_ball_id = convoy_id + 1_000_000;
        db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|item| {
                    item.location_id == hauler_ball_id
                        && item.flag == eve_db::InventoryFlag::Cargo
                        && item.quantity > 0
                })
                .map(|item| item.quantity)
                .sum()
        })
    }

    /// Resolves the parent `MiningOp` convoy for an NPC mining companion `hauler_ball_id`.
    pub fn get_mining_hauler_convoy(&self, hauler_ball_id: i64) -> Option<LivingWorldConvoy> {
        if hauler_ball_id <= 1_000_000 {
            return None;
        }
        self.get_convoy(hauler_ball_id - 1_000_000)
            .filter(|c| c.archetype == NpcArchetype::MiningOp)
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
                let escort_tid = mining_escort_type_for_system(sde, solar_system_id);
                let (e_ball, e_combat) = sde_ball_and_combat(
                    sde,
                    c.convoy_id + 2_000_000,
                    escort_tid,
                    grid_origin + DVec3::new(1_850.0, 300.0, 0.0),
                );
                let _ = handle.spawn_ball_with_combat(e_ball, e_combat).await;
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

    /// Marks a live convoy destroyed exactly once (combat kill path). Losing a convoy does not
    /// rewrite NPC market prices: no supply-shock multiplier exists in the client data.
    pub fn mark_convoy_destroyed_in_combat(&self, convoy_id: i64) {
        let mut guard = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.get_mut(&convoy_id).filter(|c| !c.destroyed) {
            c.destroyed = true;
            if c.archetype == NpcArchetype::HaulerConvoy {
                let mut cd = self.hauler_cooldowns.write().unwrap_or_else(|e| e.into_inner());
                cd.insert(c.origin_system_id, std::time::Instant::now() + std::time::Duration::from_secs(1800));
            }
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
    /// space wreck (`type_id: 26557`) in `convoy.current_system_id`; returns the destination station's
    /// current NPC ask price for the cargo type.
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

            // Report the destination station's current NPC ask price; convoy loss does not move it.
            let new_ask_price = snap
                .market_orders
                .values()
                .filter(|o| {
                    !o.is_bid && o.is_npc && o.station_id == convoy.destination_station_id && o.type_id == convoy.cargo_type_id
                })
                .map(|o| o.price)
                .min_by_key(|p| p.0)
                .unwrap_or(IskCents::from_isk(6));
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
        let (sys_id, ship_type_id, cargo_type, current_cargo, dest_station_id, destroyed) = {
            let guard = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            let c = guard
                .get(&convoy_id)
                .ok_or_else(|| format!("Convoy {convoy_id} not found"))?;
            if c.archetype != NpcArchetype::MiningOp {
                return Err(format!("Convoy {convoy_id} is not a MiningOp"));
            }
            (
                c.current_system_id,
                c.ship_type_id,
                c.cargo_type_id,
                c.cargo_quantity,
                c.destination_station_id,
                c.destroyed,
            )
        };

        let handle = systems.get_or_spawn(sys_id);
        let hauler_ball_id = convoy_id + 1_000_000;
        let escort_ball_id = convoy_id + 2_000_000;
        if destroyed {
            let _ = handle.remove_ball(escort_ball_id).await;
            let _ = handle.remove_ball(hauler_ball_id).await;
            let _ = handle.remove_ball(convoy_id).await;
            db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, item| {
                    !(item.location_id == hauler_ball_id && item.flag == eve_db::InventoryFlag::Cargo)
                });
            });
            return Err(format!("Convoy {convoy_id} is destroyed"));
        }
        let asteroids = handle
            .populate_asteroid_belt_entities()
            .await
            .unwrap_or_default();

        let target_ast = asteroids
            .iter()
            .find(|a| a.type_id == cargo_type && !a.depleted && a.remaining_units > 0)
            .or_else(|| asteroids.iter().find(|a| !a.depleted && a.remaining_units > 0));

        let Some(target) = target_ast else {
            let _ = handle.remove_ball(escort_ball_id).await;
            let _ = handle.remove_ball(hauler_ball_id).await;
            let _ = handle.remove_ball(convoy_id).await;
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

        let belt_hauler_pos = ast_pos + DVec3::new(1_200.0, 0.0, 200.0);
        let dest_station_pos = sde
            .solar_systems
            .get(&sys_id)
            .and_then(|sys| sys.stations.iter().find(|st| st.station_id == dest_station_id))
            .map(|st| st.position)
            .unwrap_or(belt_hauler_pos + DVec3::new(100_000_000.0, 0.0, 0.0));

        // Ensure miner, companion hauler, and defense escort balls exist at the asteroid belt (`feat:npc.mining-op-npc`).
        // If the miner or hauler has taken hostile fire, evade by warping toward destination station instead of snapping back.
        let miner_snap = handle.get_ball_snapshot(convoy_id).await.ok().flatten();
        let miner_under_fire = {
            let bp = handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
            [convoy_id, hauler_ball_id].into_iter().any(|bid| {
                bp.combat_states.get(&bid).is_some_and(|mc| {
                    mc.shield_hp + 1.0 < mc.max_shield_hp || mc.armor_hp + 1.0 < mc.max_armor_hp
                })
            })
        };
        match miner_snap {
            None => {
                let (m_ball, m_combat) = sde_ball_and_combat(
                    sde,
                    convoy_id,
                    ship_type_id,
                    ast_pos + DVec3::new(500.0, 0.0, 0.0),
                );
                let _ = handle.spawn_ball_with_combat(m_ball, m_combat).await;
            }
            Some((_, mb)) if miner_under_fire => {
                if !matches!(mb.mode, MovementMode::Warp { .. }) && (mb.position - dest_station_pos).length() > 5_000.0 {
                    let _ = handle
                        .set_movement_mode(
                            convoy_id,
                            MovementMode::new_warp(mb.position, dest_station_pos, 3.0),
                        )
                        .await;
                }
            }
            Some(_) => {
                let _ = handle.set_ball_position(convoy_id, ast_pos + DVec3::new(500.0, 0.0, 0.0)).await;
            }
        }
        if handle
            .get_ball_snapshot(escort_ball_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            let escort_tid = mining_escort_type_for_system(sde, sys_id);
            let (e_ball, e_combat) = sde_ball_and_combat(
                sde,
                escort_ball_id,
                escort_tid,
                ast_pos + DVec3::new(900.0, 250.0, 0.0),
            );
            let _ = handle.spawn_ball_with_combat(e_ball, e_combat).await;
        }
        let in_transit_cargo = self.hauler_cargo_units(db, convoy_id);
        let hauler_snap = handle.get_ball_snapshot(hauler_ball_id).await.ok().flatten();
        if in_transit_cargo > 0 || miner_under_fire {
            if let Some((_, hb)) = hauler_snap.as_ref() {
                if (hb.position - dest_station_pos).length() > 5_000.0 {
                    let _ = handle
                        .set_movement_mode(
                            hauler_ball_id,
                            eve_destiny::MovementMode::new_warp(hb.position, dest_station_pos, 3.0),
                        )
                        .await;
                }
            } else {
                // Hauler was destroyed in transit; clear any uncollected cargo items from DB
                db.with_mut_snapshot(|snap| {
                    snap.items.retain(|_, item| {
                        !(item.location_id == hauler_ball_id && item.flag == eve_db::InventoryFlag::Cargo)
                    });
                });
            }
        } else if hauler_snap.is_none() {
            let hauler_tid = hauler_type_for_system(sde, sys_id);
            let (h_ball, h_combat) = sde_ball_and_combat(
                sde,
                hauler_ball_id,
                hauler_tid,
                dest_station_pos,
            );
            let _ = handle.spawn_ball_with_combat(h_ball, h_combat).await;
            let _ = handle
                .set_movement_mode(
                    hauler_ball_id,
                    eve_destiny::MovementMode::new_warp(dest_station_pos, belt_hauler_pos, 3.0),
                )
                .await;
        }

        // DERIVED (MN-17): NPC mining op simulation uses SDE hull + Miner I (`483`) attributes rather
        // than `mining_cycle::resolve_cycle` with fitted-module attributes (`feat:mining.npc-mining-ops`
        // and `feat:mining.mining-missions` are `DONE` in `parity.json` from pre-existing tests;
        // `plan:MN-17` remains open until NPC miners run through the shared cycle engine and are attackable).
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
            .and_then(|t| {
                t.default_attributes
                    .get(&1556)
                    .copied()
                    .filter(|&v| v > 0.0)
                    .or_else(|| (t.capacity > 0.0).then_some(t.capacity))
            })
            .unwrap_or(5_000.0)
            .clamp(500.0, 5_000.0);

        let extraction = handle.mine_asteroid(target_id, units_per_tick, 0.0).await?;

        let mut hauled = false;
        let mut final_cargo = current_cargo + extraction.mined_units as i32;
        let hold_full = (final_cargo as f64) * ore_unit_vol >= ore_hold_m3 - 1e-6;
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

        if in_transit_cargo > 0 {
            if let Some((_, hb)) = hauler_snap {
                let arrived = (hb.position - dest_station_pos).length() <= 5_000.0 || elapsed_tick_secs >= 15.0;
                if arrived {
                    hauled = true;
                    let _ = handle.remove_ball(hauler_ball_id).await;
                    db.with_mut_snapshot(|snap| {
                        snap.items.retain(|_, item| {
                            !(item.location_id == hauler_ball_id && item.flag == eve_db::InventoryFlag::Cargo)
                        });
                        if let Some(existing) = snap.items.values_mut().find(|it| {
                            it.owner_id == 1
                                && it.location_id == dest_station_id
                                && it.type_id == ore_type_id
                                && it.flag == eve_db::InventoryFlag::Hangar
                                && !it.singleton
                        }) {
                            existing.quantity =
                                existing.quantity.saturating_add(in_transit_cargo as i32);
                            existing.version += 1;
                        } else {
                            snap.next_entity_id += 1;
                            let iid = snap.next_entity_id;
                            snap.items.insert(
                                iid,
                                eve_db::InventoryItemRecord {
                                    item_id: iid,
                                    type_id: ore_type_id,
                                    owner_id: 1,
                                    location_id: dest_station_id,
                                    flag: eve_db::InventoryFlag::Hangar,
                                    quantity: in_transit_cargo as i32,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                        let ore_price = snap
                            .market_orders
                            .values()
                            .filter(|o| !o.is_bid && o.region_id == sys_region_id && o.type_id == ore_type_id && o.price.0 > 0)
                            .map(|o| o.price)
                            .min()
                            .unwrap_or_else(|| {
                                let raw = sde
                                    .types
                                    .get(&ore_type_id)
                                    .map(|t| t.base_price.max(5.0))
                                    .unwrap_or(5.0);
                                IskCents::from_f64_isk(
                                    crate::services::market_contracts::round_to_4_sig_fig_price(raw),
                                )
                            });
                        let mut found_order = false;
                        for order in snap.market_orders.values_mut() {
                            if !order.is_bid
                                && order.is_npc
                                && order.station_id == dest_station_id
                                && order.type_id == ore_type_id
                            {
                                order.vol_remaining += in_transit_cargo;
                                order.vol_entered += in_transit_cargo;
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
                                    vol_entered: in_transit_cargo,
                                    vol_remaining: in_transit_cargo,
                                    min_volume: 1,
                                    is_npc: true,
                                    escrow: IskCents(0),
                                },
                            );
                        }
                    });
                    if extraction.remaining_units > 0 || final_cargo > 0 {
                        let hauler_tid = hauler_type_for_system(sde, sys_id);
                        let (h_ball, h_combat) =
                            sde_ball_and_combat(sde, hauler_ball_id, hauler_tid, dest_station_pos);
                        let _ = handle.spawn_ball_with_combat(h_ball, h_combat).await;
                        let _ = handle
                            .set_movement_mode(
                                hauler_ball_id,
                                eve_destiny::MovementMode::new_warp(
                                    dest_station_pos,
                                    belt_hauler_pos,
                                    3.0,
                                ),
                            )
                            .await;
                    }
                }
            }
        } else if hold_full {
            // Load mined ore onto hauler in eve-db and initiate warp to destination station (multi-tick transit)
            let loaded_units = final_cargo;
            final_cargo = 0;
            db.with_mut_snapshot(|snap| {
                if let Some(existing) = snap.items.values_mut().find(|it| {
                    it.owner_id == owner_corp_id
                        && it.location_id == hauler_ball_id
                        && it.type_id == ore_type_id
                        && it.flag == eve_db::InventoryFlag::Cargo
                        && !it.singleton
                }) {
                    existing.quantity = existing.quantity.saturating_add(loaded_units);
                    existing.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let item_id = snap.next_entity_id;
                    snap.items.insert(
                        item_id,
                        eve_db::InventoryItemRecord {
                            item_id,
                            type_id: ore_type_id,
                            owner_id: owner_corp_id,
                            location_id: hauler_ball_id,
                            flag: eve_db::InventoryFlag::Cargo,
                            quantity: loaded_units,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
            });
            if hauler_snap.is_none() {
                let hauler_tid = hauler_type_for_system(sde, sys_id);
                let (h_ball, h_combat) =
                    sde_ball_and_combat(sde, hauler_ball_id, hauler_tid, belt_hauler_pos);
                let _ = handle.spawn_ball_with_combat(h_ball, h_combat).await;
            }
            let _ = handle
                .set_movement_mode(
                    hauler_ball_id,
                    eve_destiny::MovementMode::new_warp(belt_hauler_pos, dest_station_pos, 3.0),
                )
                .await;
        } else if extraction.remaining_units == 0 && final_cargo == 0 && in_transit_cargo == 0 {
            let _ = handle.remove_ball(hauler_ball_id).await;
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
                // DERIVED: warp speed multiplier clamped to [1.5, 12.0] AU/s to bound industrial haulers to scout ceilings
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
                    .filter(|g| g.from_solar_system_id == current_system_id && g.to_solar_system_id == prev)
                    .min_by_key(|g| g.stargate_id)
                    .map(|g| g.position)
            })
            .or_else(|| {
                sde.stations
                    .values()
                    .filter(|sta| sta.solar_system_id == current_system_id)
                    .min_by_key(|sta| sta.station_id)
                    .map(|sta| sta.position)
            })
            .or_else(|| {
                sde.stargates()
                    .values()
                    .filter(|g| g.from_solar_system_id == current_system_id)
                    .min_by_key(|g| g.stargate_id)
                    .map(|g| g.position)
            });

        let dest_pos = next_system_id
            .and_then(|next| {
                sde.stargates()
                    .values()
                    .filter(|g| g.from_solar_system_id == current_system_id && g.to_solar_system_id == next)
                    .min_by_key(|g| g.stargate_id)
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
                    .filter(|sta| sta.solar_system_id == current_system_id)
                    .min_by_key(|sta| sta.station_id)
                    .map(|sta| sta.position)
            });

        // DERIVED: fallback to canonical 15 AU average system intra-gate distance if stargates missing in SDE
        let distance = match (origin_pos, dest_pos) {
            (Some(pos1), Some(pos2)) => pos1.distance(pos2),
            _ => 15.0 * AU_METERS,
        };

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
        crate::living_world_haulers::step_hauler_convoy_internal(self, convoy_id, systems, db, sde).await
    }

    fn resolve_npc_station_for_system(
        sde: &eve_sde::SdeRegistry,
        solar_system_id: SolarSystemId,
    ) -> i64 {
        let Some(cur) = sde.solar_systems.get(&solar_system_id) else {
            return 0;
        };
        cur.stations
            .iter()
            .min_by_key(|st| st.station_id)
            .map(|st| st.station_id)
            .or_else(|| {
                sde.stations
                    .values()
                    .filter(|st| {
                        sde.solar_systems
                            .get(&st.solar_system_id)
                            .is_some_and(|s| s.constellation_id == cur.constellation_id)
                    })
                    .min_by_key(|st| st.station_id)
                    .map(|st| st.station_id)
            })
            .or_else(|| {
                sde.stations
                    .values()
                    .filter(|st| {
                        sde.solar_systems
                            .get(&st.solar_system_id)
                            .is_some_and(|s| s.region_id == cur.region_id)
                    })
                    .min_by_key(|st| st.station_id)
                    .map(|st| st.station_id)
            })
            .unwrap_or(0)
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
                .filter(|c| c.archetype == NpcArchetype::MiningOp && c.current_system_id == solar_system_id)
                .min_by_key(|c| c.convoy_id)
            {
                return existing.convoy_id;
            }
        }
        let dest_station_id = Self::resolve_npc_station_for_system(sde, solar_system_id);
        let miner_type_id = miner_type_for_system(sde, solar_system_id);
        let default_ore_id = sde
            .belt_ores_for_system(solar_system_id)
            .first()
            .map(|t| t.type_id)
            .unwrap_or(1230);
        let convoy_id = 800_100_000 + (solar_system_id as i64 % 100_000);
        let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        convoys.entry(convoy_id).or_insert(LivingWorldConvoy {
            convoy_id,
            archetype: NpcArchetype::MiningOp,
            ship_type_id: miner_type_id,
            origin_system_id: solar_system_id,
            current_system_id: solar_system_id,
            destination_system_id: solar_system_id,
            destination_station_id: dest_station_id,
            cargo_type_id: default_ore_id,
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
                if !existing.destroyed && !self.is_convoy_docked(convoy_id) {
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
        let route = crate::living_world_haulers::compute_trade_route(sde, solar_system_id);
        let dest_sys = *route.last().unwrap_or(&solar_system_id);
        let dest_station_id = Self::resolve_npc_station_for_system(sde, dest_sys);
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
                    destination_system_id: dest_sys,
                    destination_station_id: dest_station_id,
                    cargo_type_id,
                    cargo_quantity,
                    lod_level: 1,
                    destroyed: false,
                },
            );
        }
        self.set_convoy_route(convoy_id, route);
        convoy_id
    }

    /// Ensures an `EmpireNavyPatrol` entry exists in `solar_system_id` for empire-owned systems (`feat:npc.navy-npc`).
    pub fn ensure_system_navy_patrol(
        &self,
        solar_system_id: SolarSystemId,
        sde: &eve_sde::SdeRegistry,
    ) -> Option<i64> {
        let sys = sde.solar_systems.get(&solar_system_id)?;
        let faction_id = sys
            .stations
            .first()
            .and_then(|st| st.owner_faction_id.filter(|&f| f > 0).map(|f| f as u32))
            .or_else(|| sde.constellations.get(&sys.constellation_id).and_then(|c| c.faction_id.filter(|&f| f > 0)))
            .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.faction_id.filter(|&f| f > 0)))?;
        if !matches!(faction_id, 500_001..=500_004) {
            return None;
        }
        let convoy_id = 800_300_000 + (solar_system_id as i64 % 100_000);
        let ship_type_id = match faction_id {
            500_001 => 52_936, // State Navy Cerberus
            500_002 => 52_959, // Republic Fleet Muninn
            500_003 => 52_927, // Imperial Navy Zealot
            500_004 => 52_950, // Federal Navy Deimos
            _ => 52_936,
        };
        let mut convoys = self.convoys.write().unwrap_or_else(|e| e.into_inner());
        convoys.entry(convoy_id).or_insert(LivingWorldConvoy {
            convoy_id,
            archetype: NpcArchetype::EmpireNavyPatrol,
            ship_type_id,
            origin_system_id: solar_system_id,
            current_system_id: solar_system_id,
            destination_system_id: solar_system_id,
            destination_station_id: sys.stations.first().map(|s| s.station_id).unwrap_or(0),
            cargo_type_id: 0,
            cargo_quantity: 0,
            lod_level: 2,
            destroyed: false,
        });
        Some(convoy_id)
    }

    /// Advances all active NPC mining operations on living-world ticks (`feat:npc.mining-op-npc`, `feat:mining.npc-mining-ops`).
    pub async fn step_all_mining_ops(
        &self,
        systems: &SystemRegistry,
        db: &WorldRepository,
        sde: &eve_sde::SdeRegistry,
    ) -> Vec<(i64, Result<NpcMiningStepResult, String>)> {
        let mut active_ids: Vec<i64> = {
            let convoys = self.convoys.read().unwrap_or_else(|e| e.into_inner());
            convoys
                .values()
                .filter(|c| !c.destroyed && c.archetype == NpcArchetype::MiningOp)
                .map(|c| c.convoy_id)
                .collect()
        };
        active_ids.sort_unstable();
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
        let mut active_ids: Vec<i64> = {
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
        active_ids.sort_unstable();
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

#[cfg(test)]
#[path = "living_world_tests.rs"]
mod living_world_tests;

#[cfg(test)]
#[path = "living_world_extra_tests.rs"]
mod living_world_extra_tests;

#[path = "living_world_cov_unit_tests.rs"]
#[cfg(test)]
mod living_world_cov_unit_tests;

