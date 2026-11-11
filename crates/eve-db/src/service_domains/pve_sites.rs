use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub type ConstellationId = u32;

// ============================================================================
// PvE Special Sites (Incursions, Homefront, Storms, Drifter Hives, Triglavian)
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomefrontSiteRecord {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub optimal_fleet_size: usize,
    pub base_pilot_isk: i64,
    pub completed: bool,
    pub reward_paid_isk: i64,
    pub boss_npc_ball_id: i64,
    #[serde(default)]
    pub boss_type_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrifterHiveRoom {
    pub room_index: u8,
    pub room_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub npc_type_ids: Vec<u32>,
    pub beacon_ball_id: i64,
    pub cleared: bool,
    #[serde(default)]
    pub destroyed_npc_ball_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrifterHiveRecord {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub element_type_id: TypeId,
    pub element_name: String,
    pub current_room: u8,
    pub total_rooms: u8,
    pub warden_npc_ball_id: i64,
    pub cleared: bool,
    #[serde(default)]
    pub rooms: Vec<DrifterHiveRoom>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriglavianSiteRecord {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub faction_owner_id: u32,
    pub base_isk: i64,
    pub survey_db_drop: i64,
    pub completed: bool,
    pub boss_npc_ball_id: i64,
    #[serde(default)]
    pub boss_type_id: u32,
    #[serde(default)]
    pub escort_ball_ids: Vec<i64>,
    #[serde(default)]
    pub escort_type_ids: Vec<u32>,
    #[serde(default)]
    pub beacon_ball_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInterferenceRecord {
    pub solar_system_id: SolarSystemId,
    pub interference_level: f64,
    pub band: u8,
    #[serde(default)]
    pub last_updated_sec: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeasonalEventSiteRecord {
    pub instance_id: i64,
    pub season_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub window_start_ft: i64,
    pub window_end_ft: i64,
    pub boss_npc_ball_id: i64,
    pub boss_type_id: u32,
    pub escort_ball_ids: Vec<i64>,
    pub reward_isk: i64,
    pub reward_points: i64,
    pub loot_type_id: u32,
    pub completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionNpcSpawn {
    pub npc_ball_id: i64,
    pub type_id: u32,
    pub offset: [f64; 3],
    #[serde(default)]
    pub destroyed: bool,
}

fn default_boss_type_id() -> u32 {
    2933
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionSiteEntry {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub tier: String,
    pub min_pilots: usize,
    pub max_optimal_pilots: usize,
    pub base_isk: i64,
    pub base_concord_lp: i64,
    pub influence_delta: f64,
    pub locked_until_low_influence: bool,
    pub completed_count: u32,
    pub boss_npc_ball_id: i64,
    #[serde(default = "default_boss_type_id")]
    pub boss_type_id: u32,
    #[serde(default)]
    pub position: [f64; 3],
    #[serde(default)]
    pub escort_spawns: Vec<IncursionNpcSpawn>,
    #[serde(default)]
    pub boss_destroyed: bool,
    #[serde(default)]
    pub killer_char_id: Option<i64>,
    #[serde(default)]
    pub killer_fleet_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionTaleRecord {
    pub tale_id: i64,
    pub constellation_id: ConstellationId,
    pub constellation_name: String,
    pub staging_solar_system_id: SolarSystemId,
    pub vanguard_solar_system_ids: Vec<SolarSystemId>,
    pub assault_solar_system_ids: Vec<SolarSystemId>,
    pub hq_solar_system_id: SolarSystemId,
    pub influence: f64,
    pub state: String,
    pub mothership_spawned: bool,
    pub mothership_defeated: bool,
    pub sites: Vec<IncursionSiteEntry>,
    #[serde(default)]
    pub lockout_until_filetime: Option<i64>,
    #[serde(default)]
    pub tale_stop_published: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionLifecycleMeta {
    #[serde(default)]
    pub spawned_at_sec: u64,
    #[serde(default)]
    pub last_site_completed_sec: u64,
    #[serde(default)]
    pub last_regen_sec: u64,
    #[serde(default)]
    pub withdraw_until_sec: Option<u64>,
    #[serde(default)]
    pub ended_at_sec: Option<u64>,
}

fn default_one_u8() -> u8 {
    1
}
fn default_three_u8() -> u8 {
    3
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionSiteWaveMeta {
    #[serde(default = "default_one_u8")]
    pub current_wave: u8,
    #[serde(default = "default_three_u8")]
    pub total_waves: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldEventTaleRecord {
    pub tale_id: i64,
    pub template_id: i64,
    pub template_class_id: String,
    pub solar_system_id: SolarSystemId,
    pub constellation_id: ConstellationId,
    pub aggressor_faction_id: i64,
    pub state: String,
    pub influence: f64,
    pub start_unix: u64,
    pub end_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionLPLogEntry {
    pub log_id: i64,
    pub character_id: i64,
    pub site_id: String,
    pub tier: String,
    pub constellation_id: ConstellationId,
    pub solar_system_id: SolarSystemId,
    pub isk_earned: i64,
    pub concord_lp_earned: i64,
    pub fleet_size: usize,
    pub fleet_scale_factor: f64,
    pub timestamp_filetime: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbyssalWeatherKind {
    #[default]
    Electrical,
    Dark,
    Exotic,
    Firestorm,
    Gamma,
}

impl AbyssalWeatherKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Electrical => "Electrical",
            Self::Dark => "Dark",
            Self::Exotic => "Exotic",
            Self::Firestorm => "Firestorm",
            Self::Gamma => "Gamma",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoamingStormRecord {
    pub storm_id: u32,
    pub weather: AbyssalWeatherKind,
    pub center_solar_system_id: SolarSystemId,
    pub strong_core_systems: Vec<SolarSystemId>,
    pub weak_periphery_systems: Vec<SolarSystemId>,
    pub step_count: u32,
    #[serde(default)]
    pub next_move_at_sec: u64,
    #[serde(default)]
    pub recent_centers: Vec<SolarSystemId>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PveSpecialSitesState {
    pub initialized: bool,
    pub incursions: HashMap<ConstellationId, IncursionTaleRecord>,
    pub incursion_lifecycle: HashMap<ConstellationId, IncursionLifecycleMeta>,
    pub incursion_site_waves: HashMap<String, IncursionSiteWaveMeta>,
    pub world_event_tales: HashMap<i64, WorldEventTaleRecord>,
    pub incursion_lp_logs: HashMap<i64, Vec<IncursionLPLogEntry>>,
    pub homefront_sites: HashMap<SolarSystemId, Vec<HomefrontSiteRecord>>,
    pub roaming_storms: Vec<RoamingStormRecord>,
    pub drifter_hives: HashMap<SolarSystemId, DrifterHiveRecord>,
    pub triglavian_sites: HashMap<SolarSystemId, Vec<TriglavianSiteRecord>>,
    pub edencom_fortress_systems: Vec<SolarSystemId>,
    pub edencom_minor_victory_systems: Vec<SolarSystemId>,
    pub triglavian_minor_victory_systems: Vec<SolarSystemId>,
    pub interference_by_system: HashMap<SolarSystemId, SystemInterferenceRecord>,
    pub seasonal_event_sites: HashMap<SolarSystemId, Vec<SeasonalEventSiteRecord>>,
    pub sim_clock_sec: u64,
    pub next_seq: i64,
}

// ============================================================================
// CRAB Beacons (decoupled from PveSpecialSitesState)
// ============================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrabBeaconPhase {
    #[default]
    Idle,
    Linking,
    Scanning,
    LootRestricted,
    SelfDestructed,
}

impl CrabBeaconPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Linking => "linking",
            Self::Scanning => "scanning",
            Self::LootRestricted => "loot_restricted",
            Self::SelfDestructed => "self_destructed",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrabBeaconRecord {
    pub beacon_id: i64,
    pub type_id: TypeId,
    pub deployer_char_id: i64,
    pub solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub deployed_at_sec: u64,
    pub phase: CrabBeaconPhase,
    pub linked_char_id: Option<i64>,
    pub linked_ship_id: Option<i64>,
    pub link_started_at_sec: u64,
    pub link_completes_at_sec: u64,
    pub scan_completes_at_sec: u64,
    pub self_destruct_at_sec: u64,
    pub waves_spawned: u8,
    pub wave_npc_ball_ids: Vec<i64>,
    pub open_container_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrabBeaconStore {
    pub beacons: HashMap<i64, CrabBeaconRecord>,
}

// ============================================================================
// Abyssal Pockets, Traces, Hazards, Boundary Ticks, Void Space
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AbyssalRoomRecord {
    pub room_index: u8,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub cache_item_id: i64,
    pub cleared: bool,
    #[serde(default)]
    pub gate_unlocked_at_sec: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AbyssalPocketSession {
    pub pocket_id: i64,
    pub character_id: i64,
    pub ship_id: i64,
    pub origin_solar_system_id: SolarSystemId,
    pub abyssal_solar_system_id: SolarSystemId,
    #[serde(default)]
    pub origin_position: [f64; 3],
    #[serde(default)]
    pub grid_offset: [f64; 3],
    pub filament_type_id: TypeId,
    pub tier: u8,
    pub weather: AbyssalWeatherKind,
    pub weather_penalty_pct: f64,
    pub weather_bonus_pct: f64,
    pub npc_difficulty_mult: f64,
    pub current_room: u8,
    pub rooms: [AbyssalRoomRecord; 3],
    pub started_at_sec: u64,
    pub collapse_at_sec: u64,
    pub completed: bool,
    pub collapsed_destroyed: bool,
    #[serde(default)]
    pub trace_ball_id: i64,
    #[serde(default)]
    pub fleet_ship_ids: Vec<i64>,
    #[serde(default)]
    pub ship_room: HashMap<i64, u8>,
    #[serde(default = "default_one_u8")]
    pub max_ships: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AbyssalTraceRecord {
    pub trace_ball_id: i64,
    pub char_id: i64,
    pub filament_type_id: TypeId,
    pub origin_solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub created_unix: u64,
    #[serde(default)]
    pub expires_at_sec: u64,
    #[serde(default)]
    pub max_ships: u8,
    #[serde(default)]
    pub entered_char_ids: Vec<i64>,
    #[serde(default)]
    pub pocket_id: Option<i64>,
    #[serde(default)]
    pub fleet_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoidSpaceSessionRecord {
    pub char_id: i64,
    pub ship_id: i64,
    pub origin_solar_system_id: SolarSystemId,
    pub origin_position: [f64; 3],
    pub target_id: i64,
    pub entered_unix: u64,
    pub expires_at_sec: u64,
    #[serde(default)]
    pub nebula_graphic_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AbyssalRuntimeStore {
    pub pockets: HashMap<i64, AbyssalPocketSession>,
    pub traces: HashMap<i64, AbyssalTraceRecord>,
    pub void_space_sessions: HashMap<i64, VoidSpaceSessionRecord>,
    pub active_clouds: HashMap<i64, TypeId>,
    pub boundary_ticks: HashMap<i64, u32>,
    pub void_space_content: HashMap<i64, i64>,
    pub deathzone_active_ships: BTreeSet<i64>,
    pub next_seq: i64,
}

// ============================================================================
// Multi-Room Deadspace Dungeons & GM Dungeon Editor
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DungeonTriggerKind {
    OnKill {
        target_npc_group: Option<u8>,
        target_ball_id: Option<i64>,
    },
    OnApproach {
        target_ball_id: Option<i64>,
        target_object_index: Option<usize>,
        anchor_pos: [f64; 3],
        radius_m: f64,
    },
    OnLoot {
        container_ball_id: Option<i64>,
        target_object_index: Option<usize>,
        item_type_id: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DungeonTriggerAction {
    UnlockGate,
    SpawnNpcGroup(u8),
    CompleteDungeon,
    CameraInterest {
        item_id: i64,
        speed_bits: u64,
        duration_bits: u64,
    },
    TriggerAmbientSound {
        sound_id: i64,
        entity_item_id: i64,
        emitter_id: i64,
    },
    TriggerAudio {
        audio_url: String,
    },
    TriggerMessage {
        message_type: i64,
        message_id: i64,
    },
    TriggerMusic {
        music_trigger_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DungeonTrigger {
    pub trigger_id: u32,
    pub kind: DungeonTriggerKind,
    pub action: DungeonTriggerAction,
    #[serde(default)]
    pub fired: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRuntimeObject {
    #[serde(default)]
    pub ball_id: i64,
    pub type_id: u32,
    pub name: String,
    pub position: [f64; 3],
    pub radius: f64,
    #[serde(default)]
    pub is_container: bool,
    #[serde(default)]
    pub loot_items: Vec<(u32, i32)>,
    #[serde(default)]
    pub spawned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonNpcGroup {
    pub group_id: u8,
    pub wave: u8,
    pub npc_specs: Vec<(u32, usize)>,
    #[serde(default)]
    pub npc_ball_ids: Vec<i64>,
    #[serde(default)]
    pub spawn_immediately: bool,
    #[serde(default)]
    pub spawned: bool,
    #[serde(default)]
    pub cleared: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRuntimeRoom {
    pub room_index: usize,
    pub room_id: i64,
    pub room_name: String,
    pub position: [f64; 3],
    pub gate_destination_pos: [f64; 3],
    pub gated: bool,
    #[serde(default)]
    pub disallow_mwd: Option<bool>,
    #[serde(default)]
    pub gate_ball_id: Option<i64>,
    #[serde(default)]
    pub gate_locked: bool,
    #[serde(default)]
    pub objects: Vec<DungeonRuntimeObject>,
    #[serde(default)]
    pub npc_groups: Vec<DungeonNpcGroup>,
    #[serde(default)]
    pub triggers: Vec<DungeonTrigger>,
    #[serde(default)]
    pub spawned: bool,
    #[serde(default)]
    pub cleared: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRuntimeInstance {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub category: String,
    pub archetype: String,
    pub solar_system_id: SolarSystemId,
    pub owner_char_id: i64,
    pub base_position: [f64; 3],
    #[serde(default)]
    pub disallow_mwd: bool,
    #[serde(default)]
    pub entry_gate_ball_id: Option<i64>,
    #[serde(default)]
    pub entry_gate_pos: Option<[f64; 3]>,
    #[serde(default)]
    pub entry_gate_locked: bool,
    #[serde(default)]
    pub current_room_index: usize,
    #[serde(default)]
    pub rooms: Vec<DungeonRuntimeRoom>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub despawned: bool,
    #[serde(default)]
    pub active_objective_id: i64,
    #[serde(default)]
    pub last_completed_task_id: Option<i64>,
    #[serde(default)]
    pub completed_task_ids: Vec<i64>,
    pub created_unix: u64,
}

fn default_next_dungeon_instance_id() -> i64 {
    950_001
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRuntimeStore {
    #[serde(default)]
    pub instances: BTreeMap<i64, DungeonRuntimeInstance>,
    #[serde(default = "default_next_dungeon_instance_id")]
    pub next_instance_id: i64,
}

impl Default for DungeonRuntimeStore {
    fn default() -> Self {
        Self {
            instances: BTreeMap::new(),
            next_instance_id: default_next_dungeon_instance_id(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRoomObject {
    pub object_id: i64,
    pub room_id: i64,
    pub type_id: u32,
    pub quantity: i64,
    pub radius: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonEditorRoom {
    pub room_id: i64,
    pub room_name: String,
    pub dungeon_id: i64,
    pub objects: Vec<DungeonRoomObject>,
    #[serde(default)]
    pub disallow_mwd: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GmActiveEditorState {
    pub dungeon_id: Option<i64>,
    pub room_id: Option<i64>,
    pub godmode: bool,
    pub selected_object_ids: Vec<i64>,
    pub spawned_ball_ids: Vec<i64>,
    pub solar_system_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonEditorStore {
    pub rooms_by_dungeon: BTreeMap<i64, Vec<DungeonEditorRoom>>,
    #[serde(default)]
    pub dungeon_disallow_mwd: BTreeMap<i64, bool>,
    pub active_editors: BTreeMap<i64, GmActiveEditorState>,
    pub next_custom_room_id: i64,
    pub next_custom_object_id: i64,
}

impl Default for DungeonEditorStore {
    fn default() -> Self {
        Self {
            rooms_by_dungeon: BTreeMap::new(),
            dungeon_disallow_mwd: BTreeMap::new(),
            active_editors: BTreeMap::new(),
            next_custom_room_id: 9_000_001,
            next_custom_object_id: 1_965_000_001,
        }
    }
}

// ============================================================================
// FOBs & Jovian Observatories
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservatorySiteState {
    pub site_id: i64,
    pub solar_system_id: u32,
    pub site_pos: [f64; 3],
    pub gate_pos: [f64; 3],
    pub gate_id: i64,
    pub world_ark_id: i64,
    pub defense_wave_ids: Vec<i64>,
    pub dungeon_instance_id: i64,
    #[serde(default)]
    pub pocket_entrants: BTreeSet<i64>,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FobObservatoryStore {
    pub observatory_sites: HashMap<i64, ObservatorySiteState>,
    pub observatory_respawn_until: HashMap<u32, u64>,
    pub fob_combatants: HashMap<i64, BTreeSet<i64>>,
    pub fob_factions: HashMap<i64, i64>,
    pub fob_respawn_until: HashMap<u32, u64>,
    pub fob_defense_triggered: BTreeSet<i64>,
}

// ============================================================================
// ESS Reserve Pulses, Phased Asteroids, Moon Fractures & Mining Waste
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct EssReservePulseState {
    pub pulses_total: u32,
    pub pulses_remaining: u32,
    pub last_pulse_sec: Option<u64>,
    #[serde(default)]
    pub reserve_linked_character_id: Option<i64>,
    #[serde(default)]
    pub reserve_link_started_tick: Option<u64>,
    #[serde(default)]
    pub link_started_unix: Option<u64>,
    #[serde(default)]
    pub unlocked_until_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhasedAsteroidFieldState {
    pub signature_id: String,
    pub solar_system_id: u32,
    pub progress: f64,
    pub beacon_spawned: bool,
    pub collapsed: bool,
    pub phased_in: bool,
    pub mobile_phase_anchor_energy: u32,
    #[serde(default)]
    pub connected_ship_ids: Vec<i64>,
    #[serde(default)]
    pub asteroid_ids: Vec<i64>,
}

impl PhasedAsteroidFieldState {
    pub fn new(signature_id: String, solar_system_id: u32) -> Self {
        Self {
            signature_id,
            solar_system_id,
            progress: 0.1,
            beacon_spawned: false,
            collapsed: false,
            phased_in: false,
            mobile_phase_anchor_energy: 0,
            connected_ship_ids: Vec::new(),
            asteroid_ids: Vec::new(),
        }
    }

    pub fn advance_fracture(&mut self, delta_progress: f64) {
        self.progress = (self.progress + delta_progress).min(1.0);
        if self.progress >= 0.5 {
            self.beacon_spawned = true;
        }
        if self.progress >= 1.0 {
            self.collapsed = true;
            self.phased_in = false;
        }
    }

    pub fn connect_industrial_command_ship(&mut self, ship_id: i64, energy: u32) -> bool {
        if self.connected_ship_ids.contains(&ship_id) {
            return self.phased_in;
        }
        if energy > 0 {
            self.connected_ship_ids.push(ship_id);
            self.mobile_phase_anchor_energy += energy;
            if self.mobile_phase_anchor_energy >= 100 && !self.collapsed {
                self.phased_in = true;
            }
        }
        self.phased_in
    }

    pub fn mining_efficiency(&self) -> f64 {
        if self.collapsed {
            0.0
        } else if self.phased_in {
            1.0
        } else {
            0.10
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MiningAndEssRuntimeStore {
    pub ess_reserve_pulses: HashMap<u32, EssReservePulseState>,
    pub phased_asteroid_fields: HashMap<u32, PhasedAsteroidFieldState>,
    pub phased_field_last_tick_unix: u64,
    pub moon_fractured_fields: Vec<(i64, u32, u64, Vec<i64>)>,
    pub mining_ledger_waste: BTreeMap<usize, i64>,
    pub metenox_last_tick_unix: u64,
}
