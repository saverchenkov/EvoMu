//! `eve-sde` — Version-agnostic EVE Online Static Data Export (SDE) registry.
//!
//! Stores item types (`invTypes`), Dogma attributes (`dgmAttributeTypes`), effects (`dgmEffects`),
//! character creation lineages (`races`, `bloodlines`, `ancestries`, `schools`), solar systems,
//! stargate jumps, clone grades, blueprints, command burst `dbuffs`, fighter abilities, wrecks,
//! ore compression mappings, mutaplasmids, space components, missions, dungeons, epic arcs,
//! AIR NPE node graphs (`473`), and AIR Career Program goals.

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

const EMBEDDED_SDE_SEED_JSON: &str = include_str!("../../../docs/index/sde_seed.json");

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
    pub const POWER: AttributeId = 30;
    pub const MAX_VELOCITY: AttributeId = 37;
    pub const CAPACITY: AttributeId = 38;
    pub const CPU_OUTPUT: AttributeId = 48;
    pub const CPU_LOAD: AttributeId = 49;
    pub const CPU: AttributeId = 50;
    pub const SPEED: AttributeId = 51;
    pub const MAX_RANGE: AttributeId = 54;
    pub const RECHARGE_RATE: AttributeId = 55;
    pub const DAMAGE_MULTIPLIER: AttributeId = 64;
    pub const SHIELD_BONUS: AttributeId = 68;
    pub const AGILITY: AttributeId = 70; // Inertia Modifier
    pub const DURATION: AttributeId = 73;
    pub const MAX_TARGET_RANGE: AttributeId = 76;
    pub const SCAN_SPEED: AttributeId = 79;
    pub const LAUNCHER_SLOTS_LEFT: AttributeId = 101;
    pub const TURRET_SLOTS_LEFT: AttributeId = 102;
    pub const WARP_SCRAMBLE_RANGE: AttributeId = 103;
    pub const WARP_SCRAMBLE_STRENGTH: AttributeId = 105;
    pub const KINETIC_DAMAGE_RESONANCE: AttributeId = 109;
    pub const THERMAL_DAMAGE_RESONANCE: AttributeId = 110;
    pub const EXPLOSIVE_DAMAGE_RESONANCE: AttributeId = 111;
    pub const EM_DAMAGE_RESONANCE: AttributeId = 113;
    pub const EM_DAMAGE: AttributeId = 114;
    pub const EXPLOSIVE_DAMAGE: AttributeId = 116;
    pub const KINETIC_DAMAGE: AttributeId = 117;
    pub const THERMAL_DAMAGE: AttributeId = 118;
    pub const FALLOFF: AttributeId = 158;
    pub const TRACKING_SPEED: AttributeId = 160;
    pub const MAX_LOCKED_TARGETS: AttributeId = 192;
    pub const SCAN_RADAR_STRENGTH: AttributeId = 208;
    pub const SCAN_LADAR_STRENGTH: AttributeId = 209;
    pub const SCAN_MAGNETOMETRIC_STRENGTH: AttributeId = 210;
    pub const SCAN_GRAVIMETRIC_STRENGTH: AttributeId = 211;
    pub const SHIELD_CAPACITY: AttributeId = 263;
    pub const ARMOR_HP: AttributeId = 265;
    pub const ARMOR_EM_DAMAGE_RESONANCE: AttributeId = 267;
    pub const ARMOR_EXPLOSIVE_DAMAGE_RESONANCE: AttributeId = 268;
    pub const ARMOR_KINETIC_DAMAGE_RESONANCE: AttributeId = 269;
    pub const ARMOR_THERMAL_DAMAGE_RESONANCE: AttributeId = 270;
    pub const SHIELD_EM_DAMAGE_RESONANCE: AttributeId = 271;
    pub const SHIELD_EXPLOSIVE_DAMAGE_RESONANCE: AttributeId = 272;
    pub const SHIELD_KINETIC_DAMAGE_RESONANCE: AttributeId = 273;
    pub const SHIELD_THERMAL_DAMAGE_RESONANCE: AttributeId = 274;
    pub const SKILL_TIME_CONSTANT: AttributeId = 275;
    pub const DRONE_CAPACITY: AttributeId = 283;
    pub const SHIELD_RECHARGE_RATE: AttributeId = 479;
    pub const ENTITY_KILL_BOUNTY: AttributeId = 481;
    pub const CAPACITOR_CAPACITY: AttributeId = 482;
    pub const SIGNATURE_RADIUS: AttributeId = 552;
    pub const SCAN_RESOLUTION: AttributeId = 564;
    pub const WARP_SPEED_MULTIPLIER: AttributeId = 600;
    pub const REPROCESSING_SKILL_TYPE: AttributeId = 790;
    pub const MASS_ADDITION: AttributeId = 796;
    pub const UPGRADE_CAPACITY: AttributeId = 1132;
    pub const RIG_SLOTS: AttributeId = 1137;
    pub const UPGRADE_COST: AttributeId = 1153;
    pub const HEAT_DISSIPATION_RATE_HI: AttributeId = 1179;
    pub const HEAT_DAMAGE: AttributeId = 1196;
    pub const HEAT_GENERATION_MULTIPLIER: AttributeId = 1211;
    pub const DRONE_BANDWIDTH: AttributeId = 1271;
    pub const CHARGE_SIZE: AttributeId = 128;
    pub const REQUIRED_SKILL_1: AttributeId = 182;
    pub const REQUIRED_SKILL_2: AttributeId = 183;
    pub const REQUIRED_SKILL_3: AttributeId = 184;
    pub const REQUIRED_SKILL_1_LEVEL: AttributeId = 277;
    pub const REQUIRED_SKILL_2_LEVEL: AttributeId = 278;
    pub const REQUIRED_SKILL_3_LEVEL: AttributeId = 279;
    pub const CAN_FIT_SHIP_GROUP_01: AttributeId = 1298;
    pub const CAN_FIT_SHIP_GROUP_02: AttributeId = 1299;
    pub const CAN_FIT_SHIP_GROUP_03: AttributeId = 1300;
    pub const CAN_FIT_SHIP_TYPE_1: AttributeId = 1302;
    pub const CAN_FIT_SHIP_TYPE_2: AttributeId = 1303;
    pub const RIG_SIZE: AttributeId = 1547;
}

const fn default_portion_size() -> u64 {
    1
}

const fn default_has_clone_bay() -> bool {
    true
}

/// Static definition of an item type (`invTypes` / `types.static`).
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
    #[serde(default = "default_portion_size")]
    pub portion_size: u64,
    #[serde(default)]
    pub effect_ids: Vec<EffectId>,
    pub default_attributes: HashMap<AttributeId, f64>,
}

/// Static definition of a material output row from `res:/staticdata/typematerials.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeMaterialEntry {
    pub material_type_id: TypeId,
    pub quantity: u64,
}

/// Static definition of a Dogma attribute (`dgmAttributeTypes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaAttributeType {
    pub attribute_id: AttributeId,
    pub name: String,
    pub default_value: f64,
    pub high_is_good: bool,
    /// `false` means stacking penalties apply when modified via `PostMul` / `PostPercent`.
    pub stackable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectModifierSpec {
    pub domain: String,
    pub func: String,
    pub modified_attribute_id: AttributeId,
    pub modifying_attribute_id: AttributeId,
    pub operator: i32,
}

/// Static definition of a Dogma effect (`dgmEffects`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DogmaEffectType {
    pub effect_id: EffectId,
    pub name: String,
    pub effect_category: u16,
    pub is_offensive: bool,
    pub is_assistance: bool,
    #[serde(default)]
    pub modifiers: Vec<EffectModifierSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceInfo {
    pub race_id: u32,
    pub name: String,
    pub corvette_type_id: TypeId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloodlineInfo {
    pub bloodline_id: u32,
    pub race_id: u32,
    pub name: String,
    pub corporation_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncestryInfo {
    pub ancestry_id: u32,
    pub bloodline_id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolInfo {
    pub school_id: u32,
    pub race_id: u32,
    pub corporation_id: i64,
    pub station_id: i64,
    pub solar_system_id: SolarSystemId,
    pub name: String,
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
    #[serde(default)]
    pub galactic_pos_ly: [f64; 3],
    #[serde(default)]
    pub planet_ids: Vec<i64>,
    pub stations: Vec<StationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationInfo {
    pub station_id: i64,
    pub station_type_id: TypeId,
    pub name: String,
    pub position: DVec3,
    #[serde(default)]
    pub owner_corporation_id: i64,
    #[serde(default)]
    pub owner_faction_id: i64,
    #[serde(default = "default_has_clone_bay")]
    pub has_clone_bay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StargateJump {
    pub stargate_id: i64,
    pub from_solar_system_id: SolarSystemId,
    pub to_solar_system_id: SolarSystemId,
    pub destination_stargate_id: i64,
    pub position: DVec3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneGradeInfo {
    pub grade_id: u32,
    pub name: String,
    pub max_skill_points: u64,
    pub training_speed_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintMaterial {
    pub type_id: TypeId,
    pub quantity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintInfo {
    pub blueprint_type_id: TypeId,
    pub product_type_id: TypeId,
    pub product_quantity: u32,
    pub manufacturing_time_sec: u64,
    pub research_me_time_sec: u64,
    pub research_te_time_sec: u64,
    pub copy_time_sec: u64,
    pub invention_time_sec: u64,
    pub invention_base_probability: f64,
    pub materials: Vec<BlueprintMaterial>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbuffCollectionInfo {
    pub dbuff_id: u32,
    pub name: String,
    pub operation: String,
    pub aggregate_mode: String,
    pub target_attribute_ids: Vec<AttributeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FighterAbilityInfo {
    pub ability_id: u32,
    pub name: String,
    pub cooldown_sec: f64,
    pub damage_multiplier: f64,
    pub charges: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutaplasmidInfo {
    pub mutaplasmid_type_id: TypeId,
    pub applicable_type_ids: Vec<TypeId>,
    pub resulting_type_id: TypeId,
    pub attribute_ranges: HashMap<AttributeId, [f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionTemplate {
    pub mission_id: u32,
    pub title: String,
    pub level: u8,
    pub mission_type: String,
    pub agent_id: i64,
    pub corporation_id: i64,
    pub isk_reward: i64,
    pub time_bonus_isk: i64,
    pub lp_reward: i64,
    pub standing_gain_pct: f64,
    pub dungeon_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonSpawnGroup {
    pub npc_type_id: TypeId,
    pub count: u32,
    pub bounty_isk: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRoom {
    pub room_id: u32,
    pub spawn_groups: Vec<DungeonSpawnGroup>,
    pub acceleration_gate_to_room: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonTemplate {
    pub dungeon_id: u32,
    pub name: String,
    pub archetype_id: u32,
    pub rooms: Vec<DungeonRoom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpicArcTemplate {
    pub epic_arc_id: u32,
    pub name: String,
    pub chapters: u32,
    pub missions_count: u32,
    pub cooldown_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeGraphStep {
    pub node_id: u32,
    pub node_type: String,
    pub next_nodes: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeGraphTemplate {
    pub node_graph_id: u32,
    pub name: String,
    pub nodes: Vec<NodeGraphStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CareerGoalTemplate {
    pub goal_id: u32,
    pub career_path: String,
    pub name: String,
    pub target_progress: u64,
    pub sp_reward: u64,
}

#[derive(Debug, Deserialize)]
struct SdeSeedFile {
    attributes: Vec<DogmaAttributeType>,
    effects: Vec<DogmaEffectType>,
    types: Vec<InvType>,
    races: Vec<RaceInfo>,
    bloodlines: Vec<BloodlineInfo>,
    ancestries: Vec<AncestryInfo>,
    schools: Vec<SchoolInfo>,
    solar_systems: Vec<SolarSystem>,
    jumps: Vec<StargateJump>,
    clone_grades: Vec<CloneGradeInfo>,
    blueprints: Vec<BlueprintInfo>,
    dbuff_collections: Vec<DbuffCollectionInfo>,
    fighter_abilities: Vec<FighterAbilityInfo>,
    wrecks: HashMap<TypeId, TypeId>,
    compressible_types: HashMap<TypeId, TypeId>,
    #[serde(default)]
    type_materials: HashMap<TypeId, Vec<TypeMaterialEntry>>,
    dynamic_item_attributes: Vec<MutaplasmidInfo>,
    space_components_by_type: HashMap<TypeId, Vec<String>>,
    missions: Vec<MissionTemplate>,
    dungeons: Vec<DungeonTemplate>,
    epic_arcs: Vec<EpicArcTemplate>,
    node_graphs: Vec<NodeGraphTemplate>,
    career_goals: Vec<CareerGoalTemplate>,
}

/// Thread-safe, read-only in-memory registry of V24.01 Static Data Export records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SdeRegistry {
    pub types: HashMap<TypeId, InvType>,
    pub attributes: HashMap<AttributeId, DogmaAttributeType>,
    pub effects: HashMap<EffectId, DogmaEffectType>,
    pub races: HashMap<u32, RaceInfo>,
    pub bloodlines: HashMap<u32, BloodlineInfo>,
    pub ancestries: HashMap<u32, AncestryInfo>,
    pub schools: HashMap<u32, SchoolInfo>,
    pub solar_systems: HashMap<SolarSystemId, SolarSystem>,
    pub jumps: HashMap<i64, StargateJump>,
    pub clone_grades: HashMap<u32, CloneGradeInfo>,
    pub blueprints: HashMap<TypeId, BlueprintInfo>,
    pub dbuff_collections: HashMap<u32, DbuffCollectionInfo>,
    pub fighter_abilities: HashMap<u32, FighterAbilityInfo>,
    pub wrecks: HashMap<TypeId, TypeId>,
    pub compressible_types: HashMap<TypeId, TypeId>,
    #[serde(default)]
    pub type_materials: HashMap<TypeId, Vec<TypeMaterialEntry>>,
    pub dynamic_item_attributes: HashMap<TypeId, MutaplasmidInfo>,
    pub space_components_by_type: HashMap<TypeId, Vec<String>>,
    pub missions: HashMap<u32, MissionTemplate>,
    pub dungeons: HashMap<u32, DungeonTemplate>,
    pub epic_arcs: HashMap<u32, EpicArcTemplate>,
    pub node_graphs: HashMap<u32, NodeGraphTemplate>,
    pub career_goals: HashMap<u32, CareerGoalTemplate>,
}

impl SdeRegistry {
    /// Parses a JSON SDE seed string (`docs/index/sde_seed.json` schema) into an [`SdeRegistry`].
    pub fn from_json_str(json_str: &str) -> Result<Self, serde_json::Error> {
        let parsed: SdeSeedFile = serde_json::from_str(json_str)?;
        Ok(Self {
            types: parsed.types.into_iter().map(|t| (t.type_id, t)).collect(),
            attributes: parsed
                .attributes
                .into_iter()
                .map(|a| (a.attribute_id, a))
                .collect(),
            effects: parsed
                .effects
                .into_iter()
                .map(|e| (e.effect_id, e))
                .collect(),
            races: parsed.races.into_iter().map(|r| (r.race_id, r)).collect(),
            bloodlines: parsed
                .bloodlines
                .into_iter()
                .map(|b| (b.bloodline_id, b))
                .collect(),
            ancestries: parsed
                .ancestries
                .into_iter()
                .map(|a| (a.ancestry_id, a))
                .collect(),
            schools: parsed
                .schools
                .into_iter()
                .map(|s| (s.school_id, s))
                .collect(),
            solar_systems: parsed
                .solar_systems
                .into_iter()
                .map(|s| (s.solar_system_id, s))
                .collect(),
            jumps: parsed
                .jumps
                .into_iter()
                .map(|j| (j.stargate_id, j))
                .collect(),
            clone_grades: parsed
                .clone_grades
                .into_iter()
                .map(|c| (c.grade_id, c))
                .collect(),
            blueprints: parsed
                .blueprints
                .into_iter()
                .map(|b| (b.blueprint_type_id, b))
                .collect(),
            dbuff_collections: parsed
                .dbuff_collections
                .into_iter()
                .map(|d| (d.dbuff_id, d))
                .collect(),
            fighter_abilities: parsed
                .fighter_abilities
                .into_iter()
                .map(|f| (f.ability_id, f))
                .collect(),
            wrecks: parsed.wrecks,
            compressible_types: parsed.compressible_types,
            type_materials: parsed.type_materials,
            dynamic_item_attributes: parsed
                .dynamic_item_attributes
                .into_iter()
                .map(|m| (m.mutaplasmid_type_id, m))
                .collect(),
            space_components_by_type: parsed.space_components_by_type,
            missions: parsed
                .missions
                .into_iter()
                .map(|m| (m.mission_id, m))
                .collect(),
            dungeons: parsed
                .dungeons
                .into_iter()
                .map(|d| (d.dungeon_id, d))
                .collect(),
            epic_arcs: parsed
                .epic_arcs
                .into_iter()
                .map(|e| (e.epic_arc_id, e))
                .collect(),
            node_graphs: parsed
                .node_graphs
                .into_iter()
                .map(|n| (n.node_graph_id, n))
                .collect(),
            career_goals: parsed
                .career_goals
                .into_iter()
                .map(|g| (g.goal_id, g))
                .collect(),
        })
    }

    /// Creates an in-memory SDE registry pre-populated from `docs/index/sde_seed.json`
    /// (`extract_staticdata.py`).
    pub fn with_canonical_seed() -> Self {
        Self::from_json_str(EMBEDDED_SDE_SEED_JSON)
            .expect("Embedded docs/index/sde_seed.json must be valid JSON")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // parity: plan:S-1
    fn test_canonical_sde_seed_lookup() {
        let sde = SdeRegistry::with_canonical_seed();
        assert_eq!(sde.types.len(), 100);
        assert_eq!(sde.attributes.len(), 95);
        assert_eq!(sde.effects.len(), 253);
        assert_eq!(sde.solar_systems.len(), 22);
        assert_eq!(sde.jumps.len(), 6);
        assert_eq!(sde.blueprints.len(), 2);
        assert_eq!(sde.missions.len(), 17);

        let rifter = sde.types.get(&587).expect("Rifter type 587 must exist");
        assert_eq!(rifter.name, "Rifter");
        assert_eq!(
            rifter.default_attributes.get(&attr_ids::MAX_VELOCITY),
            Some(&365.0)
        );
        assert_eq!(rifter.portion_size, 1);
        assert_eq!(
            sde.type_materials.get(&587),
            Some(&vec![
                TypeMaterialEntry {
                    material_type_id: 34,
                    quantity: 13333
                },
                TypeMaterialEntry {
                    material_type_id: 35,
                    quantity: 3333
                },
                TypeMaterialEntry {
                    material_type_id: 36,
                    quantity: 1333
                },
            ])
        );
        assert_eq!(
            sde.type_materials.get(&18),
            Some(&vec![
                TypeMaterialEntry {
                    material_type_id: 34,
                    quantity: 175
                },
                TypeMaterialEntry {
                    material_type_id: 36,
                    quantity: 70
                },
            ])
        );

        let jita = sde
            .solar_systems
            .get(&30000142)
            .expect("Jita 30000142 must exist");
        assert_eq!(jita.name, "Jita");
        assert_eq!(jita.stations[0].station_id, 60003760);
        assert!(jita.stations[0].has_clone_bay);

        let perimeter = sde
            .solar_systems
            .get(&30000144)
            .expect("Perimeter 30000144 must exist");
        assert!(!perimeter.stations[0].has_clone_bay);

        // Verify blueprints, compressible types, mutaplasmids, node graphs, and space components
        assert!(sde.blueprints.contains_key(&687));
        assert_eq!(sde.compressible_types.get(&1230), Some(&28432));
        assert!(sde.dynamic_item_attributes.contains_key(&47702));
        assert!(sde.node_graphs.contains_key(&473));
        assert!(sde.space_components_by_type.contains_key(&32882));
        assert!(sde.space_components_by_type.contains_key(&35840));
        let beacon = sde
            .types
            .get(&35840)
            .expect("Pharolux Cyno Beacon 35840 must exist");
        assert_eq!(beacon.category_id, 65);
        let pithi = sde
            .types
            .get(&11001)
            .expect("Guristas Pithi Invader 11001 must exist");
        assert_eq!(
            pithi.default_attributes.get(&attr_ids::ENTITY_KILL_BOUNTY),
            Some(&100_000.0)
        );
        let horkkisen = sde
            .solar_systems
            .get(&30002741)
            .expect("Horkkisen 30002741 must exist");
        assert_eq!(horkkisen.name, "Horkkisen");

        // Verify all 12 schools reference K-space solar systems (< 30_005_500) and their station_id is present in that system
        assert_eq!(sde.schools.len(), 12);
        for school in sde.schools.values() {
            assert!(
                (30_000_001..30_005_500).contains(&school.solar_system_id),
                "School {} has out-of-range solar_system_id {}",
                school.school_id,
                school.solar_system_id
            );
            let sys = sde
                .solar_systems
                .get(&school.solar_system_id)
                .expect("School solar system must exist in SDE");
            assert!(
                sys.stations.iter().any(|st| st.station_id == school.station_id),
                "School {} station {} missing from solar system {}",
                school.school_id,
                school.station_id,
                school.solar_system_id
            );
        }
    }
}
