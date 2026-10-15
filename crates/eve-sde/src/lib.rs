//! `eve-sde` — Version-agnostic EVE Online Static Data Export (SDE) registry.
//!
//! Stores item types (`invTypes`), Dogma attributes (`dgmAttributeTypes`), effects (`dgmEffects`),
//! and solar systems (`mapSolarSystems`) in fast in-memory lookup tables.

#![forbid(unsafe_code)]

use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type TypeId = u32;
pub type GroupId = u32;
pub type CategoryId = u32;
pub type AttributeId = u16;
pub type EffectId = u16;
pub type SolarSystemId = u32;

/// Well-known EVE Dogma Attribute IDs (`dgmAttributeTypes`).
pub mod attr_ids {
    use super::AttributeId;

    pub const MASS: AttributeId = 4;
    pub const CAPACITOR_NEED: AttributeId = 6;
    pub const HP_STRUCT: AttributeId = 9;
    pub const POWER_OUTPUT: AttributeId = 11;
    pub const LOW_SLOTS: AttributeId = 12;
    pub const MED_SLOTS: AttributeId = 13;
    pub const HI_SLOTS: AttributeId = 14;
    pub const POWER_LOAD: AttributeId = 15;
    pub const SPEED_FACTOR: AttributeId = 20;
    pub const MAX_VELOCITY: AttributeId = 37;
    pub const CAPACITY: AttributeId = 38;
    pub const CPU_OUTPUT: AttributeId = 48;
    pub const CPU_LOAD: AttributeId = 49;
    pub const RECHARGE_RATE: AttributeId = 55;
    pub const AGILITY: AttributeId = 70; // Inertia Modifier
    pub const SHIELD_CAPACITY: AttributeId = 263;
    pub const ARMOR_HP: AttributeId = 265;
    pub const SHIELD_RECHARGE_RATE: AttributeId = 479;
    pub const CAPACITOR_CAPACITY: AttributeId = 482;
    pub const WARP_SPEED_MULTIPLIER: AttributeId = 600;
}

/// Static definition of an item type (`invTypes` / `types.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvType {
    pub type_id: TypeId,
    pub group_id: GroupId,
    pub category_id: CategoryId,
    pub name: String,
    pub mass: f64,
    pub volume: f64,
    pub capacity: f64,
    pub radius: f64,
    pub base_price: f64,
    pub published: bool,
    pub default_attributes: HashMap<AttributeId, f64>,
}

/// Static definition of a Dogma attribute (`dgmAttributeTypes` / `dogmaAttributes.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaAttributeType {
    pub attribute_id: AttributeId,
    pub name: String,
    pub default_value: f64,
    pub high_is_good: bool,
    /// `false` means stacking penalties apply when modified via `PostMul` / `PostPercent`.
    pub stackable: bool,
}

/// Static definition of a solar system (`mapSolarSystems`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolarSystem {
    pub solar_system_id: SolarSystemId,
    pub constellation_id: u32,
    pub region_id: u32,
    pub name: String,
    pub security_status: f64,
    pub radius: f64,
    pub stations: Vec<StationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationInfo {
    pub station_id: i64,
    pub station_type_id: TypeId,
    pub name: String,
    pub position: DVec3,
}

/// Thread-safe, read-only in-memory registry of Static Data Export records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SdeRegistry {
    pub types: HashMap<TypeId, InvType>,
    pub attributes: HashMap<AttributeId, DogmaAttributeType>,
    pub solar_systems: HashMap<SolarSystemId, SolarSystem>,
}

impl SdeRegistry {
    /// Creates an in-memory SDE registry pre-populated with canonical EVE Online reference entities
    /// (Jita 4-4, Rifter frigate, 1MN Afterburner II, Overdrive Injector, core Dogma attributes).
    pub fn with_canonical_seed() -> Self {
        let mut sde = Self::default();

        // Register canonical Dogma attributes
        let attrs = [
            (attr_ids::MASS, "mass", 1.0, false, true),
            (attr_ids::MAX_VELOCITY, "maxVelocity", 0.0, true, false),
            (attr_ids::AGILITY, "agility", 1.0, false, false),
            (
                attr_ids::CAPACITOR_CAPACITY,
                "capacitorCapacity",
                0.0,
                true,
                true,
            ),
            (attr_ids::RECHARGE_RATE, "rechargeRate", 1000.0, false, true),
            (attr_ids::SHIELD_CAPACITY, "shieldCapacity", 0.0, true, true),
            (
                attr_ids::SHIELD_RECHARGE_RATE,
                "shieldRechargeRate",
                1000.0,
                false,
                true,
            ),
            (attr_ids::ARMOR_HP, "armorHP", 0.0, true, true),
            (attr_ids::HP_STRUCT, "hp", 0.0, true, true),
            (attr_ids::CPU_OUTPUT, "cpuOutput", 0.0, true, true),
            (attr_ids::POWER_OUTPUT, "powerOutput", 0.0, true, true),
            (attr_ids::SPEED_FACTOR, "speedFactor", 0.0, true, false),
            (
                attr_ids::WARP_SPEED_MULTIPLIER,
                "warpSpeedMultiplier",
                1.0,
                true,
                false,
            ),
        ];

        for (id, name, def_val, high_good, stackable) in attrs {
            sde.attributes.insert(
                id,
                DogmaAttributeType {
                    attribute_id: id,
                    name: name.to_string(),
                    default_value: def_val,
                    high_is_good: high_good,
                    stackable,
                },
            );
        }

        // Register Rifter (typeID 587)
        let mut rifter_attrs = HashMap::new();
        rifter_attrs.insert(attr_ids::MASS, 1_067_000.0);
        rifter_attrs.insert(attr_ids::MAX_VELOCITY, 365.0);
        rifter_attrs.insert(attr_ids::AGILITY, 3.45);
        rifter_attrs.insert(attr_ids::CAPACITOR_CAPACITY, 250.0);
        rifter_attrs.insert(attr_ids::RECHARGE_RATE, 125_000.0); // ms
        rifter_attrs.insert(attr_ids::SHIELD_CAPACITY, 450.0);
        rifter_attrs.insert(attr_ids::SHIELD_RECHARGE_RATE, 625_000.0); // ms
        rifter_attrs.insert(attr_ids::ARMOR_HP, 450.0);
        rifter_attrs.insert(attr_ids::HP_STRUCT, 350.0);
        rifter_attrs.insert(attr_ids::CPU_OUTPUT, 130.0);
        rifter_attrs.insert(attr_ids::POWER_OUTPUT, 41.0);
        rifter_attrs.insert(attr_ids::WARP_SPEED_MULTIPLIER, 5.0); // 5.0 AU/s

        sde.types.insert(
            587,
            InvType {
                type_id: 587,
                group_id: 25,   // Frigate
                category_id: 6, // Ship
                name: "Rifter".to_string(),
                mass: 1_067_000.0,
                volume: 27_289.0,
                capacity: 140.0,
                radius: 37.0,
                base_price: 230_000.0,
                published: true,
                default_attributes: rifter_attrs,
            },
        );

        // Register Jita (30000142) & Jita IV - Moon 4 - Caldari Navy Assembly Plant (60003760)
        sde.solar_systems.insert(
            30000142,
            SolarSystem {
                solar_system_id: 30000142,
                constellation_id: 20000020,
                region_id: 10000002, // The Forge
                name: "Jita".to_string(),
                security_status: 0.9459,
                radius: 4.389e12,
                stations: vec![StationInfo {
                    station_id: 60003760,
                    station_type_id: 1529,
                    name: "Jita IV - Moon 4 - Caldari Navy Assembly Plant".to_string(),
                    position: DVec3::new(-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0),
                }],
            },
        );

        sde
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_sde_seed_lookup() {
        let sde = SdeRegistry::with_canonical_seed();
        let rifter = sde.types.get(&587).expect("Rifter type 587 must exist");
        assert_eq!(rifter.name, "Rifter");
        assert_eq!(
            rifter.default_attributes.get(&attr_ids::MAX_VELOCITY),
            Some(&365.0)
        );

        let jita = sde
            .solar_systems
            .get(&30000142)
            .expect("Jita 30000142 must exist");
        assert_eq!(jita.name, "Jita");
        assert_eq!(jita.stations[0].station_id, 60003760);
    }
}
