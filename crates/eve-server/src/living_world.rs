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

pub struct LivingWorldDirector {
    convoys: RwLock<HashMap<i64, LivingWorldConvoy>>,
}

impl Default for LivingWorldDirector {
    fn default() -> Self {
        Self::new()
    }
}

impl LivingWorldDirector {
    /// Initializes an empty `LivingWorldDirector` with `0` pre-seeded convoys (`S-5`).
    pub fn new() -> Self {
        Self {
            convoys: RwLock::new(HashMap::new()),
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
                ship_type_id: 587,
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
            let grid_origin = station_pos + DVec3::new(1_500.0, 0.0, 0.0);
            let ball = Ball {
                id: c.convoy_id,
                type_id: c.ship_type_id,
                position: grid_origin + DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.5,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let _ = handle
                .spawn_ball_with_combat(
                    ball,
                    eve_destiny::CombatState {
                        shield_hp: 50.0,
                        max_shield_hp: 50.0,
                        armor_hp: 50.0,
                        max_armor_hp: 50.0,
                        structure_hp: 50.0,
                        max_structure_hp: 50.0,
                        ..eve_destiny::CombatState::default()
                    },
                )
                .await;
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
        }
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
            c.clone()
        };

        db.try_with_mut_snapshot(|snap| -> Result<(i64, i64, IskCents), String> {
            snap.next_entity_id += 1;
            let wreck_item_id = snap.next_entity_id;
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
}
