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
        Self::new_seeded()
    }
}

impl LivingWorldDirector {
    pub fn new_seeded() -> Self {
        let mut convoys = HashMap::new();
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
                ship_type_id: 11001, // Guristas Pithi Invader
                origin_system_id: 30_002_813,
                current_system_id: 30_002_813,
                destination_system_id: 30_000_144,
                destination_station_id: 60_003_766,
                cargo_type_id: 22989,
                cargo_quantity: 200,
                lod_level: 1,
                destroyed: false,
            },
        );

        Self {
            convoys: RwLock::new(convoys),
        }
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

    /// Promotes all active convoys in `solar_system_id` to `LOD 2` (full tactical grid `Ball`s).
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
                if c.current_system_id == solar_system_id && !c.destroyed {
                    c.lod_level = 2;
                    out.push(c.clone());
                }
            }
            out
        };

        let station_pos = sde
            .solar_systems
            .get(&solar_system_id)
            .and_then(|sys| sys.stations.first().map(|st| st.position))
            .unwrap_or(DVec3::ZERO);
        let grid_origin = station_pos + DVec3::new(1_500.0, 0.0, 0.0);

        let handle = systems.get_or_spawn(solar_system_id);
        let mut count = 0;
        for c in to_spawn {
            let ball = Ball {
                id: c.convoy_id,
                type_id: c.ship_type_id,
                position: grid_origin + DVec3::new(1_500.0, 200.0, 0.0),
                velocity: DVec3::new(150.0, 0.0, 0.0),
                max_velocity: 300.0,
                inertia: 3.5,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.5,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            };
            let _ = handle.spawn_ball(ball).await;
            count += 1;
        }
        count
    }

    pub fn get_convoy(&self, convoy_id: i64) -> Option<LivingWorldConvoy> {
        self.convoys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&convoy_id)
            .cloned()
    }

    /// Interdicts and destroys a Living World convoy (`E2E-17`), dropping its physical cargo into the
    /// attacker's hangar/wreck and increasing destination station ask prices by `+15%` due to supply loss.
    pub fn interdict_convoy(
        &self,
        convoy_id: i64,
        attacker_char_id: i64,
        db: &WorldRepository,
    ) -> Result<(i64, IskCents), String> {
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

        db.try_with_mut_snapshot(|snap| -> Result<(i64, IskCents), String> {
            snap.next_entity_id += 1;
            let loot_item_id = snap.next_entity_id;
            snap.items.insert(
                loot_item_id,
                InventoryItemRecord {
                    item_id: loot_item_id,
                    type_id: convoy.cargo_type_id,
                    owner_id: attacker_char_id,
                    location_id: convoy.destination_station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: convoy.cargo_quantity,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                },
            );

            // Spike destination station ask orders for `cargo_type_id` by +15% (supply shock)
            let mut new_ask_price = IskCents::from_isk(6);
            for order in snap.market_orders.values_mut() {
                if !order.is_bid
                    && order.station_id == convoy.destination_station_id
                    && order.type_id == convoy.cargo_type_id
                {
                    let spiked = ((order.price.0 as f64) * 1.15).ceil();
                    order.price = IskCents(spiked.clamp(1.0, i64::MAX as f64) as i64);
                    new_ask_price = order.price;
                }
            }
            Ok((loot_item_id, new_ask_price))
        })
    }
}
