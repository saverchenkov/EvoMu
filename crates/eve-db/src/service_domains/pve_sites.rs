use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

pub type ConstellationId = u32;

/// Canonical typed identifier for a persisted exploration site instance (`PveSiteInstance`).
/// Distinct from `DungeonInstanceId` and `DungeonTemplateId` so callers cannot mix site vs dungeon IDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExplorationSiteId(pub i64);

impl ExplorationSiteId {
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for ExplorationSiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Canonical typed identifier for a live runtime deadspace dungeon instance (`DungeonRuntimeInstance`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DungeonInstanceId(pub i64);

impl DungeonInstanceId {
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for DungeonInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Canonical typed identifier for an SDE / wiki authored dungeon template (`dungeonID`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DungeonTemplateId(pub u32);

impl DungeonTemplateId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for DungeonTemplateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Canonical typed identifier for a persisted escalation (`ExpeditionInstance`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EscalationInstanceId(pub i64);

impl EscalationInstanceId {
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for EscalationInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Single canonical expiration rule for finite-lifetime PvE sites and runtime records.
/// A `lifetime_seconds == 0` record does not expire via timer alone (e.g. escalation-linked sites
/// fall back to escalation timer checks).
pub const fn is_lifetime_expired(created_unix: u64, lifetime_seconds: u64, now_unix: u64) -> bool {
    lifetime_seconds > 0 && now_unix >= created_unix.saturating_add(lifetime_seconds)
}

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
    OnKillTagged {
        target_tag: String,
        #[serde(default)]
        required_kills: Option<u32>,
        #[serde(default)]
        type_ids: Vec<u32>,
    },
    OnKillCount {
        #[serde(default)]
        target_npc_group: Option<u8>,
        required_kills: u32,
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
    OnTimer {
        delay_secs: u64,
        #[serde(default)]
        timer_id: Option<String>,
        #[serde(default)]
        after_group_cleared: Option<u8>,
    },
    OnRoomEntered {
        #[serde(default)]
        target_room_index: Option<usize>,
    },
    OnNpcDamaged {
        #[serde(default)]
        target_npc_group: Option<u8>,
        #[serde(default)]
        target_tag: Option<String>,
        #[serde(default)]
        target_ball_id: Option<i64>,
        hull_or_shield_below_pct: u8,
        #[serde(default)]
        on_first_attack: bool,
    },
    CompoundAnd {
        clauses: Vec<DungeonTriggerKind>,
    },
    CompoundOr {
        clauses: Vec<DungeonTriggerKind>,
    },
}

impl Default for DungeonTriggerKind {
    fn default() -> Self {
        Self::OnKill {
            target_npc_group: None,
            target_ball_id: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum DungeonTriggerAction {
    #[default]
    UnlockGate,
    LockGate,
    UnlockEntryGate,
    SpawnNpcGroup(u8),
    DespawnNpcGroup(u8),
    SpawnObject {
        object_index: usize,
    },
    GrantLoot {
        items: Vec<(u32, i32)>,
        #[serde(default)]
        target_object_index: Option<usize>,
    },
    GrantReward {
        isk: i64,
        lp: i64,
        #[serde(default)]
        corp_id: u32,
        #[serde(default)]
        faction_id: u32,
        #[serde(default)]
        standing_milli: i32,
    },
    CompleteDungeon,
    FailDungeon {
        #[serde(default)]
        reason: String,
    },
    StartTimer {
        timer_id: String,
        delay_secs: u64,
    },
    SetObjective {
        objective_id: i64,
        #[serde(default)]
        objective_key: String,
        title: String,
        #[serde(default)]
        completed: bool,
    },
    StartEscalation {
        #[serde(default)]
        target_dungeon_id: Option<u32>,
        #[serde(default)]
        chance_bps: u16,
        #[serde(default)]
        target_title: Option<String>,
    },
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonTrigger {
    pub trigger_id: u32,
    pub kind: DungeonTriggerKind,
    pub action: DungeonTriggerAction,
    #[serde(default)]
    pub extra_actions: Vec<DungeonTriggerAction>,
    #[serde(default)]
    pub fired: bool,
    #[serde(default)]
    pub repeatable: bool,
    #[serde(default)]
    pub delay_secs: u64,
    #[serde(default)]
    pub armed_at_unix: Option<u64>,
    #[serde(default)]
    pub armed_by_char_id: Option<i64>,
    #[serde(default)]
    pub target_room_index: Option<usize>,
}

impl DungeonTrigger {
    /// Returns the primary action followed by every configured entry in `extra_actions`.
    pub fn all_actions(&self) -> impl Iterator<Item = &DungeonTriggerAction> {
        std::iter::once(&self.action).chain(self.extra_actions.iter())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonNpcSpawnEntry {
    pub type_id: u32,
    pub count: usize,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub relative_offsets_m: Vec<[i64; 3]>,
    #[serde(default)]
    pub bounty_isk_override: Option<i64>,
    #[serde(default)]
    pub custom_loot: Option<(u32, i64)>,
    #[serde(default)]
    pub destructible: Option<bool>,
}

impl DungeonNpcSpawnEntry {
    pub fn offset_m_at(&self, index: usize) -> Option<[f64; 3]> {
        self.relative_offsets_m
            .get(index)
            .map(|&[x, y, z]| [x as f64, y as f64, z as f64])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonGateLink {
    #[serde(default)]
    pub gate_ball_id: Option<i64>,
    #[serde(default)]
    pub gate_position: [f64; 3],
    pub target_room_index: usize,
    #[serde(default)]
    pub destination_pos: [f64; 3],
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub gate_type_id: Option<u32>,
    #[serde(default)]
    pub spawned: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DungeonObjectiveStatus {
    #[default]
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonObjective {
    pub objective_id: i64,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub room_index: Option<usize>,
    #[serde(default)]
    pub target_type_id: Option<u32>,
    #[serde(default)]
    pub target_count: u32,
    #[serde(default)]
    pub progress_count: u32,
    #[serde(default)]
    pub status: DungeonObjectiveStatus,
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
    #[serde(default)]
    pub spawn_on_trigger: bool,
    #[serde(default)]
    pub destructible: bool,
    #[serde(default)]
    pub destroyed: bool,
}

/// Per-NPC spawn metadata persisted by `npc_ball_id` so live kills and restart
/// reconciliation restore the exact NPC type, offset, and attributes without vector-index shift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonSpawnedNpcMeta {
    pub type_id: u32,
    pub offset_m: Option<[i64; 3]>,
    pub custom_loot: Option<(u32, i64)>,
    pub destructible: bool,
    pub spawn_index: usize,
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
    #[serde(default)]
    pub npc_entries: Vec<DungeonNpcSpawnEntry>,
    #[serde(default)]
    pub npc_ball_tags: BTreeMap<i64, String>,
    #[serde(default)]
    pub npc_ball_names: BTreeMap<i64, String>,
    #[serde(default)]
    pub npc_ball_bounties: BTreeMap<i64, i64>,
    #[serde(default)]
    pub npc_ball_meta: BTreeMap<i64, DungeonSpawnedNpcMeta>,
    #[serde(default)]
    pub dead_npc_ball_ids: Vec<i64>,
    #[serde(default)]
    pub kills_recorded: u32,
    #[serde(default)]
    pub repeatable: bool,
    #[serde(default)]
    pub spawn_count: u32,
    #[serde(default)]
    pub spawn_started_unix: Option<u64>,
}

impl DungeonNpcGroup {
    pub fn effective_entries(&self) -> Vec<DungeonNpcSpawnEntry> {
        if !self.npc_entries.is_empty() {
            return self
                .npc_entries
                .iter()
                .filter(|e| e.type_id > 0 && e.count > 0)
                .cloned()
                .collect();
        }
        self.npc_specs
            .iter()
            .filter(|&&(tid, cnt)| tid > 0 && cnt > 0)
            .map(|&(type_id, count)| DungeonNpcSpawnEntry {
                type_id,
                count,
                ..Default::default()
            })
            .collect()
    }

    pub fn total_npc_count(&self) -> usize {
        if !self.npc_entries.is_empty() {
            self.npc_entries
                .iter()
                .filter(|e| e.type_id > 0 && e.count > 0)
                .map(|e| e.count)
                .sum()
        } else {
            self.npc_specs
                .iter()
                .filter(|&&(tid, cnt)| tid > 0 && cnt > 0)
                .map(|&(_, c)| c)
                .sum()
        }
    }
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
    #[serde(default)]
    pub gate_links: Vec<DungeonGateLink>,
    #[serde(default)]
    pub entered: bool,
    #[serde(default)]
    pub entered_unix: Option<u64>,
    #[serde(default)]
    pub last_actor_char_id: i64,
    #[serde(default)]
    pub gate_explicitly_locked: bool,
    #[serde(default)]
    pub armed_timers: BTreeMap<String, u64>,
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
    #[serde(default)]
    pub last_actor_char_id: i64,
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
    pub failed: bool,
    #[serde(default)]
    pub despawned: bool,
    #[serde(default)]
    pub active_objective_id: i64,
    #[serde(default)]
    pub last_completed_task_id: Option<i64>,
    #[serde(default)]
    pub completed_task_ids: Vec<i64>,
    pub created_unix: u64,
    #[serde(default)]
    pub objectives: Vec<DungeonObjective>,
    #[serde(default)]
    pub completed_unix: Option<u64>,
    #[serde(default)]
    pub loot_grace_until_unix: Option<u64>,
    #[serde(default)]
    pub despawn_after_unix: Option<u64>,
    #[serde(default)]
    pub escalation_instance_id: Option<i64>,
}

impl DungeonRuntimeInstance {
    pub const fn typed_instance_id(&self) -> DungeonInstanceId {
        DungeonInstanceId(self.instance_id)
    }

    pub const fn typed_template_id(&self) -> DungeonTemplateId {
        DungeonTemplateId(self.dungeon_id)
    }

    pub fn room_vec_index_by_room_index(&self, room_index: usize) -> Option<usize> {
        self.rooms.iter().position(|r| r.room_index == room_index)
    }

    pub fn current_objective_id(&self) -> i64 {
        if self.active_objective_id > 0 {
            return self.active_objective_id;
        }
        self.objectives
            .iter()
            .find(|o| o.status == DungeonObjectiveStatus::Active)
            .or_else(|| self.objectives.last())
            .map(|o| o.objective_id)
            .unwrap_or(0)
    }
}

fn default_next_dungeon_instance_id() -> i64 {
    950_001
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HomefrontArchetypeKind {
    #[default]
    Raid,
    EmergencyAid,
    MetaliminalMeteoroid,
    DreadAssault,
    AbyssalArtifactRecovery,
    SuspiciousSignal,
    SalvageResearch,
    StabilizeRift,
    TrafficStop,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HomefrontShipPolicyKind {
    #[default]
    FivePilotStandard,
    ThreePilotFrigateDestroyer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HomefrontRoleKind {
    #[default]
    Dps,
    Logistics,
    Hauler,
    Miner,
    Hacker,
    CapBattery,
    Tackler,
    Neutralizer,
    Salvager,
    CargoScanner,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomefrontBlueprintRecord {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub archetype: HomefrontArchetypeKind,
    pub difficulty: u32,
    pub sovereign_faction_id: u32,
    pub hostile_faction_id: u32,
    pub grants_combat_standing: bool,
    pub standings_faction_id: Option<u32>,
    pub optimal_fleet_size: usize,
    pub max_payout_pilots: usize,
    pub is_exordium_system: bool,
    pub base_pilot_isk: i64,
    pub base_pilot_lp: i64,
    pub abyssal_wave_payouts_isk: Vec<i64>,
    pub time_limit_secs: u64,
    pub total_stages: u8,
    pub required_roles: Vec<HomefrontRoleKind>,
    pub dispatch_commander_type_id: TypeId,
    pub objective_structure_type_id: Option<TypeId>,
    pub support_entity_type_id: Option<TypeId>,
    pub loot_commodity_type_id: Option<TypeId>,
    pub escort_wave_type_ids: Vec<TypeId>,
    pub damage_profile: [f64; 4],
    pub ewar_profile: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomefrontShipEligibilityRecord {
    pub ship_type_id: TypeId,
    pub ship_group_id: u32,
    pub hull_class: String,
    pub role_hints: Vec<HomefrontRoleKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomefrontContributionRecord {
    pub character_id: i64,
    pub entry_sequence: u64,
    pub damage_dealt: f64,
    pub repair_hp: f64,
    pub cap_transferred: f64,
    pub ore_mined_units: u32,
    pub cargo_delivered: u32,
    pub arrays_hacked: u32,
    pub objective_actions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HomefrontProgressRecord {
    Raid {
        haulers_spawned: u8,
        haulers_destroyed: u8,
        haulers_escaped: u8,
        active_hauler_ball_id: Option<i64>,
        active_hauler_tackled: bool,
        #[serde(default)]
        active_hauler_escape_at_sec: u64,
        #[serde(default)]
        freighter_ball_id: i64,
        #[serde(default)]
        delivery_depot_ball_id: i64,
        #[serde(default)]
        dropped_crate_ball_ids: Vec<i64>,
        cargo_dropped: u8,
        cargo_delivered: u8,
        required_deliveries: u8,
    },
    EmergencyAid {
        protectee_ball_id: i64,
        protectee_hp: f64,
        protectee_max_hp: f64,
        survival_elapsed_secs: u64,
        required_survival_secs: u64,
        waves_repelled: u8,
    },
    MetaliminalMeteoroid {
        meteoroid_ball_id: i64,
        mutanite_type_id: TypeId,
        remaining_units: u32,
        total_units: u32,
        total_residue_units: u32,
        ore_mined_by_fleet: u32,
    },
    DreadAssault {
        dread_ball_id: i64,
        base_ball_id: i64,
        capacitor_gj: f64,
        capacitor_target_gj: f64,
        siege_cycles_completed: u8,
        target_siege_cycles: u8,
        in_siege_mode: bool,
        siege_remaining_secs: u64,
        enemy_base_hp: f64,
        enemy_base_max_hp: f64,
    },
    AbyssalArtifactRecovery {
        current_wave: u8,
        total_waves: u8,
        #[serde(default)]
        rock_ball_ids: Vec<i64>,
        #[serde(default)]
        wave_hostile_ball_ids: Vec<i64>,
        #[serde(default)]
        weather_kind: u8,
        #[serde(default)]
        setup_remaining_secs: u64,
        #[serde(default)]
        wave_time_remaining_secs: u64,
        #[serde(default)]
        evacuation_active: bool,
        #[serde(default)]
        basileus_spawned: bool,
        #[serde(default)]
        basileus_ball_id: Option<i64>,
        #[serde(default)]
        cache_ball_id: Option<i64>,
        mutanite_units_remaining: u32,
        wave_hostiles_remaining: u8,
        warp_bubble_active: bool,
        caches_recovered: u32,
        waves_paid: u8,
        #[serde(default)]
        waves_paid_mask: u16,
        extraction_window_secs: u64,
    },
    SuspiciousSignal {
        relay_ball_id: i64,
        array_ball_ids: Vec<i64>,
        arrays_hacked: [bool; 3],
        vulnerability_window_secs: u64,
        relay_hp: f64,
        relay_max_hp: f64,
    },
    SalvageResearch {
        #[serde(default)]
        werpost_ball_id: i64,
        #[serde(default)]
        wreck_ball_ids: Vec<i64>,
        wrecks_salvaged: u8,
        total_wrecks: u8,
        werpost_activated: bool,
    },
    StabilizeRift {
        #[serde(default)]
        array_ball_ids: Vec<i64>,
        arrays_cap_pct: [f64; 5],
        held_low_secs: u64,
        required_hold_secs: u64,
    },
    TrafficStop {
        #[serde(default)]
        station_ball_id: i64,
        #[serde(default)]
        active_hauler_ball_ids: Vec<i64>,
        #[serde(default)]
        smuggler_ball_ids: Vec<i64>,
        #[serde(default)]
        civilian_ball_ids: Vec<i64>,
        #[serde(default)]
        scanned_hauler_ball_ids: Vec<i64>,
        #[serde(default)]
        survival_elapsed_secs: u64,
        #[serde(default)]
        required_survival_secs: u64,
        smugglers_destroyed: u8,
        required_smugglers: u8,
        smugglers_escaped: u8,
        civilians_harmed: u8,
    },
}

impl Default for HomefrontProgressRecord {
    fn default() -> Self {
        Self::Raid {
            haulers_spawned: 0,
            haulers_destroyed: 0,
            haulers_escaped: 0,
            active_hauler_ball_id: None,
            active_hauler_tackled: false,
            active_hauler_escape_at_sec: 0,
            freighter_ball_id: 0,
            delivery_depot_ball_id: 0,
            dropped_crate_ball_ids: Vec::new(),
            cargo_dropped: 0,
            cargo_delivered: 0,
            required_deliveries: 4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomefrontRuntimeRecord {
    pub blueprint: HomefrontBlueprintRecord,
    pub solar_system_id: SolarSystemId,
    pub instance_id: i64,
    pub site_position: [f64; 3],
    pub pocket_position: [f64; 3],
    pub outer_beacon_ball_id: i64,
    pub outer_gate_ball_id: i64,
    pub dispatch_boss_ball_id: i64,
    pub started: bool,
    pub pocket_spawned: bool,
    pub started_at_sec: u64,
    pub expires_at_sec: u64,
    pub last_tick_sec: u64,
    pub completed: bool,
    pub failed: bool,
    pub failure_reason: Option<String>,
    pub next_entry_sequence: u64,
    pub spawned_ball_ids: Vec<i64>,
    pub entered_pilots: BTreeMap<i64, HomefrontShipEligibilityRecord>,
    pub contributions: BTreeMap<i64, HomefrontContributionRecord>,
    pub progress: HomefrontProgressRecord,
    #[serde(default)]
    pub elapsed_sim_secs: f64,
    #[serde(default)]
    pub last_sim_elapsed_secs: Option<f64>,
    #[serde(default)]
    pub sim_clock_initialized: bool,
}

impl HomefrontRuntimeRecord {
    /// Seeds `elapsed_sim_secs` from `last_tick_sec - started_at_sec` once when loading a
    /// pre-migration started SQLite row (`#1196` R14), then marks `sim_clock_initialized`.
    pub fn seed_legacy_elapsed_sim_secs(&mut self) {
        if self.started && !self.sim_clock_initialized {
            if self.elapsed_sim_secs == 0.0
                && self.last_sim_elapsed_secs.is_none()
                && self.last_tick_sec > self.started_at_sec
            {
                self.elapsed_sim_secs = self
                    .last_tick_sec
                    .saturating_sub(self.started_at_sec)
                    .min(self.blueprint.time_limit_secs) as f64;
            }
            self.sim_clock_initialized = true;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DungeonRuntimeStore {
    #[serde(default)]
    pub instances: BTreeMap<i64, DungeonRuntimeInstance>,
    #[serde(default)]
    pub homefront_runtimes: BTreeMap<i64, HomefrontRuntimeRecord>,
    #[serde(default = "default_next_dungeon_instance_id")]
    pub next_instance_id: i64,
}

impl Default for DungeonRuntimeStore {
    fn default() -> Self {
        Self {
            instances: BTreeMap::new(),
            homefront_runtimes: BTreeMap::new(),
            next_instance_id: default_next_dungeon_instance_id(),
        }
    }
}

impl DungeonRuntimeStore {
    pub fn get_instance(&self, id: DungeonInstanceId) -> Option<&DungeonRuntimeInstance> {
        self.instances.get(&id.as_i64())
    }

    pub fn get_instance_mut(
        &mut self,
        id: DungeonInstanceId,
    ) -> Option<&mut DungeonRuntimeInstance> {
        self.instances.get_mut(&id.as_i64())
    }

    pub fn insert_instance(
        &mut self,
        inst: DungeonRuntimeInstance,
    ) -> Option<DungeonRuntimeInstance> {
        self.instances.insert(inst.instance_id, inst)
    }

    pub fn remove_instance(&mut self, id: DungeonInstanceId) -> Option<DungeonRuntimeInstance> {
        self.instances.remove(&id.as_i64())
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
    pub belt_respawn_due_unix: HashMap<i64, u64>,
}
