use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ============================================================================
// PvP Filaments / Proving Grounds
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PvpQueueCharacterEntry {
    pub item_id: i64,
    pub type_id: u32,
    pub location_id: i64,
    pub flag: u16,
    #[serde(default)]
    pub solar_system_id: u32,
    pub joined_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PvpEventSchedule {
    pub event_id: i64,
    pub schedule_id: i64,
    pub event_name: String,
    pub description: String,
    pub match_type_id: i64,
    pub start_unix: i64,
    pub end_unix: i64,
    pub type_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PvpArenaMatchInstance {
    pub match_id: i64,
    pub event_id: i64,
    pub arena_solar_system_id: u32,
    pub team_a: Vec<i64>,
    pub team_b: Vec<i64>,
    #[serde(default)]
    pub origin_systems: HashMap<i64, u32>,
    pub started_unix: u64,
    pub active: bool,
    pub winner_char_id: Option<i64>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub exited_pilots: HashSet<i64>,
    #[serde(default)]
    pub end_gate_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PvpFilamentStore {
    pub queue: HashMap<i64, PvpQueueCharacterEntry>,
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
    pub event_stats: HashMap<i64, HashMap<i64, (i64, i64, i64, i64, f64)>>,
    pub events: Vec<PvpEventSchedule>,
    pub seeded_default_events: bool,
    pub active_matches: HashMap<i64, PvpArenaMatchInstance>,
    pub match_teams: HashMap<i64, Vec<Vec<i64>>>,
    #[serde(default, with = "crate::durable_domain_types::sorted_map_of_sets_serde")]
    pub match_eliminated: HashMap<i64, HashSet<i64>>,
    pub match_arena_offsets: HashMap<i64, [f64; 3]>,
    pub match_filament_types: HashMap<i64, u32>,
    pub hazard_ticks: HashMap<i64, u32>,
    pub queue_refund_targets: HashMap<i64, (i64, i64)>,
    pub queue_tickets: HashMap<i64, i64>,
    pub next_ticket_id: i64,
    pub origin_positions: HashMap<i64, [f64; 3]>,
    pub next_match_id: i64,
}

impl PvpFilamentStore {
    pub fn ticket_id_for_entry(&self, char_id: i64, entry: &PvpQueueCharacterEntry) -> i64 {
        self.queue_tickets
            .get(&char_id)
            .copied()
            .filter(|&tid| tid > 0)
            .unwrap_or(entry.item_id)
    }
}

// ============================================================================
// Alliance Tournaments
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TourneyMatchState {
    Setup,
    Banning,
    Countdown,
    InProgress,
    Completed,
}

impl TourneyMatchState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Banning => "Banning",
            Self::Countdown => "Countdown",
            Self::InProgress => "InProgress",
            Self::Completed => "Completed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TourneyPilot {
    pub character_id: i64,
    pub ship_type_id: i64,
    pub ship_id: i64,
    pub points: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyMatch {
    pub match_id: i64,
    pub tourney_id: i64,
    pub solar_system_id: i64,
    pub state: TourneyMatchState,
    pub red_team_id: i64,
    pub red_team_name: String,
    pub blue_team_id: i64,
    pub blue_team_name: String,
    pub red_players: Vec<TourneyPilot>,
    pub blue_players: Vec<TourneyPilot>,
    pub banned_ship_types_red: Vec<i64>,
    pub banned_ship_types_blue: Vec<i64>,
    pub banned_ships: Vec<i64>,
    pub bans_finalized: bool,
    pub member_list_locked_red: bool,
    pub member_list_locked_blue: bool,
    pub countdown_active: bool,
    pub countdown_start_unix: u64,
    pub countdown_seconds: i64,
    pub max_points: i64,
    pub match_time: i64,
    pub overtime: i64,
    pub external_ruleset_id: i64,
    pub red_score: i64,
    pub blue_score: i64,
    pub red_fleet_boss_id: Option<i64>,
    pub blue_fleet_boss_id: Option<i64>,
    #[serde(default)]
    pub red_fleet_id: Option<i64>,
    #[serde(default)]
    pub blue_fleet_id: Option<i64>,
    pub referees: Vec<i64>,
    pub beacon_ids: Vec<i64>,
    #[serde(default)]
    pub arena_centers: Vec<[f64; 3]>,
    pub pregame_checks_complete: bool,
    pub grid_cleaned: bool,
    pub autobanning_active: bool,
    pub autoban_qty: i64,
    pub drone_restrict: bool,
    pub lock_restrict: bool,
    pub move_restrict: bool,
    pub warp_restrict: bool,
    pub pilot_errors: HashMap<i64, Vec<String>>,
    pub teleported_teams: Vec<i64>,
    pub returned_teams: Vec<i64>,
    pub fleet_details_updated: bool,
}

impl TourneyMatch {
    pub fn new(
        match_id: i64,
        tourney_id: i64,
        solar_system_id: i64,
        max_points: i64,
        match_time: i64,
        overtime: i64,
        external_ruleset_id: i64,
    ) -> Self {
        Self {
            match_id,
            tourney_id,
            solar_system_id,
            state: TourneyMatchState::Setup,
            red_team_id: 0,
            red_team_name: String::new(),
            blue_team_id: 0,
            blue_team_name: String::new(),
            red_players: Vec::new(),
            blue_players: Vec::new(),
            banned_ship_types_red: Vec::new(),
            banned_ship_types_blue: Vec::new(),
            banned_ships: Vec::new(),
            bans_finalized: false,
            member_list_locked_red: false,
            member_list_locked_blue: false,
            countdown_active: false,
            countdown_start_unix: 0,
            countdown_seconds: 60,
            max_points,
            match_time,
            overtime,
            external_ruleset_id,
            red_score: 0,
            blue_score: 0,
            red_fleet_boss_id: None,
            blue_fleet_boss_id: None,
            red_fleet_id: None,
            blue_fleet_id: None,
            referees: Vec::new(),
            beacon_ids: Vec::new(),
            arena_centers: Vec::new(),
            pregame_checks_complete: false,
            grid_cleaned: false,
            autobanning_active: false,
            autoban_qty: 0,
            drone_restrict: true,
            lock_restrict: true,
            move_restrict: true,
            warp_restrict: true,
            pilot_errors: HashMap::new(),
            teleported_teams: Vec::new(),
            returned_teams: Vec::new(),
            fleet_details_updated: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TourneyArenaSystem {
    pub combat_beacons: Vec<i64>,
    pub warp_bubbles: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TourneyStore {
    pub matches: HashMap<i64, TourneyMatch>,
    pub active_match_id: Option<i64>,
    pub ban_sessions: HashMap<i64, Vec<i64>>,
    pub tournaments: BTreeMap<i64, String>,
    pub arena_systems: BTreeMap<i64, TourneyArenaSystem>,
}

// ============================================================================
// Mobile Deployables
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MmjuSpoolEntry {
    pub ship_id: i64,
    pub char_id: i64,
    pub unit_id: i64,
    pub solar_system_id: u32,
    pub started_unix: u64,
    pub complete_at_unix: u64,
    pub direction: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecloakNotice {
    pub char_id: i64,
    pub ship_id: i64,
    pub module_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MobileDeployableRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub group_id: u32,
    pub owner_id: i64,
    pub solar_system_id: u32,
    pub anchor_start_unix: u64,
    pub anchor_duration_secs: u64,
    pub anchor_completed: bool,
    #[serde(default)]
    pub is_unanchored: bool,
    pub expires_at_unix: u64,
    pub last_interaction_unix: u64,
    pub reinforced_until_unix: Option<u64>,
    #[serde(default)]
    pub last_ping_unix: Option<u64>,
    pub position: [f64; 3],
    #[serde(default)]
    pub deployed_fleet_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedDamageRecord {
    pub shield_hp: f64,
    pub max_shield_hp: f64,
    pub armor_hp: f64,
    pub max_armor_hp: f64,
    pub structure_hp: f64,
    pub max_structure_hp: f64,
}

fn deserialize_mmju_spools<'de, D>(deserializer: D) -> Result<HashMap<i64, MmjuSpoolEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct MmjuSpoolsVisitor;

    impl<'de> serde::de::Visitor<'de> for MmjuSpoolsVisitor {
        type Value = HashMap<i64, MmjuSpoolEntry>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a map or sequence of MmjuSpoolEntry")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut out = HashMap::new();
            while let Some((k, v)) = map.next_entry::<String, MmjuSpoolEntry>()? {
                if let Ok(id) = k.parse::<i64>() {
                    out.insert(id, v);
                } else {
                    out.insert(v.unit_id, v);
                }
            }
            Ok(out)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut out = HashMap::new();
            while let Some(v) = seq.next_element::<MmjuSpoolEntry>()? {
                out.insert(v.ship_id, v);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_any(MmjuSpoolsVisitor)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MobileDeployableRuntimeState {
    pub deployables: HashMap<i64, MobileDeployableRecord>,
    pub damaged_items: HashMap<i64, PersistedDamageRecord>,
    pub observatory_pings_done: HashMap<i64, u32>,
    pub cloak_activated_unix: HashMap<i64, u64>,
    #[serde(deserialize_with = "deserialize_mmju_spools")]
    pub mmju_spools: HashMap<i64, MmjuSpoolEntry>,
    pub siphon_last_cycle_unix: HashMap<i64, u64>,
    pub pending_decloak_notifications: Vec<PendingDecloakNotice>,
}

// ============================================================================
// Titan AoE Superweapons
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SuperweaponCycleKind {
    Cone {
        origin_pos: [f64; 3],
        dir_vec: [f64; 3],
        cone_len_m: f64,
        tick_dmg: [f64; 4],
        sig_radius_m: f64,
    },
    SpatialGtfo {
        center: [f64; 3],
        sphere_radius_m: f64,
    },
    ReaperSweep {
        start_pt: [f64; 3],
        end_pt: [f64; 3],
        beam_radius_m: f64,
        sweep_dmg: [f64; 4],
        sig_radius_m: f64,
        already_hit_balls: Vec<i64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistedSuperweaponCycle {
    pub sys_id: u32,
    pub char_id: i64,
    pub ship_id: i64,
    pub ship_type_id: u32,
    pub mod_type_id: u32,
    pub kind: SuperweaponCycleKind,
    pub started_at_unix: u64,
    pub warmup_secs: u64,
    pub total_ticks: usize,
    pub ticks_applied: usize,
    pub immobilize_until_unix: u64,
    pub orig_max_velocity: f64,
    pub orig_warp_scramble_strength: i32,
    pub orig_disallow_assistance: bool,
    pub mobility_restored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWarpSuppression {
    pub sys_id: u32,
    pub ship_id: i64,
    pub suppressed_until_unix: u64,
    pub orig_warp_scramble_strength: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedSuperweaponRuntime {
    pub active_cycles: Vec<PersistedSuperweaponCycle>,
    pub warp_suppressions: Vec<PersistedWarpSuppression>,
}

// ============================================================================
// Pirate Insurgency Complexes & Ice Heist
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutpostRaidSize {
    Small,
    Medium,
    Moderate,
    LargeAdv,
}

impl OutpostRaidSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Moderate, Self::LargeAdv];

    pub fn slot_index(self) -> i64 {
        match self {
            Self::Small => 1,
            Self::Medium => 2,
            Self::Moderate => 3,
            Self::LargeAdv => 4,
        }
    }

    pub fn beacon_type_id(self) -> u32 {
        match self {
            Self::Small => 78_799,
            Self::Medium => 78_805,
            Self::Moderate => 97_658,
            Self::LargeAdv => 78_814,
        }
    }

    pub fn base_capture_duration_secs(self) -> u64 {
        match self {
            Self::Small => 600,
            Self::Medium | Self::Moderate | Self::LargeAdv => 900,
        }
    }

    pub fn contribution_percent(self) -> f64 {
        match self {
            Self::Small => 0.25,
            Self::Medium => 0.5,
            Self::Moderate => 1.5,
            Self::LargeAdv => 1.0,
        }
    }

    pub fn contribution_subpoints(self) -> i64 {
        match self {
            Self::Small => 250,
            Self::Medium => 500,
            Self::Moderate => 1_500,
            Self::LargeAdv => 1_000,
        }
    }

    pub fn base_lp_reward(self) -> i64 {
        match self {
            Self::Small => 29_000,
            Self::Medium => 41_500,
            Self::Moderate | Self::LargeAdv => 52_500,
        }
    }

    pub fn respawn_delay_secs(self) -> u64 {
        if self == Self::LargeAdv { 40 * 60 } else { 900 }
    }

    pub fn allows_ship_group(self, group_id: u32) -> bool {
        let frig_dessie = matches!(
            group_id,
            25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 420 | 541 | 1534
        );
        let t3_dessie = group_id == 1305;
        let cruiser = matches!(group_id, 26 | 358 | 832 | 833 | 894 | 906 | 1972);
        let t3_cruiser = group_id == 963;
        let bc = matches!(group_id, 419 | 540 | 1201);
        let bs = matches!(group_id, 27 | 898 | 900);
        match self {
            Self::Small => frig_dessie,
            Self::Medium => frig_dessie || t3_dessie || cruiser,
            Self::Moderate => frig_dessie || t3_dessie || cruiser || bc,
            Self::LargeAdv => frig_dessie || t3_dessie || cruiser || t3_cruiser || bc || bs,
        }
    }

    pub fn mordus_defender_type_id(self) -> u32 {
        match self {
            Self::Small => 79_545,
            Self::Medium => 79_549,
            Self::Moderate => 79_553,
            Self::LargeAdv => 79_557,
        }
    }

    pub fn pirate_defender_type_id(self, pirate_faction_id: i64) -> u32 {
        if pirate_faction_id == 500_011 {
            match self {
                Self::Small => 79_515,
                Self::Medium => 79_520,
                Self::Moderate => 79_523,
                Self::LargeAdv => 79_526,
            }
        } else {
            match self {
                Self::Small => 79_529,
                Self::Medium => 79_533,
                Self::Moderate => 79_537,
                Self::LargeAdv => 79_541,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutpostRaidInstance {
    pub gate_id: i64,
    pub beacon_id: i64,
    pub campaign_id: i64,
    pub solar_system_id: u32,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub size: OutpostRaidSize,
    pub is_static: bool,
    pub occupants: Vec<(i64, i64)>,
    pub mordus_defender_ball_id: Option<i64>,
    pub pirate_defender_ball_id: Option<i64>,
    pub mordus_defender_cleared: bool,
    pub pirate_defender_cleared: bool,
    pub weighted_elapsed_secs: f64,
    pub last_step_unix: u64,
    pub completed_on_cooldown: bool,
    pub respawn_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiningAmbushInstance {
    pub campaign_id: i64,
    pub solar_system_id: u32,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub beacon_id: i64,
    pub rorqual_ball_id: i64,
    pub pirate_tackler_ids: Vec<i64>,
    pub ore_defender_ids: Vec<i64>,
    pub ransom_elapsed_secs: u64,
    pub last_step_unix: u64,
    pub resolved: bool,
    pub rorqual_warping_off_until: Option<u64>,
    pub cooldown_until_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InsurgencyComplexOpsState {
    pub outpost_raids: HashMap<i64, OutpostRaidInstance>,
    pub mining_ambushes: HashMap<u32, MiningAmbushInstance>,
    pub subpoint_remainders: HashMap<String, i64>,
    pub named_roamer_rolls: HashMap<String, bool>,
    pub ice_heist_respawn_until: HashMap<u32, u64>,
}
