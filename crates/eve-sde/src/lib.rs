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
use std::sync::{Arc, Mutex, OnceLock};

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

    pub const DAMAGE: AttributeId = 3;
    pub const MASS: AttributeId = 4;
    pub const CAPACITOR_NEED: AttributeId = 6;
    pub const HP_STRUCT: AttributeId = 9;
    pub const SHIELD_CHARGE: AttributeId = 264;
    pub const ARMOR_DAMAGE: AttributeId = 266;
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
    pub const DRONE_BANDWIDTH_USED: AttributeId = 1272;
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
    pub description_id: Option<u32>,
    #[serde(default)]
    pub icon_id: Option<u32>,
    #[serde(default)]
    pub graphic_id: Option<u32>,
    #[serde(default)]
    pub sound_id: Option<u32>,
    #[serde(default)]
    pub meta_group_id: Option<u32>,
    #[serde(default)]
    pub meta_level: Option<i64>,
    #[serde(default)]
    pub tech_level: Option<u32>,
    #[serde(default)]
    pub default_effect_id: Option<EffectId>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub effect_ids: Vec<EffectId>,
    pub default_attributes: HashMap<AttributeId, f64>,
}

/// Detailed static show-info metadata for an item type (`D-1`, `feat:ui.show-info-types`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeShowInfo {
    pub type_id: TypeId,
    pub name: String,
    pub group_id: GroupId,
    pub category_id: CategoryId,
    pub mass: f64,
    pub volume: f64,
    pub capacity: f64,
    pub portion_size: u64,
    pub base_price: f64,
    pub published: bool,
    pub market_group_id: Option<u32>,
    pub race_id: Option<u32>,
    pub faction_id: Option<u32>,
    pub variation_parent_type_id: Option<TypeId>,
    pub description_id: Option<u32>,
    pub icon_id: Option<u32>,
    pub graphic_id: Option<u32>,
    pub sound_id: Option<u32>,
    pub meta_group_id: Option<u32>,
    pub meta_level: Option<i64>,
    pub tech_level: Option<u32>,
    pub description: String,
    pub variations: Vec<TypeId>,
    pub default_attributes: HashMap<AttributeId, f64>,
    pub effect_ids: Vec<EffectId>,
}

/// Static definition of a material output row from `res:/staticdata/typematerials.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeMaterialEntry {
    pub material_type_id: TypeId,
    pub quantity: u64,
}

/// Static definition of a Dogma attribute (`dgmAttributeTypes`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DogmaAttributeType {
    pub attribute_id: AttributeId,
    pub name: String,
    pub default_value: f64,
    pub high_is_good: bool,
    /// `false` means stacking penalties apply when modified via `PostMul` / `PostPercent`.
    pub stackable: bool,
    #[serde(default)]
    pub published: bool,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DogmaEffectType {
    pub effect_id: EffectId,
    pub name: String,
    pub effect_category: u16,
    pub is_offensive: bool,
    pub is_assistance: bool,
    #[serde(default)]
    pub is_warp_safe: bool,
    #[serde(default)]
    pub disallow_auto_repeat: bool,
    #[serde(default)]
    pub electronic_chance: bool,
    #[serde(default)]
    pub propulsion_chance: bool,
    #[serde(default)]
    pub range_chance: bool,
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub published: bool,
    #[serde(default)]
    pub modifiers: Vec<EffectModifierSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceInfo {
    pub race_id: u32,
    pub name: String,
    #[serde(default)]
    pub corvette_type_id: Option<TypeId>,
    #[serde(default)]
    pub starter_skills: HashMap<TypeId, u8>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloodlineInfo {
    pub bloodline_id: u32,
    pub race_id: u32,
    pub name: String,
    pub corporation_id: i64,
    #[serde(default)]
    pub ship_type_id: Option<TypeId>,
    #[serde(default)]
    pub charisma: i64,
    #[serde(default)]
    pub intelligence: i64,
    #[serde(default)]
    pub memory: i64,
    #[serde(default)]
    pub perception: i64,
    #[serde(default)]
    pub willpower: i64,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncestryInfo {
    pub ancestry_id: u32,
    pub bloodline_id: u32,
    pub name: String,
    #[serde(default)]
    pub charisma: i64,
    #[serde(default)]
    pub intelligence: i64,
    #[serde(default)]
    pub memory: i64,
    #[serde(default)]
    pub perception: i64,
    #[serde(default)]
    pub willpower: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub short_description: String,
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
    #[serde(default)]
    pub solar_system_id: SolarSystemId,
    #[serde(default)]
    pub constellation_id: u32,
    #[serde(default)]
    pub region_id: u32,
    pub station_type_id: TypeId,
    pub name: String,
    pub position: DVec3,
    #[serde(default)]
    pub owner_corporation_id: i64,
    #[serde(default)]
    pub owner_faction_id: Option<i64>,
    #[serde(default)]
    pub operation_id: u32,
    #[serde(default)]
    pub reprocessing_efficiency: f64,
    #[serde(default)]
    pub reprocessing_stations_take: f64,
    #[serde(default)]
    pub is_conquerable: bool,
    #[serde(default)]
    pub orbit_id: Option<i64>,
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
    #[serde(default)]
    pub size_factor: f64,
    #[serde(default)]
    pub description: String,
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

/// DERIVED (`sde_npc_spawn_groups`): Synthesized from V24.01 SDE Category 11 (`Entity`)
/// groups and types (`types.fsdbinary` / `groups.fsdbinary`), since server-side NPC spawn
/// tables are not distributed in client FSD static data.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FighterAbilityInfo {
    pub ability_id: u32,
    pub target_mode: String,
    pub disallow_in_high_sec: bool,
    pub disallow_in_low_sec: bool,
    pub icon_id: Option<u32>,
    pub display_name_id: Option<u32>,
    #[serde(default)]
    pub ability_name_id: Option<u32>,
    #[serde(default)]
    pub tooltip_text_id: Option<u32>,
    #[serde(default)]
    pub turret_graphic_id: Option<u32>,
    #[serde(default)]
    pub raw_json: serde_json::Value,
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

/// One row of the client's `res:/staticdata/asteroidcountbytypeidroomiddungeonid.static`:
/// `asteroid_count` asteroids of `type_id` are placed in dungeon room `room_id`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DungeonAsteroidEntry {
    pub room_id: u32,
    pub type_id: TypeId,
    pub asteroid_count: u32,
}

/// A client type list (`res:/staticdata/typelist.fsdbinary`), referenced by dogma attributes such as
/// `3255` (`compressibleItemsTypeList`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeListInfo {
    pub type_list_id: u32,
    pub name: String,
    pub included_category_ids: Vec<CategoryId>,
    pub included_group_ids: Vec<GroupId>,
    pub included_type_ids: Vec<TypeId>,
    pub excluded_category_ids: Vec<CategoryId>,
    pub excluded_group_ids: Vec<GroupId>,
    pub excluded_type_ids: Vec<TypeId>,
}

impl TypeListInfo {
    /// Returns `true` when `ty` is matched by an inclusion rule (type, group or category) and by
    /// no exclusion rule. DERIVED: the client stores only the six include/exclude ID lists; the
    /// "union of includes minus union of excludes" evaluation is inferred from those field names.
    pub fn contains(&self, ty: &InvType) -> bool {
        let included = self.included_type_ids.contains(&ty.type_id)
            || self.included_group_ids.contains(&ty.group_id)
            || self.included_category_ids.contains(&ty.category_id);
        let excluded = self.excluded_type_ids.contains(&ty.type_id)
            || self.excluded_group_ids.contains(&ty.group_id)
            || self.excluded_category_ids.contains(&ty.category_id);
        included && !excluded
    }
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
pub struct SkinMaterialInfo {
    pub skin_material_id: u32,
    #[serde(default)]
    pub display_name_id: Option<u32>,
    #[serde(default)]
    pub material_set_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinInfo {
    pub skin_id: u32,
    pub internal_name: String,
    pub skin_material_id: u32,
    pub types: Vec<TypeId>,
    #[serde(default)]
    pub allow_ccp_devs: bool,
    #[serde(default)]
    pub visible_serenity: bool,
    #[serde(default)]
    pub visible_tranquility: bool,
    #[serde(default)]
    pub is_structure_skin: bool,
    #[serde(default)]
    pub skin_description: Option<String>,
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
    #[serde(default)]
    pub stations: HashMap<i64, StationInfo>,
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
    pub type_lists: HashMap<u32, TypeListInfo>,
    #[serde(default)]
    pub dungeon_asteroids: HashMap<u32, Vec<DungeonAsteroidEntry>>,
    #[serde(default)]
    pub dungeon_gas_types: HashMap<u32, Vec<TypeId>>,
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
    pub skin_materials: HashMap<u32, SkinMaterialInfo>,
    #[serde(default)]
    pub skins: HashMap<u32, SkinInfo>,
}

/// Per-(path, size) slot. The outer map lock is held only to find/insert the slot; the slot's
/// own lock is held for the whole load, so concurrent first `open` calls for the same database
/// wait for a single load instead of each reading the SQLite file.
pub(crate) type SdeCacheSlot = Arc<Mutex<Option<SdeRegistry>>>;
pub(crate) static SDE_CACHE: OnceLock<Mutex<HashMap<(PathBuf, u64), SdeCacheSlot>>> =
    OnceLock::new();

#[derive(Deserialize)]
pub(crate) struct RawModifier {
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
pub(crate) struct RawMutaMapping {
    resulting_type_id: TypeId,
    applicable_type_ids: Vec<TypeId>,
}

#[derive(Deserialize)]
pub(crate) struct RawCloneSkillCap {
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

    /// Returns all published type IDs assigned to `market_group_id` (`D-1`, `feat:market.market-groups`).
    pub fn types_in_market_group(&self, market_group_id: u32) -> Vec<TypeId> {
        let mut list: Vec<TypeId> = self
            .types
            .values()
            .filter(|t| t.published && t.market_group_id == Some(market_group_id))
            .map(|t| t.type_id)
            .collect();
        list.sort_unstable();
        list
    }

    /// Returns all published types that belong to any market group (`D-1`, `feat:market.market-groups`).
    pub fn published_market_types(&self) -> Vec<TypeId> {
        let mut list: Vec<TypeId> = self
            .types
            .values()
            .filter(|t| t.published && t.market_group_id.is_some())
            .map(|t| t.type_id)
            .collect();
        list.sort_unstable();
        list
    }

    /// Returns child market groups of `parent_group_id` (`D-1`, `feat:market.market-groups`).
    pub fn market_groups_for_parent(&self, parent_group_id: Option<u32>) -> Vec<&MarketGroupInfo> {
        let mut list: Vec<&MarketGroupInfo> = self
            .market_groups
            .values()
            .filter(|mg| mg.parent_group_id == parent_group_id)
            .collect();
        list.sort_by_key(|mg| mg.market_group_id);
        list
    }

    /// Returns show-info metadata for a type, including its SDE attributes, variations, and grouping (`D-1`, `feat:ui.show-info-types`).
    pub fn show_info_for_type(&self, type_id: TypeId) -> Option<TypeShowInfo> {
        let t = self.types.get(&type_id)?;
        let mut variations = Vec::new();
        if let Some(parent) = t.variation_parent_type_id {
            variations.push(parent);
            for &v in self.variations_of(parent) {
                if v != type_id && !variations.contains(&v) {
                    variations.push(v);
                }
            }
        } else {
            for &v in self.variations_of(type_id) {
                if !variations.contains(&v) {
                    variations.push(v);
                }
            }
        }
        variations.sort_unstable();

        Some(TypeShowInfo {
            type_id: t.type_id,
            name: t.name.clone(),
            group_id: t.group_id,
            category_id: t.category_id,
            mass: t.mass,
            volume: t.volume,
            capacity: t.capacity,
            portion_size: t.portion_size,
            base_price: t.base_price,
            published: t.published,
            market_group_id: t.market_group_id,
            race_id: t.race_id,
            faction_id: t.faction_id,
            variation_parent_type_id: t.variation_parent_type_id,
            description_id: t.description_id,
            icon_id: t.icon_id,
            graphic_id: t.graphic_id,
            sound_id: t.sound_id,
            meta_group_id: t.meta_group_id,
            meta_level: t.meta_level,
            tech_level: t.tech_level,
            description: t.description.clone(),
            variations,
            default_attributes: t.default_attributes.clone(),
            effect_ids: t.effect_ids.clone(),
        })
    }

    pub fn get_type(&self, type_id: TypeId) -> Option<&InvType> { self.types.get(&type_id) }
    pub fn group(&self, group_id: GroupId) -> Option<&GroupInfo> { self.groups.get(&group_id) }
    pub fn category(&self, category_id: CategoryId) -> Option<&CategoryInfo> { self.categories.get(&category_id) }
    pub fn dogma_attributes(&self) -> &HashMap<AttributeId, DogmaAttributeType> { &self.attributes }
    pub fn dogma_attribute(&self, attr_id: AttributeId) -> Option<&DogmaAttributeType> { self.attributes.get(&attr_id) }
    pub fn dogma_effects(&self) -> &HashMap<EffectId, DogmaEffectType> { &self.effects }
    pub fn dogma_effect(&self, effect_id: EffectId) -> Option<&DogmaEffectType> { self.effects.get(&effect_id) }
    pub fn type_materials_for(&self, type_id: TypeId) -> &[TypeMaterialEntry] {
        self.type_materials.get(&type_id).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn blueprint(&self, bp_type_id: TypeId) -> Option<&BlueprintInfo> { self.blueprints.get(&bp_type_id) }
    pub fn solar_system(&self, solar_system_id: SolarSystemId) -> Option<&SolarSystem> { self.solar_systems.get(&solar_system_id) }
    pub fn stargates(&self) -> &HashMap<i64, StargateJump> { &self.jumps }
    pub fn stargate(&self, stargate_id: i64) -> Option<&StargateJump> { self.jumps.get(&stargate_id) }
    pub fn station(&self, station_id: i64) -> Option<&StationInfo> { self.stations.get(&station_id) }
    pub fn agent(&self, agent_id: i64) -> Option<&AgentInfo> { self.agents.get(&agent_id) }
    pub fn planet_schematic(&self, schematic_id: u32) -> Option<&PlanetSchematicInfo> { self.planet_schematics.get(&schematic_id) }
    pub fn school(&self, school_id: u32) -> Option<&SchoolInfo> { self.schools.get(&school_id) }
    pub fn race(&self, race_id: u32) -> Option<&RaceInfo> { self.races.get(&race_id) }
    pub fn bloodline(&self, bloodline_id: u32) -> Option<&BloodlineInfo> { self.bloodlines.get(&bloodline_id) }
    pub fn ancestry(&self, ancestry_id: u32) -> Option<&AncestryInfo> { self.ancestries.get(&ancestry_id) }
    pub fn faction(&self, faction_id: u32) -> Option<&FactionInfo> { self.factions.get(&faction_id) }
    pub fn npc_corporation(&self, corporation_id: i64) -> Option<&NpcCorporationInfo> { self.npc_corporations.get(&corporation_id) }
}

pub mod cfsd_domain_loaders;
pub mod cfsd_flat_tables;
pub mod cfsd_nested_layouts;
pub mod cfsd_nested_layouts_ops;
pub mod cfsd_nested_tables;
pub mod sqlite_loader;
pub mod sqlite_loader_skins;
pub mod static_domain_loaders;
pub use cfsd_domain_loaders::*;
pub use cfsd_flat_tables::*;
pub use cfsd_nested_tables::*;
pub use static_domain_loaders::*;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_real_sde_sqlite_open_and_counts() {
        // parity: plan:D-2, plan:S-11, plan:S-1, feat:serverops.static-data, sde:res:/staticdata/types.fsdbinary
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
        assert_eq!(sde.dogma_attributes().len(), 2871);
        assert_eq!(sde.dogma_effects().len(), 3427);
        assert_eq!(sde.type_materials.len(), 9866);
        assert_eq!(sde.blueprints.len(), 5100);
        assert_eq!(sde.regions.len(), 114);
        assert_eq!(sde.constellations.len(), 1184);
        assert_eq!(sde.solar_systems.len(), 8490);
        assert!(sde.solar_systems.len() >= 5_000);
        assert_eq!(sde.stations.len(), 5210);
        assert_eq!(sde.stargates().len(), 13_978);
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
        assert_eq!(sde.type_lists.len(), 469);
        assert_eq!(sde.dungeon_asteroids.values().map(|v| v.len()).sum::<usize>(), 3054);
        assert_eq!(sde.dungeon_gas_types.values().map(|v| v.len()).sum::<usize>(), 92);
        let skod = sde.dungeon_asteroids.get(&1347).expect("dungeon 1347 asteroid layout");
        assert!(skod.contains(&DungeonAsteroidEntry { room_id: 19522, type_id: 20, asteroid_count: 4 }));
        assert!(skod.contains(&DungeonAsteroidEntry { room_id: 19522, type_id: 1227, asteroid_count: 7 }));
        let tl334 = sde.type_lists.get(&334).expect("type list 334");
        assert!(tl334.contains(sde.get_type(1230).unwrap()));
        assert!(!tl334.contains(sde.get_type(11396).unwrap()));
        assert_eq!(sde.clone_grades.len(), 4);
        assert_eq!(sde.dungeons.len(), 5516);
        assert_eq!(sde.missions.len(), 2892);
        assert_eq!(sde.node_graphs.len(), 584);
        assert_eq!(sde.epic_arcs.len(), 21);
        assert!(!sde.variations_of(587).is_empty());
        assert_eq!(sde.group(25).unwrap().name, "Frigate");
        assert_eq!(sde.category(6).unwrap().name, "Ship");
        assert_eq!(sde.dogma_attribute(attr_ids::MAX_VELOCITY).unwrap().attribute_id, attr_ids::MAX_VELOCITY);
        assert_eq!(sde.dogma_effect(16).unwrap().effect_id, 16);
        assert_eq!(sde.blueprint(688).unwrap().product_type_id, 638);
        assert_eq!(sde.agent(3008416).unwrap().corporation_id, 1000002);
        assert_eq!(sde.planet_schematic(65).unwrap().schematic_id, 65);
        assert_eq!(sde.race(1).unwrap().corvette_type_id, Some(601));
        assert_eq!(sde.race(16).unwrap().corvette_type_id, None);
        assert_eq!(sde.bloodline(1).unwrap().race_id, 1);
        assert_eq!(sde.ancestry(1).unwrap().bloodline_id, 5);
        assert_eq!(sde.faction(500001).unwrap().name, "Caldari State");
        assert_eq!(sde.npc_corporation(1000035).unwrap().faction_id, Some(500001));

        // 3. Verify Rifter (587) attributes, 7 type metadata fields, and reprocessing materials on real SDE
        let rifter = sde.get_type(587).expect("Rifter type 587 must exist");
        assert_eq!(rifter.name, "Rifter");
        assert_eq!(rifter.default_attributes.get(&attr_ids::MAX_VELOCITY), Some(&365.0));
        assert_eq!(rifter.portion_size, 1);
        assert_eq!(rifter.description_id, Some(93841));
        assert_eq!(rifter.icon_id, None);
        assert_eq!(rifter.graphic_id, Some(46));
        assert_eq!(rifter.sound_id, Some(20078));
        assert_eq!(rifter.meta_group_id, Some(1));
        assert_eq!(rifter.meta_level, Some(0));
        assert_eq!(rifter.tech_level, Some(1));
        let rifter_info = sde.show_info_for_type(587).expect("Rifter show_info");
        assert_eq!(rifter_info.description_id, Some(93841));
        assert_eq!(rifter_info.graphic_id, Some(46));
        assert_eq!(rifter_info.sound_id, Some(20078));
        assert_eq!(rifter_info.meta_group_id, Some(1));
        assert_eq!(rifter_info.meta_level, Some(0));
        assert_eq!(rifter_info.tech_level, Some(1));
        assert_eq!(
            sde.type_materials_for(587),
            &[
                TypeMaterialEntry { material_type_id: 34, quantity: 13333 },
                TypeMaterialEntry { material_type_id: 35, quantity: 3333 },
                TypeMaterialEntry { material_type_id: 36, quantity: 1333 },
            ]
        );

        // 4. Verify Jita <-> Perimeter real gate pair (50001249 <-> 50002185) and O(1) station lookup (D-2)
        let jita = sde.solar_system(30000142).expect("Jita 30000142 must exist");
        assert_eq!(jita.name, "Jita");
        let jita_44 = sde.station(60003760).expect("Jita IV - Moon 4 Caldari Navy Assembly Plant (60003760) must exist");
        assert!(jita_44.has_clone_bay);
        assert_eq!(jita_44.solar_system_id, 30000142);
        assert_eq!(jita_44.constellation_id, 20000020);
        assert_eq!(jita_44.region_id, 10000002);
        assert_eq!(jita_44.operation_id, 14);
        assert_eq!(jita_44.reprocessing_efficiency, 0.5);
        assert_eq!(jita_44.reprocessing_stations_take, 0.05);
        assert!(!jita_44.is_conquerable);
        assert_eq!(jita_44.orbit_id, Some(40009087));
        assert_eq!(jita_44.owner_faction_id, Some(500001));
        assert_eq!(sde.station(60014839).unwrap().owner_faction_id, None);

        let jita_to_perim = sde.stargate(50001249).expect("Jita->Perimeter stargate 50001249 must exist");
        assert_eq!(jita_to_perim.from_solar_system_id, 30000142);
        assert_eq!(jita_to_perim.to_solar_system_id, 30000144);
        assert_eq!(jita_to_perim.destination_stargate_id, 50002185);

        let perim_to_jita = sde.stargate(50002185).expect("Perimeter->Jita stargate 50002185 must exist");
        assert_eq!(perim_to_jita.from_solar_system_id, 30000144);
        assert_eq!(perim_to_jita.to_solar_system_id, 30000142);
        assert_eq!(perim_to_jita.destination_stargate_id, 50001249);

        let jita_celestials = sde.celestials_in_system(30000142);
        assert_eq!(jita_celestials.len(), 49);

        // 5. Verify school -> race/corp mapping and all 12 starter schools (11..=22) (D-2, S-11)
        assert_eq!(sde.school(11).unwrap().race_id, 4);
        assert_eq!(sde.school(11).unwrap().corporation_id, 1000166);
        assert_eq!(sde.school(14).unwrap().race_id, 2);
        assert_eq!(sde.school(14).unwrap().corporation_id, 1000170);
        assert_eq!(sde.school(17).unwrap().race_id, 1);
        assert_eq!(sde.school(17).unwrap().corporation_id, 1000167);
        assert_eq!(sde.school(20).unwrap().race_id, 8);
        assert_eq!(sde.school(20).unwrap().corporation_id, 1000168);

        for sid in 11..=22 {
            let school = sde.school(sid).expect("Starter school 11..=22 must exist");
            assert!((30_000_001..30_005_500).contains(&school.solar_system_id));
            let st = sde.station(school.station_id).expect("School station must exist in sde.stations");
            assert_eq!(st.solar_system_id, school.solar_system_id);
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
        let tc = manifest_val.get("table_counts").expect("table_counts in sde_import_manifest.json");
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

        // 7. Directly open and decode raw client res:/staticdata/types.fsdbinary and verify all 53,288 records
        let types_res = sde
            .resources
            .get("res:/staticdata/types.fsdbinary")
            .expect("types.fsdbinary must be indexed in sde_resources");
        assert_eq!(types_res.md5, "c0ee9c292aa113737029c9ec63503233");
        assert_eq!(types_res.byte_size, 9_169_272);
        assert_eq!(types_res.record_count, 53_288);
        let (raw_len, raw_types) = sde.load_raw_types_fsdbinary().expect("open raw client types.fsdbinary");
        assert_eq!(raw_len as u64, types_res.byte_size);
        assert_eq!(raw_types.len(), 53_288);
        for (&tid, rt) in &raw_types {
            let inv = sde.get_type(tid).expect("raw types.fsdbinary record in sde.types");
            assert_eq!(inv.group_id, rt.group_id);
            assert_eq!(inv.base_price, rt.base_price);
            assert_eq!(inv.mass, rt.mass);
            assert_eq!(inv.volume, rt.volume);
            assert_eq!(inv.capacity, rt.capacity);
            assert_eq!(inv.description_id, rt.description_id);
            assert_eq!(inv.icon_id, rt.icon_id);
            assert_eq!(inv.graphic_id, rt.graphic_id);
            assert_eq!(inv.sound_id, rt.sound_id);
            assert_eq!(inv.meta_group_id, rt.meta_group_id);
            assert_eq!(inv.meta_level, rt.meta_level);
            assert_eq!(inv.tech_level, rt.tech_level);
        }
        assert_eq!(raw_types[&34].icon_id, Some(22));
        assert_eq!(raw_types[&587].graphic_id, Some(46));
        assert_eq!(raw_types[&638].graphic_id, Some(43));
        assert_eq!(raw_types[&2410].meta_group_id, Some(2));
        assert_eq!(raw_types[&2410].meta_level, Some(5));
        assert_eq!(raw_types[&2410].tech_level, Some(2));

        eve_testkit::attest(&[
            "plan:D-2",
            "plan:S-11",
            "plan:S-1",
            "feat:serverops.static-data",
            "sde:res:/staticdata/types.fsdbinary",
        ]);
    }
}
