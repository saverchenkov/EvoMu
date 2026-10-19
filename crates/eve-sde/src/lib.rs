//! `eve-sde` — Runtime-loaded EVE Online V24.01 Static Data Export (SDE) registry (`D-1`, `D-2`).
//!
//! Opens the on-disk SQLite SDE database (`data/evomu_sde.sqlite`) produced by `tools/sde-import`
//! from the V24.01 client's `.static` SQLite3 / FSD archives and `.fsdbinary` resources.

#![forbid(unsafe_code)]

use glam::DVec3;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub type TypeId = u32;
pub type GroupId = u32;
pub type CategoryId = u32;
pub type AttributeId = u16;
pub type EffectId = u16;
pub type SolarSystemId = u32;

/// Errors returned when opening or querying the on-disk SQLite SDE database (`D-2`).
#[derive(Debug, thiserror::Error)]
pub enum SdeError {
    #[error("SDE SQLite database not found at '{0}' (run `tools/sde-import/import_sde.py` first)")]
    MissingDatabase(String),
    #[error("SDE SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("SDE JSON deserialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SDE schema error: {0}")]
    InvalidSchema(String),
}

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
    pub const PRIMARY_ATTRIBUTE: AttributeId = 180;
    pub const SECONDARY_ATTRIBUTE: AttributeId = 181;
    pub const CHARISMA: AttributeId = 164;
    pub const INTELLIGENCE: AttributeId = 165;
    pub const MEMORY: AttributeId = 166;
    pub const PERCEPTION: AttributeId = 167;
    pub const WILLPOWER: AttributeId = 168;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdeResourceInfo {
    pub res_path: String,
    pub cdn_relative_path: String,
    pub md5: String,
    pub byte_size: u64,
    pub format: String,
    pub record_count: usize,
    pub spot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub category_id: CategoryId,
    pub name: String,
    pub published: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfo {
    pub group_id: GroupId,
    pub category_id: CategoryId,
    pub name: String,
    pub anchorable: bool,
    pub anchored: bool,
    pub fittable_non_singleton: bool,
    pub use_base_price: bool,
    pub published: bool,
}

/// Static definition of an item type (`invTypes` / `types.fsdbinary`).
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
    pub market_group_id: Option<u32>,
    #[serde(default)]
    pub race_id: Option<u32>,
    #[serde(default)]
    pub faction_id: Option<u32>,
    #[serde(default)]
    pub variation_parent_type_id: Option<TypeId>,
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
    #[serde(default)]
    pub group_id: Option<GroupId>,
    #[serde(default)]
    pub skill_type_id: Option<TypeId>,
    #[serde(default)]
    pub effect_id: Option<EffectId>,
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
    #[serde(default)]
    pub starter_skills: HashMap<TypeId, u8>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionInfo {
    pub region_id: u32,
    pub name: String,
    pub center: [f64; 3],
    pub faction_id: Option<u32>,
    pub wormhole_class_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationInfo {
    pub constellation_id: u32,
    pub region_id: u32,
    pub name: String,
    pub center: [f64; 3],
    pub faction_id: Option<u32>,
    pub wormhole_class_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CelestialInfo {
    pub celestial_id: i64,
    pub solar_system_id: SolarSystemId,
    pub type_id: TypeId,
    pub group_id: GroupId,
    pub name: String,
    pub radius: f64,
    pub position: DVec3,
    pub orbit_id: Option<i64>,
    pub orbit_index: Option<i32>,
    pub celestial_index: Option<i32>,
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
pub struct FactionInfo {
    pub faction_id: u32,
    pub name: String,
    pub corporation_id: Option<i64>,
    pub militia_corporation_id: Option<i64>,
    pub solar_system_id: Option<SolarSystemId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcCorporationInfo {
    pub corporation_id: i64,
    pub name: String,
    pub ticker_name: String,
    pub faction_id: Option<u32>,
    pub solar_system_id: Option<SolarSystemId>,
    pub station_id: Option<i64>,
    pub tax_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: i64,
    pub name: String,
    pub corporation_id: i64,
    pub station_id: Option<i64>,
    pub solar_system_id: Option<SolarSystemId>,
    pub level: u8,
    pub division_id: u32,
    pub agent_type_id: u32,
    pub is_locator: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketGroupInfo {
    pub market_group_id: u32,
    pub parent_group_id: Option<u32>,
    pub name: String,
    pub has_types: bool,
    pub icon_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanetSchematicTypeEntry {
    pub type_id: TypeId,
    pub quantity: u64,
    pub is_input: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanetSchematicInfo {
    pub schematic_id: u32,
    pub name: String,
    pub cycle_time: u64,
    pub pins: Vec<TypeId>,
    pub types: Vec<PlanetSchematicTypeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WormholeTypeInfo {
    pub type_id: TypeId,
    pub name: String,
    pub target_system_class: i32,
    pub max_stable_time_min: f64,
    pub max_stable_mass: f64,
    pub mass_regeneration: f64,
    pub max_jump_mass: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcSpawnGroupInfo {
    pub spawn_group_id: u32,
    pub faction_id: u32,
    pub ship_class: u32,
    pub group_id: GroupId,
    pub group_name: String,
    pub npc_type_ids: Vec<TypeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneGradeInfo {
    pub grade_id: u32,
    #[serde(default)]
    pub alpha_skill_caps: HashMap<TypeId, u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlueprintMaterial {
    #[serde(alias = "typeID")]
    pub type_id: TypeId,
    pub quantity: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlueprintProduct {
    #[serde(alias = "typeID")]
    pub type_id: TypeId,
    pub quantity: u64,
    #[serde(default)]
    pub probability: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlueprintSkill {
    #[serde(alias = "typeID")]
    pub type_id: TypeId,
    pub level: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlueprintActivity {
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub materials: Vec<BlueprintMaterial>,
    #[serde(default)]
    pub products: Vec<BlueprintProduct>,
    #[serde(default)]
    pub skills: Vec<BlueprintSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintInfo {
    pub blueprint_type_id: TypeId,
    pub product_type_id: TypeId,
    pub product_quantity: u32,
    pub manufacturing_time_sec: u64,
    #[serde(default)]
    pub max_production_limit: u32,
    pub research_me_time_sec: u64,
    pub research_te_time_sec: u64,
    pub copy_time_sec: u64,
    pub invention_time_sec: u64,
    pub invention_base_probability: f64,
    pub materials: Vec<BlueprintMaterial>,
    #[serde(default)]
    pub activities: HashMap<String, BlueprintActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbuffGroupModifier {
    pub attribute_id: AttributeId,
    pub group_id: GroupId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbuffSkillModifier {
    pub attribute_id: AttributeId,
    pub skill_id: TypeId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbuffCollectionInfo {
    pub dbuff_id: u32,
    pub name: String,
    pub operation: String,
    pub aggregate_mode: String,
    #[serde(default)]
    pub display_name_id: Option<u32>,
    #[serde(default)]
    pub show_output_value_in_ui: Option<String>,
    #[serde(default)]
    pub item_modifiers: Vec<AttributeId>,
    #[serde(default)]
    pub location_modifiers: Vec<AttributeId>,
    #[serde(default)]
    pub location_group_modifiers: Vec<DbuffGroupModifier>,
    #[serde(default)]
    pub location_required_skill_modifiers: Vec<DbuffSkillModifier>,
    #[serde(default)]
    pub target_attribute_ids: Vec<AttributeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FighterAbilityInfo {
    pub ability_id: u32,
    pub target_mode: String,
    pub disallow_in_high_sec: bool,
    pub disallow_in_low_sec: bool,
    pub icon_id: Option<u32>,
    pub display_name_id: Option<u32>,
    #[serde(default)]
    pub tooltip_text_id: Option<u32>,
    #[serde(default)]
    pub turret_graphic_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FighterAbilityCharges {
    pub charge_count: u32,
    pub rearm_time_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FighterAbilitySlot {
    pub slot_index: u8,
    pub ability_id: u32,
    pub cooldown_seconds: Option<f64>,
    pub charges: Option<FighterAbilityCharges>,
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
    #[serde(default)]
    pub is_courier: bool,
    #[serde(default)]
    pub lp_alpha: i64,
    #[serde(default)]
    pub lp_omega: i64,
    #[serde(default)]
    pub has_standing_rewards: bool,
    #[serde(default)]
    pub content_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonTemplate {
    pub dungeon_id: u32,
    pub name: String,
    #[serde(default)]
    pub archetype_id: Option<u32>,
    #[serde(default)]
    pub faction_id: Option<u32>,
    #[serde(default)]
    pub difficulty: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpicArcTemplate {
    pub epic_arc_id: u32,
    pub name: String,
    #[serde(default)]
    pub faction_id: Option<u32>,
    #[serde(default)]
    pub restart_interval: Option<u32>,
    #[serde(default)]
    pub missions_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeGraphTemplate {
    pub node_graph_id: u32,
    pub name: String,
    #[serde(default)]
    pub parent_graph_id: Option<u32>,
    #[serde(default)]
    pub node_count: u32,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CareerGoalTemplate {
    pub goal_id: u32,
    pub career_path: String,
    pub name: String,
    pub target_progress: u64,
    pub sp_reward: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationOperationInfo {
    pub operation_id: u32,
    pub name: String,
    pub activity_id: Option<u32>,
    pub services: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationServiceInfo {
    pub service_id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinLicenseInfo {
    pub license_type_id: TypeId,
    pub skin_id: u32,
    pub duration: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinInfo {
    pub skin_id: u32,
    pub internal_name: String,
    pub skin_material_id: u32,
    pub types: Vec<TypeId>,
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
    #[serde(default)]
    career_goals: Vec<CareerGoalTemplate>,
}

/// Thread-safe, read-only in-memory registry of V24.01 Static Data Export records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SdeRegistry {
    #[serde(default)]
    pub db_path: Option<PathBuf>,
    #[serde(default)]
    pub resources: HashMap<String, SdeResourceInfo>,
    #[serde(default)]
    pub categories: HashMap<CategoryId, CategoryInfo>,
    #[serde(default)]
    pub groups: HashMap<GroupId, GroupInfo>,
    pub types: HashMap<TypeId, InvType>,
    #[serde(default)]
    pub variations_by_parent: HashMap<TypeId, Vec<TypeId>>,
    pub attributes: HashMap<AttributeId, DogmaAttributeType>,
    pub effects: HashMap<EffectId, DogmaEffectType>,
    pub races: HashMap<u32, RaceInfo>,
    pub bloodlines: HashMap<u32, BloodlineInfo>,
    pub ancestries: HashMap<u32, AncestryInfo>,
    pub schools: HashMap<u32, SchoolInfo>,
    #[serde(default)]
    pub regions: HashMap<u32, RegionInfo>,
    #[serde(default)]
    pub constellations: HashMap<u32, ConstellationInfo>,
    pub solar_systems: HashMap<SolarSystemId, SolarSystem>,
    pub jumps: HashMap<i64, StargateJump>,
    #[serde(default)]
    pub celestial_count: usize,
    #[serde(default)]
    pub factions: HashMap<u32, FactionInfo>,
    #[serde(default)]
    pub npc_corporations: HashMap<i64, NpcCorporationInfo>,
    #[serde(default)]
    pub agents: HashMap<i64, AgentInfo>,
    #[serde(default)]
    pub market_groups: HashMap<u32, MarketGroupInfo>,
    #[serde(default)]
    pub planet_schematics: HashMap<u32, PlanetSchematicInfo>,
    #[serde(default)]
    pub wormhole_types: HashMap<TypeId, WormholeTypeInfo>,
    #[serde(default)]
    pub npc_spawn_groups: HashMap<u32, NpcSpawnGroupInfo>,
    pub clone_grades: HashMap<u32, CloneGradeInfo>,
    pub blueprints: HashMap<TypeId, BlueprintInfo>,
    pub dbuff_collections: HashMap<u32, DbuffCollectionInfo>,
    pub fighter_abilities: HashMap<u32, FighterAbilityInfo>,
    #[serde(default)]
    pub fighter_abilities_by_type: HashMap<TypeId, Vec<FighterAbilitySlot>>,
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
    #[serde(default)]
    pub station_operations: HashMap<u32, StationOperationInfo>,
    #[serde(default)]
    pub station_services: HashMap<u32, StationServiceInfo>,
    #[serde(default)]
    pub skin_licenses: HashMap<TypeId, SkinLicenseInfo>,
    #[serde(default)]
    pub skins: HashMap<u32, SkinInfo>,
}

static SDE_CACHE: OnceLock<Mutex<HashMap<(PathBuf, u64), SdeRegistry>>> = OnceLock::new();

#[derive(Deserialize)]
struct RawModifier {
    domain: Option<String>,
    func: Option<String>,
    modified_attribute_id: Option<i64>,
    modifying_attribute_id: Option<i64>,
    operator: Option<i32>,
    group_id: Option<GroupId>,
    skill_type_id: Option<TypeId>,
    effect_id: Option<i64>,
}

#[derive(Deserialize)]
struct RawMutaMapping {
    resulting_type_id: TypeId,
    applicable_type_ids: Vec<TypeId>,
}

#[derive(Deserialize)]
struct RawCloneSkillCap {
    #[serde(rename = "typeID")]
    type_id: TypeId,
    #[serde(alias = "maxTrainingLevel")]
    level: u8,
}

impl SdeRegistry {
    /// Resolves the production path to `data/evomu_sde.sqlite` (overridable via `EVOMU_SDE_PATH`).
    /// Never silently falls back to any subset or fixture DB so missing SDE fails clearly
    /// with [`SdeError::MissingDatabase`] (`D-2`).
    pub fn default_db_path() -> PathBuf {
        if let Ok(env_path) = std::env::var("EVOMU_SDE_PATH") {
            if !env_path.trim().is_empty() {
                return PathBuf::from(env_path);
            }
        }
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(repo_root) = manifest_dir.parent().and_then(Path::parent) {
            return repo_root.join("data").join("evomu_sde.sqlite");
        }
        PathBuf::from("data/evomu_sde.sqlite")
    }

    /// Returns the sorted list of variation `type_id`s whose `variation_parent_type_id` equals `parent_type_id` in $O(1)$.
    pub fn variations_of(&self, parent_type_id: TypeId) -> &[TypeId] {
        self.variations_by_parent
            .get(&parent_type_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Opens an on-disk SQLite SDE database produced by `tools/sde-import` (`D-2`).
    /// Fails clearly with [`SdeError::MissingDatabase`] if `path` does not exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SdeError> {
        let p = path.as_ref();
        let meta = std::fs::metadata(p)
            .map_err(|_| SdeError::MissingDatabase(p.display().to_string()))?;
        if !meta.is_file() {
            return Err(SdeError::MissingDatabase(p.display().to_string()));
        }
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let cache_key = (canon.clone(), meta.len());

        let cache = SDE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock() {
            if let Some(existing) = guard.get(&cache_key) {
                return Ok(existing.clone());
            }
        }

        let uri = format!("file:{}?immutable=1", canon.display());
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let schema_ver: String = conn
            .query_row(
                "SELECT value FROM sde_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| SdeError::InvalidSchema(format!("missing sde_meta.schema_version: {e}")))?;
        if schema_ver != "1" {
            return Err(SdeError::InvalidSchema(format!(
                "unsupported SDE schema_version '{schema_ver}'"
            )));
        }

        let mut resources = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT res_path, cdn_relative_path, md5, byte_size, format, record_count, spot FROM sde_resources",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SdeResourceInfo {
                    res_path: r.get(0)?,
                    cdn_relative_path: r.get(1)?,
                    md5: r.get(2)?,
                    byte_size: r.get::<_, i64>(3)? as u64,
                    format: r.get(4)?,
                    record_count: r.get::<_, i64>(5)? as usize,
                    spot: r.get(6)?,
                })
            })?;
            for r in rows {
                let info = r?;
                resources.insert(info.res_path.clone(), info);
            }
        }

        let mut categories = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT category_id, name, published FROM sde_categories")?;
            let rows = stmt.query_map([], |r| {
                Ok(CategoryInfo {
                    category_id: r.get::<_, i64>(0)? as CategoryId,
                    name: r.get(1)?,
                    published: r.get::<_, i64>(2)? != 0,
                })
            })?;
            for r in rows {
                let info = r?;
                categories.insert(info.category_id, info);
            }
        }

        let mut groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT group_id, category_id, name, anchorable, anchored, fittable_non_singleton, use_base_price, published FROM sde_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(GroupInfo {
                    group_id: r.get::<_, i64>(0)? as GroupId,
                    category_id: r.get::<_, i64>(1)? as CategoryId,
                    name: r.get(2)?,
                    anchorable: r.get::<_, i64>(3)? != 0,
                    anchored: r.get::<_, i64>(4)? != 0,
                    fittable_non_singleton: r.get::<_, i64>(5)? != 0,
                    use_base_price: r.get::<_, i64>(6)? != 0,
                    published: r.get::<_, i64>(7)? != 0,
                })
            })?;
            for r in rows {
                let info = r?;
                groups.insert(info.group_id, info);
            }
        }

        fn parse_json_col<T: serde::de::DeserializeOwned>(
            col: usize,
            s: &str,
        ) -> rusqlite::Result<T> {
            serde_json::from_str(s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        }

        let mut types = HashMap::new();
        let mut variations_by_parent: HashMap<TypeId, Vec<TypeId>> = HashMap::new();
        let mut wrecks = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_id, group_id, category_id, name, mass, volume, capacity, portion_size, base_price, radius, market_group_id, race_id, faction_id, variation_parent_type_id, wreck_type_id, published, attributes_json, effects_json FROM sde_types",
            )?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let wreck_tid: Option<i64> = r.get(14)?;
                let attrs_str: String = r.get(16)?;
                let effs_str: String = r.get(17)?;
                let raw_attrs: HashMap<String, f64> = parse_json_col(16, &attrs_str)?;
                let mut default_attributes = HashMap::with_capacity(raw_attrs.len());
                for (k, v) in raw_attrs {
                    if let Ok(aid) = k.parse::<u16>() {
                        default_attributes.insert(aid, v);
                    }
                }
                let raw_effs: Vec<u32> = parse_json_col(17, &effs_str)?;
                let effect_ids: Vec<EffectId> = raw_effs
                    .into_iter()
                    .filter_map(|eid| u16::try_from(eid).ok())
                    .collect();
                Ok((
                    InvType {
                        type_id: tid,
                        group_id: r.get::<_, i64>(1)? as GroupId,
                        category_id: r.get::<_, i64>(2)? as CategoryId,
                        name: r.get(3)?,
                        mass: r.get(4)?,
                        volume: r.get(5)?,
                        capacity: r.get(6)?,
                        portion_size: (r.get::<_, i64>(7)?.max(1)) as u64,
                        base_price: r.get(8)?,
                        radius: r.get(9)?,
                        market_group_id: r.get::<_, Option<i64>>(10)?.map(|v| v as u32),
                        race_id: r.get::<_, Option<i64>>(11)?.map(|v| v as u32),
                        faction_id: r.get::<_, Option<i64>>(12)?.map(|v| v as u32),
                        variation_parent_type_id: r.get::<_, Option<i64>>(13)?.map(|v| v as TypeId),
                        published: r.get::<_, i64>(15)? != 0,
                        effect_ids,
                        default_attributes,
                    },
                    wreck_tid.map(|w| w as TypeId),
                ))
            })?;
            for r in rows {
                let (inv_type, wreck_tid) = r?;
                if let Some(w) = wreck_tid {
                    wrecks.insert(inv_type.type_id, w);
                }
                if let Some(parent_tid) = inv_type.variation_parent_type_id {
                    variations_by_parent
                        .entry(parent_tid)
                        .or_default()
                        .push(inv_type.type_id);
                }
                types.insert(inv_type.type_id, inv_type);
            }
            for list in variations_by_parent.values_mut() {
                list.sort_unstable();
            }
        }

        let mut attributes = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT attribute_id, name, default_value, high_is_good, stackable FROM sde_dogma_attributes",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DogmaAttributeType {
                    attribute_id: r.get::<_, i64>(0)? as AttributeId,
                    name: r.get(1)?,
                    default_value: r.get(2)?,
                    high_is_good: r.get::<_, i64>(3)? != 0,
                    stackable: r.get::<_, i64>(4)? != 0,
                })
            })?;
            for r in rows {
                let attr = r?;
                attributes.insert(attr.attribute_id, attr);
            }
        }

        let mut effects = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT effect_id, name, effect_category, is_offensive, is_assistance, modifiers_json FROM sde_dogma_effects",
            )?;
            let rows = stmt.query_map([], |r| {
                let eid = r.get::<_, i64>(0)? as EffectId;
                let mods_str: String = r.get(5)?;
                let raw_mods: Vec<RawModifier> = parse_json_col(5, &mods_str)?;
                let modifiers = raw_mods
                    .into_iter()
                    .filter_map(|m| {
                        let func = m.func?;
                        let domain = m.domain.unwrap_or_else(|| "shipID".to_string());
                        let effect_id = m.effect_id.and_then(|v| u16::try_from(v).ok());
                        if func == "EffectStopper" {
                            Some(EffectModifierSpec {
                                domain,
                                func,
                                modified_attribute_id: m
                                    .modified_attribute_id
                                    .and_then(|v| u16::try_from(v).ok())
                                    .unwrap_or(0),
                                modifying_attribute_id: m
                                    .modifying_attribute_id
                                    .and_then(|v| u16::try_from(v).ok())
                                    .unwrap_or(0),
                                operator: m.operator.unwrap_or(0),
                                group_id: m.group_id,
                                skill_type_id: m.skill_type_id,
                                effect_id,
                            })
                        } else {
                            Some(EffectModifierSpec {
                                domain,
                                func,
                                modified_attribute_id: u16::try_from(m.modified_attribute_id?).ok()?,
                                modifying_attribute_id: u16::try_from(m.modifying_attribute_id?).ok()?,
                                operator: m.operator.unwrap_or(0),
                                group_id: m.group_id,
                                skill_type_id: m.skill_type_id,
                                effect_id,
                            })
                        }
                    })
                    .collect();
                Ok(DogmaEffectType {
                    effect_id: eid,
                    name: r.get(1)?,
                    effect_category: r.get::<_, i64>(2)? as u16,
                    is_offensive: r.get::<_, i64>(3)? != 0,
                    is_assistance: r.get::<_, i64>(4)? != 0,
                    modifiers,
                })
            })?;
            for r in rows {
                let eff = r?;
                effects.insert(eff.effect_id, eff);
            }
        }

        let mut type_materials = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT type_id, materials_json FROM sde_type_materials")?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let m_str: String = r.get(1)?;
                let entries: Vec<TypeMaterialEntry> = parse_json_col(1, &m_str)?;
                Ok((tid, entries))
            })?;
            for r in rows {
                let (tid, entries) = r?;
                type_materials.insert(tid, entries);
            }
        }

        let mut blueprints = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT blueprint_type_id, product_type_id, product_quantity, manufacturing_time_sec, max_production_limit, materials_json, activities_json FROM sde_blueprints",
            )?;
            let rows = stmt.query_map([], |r| {
                let bpid = r.get::<_, i64>(0)? as TypeId;
                let prod_tid = r.get::<_, i64>(1)? as TypeId;
                let prod_qty = r.get::<_, i64>(2)? as u32;
                let mfg_time = r.get::<_, i64>(3)? as u64;
                let max_prod_limit = r.get::<_, i64>(4)?.max(0) as u32;
                let mats_str: String = r.get(5)?;
                let acts_str: String = r.get(6)?;
                let raw_mats: Vec<TypeMaterialEntry> = parse_json_col(5, &mats_str)?;
                let materials = raw_mats
                    .into_iter()
                    .map(|m| BlueprintMaterial {
                        type_id: m.material_type_id,
                        quantity: m.quantity,
                    })
                    .collect();
                let activities: HashMap<String, BlueprintActivity> = parse_json_col(6, &acts_str)?;
                let me_time = activities
                    .get("research_material")
                    .map(|a| a.time)
                    .unwrap_or(0);
                let te_time = activities
                    .get("research_time")
                    .map(|a| a.time)
                    .unwrap_or(0);
                let copy_time = activities.get("copying").map(|a| a.time).unwrap_or(0);
                let inv_time = activities.get("invention").map(|a| a.time).unwrap_or(0);
                let inv_prob = activities
                    .get("invention")
                    .and_then(|a| a.products.first())
                    .and_then(|p| p.probability)
                    .unwrap_or(0.34);
                Ok(BlueprintInfo {
                    blueprint_type_id: bpid,
                    product_type_id: prod_tid,
                    product_quantity: prod_qty,
                    manufacturing_time_sec: mfg_time,
                    max_production_limit: max_prod_limit,
                    research_me_time_sec: me_time,
                    research_te_time_sec: te_time,
                    copy_time_sec: copy_time,
                    invention_time_sec: inv_time,
                    invention_base_probability: inv_prob,
                    materials,
                    activities,
                })
            })?;
            for r in rows {
                let bp = r?;
                blueprints.insert(bp.blueprint_type_id, bp);
            }
        }

        let mut regions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_regions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(RegionInfo {
                    region_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    center: [r.get(2)?, r.get(3)?, r.get(4)?],
                    faction_id: r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    wormhole_class_id: r.get::<_, Option<i64>>(6)?.map(|v| v as i32),
                })
            })?;
            for r in rows {
                let reg = r?;
                regions.insert(reg.region_id, reg);
            }
        }

        let mut constellations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT constellation_id, region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_constellations",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(ConstellationInfo {
                    constellation_id: r.get::<_, i64>(0)? as u32,
                    region_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                    center: [r.get(3)?, r.get(4)?, r.get(5)?],
                    faction_id: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    wormhole_class_id: r.get::<_, Option<i64>>(7)?.map(|v| v as i32),
                })
            })?;
            for r in rows {
                let c = r?;
                constellations.insert(c.constellation_id, c);
            }
        }

        let mut op_has_clone_bay = HashMap::new();
        let mut station_operations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT operation_id, name, activity_id, services_json FROM sde_station_operations",
            )?;
            let rows = stmt.query_map([], |r| {
                let oid = r.get::<_, i64>(0)?;
                let name: String = r.get(1)?;
                let activity_id = r.get::<_, Option<i64>>(2)?.map(|v| v as u32);
                let svcs_str: String = r.get(3)?;
                let svcs: Vec<i64> = parse_json_col(3, &svcs_str)?;
                let has_cb = svcs.contains(&10) || svcs.contains(&24);
                let info = StationOperationInfo {
                    operation_id: oid as u32,
                    name,
                    activity_id,
                    services: svcs.into_iter().map(|s| s as u32).collect(),
                };
                Ok((oid, has_cb, info))
            })?;
            for r in rows {
                let (oid, has_cb, info) = r?;
                op_has_clone_bay.insert(oid, has_cb);
                station_operations.insert(info.operation_id, info);
            }
        }

        let mut station_services = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT service_id, name FROM sde_station_services")?;
            let rows = stmt.query_map([], |r| {
                Ok(StationServiceInfo {
                    service_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                })
            })?;
            for r in rows {
                let svc = r?;
                station_services.insert(svc.service_id, svc);
            }
        }

        let mut factions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT faction_id, name, corporation_id, militia_corporation_id, solar_system_id FROM sde_factions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FactionInfo {
                    faction_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    corporation_id: r.get(2)?,
                    militia_corporation_id: r.get(3)?,
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                })
            })?;
            for r in rows {
                let f = r?;
                factions.insert(f.faction_id, f);
            }
        }

        let mut npc_corporations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT corporation_id, name, ticker_name, faction_id, solar_system_id, station_id, tax_rate FROM sde_npc_corporations",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(NpcCorporationInfo {
                    corporation_id: r.get(0)?,
                    name: r.get(1)?,
                    ticker_name: r.get(2)?,
                    faction_id: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    station_id: r.get(5)?,
                    tax_rate: r.get(6)?,
                })
            })?;
            for r in rows {
                let c = r?;
                npc_corporations.insert(c.corporation_id, c);
            }
        }

        let mut stations_by_system: HashMap<SolarSystemId, Vec<StationInfo>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT station_id, system_id, owner_id, type_id, operation_id, name, x, y, z FROM sde_stations ORDER BY station_id",
            )?;
            let rows = stmt.query_map([], |r| {
                let sta_id: i64 = r.get(0)?;
                let sys_id = r.get::<_, i64>(1)? as SolarSystemId;
                let owner_id: i64 = r.get(2)?;
                let tid = r.get::<_, i64>(3)? as TypeId;
                let op_id: i64 = r.get(4)?;
                let name: String = r.get(5)?;
                let x: f64 = r.get(6)?;
                let y: f64 = r.get(7)?;
                let z: f64 = r.get(8)?;
                let faction_id = npc_corporations
                    .get(&owner_id)
                    .and_then(|c| c.faction_id)
                    .unwrap_or(500001) as i64;
                let has_clone_bay = op_has_clone_bay.get(&op_id).copied().unwrap_or(true);
                Ok((
                    sys_id,
                    StationInfo {
                        station_id: sta_id,
                        station_type_id: tid,
                        name,
                        position: DVec3::new(x, y, z),
                        owner_corporation_id: owner_id,
                        owner_faction_id: faction_id,
                        has_clone_bay,
                    },
                ))
            })?;
            for r in rows {
                let (sys_id, sta) = r?;
                stations_by_system.entry(sys_id).or_default().push(sta);
            }
        }

        const METERS_PER_LY: f64 = 9_460_730_472_580_800.0;
        let mut solar_systems = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT system_id, constellation_id, region_id, name, security, x, y, z, radius, planet_ids_json FROM sde_solar_systems",
            )?;
            let rows = stmt.query_map([], |r| {
                let sys_id = r.get::<_, i64>(0)? as SolarSystemId;
                let x: f64 = r.get(5)?;
                let y: f64 = r.get(6)?;
                let z: f64 = r.get(7)?;
                let pl_str: String = r.get(9)?;
                let planet_ids: Vec<i64> = parse_json_col(9, &pl_str)?;
                Ok(SolarSystem {
                    solar_system_id: sys_id,
                    constellation_id: r.get::<_, i64>(1)? as u32,
                    region_id: r.get::<_, i64>(2)? as u32,
                    name: r.get(3)?,
                    security_status: r.get(4)?,
                    radius: r.get(8)?,
                    galactic_pos_ly: [x / METERS_PER_LY, y / METERS_PER_LY, z / METERS_PER_LY],
                    planet_ids,
                    stations: stations_by_system.remove(&sys_id).unwrap_or_default(),
                })
            })?;
            for r in rows {
                let sys = r?;
                solar_systems.insert(sys.solar_system_id, sys);
            }
        }

        let mut jumps = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT stargate_id, system_id, destination_gate_id, destination_system_id, x, y, z FROM sde_stargates",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(StargateJump {
                    stargate_id: r.get(0)?,
                    from_solar_system_id: r.get::<_, i64>(1)? as SolarSystemId,
                    destination_stargate_id: r.get(2)?,
                    to_solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    position: DVec3::new(r.get(4)?, r.get(5)?, r.get(6)?),
                })
            })?;
            for r in rows {
                let j = r?;
                jumps.insert(j.stargate_id, j);
            }
        }

        let celestial_count: usize = conn
            .query_row("SELECT count(*) FROM sde_celestials", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize;

        let mut races = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT race_id, name, ship_type_id, skills_json FROM sde_races")?;
            let rows = stmt.query_map([], |r| {
                let skills_json: Option<String> = r.get(3).ok();
                let starter_skills = skills_json
                    .and_then(|js| serde_json::from_str::<HashMap<String, u8>>(&js).ok())
                    .map(|m| {
                        m.into_iter()
                            .filter_map(|(k, v)| k.parse::<TypeId>().ok().map(|id| (id, v)))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(RaceInfo {
                    race_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    corvette_type_id: r.get::<_, Option<i64>>(2)?.unwrap_or(601) as TypeId,
                    starter_skills,
                })
            })?;
            for r in rows {
                let race = r?;
                races.insert(race.race_id, race);
            }
        }

        let mut bloodlines = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT bloodline_id, race_id, name, corporation_id FROM sde_bloodlines",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(BloodlineInfo {
                    bloodline_id: r.get::<_, i64>(0)? as u32,
                    race_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                    corporation_id: r.get(3)?,
                })
            })?;
            for r in rows {
                let b = r?;
                bloodlines.insert(b.bloodline_id, b);
            }
        }

        let mut ancestries = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT ancestry_id, bloodline_id, name FROM sde_ancestries")?;
            let rows = stmt.query_map([], |r| {
                Ok(AncestryInfo {
                    ancestry_id: r.get::<_, i64>(0)? as u32,
                    bloodline_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                })
            })?;
            for r in rows {
                let a = r?;
                ancestries.insert(a.ancestry_id, a);
            }
        }

        let mut schools = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT school_id, race_id, corporation_id, starting_station_id, starting_system_id, name FROM sde_schools",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SchoolInfo {
                    school_id: r.get::<_, i64>(0)? as u32,
                    race_id: r.get::<_, i64>(1)? as u32,
                    corporation_id: r.get(2)?,
                    station_id: r.get(3)?,
                    solar_system_id: r.get::<_, i64>(4)? as SolarSystemId,
                    name: r.get(5)?,
                })
            })?;
            for r in rows {
                let s = r?;
                schools.insert(s.school_id, s);
            }
        }

        let mut agents = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT agent_id, name, corporation_id, station_id, solar_system_id, level, division_id, agent_type_id, is_locator FROM sde_agents",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(AgentInfo {
                    agent_id: r.get(0)?,
                    name: r.get(1)?,
                    corporation_id: r.get(2)?,
                    station_id: r.get(3)?,
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    level: r.get::<_, i64>(5)? as u8,
                    division_id: r.get::<_, i64>(6)? as u32,
                    agent_type_id: r.get::<_, i64>(7)? as u32,
                    is_locator: r.get::<_, i64>(8)? != 0,
                })
            })?;
            for r in rows {
                let ag = r?;
                agents.insert(ag.agent_id, ag);
            }
        }

        let mut market_groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT market_group_id, parent_group_id, name, has_types, icon_id FROM sde_market_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MarketGroupInfo {
                    market_group_id: r.get::<_, i64>(0)? as u32,
                    parent_group_id: r.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    name: r.get(2)?,
                    has_types: r.get::<_, i64>(3)? != 0,
                    icon_id: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                })
            })?;
            for r in rows {
                let mg = r?;
                market_groups.insert(mg.market_group_id, mg);
            }
        }

        let mut planet_schematics = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT schematic_id, name, cycle_time, pins_json, types_json FROM sde_planet_schematics",
            )?;
            let rows = stmt.query_map([], |r| {
                let pins_str: String = r.get(3)?;
                let types_str: String = r.get(4)?;
                Ok(PlanetSchematicInfo {
                    schematic_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    cycle_time: r.get::<_, i64>(2)? as u64,
                    pins: parse_json_col(3, &pins_str)?,
                    types: parse_json_col(4, &types_str)?,
                })
            })?;
            for r in rows {
                let ps = r?;
                planet_schematics.insert(ps.schematic_id, ps);
            }
        }

        let mut wormhole_types = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_id, name, target_system_class, max_stable_time_min, max_stable_mass, mass_regeneration, max_jump_mass FROM sde_wormhole_types",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(WormholeTypeInfo {
                    type_id: r.get::<_, i64>(0)? as TypeId,
                    name: r.get(1)?,
                    target_system_class: r.get::<_, i64>(2)? as i32,
                    max_stable_time_min: r.get(3)?,
                    max_stable_mass: r.get(4)?,
                    mass_regeneration: r.get(5)?,
                    max_jump_mass: r.get(6)?,
                })
            })?;
            for r in rows {
                let wh = r?;
                wormhole_types.insert(wh.type_id, wh);
            }
        }

        let mut npc_spawn_groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT spawn_group_id, faction_id, ship_class, group_id, group_name, npc_type_ids_json FROM sde_npc_spawn_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                let tids_str: String = r.get(5)?;
                Ok(NpcSpawnGroupInfo {
                    spawn_group_id: r.get::<_, i64>(0)? as u32,
                    faction_id: r.get::<_, i64>(1)? as u32,
                    ship_class: r.get::<_, i64>(2)? as u32,
                    group_id: r.get::<_, i64>(3)? as GroupId,
                    group_name: r.get(4)?,
                    npc_type_ids: parse_json_col(5, &tids_str)?,
                })
            })?;
            for r in rows {
                let sg = r?;
                npc_spawn_groups.insert(sg.spawn_group_id, sg);
            }
        }

        let mut clone_grades = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT grade_id, skills_json FROM sde_clone_grades")?;
            let rows = stmt.query_map([], |r| {
                let gid = r.get::<_, i64>(0)? as u32;
                let sk_str: String = r.get(1)?;
                let raw_caps: Vec<RawCloneSkillCap> = parse_json_col(1, &sk_str)?;
                let alpha_skill_caps: HashMap<TypeId, u8> = raw_caps
                    .into_iter()
                    .map(|c| (c.type_id, c.level))
                    .collect();
                Ok(CloneGradeInfo {
                    grade_id: gid,
                    alpha_skill_caps,
                })
            })?;
            for r in rows {
                let cg = r?;
                clone_grades.insert(cg.grade_id, cg);
            }
        }

        let mut dbuff_collections = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT dbuff_id, operation_name, data_json FROM sde_dbuff_collections",
            )?;
            let rows = stmt.query_map([], |r| {
                let id = r.get::<_, i64>(0)? as u32;
                let op: String = r.get(1)?;
                let d_str: String = r.get(2)?;
                let val: serde_json::Value = parse_json_col(2, &d_str)?;
                let name = val
                    .get("developerDescription")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("missing developerDescription on dbuff {id}").into(),
                        )
                    })?
                    .to_string();
                let agg = val
                    .get("aggregateMode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("missing aggregateMode on dbuff {id}").into(),
                        )
                    })?
                    .to_string();
                let display_name_id = val
                    .get("displayNameID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let show_output_value_in_ui = val
                    .get("showOutputValueInUI")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let mut item_modifiers = Vec::new();
                if let Some(arr) = val.get("itemModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(aid) = item.get("dogmaAttributeID").and_then(|v| v.as_u64()) {
                            item_modifiers.push(aid as AttributeId);
                        }
                    }
                }
                let mut location_modifiers = Vec::new();
                if let Some(arr) = val.get("locationModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(aid) = item.get("dogmaAttributeID").and_then(|v| v.as_u64()) {
                            location_modifiers.push(aid as AttributeId);
                        }
                    }
                }
                let mut location_group_modifiers = Vec::new();
                if let Some(arr) = val.get("locationGroupModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let (Some(aid), Some(gid)) = (
                            item.get("dogmaAttributeID").and_then(|v| v.as_u64()),
                            item.get("groupID").and_then(|v| v.as_u64()),
                        ) {
                            location_group_modifiers.push(DbuffGroupModifier {
                                attribute_id: aid as AttributeId,
                                group_id: gid as GroupId,
                            });
                        }
                    }
                }
                let mut location_required_skill_modifiers = Vec::new();
                if let Some(arr) = val
                    .get("locationRequiredSkillModifiers")
                    .and_then(|v| v.as_array())
                {
                    for item in arr {
                        if let (Some(aid), Some(sid)) = (
                            item.get("dogmaAttributeID").and_then(|v| v.as_u64()),
                            item.get("skillID").and_then(|v| v.as_u64()),
                        ) {
                            location_required_skill_modifiers.push(DbuffSkillModifier {
                                attribute_id: aid as AttributeId,
                                skill_id: sid as TypeId,
                            });
                        }
                    }
                }
                let mut target_attrs = Vec::new();
                target_attrs.extend(item_modifiers.iter().copied());
                target_attrs.extend(location_modifiers.iter().copied());
                target_attrs.extend(location_group_modifiers.iter().map(|m| m.attribute_id));
                target_attrs.extend(
                    location_required_skill_modifiers
                        .iter()
                        .map(|m| m.attribute_id),
                );
                Ok(DbuffCollectionInfo {
                    dbuff_id: id,
                    name,
                    operation: op,
                    aggregate_mode: agg,
                    display_name_id,
                    show_output_value_in_ui,
                    item_modifiers,
                    location_modifiers,
                    location_group_modifiers,
                    location_required_skill_modifiers,
                    target_attribute_ids: target_attrs,
                })
            })?;
            for r in rows {
                let d = r?;
                dbuff_collections.insert(d.dbuff_id, d);
            }
        }

        let mut fighter_abilities = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT ability_id, data_json FROM sde_fighter_abilities")?;
            let rows = stmt.query_map([], |r| {
                let id = r.get::<_, i64>(0)? as u32;
                let d_str: String = r.get(1)?;
                let val: serde_json::Value = parse_json_col(1, &d_str)?;
                let target_mode = val
                    .get("targetMode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            format!("missing targetMode on fighter ability {id}").into(),
                        )
                    })?
                    .to_string();
                let disallow_in_high_sec = val
                    .get("disallowInHighSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let disallow_in_low_sec = val
                    .get("disallowInLowSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let icon_id = val
                    .get("iconID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let display_name_id = val
                    .get("displayNameID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let tooltip_text_id = val
                    .get("tooltipTextID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let turret_graphic_id = val
                    .get("turretGraphicID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                Ok(FighterAbilityInfo {
                    ability_id: id,
                    target_mode,
                    disallow_in_high_sec,
                    disallow_in_low_sec,
                    icon_id,
                    display_name_id,
                    tooltip_text_id,
                    turret_graphic_id,
                })
            })?;
            for r in rows {
                let fa = r?;
                fighter_abilities.insert(fa.ability_id, fa);
            }
        }

        let mut compressible_types = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT type_id, compressed_type_id FROM sde_compressible_types")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as TypeId,
                    r.get::<_, i64>(1)? as TypeId,
                ))
            })?;
            for r in rows {
                let (k, v) = r?;
                compressible_types.insert(k, v);
            }
        }

        let mut dynamic_item_attributes = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT mutaplasmid_type_id, mappings_json, attribute_ranges_json FROM sde_dynamic_item_attributes",
            )?;
            let rows = stmt.query_map([], |r| {
                let mid = r.get::<_, i64>(0)? as TypeId;
                let map_str: String = r.get(1)?;
                let rng_str: String = r.get(2)?;
                let mappings: Vec<RawMutaMapping> = parse_json_col(1, &map_str)?;
                let raw_rng: HashMap<String, [f64; 2]> = parse_json_col(2, &rng_str)?;
                let mut attribute_ranges = HashMap::new();
                for (k, v) in raw_rng {
                    if let Ok(aid) = k.parse::<u16>() {
                        attribute_ranges.insert(aid, v);
                    }
                }
                let resulting_type_id = mappings
                    .first()
                    .map(|m| m.resulting_type_id)
                    .unwrap_or(mid);
                let applicable_type_ids: Vec<TypeId> = mappings
                    .into_iter()
                    .flat_map(|m| m.applicable_type_ids)
                    .collect();
                Ok(MutaplasmidInfo {
                    mutaplasmid_type_id: mid,
                    applicable_type_ids,
                    resulting_type_id,
                    attribute_ranges,
                })
            })?;
            for r in rows {
                let m = r?;
                dynamic_item_attributes.insert(m.mutaplasmid_type_id, m);
            }
        }

        let mut space_components_by_type = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT type_id, components_json FROM sde_space_components")?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let c_str: String = r.get(1)?;
                let comps: Vec<String> = parse_json_col(1, &c_str)?;
                Ok((tid, comps))
            })?;
            for r in rows {
                let (tid, comps) = r?;
                space_components_by_type.insert(tid, comps);
            }
        }

        let mut missions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT mission_id, name, is_courier, lp_alpha, lp_omega, has_standing_rewards, content_template FROM sde_missions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MissionTemplate {
                    mission_id: r.get::<_, i64>(0)? as u32,
                    title: r.get(1)?,
                    is_courier: r.get::<_, i64>(2)? != 0,
                    lp_alpha: r.get(3)?,
                    lp_omega: r.get(4)?,
                    has_standing_rewards: r.get::<_, i64>(5)? != 0,
                    content_template: r.get(6)?,
                })
            })?;
            for r in rows {
                let m = r?;
                missions.insert(m.mission_id, m);
            }
        }

        let mut dungeons = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT dungeon_id, name, archetype_id, faction_id, difficulty FROM sde_dungeons",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DungeonTemplate {
                    dungeon_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    archetype_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    faction_id: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    difficulty: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                })
            })?;
            for r in rows {
                let d = r?;
                dungeons.insert(d.dungeon_id, d);
            }
        }

        let mut epic_arcs = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT epic_arc_id, name, faction_id, restart_interval, mission_count FROM sde_epic_arcs",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(EpicArcTemplate {
                    epic_arc_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    faction_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    restart_interval: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    missions_count: r.get::<_, i64>(4)?.max(0) as u32,
                })
            })?;
            for r in rows {
                let ea = r?;
                epic_arcs.insert(ea.epic_arc_id, ea);
            }
        }

        let mut node_graphs = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT node_graph_id, name, parent_graph_id, node_count, tags_json FROM sde_node_graphs",
            )?;
            let rows = stmt.query_map([], |r| {
                let tags_str: String = r.get(4)?;
                Ok(NodeGraphTemplate {
                    node_graph_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    parent_graph_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    node_count: r.get::<_, i64>(3)?.max(0) as u32,
                    tags: parse_json_col(4, &tags_str)?,
                })
            })?;
            for r in rows {
                let ng = r?;
                node_graphs.insert(ng.node_graph_id, ng);
            }
        }

        let mut fighter_abilities_by_type = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_id, data_json FROM sde_fighter_abilities_by_type",
            )?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let d_str: String = r.get(1)?;
                let val: serde_json::Value = parse_json_col(1, &d_str)?;
                let mut slots = Vec::new();
                if let Some(obj) = val.as_object() {
                    for (slot_idx, slot_key) in ["abilitySlot0", "abilitySlot1", "abilitySlot2"]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(slot_obj) = obj.get(slot_key).and_then(|s| s.as_object()) {
                            if let Some(aid) = slot_obj.get("abilityID").and_then(|v| v.as_u64()) {
                                let cooldown_seconds = slot_obj
                                    .get("cooldownSeconds")
                                    .and_then(|v| v.as_f64());
                                let charges = slot_obj
                                    .get("charges")
                                    .and_then(|c| c.as_object())
                                    .and_then(|c| {
                                        Some(FighterAbilityCharges {
                                            charge_count: c.get("chargeCount")?.as_u64()? as u32,
                                            rearm_time_seconds: c
                                                .get("rearmTimeSeconds")?
                                                .as_f64()?,
                                        })
                                    });
                                slots.push(FighterAbilitySlot {
                                    slot_index: slot_idx as u8,
                                    ability_id: aid as u32,
                                    cooldown_seconds,
                                    charges,
                                });
                            }
                        }
                    }
                }
                Ok((tid, slots))
            })?;
            for r in rows {
                let (tid, slots) = r?;
                fighter_abilities_by_type.insert(tid, slots);
            }
        }

        let mut skin_licenses = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT license_type_id, skin_id, duration FROM sde_skin_licenses",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SkinLicenseInfo {
                    license_type_id: r.get::<_, i64>(0)? as TypeId,
                    skin_id: r.get::<_, i64>(1)? as u32,
                    duration: r.get::<_, i64>(2)? as i32,
                })
            })?;
            for r in rows {
                let sl = r?;
                skin_licenses.insert(sl.license_type_id, sl);
            }
        }

        let mut skins = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT skin_id, internal_name, skin_material_id, types_json FROM sde_skins",
            )?;
            let rows = stmt.query_map([], |r| {
                let t_str: String = r.get(3)?;
                Ok(SkinInfo {
                    skin_id: r.get::<_, i64>(0)? as u32,
                    internal_name: r.get(1)?,
                    skin_material_id: r.get::<_, i64>(2)? as u32,
                    types: parse_json_col(3, &t_str)?,
                })
            })?;
            for r in rows {
                let sk = r?;
                skins.insert(sk.skin_id, sk);
            }
        }

        let registry = Self {
            db_path: Some(canon),
            resources,
            categories,
            groups,
            types,
            variations_by_parent,
            attributes,
            effects,
            races,
            bloodlines,
            ancestries,
            schools,
            regions,
            constellations,
            solar_systems,
            jumps,
            celestial_count,
            factions,
            npc_corporations,
            agents,
            market_groups,
            planet_schematics,
            wormhole_types,
            npc_spawn_groups,
            clone_grades,
            blueprints,
            dbuff_collections,
            fighter_abilities,
            fighter_abilities_by_type,
            wrecks,
            compressible_types,
            type_materials,
            dynamic_item_attributes,
            space_components_by_type,
            missions,
            dungeons,
            epic_arcs,
            node_graphs,
            career_goals: HashMap::new(),
            station_operations,
            station_services,
            skin_licenses,
            skins,
        };

        if let Ok(mut guard) = cache.lock() {
            guard.insert(cache_key, registry.clone());
        }

        Ok(registry)
    }

    /// Queries celestials (stars, planets, moons, asteroid belts, stargates) in `solar_system_id`
    /// from the on-disk SQLite SDE database (`sde_celestials`, `475,859` records).
    pub fn celestials_in_system(&self, solar_system_id: SolarSystemId) -> Vec<CelestialInfo> {
        let Some(ref p) = self.db_path else {
            return Vec::new();
        };
        let uri = format!("file:{}?immutable=1", p.display());
        let Ok(conn) = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        ) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT celestial_id, system_id, type_id, group_id, name, radius, x, y, z, orbit_id, orbit_index, celestial_index FROM sde_celestials WHERE system_id = ?1 ORDER BY celestial_id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([i64::from(solar_system_id)], |r| {
            Ok(CelestialInfo {
                celestial_id: r.get(0)?,
                solar_system_id: r.get::<_, i64>(1)? as SolarSystemId,
                type_id: r.get::<_, i64>(2)? as TypeId,
                group_id: r.get::<_, i64>(3)? as GroupId,
                name: r.get(4)?,
                radius: r.get(5)?,
                position: DVec3::new(r.get(6)?, r.get(7)?, r.get(8)?),
                orbit_id: r.get(9)?,
                orbit_index: r.get(10)?,
                celestial_index: r.get(11)?,
            })
        }) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Parses a JSON SDE seed string (`docs/index/sde_seed.json` schema) into an [`SdeRegistry`].
    pub fn from_json_str(json_str: &str) -> Result<Self, serde_json::Error> {
        let parsed: SdeSeedFile = serde_json::from_str(json_str)?;
        let mut variations_by_parent: HashMap<TypeId, Vec<TypeId>> = HashMap::new();
        for t in &parsed.types {
            if let Some(parent_tid) = t.variation_parent_type_id {
                variations_by_parent
                    .entry(parent_tid)
                    .or_default()
                    .push(t.type_id);
            }
        }
        for list in variations_by_parent.values_mut() {
            list.sort_unstable();
        }
        Ok(Self {
            db_path: None,
            resources: HashMap::new(),
            categories: HashMap::new(),
            groups: HashMap::new(),
            types: parsed.types.into_iter().map(|t| (t.type_id, t)).collect(),
            variations_by_parent,
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
            regions: HashMap::new(),
            constellations: HashMap::new(),
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
            celestial_count: 0,
            factions: HashMap::new(),
            npc_corporations: HashMap::new(),
            agents: HashMap::new(),
            market_groups: HashMap::new(),
            planet_schematics: HashMap::new(),
            wormhole_types: HashMap::new(),
            npc_spawn_groups: HashMap::new(),
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
            fighter_abilities_by_type: HashMap::new(),
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
            station_operations: HashMap::new(),
            station_services: HashMap::new(),
            skin_licenses: HashMap::new(),
            skins: HashMap::new(),
        })
    }

    /// Loads the runtime SQLite SDE database at [`Self::default_db_path`] (`data/evomu_sde.sqlite`,
    /// overridable via `EVOMU_SDE_PATH`), failing loudly if it is missing (`D-2`).
    pub fn with_canonical_seed() -> Self {
        let p = Self::default_db_path();
        Self::open(&p).unwrap_or_else(|e| {
            panic!(
                "Failed to open runtime SDE SQLite database at '{}': {e}",
                p.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // parity: plan:D-2, plan:S-11, plan:S-1, feat:serverops.static-data
    fn test_real_sde_sqlite_open_and_counts() {
        // 1. Missing database path must fail clearly with SdeError::MissingDatabase (D-2)
        let missing = SdeRegistry::open("/tmp/nonexistent_evomu_sde_db_999999.sqlite");
        assert!(
            matches!(missing, Err(SdeError::MissingDatabase(_))),
            "Expected MissingDatabase error on missing path, got {missing:?}"
        );

        // Verify default_db_path() resolves to data/evomu_sde.sqlite when EVOMU_SDE_PATH is unset (D-2)
        if std::env::var("EVOMU_SDE_PATH")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            assert!(
                SdeRegistry::default_db_path().ends_with("data/evomu_sde.sqlite"),
                "default_db_path() must resolve to data/evomu_sde.sqlite, got {:?}",
                SdeRegistry::default_db_path()
            );
        }

        // 2. Open the local full V24.01 SDE database (`data/evomu_sde.sqlite`) unconditionally
        // and fail loudly if missing (AGENTS.md & D-2)
        let full_path = SdeRegistry::default_db_path();
        let sde = SdeRegistry::open(&full_path).expect("data/evomu_sde.sqlite must open cleanly");
        assert_eq!(sde.resources.len(), 213);
        assert_eq!(sde.categories.len(), 48);
        assert_eq!(sde.groups.len(), 1611);
        assert_eq!(sde.types.len(), 53_288);
        assert_eq!(sde.attributes.len(), 2871);
        assert_eq!(sde.effects.len(), 3427);
        assert_eq!(sde.type_materials.len(), 9866);
        assert_eq!(sde.blueprints.len(), 5100);
        assert_eq!(sde.regions.len(), 114);
        assert_eq!(sde.constellations.len(), 1184);
        assert_eq!(sde.solar_systems.len(), 8490);
        assert!(sde.solar_systems.len() >= 5_000);
        assert_eq!(sde.jumps.len(), 13_978);
        assert_eq!(sde.celestial_count, 475_859);
        assert_eq!(sde.races.len(), 11);
        assert_eq!(sde.bloodlines.len(), 18);
        assert_eq!(sde.ancestries.len(), 43);
        assert_eq!(sde.schools.len(), 23);
        assert_eq!(sde.factions.len(), 27);
        assert_eq!(sde.npc_corporations.len(), 283);
        assert_eq!(sde.agents.len(), 10_972);
        assert_eq!(sde.market_groups.len(), 2114);
        assert_eq!(sde.planet_schematics.len(), 68);
        assert_eq!(sde.wormhole_types.len(), 132);
        assert_eq!(sde.npc_spawn_groups.len(), 392);
        assert_eq!(sde.fighter_abilities.len(), 36);
        assert_eq!(sde.dbuff_collections.len(), 277);
        assert_eq!(sde.dynamic_item_attributes.len(), 441);
        assert_eq!(sde.compressible_types.len(), 212);
        assert_eq!(sde.clone_grades.len(), 4);
        assert_eq!(sde.dungeons.len(), 5516);
        assert_eq!(sde.missions.len(), 2892);
        assert_eq!(sde.node_graphs.len(), 584);
        assert_eq!(sde.epic_arcs.len(), 21);
        assert!(!sde.variations_of(587).is_empty());

        // 3. Verify Rifter (587) attributes and reprocessing materials on real SDE
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

        // 4. Verify Jita <-> Perimeter real gate pair (50001249 <-> 50002185) and stations (D-2)
        let jita = sde
            .solar_systems
            .get(&30000142)
            .expect("Jita 30000142 must exist");
        assert_eq!(jita.name, "Jita");
        let jita_44 = jita
            .stations
            .iter()
            .find(|st| st.station_id == 60003760)
            .expect("Jita IV - Moon 4 Caldari Navy Assembly Plant (60003760) must exist");
        assert!(jita_44.has_clone_bay);

        let jita_to_perim = sde
            .jumps
            .get(&50001249)
            .expect("Jita->Perimeter stargate 50001249 must exist");
        assert_eq!(jita_to_perim.from_solar_system_id, 30000142);
        assert_eq!(jita_to_perim.to_solar_system_id, 30000144);
        assert_eq!(jita_to_perim.destination_stargate_id, 50002185);

        let perim_to_jita = sde
            .jumps
            .get(&50002185)
            .expect("Perimeter->Jita stargate 50002185 must exist");
        assert_eq!(perim_to_jita.from_solar_system_id, 30000144);
        assert_eq!(perim_to_jita.to_solar_system_id, 30000142);
        assert_eq!(perim_to_jita.destination_stargate_id, 50001249);

        let jita_celestials = sde.celestials_in_system(30000142);
        assert_eq!(
            jita_celestials.len(),
            49,
            "Expected 49 Jita celestials from sde_celestials, got {}",
            jita_celestials.len()
        );

        // 5. Verify school -> race/corp mapping and all 12 starter schools (11..=22) (D-2, S-11)
        assert_eq!(sde.schools[&11].race_id, 4);
        assert_eq!(sde.schools[&11].corporation_id, 1000166);
        assert_eq!(sde.schools[&14].race_id, 2);
        assert_eq!(sde.schools[&14].corporation_id, 1000170);
        assert_eq!(sde.schools[&17].race_id, 1);
        assert_eq!(sde.schools[&17].corporation_id, 1000167);
        assert_eq!(sde.schools[&20].race_id, 8);
        assert_eq!(sde.schools[&20].corporation_id, 1000168);

        for sid in 11..=22 {
            let school = sde
                .schools
                .get(&sid)
                .expect("Starter school 11..=22 must exist");
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

        // 6. Verify tracked data/sde_import_manifest.json table_counts match loaded sde (D-1, D-2)
        let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("data")
            .join("sde_import_manifest.json");
        let manifest_val: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).expect("read data/sde_import_manifest.json"),
        )
        .expect("parse data/sde_import_manifest.json");
        let tc = manifest_val
            .get("table_counts")
            .expect("table_counts in sde_import_manifest.json");
        assert_eq!(tc["sde_resources"].as_u64(), Some(213));
        assert_eq!(tc["sde_categories"].as_u64(), Some(48));
        assert_eq!(tc["sde_groups"].as_u64(), Some(1611));
        assert_eq!(tc["sde_types"].as_u64(), Some(53_288));
        assert_eq!(tc["sde_dogma_attributes"].as_u64(), Some(2871));
        assert_eq!(tc["sde_dogma_effects"].as_u64(), Some(3427));
        assert_eq!(tc["sde_type_materials"].as_u64(), Some(9866));
        assert_eq!(tc["sde_blueprints"].as_u64(), Some(5100));
        assert_eq!(tc["sde_regions"].as_u64(), Some(114));
        assert_eq!(tc["sde_constellations"].as_u64(), Some(1184));
        assert_eq!(tc["sde_solar_systems"].as_u64(), Some(8490));
        assert_eq!(tc["sde_stargates"].as_u64(), Some(13_978));
        assert_eq!(tc["sde_jumps"].as_u64(), Some(6989));
        assert_eq!(tc["sde_stations"].as_u64(), Some(5210));
        assert_eq!(tc["sde_celestials"].as_u64(), Some(475_859));
        assert_eq!(tc["sde_races"].as_u64(), Some(11));
        assert_eq!(tc["sde_bloodlines"].as_u64(), Some(18));
        assert_eq!(tc["sde_ancestries"].as_u64(), Some(43));
        assert_eq!(tc["sde_schools"].as_u64(), Some(23));
        assert_eq!(tc["sde_factions"].as_u64(), Some(27));
        assert_eq!(tc["sde_npc_corporations"].as_u64(), Some(283));
        assert_eq!(tc["sde_agents"].as_u64(), Some(10_972));
        assert_eq!(tc["sde_market_groups"].as_u64(), Some(2114));
        assert_eq!(tc["sde_dungeons"].as_u64(), Some(5516));
        assert_eq!(tc["sde_missions"].as_u64(), Some(2892));
        assert_eq!(tc["sde_node_graphs"].as_u64(), Some(584));

        // Provenance verification: verify md5, byte_size, and record_count in sde_resources
        let types_res = sde
            .resources
            .get("res:/staticdata/types.fsdbinary")
            .expect("types.fsdbinary must be indexed in sde_resources");
        assert_eq!(types_res.md5, "c0ee9c292aa113737029c9ec63503233");
        assert_eq!(types_res.byte_size, 9_169_272);
        assert_eq!(types_res.record_count, 53_288);
    }
}
