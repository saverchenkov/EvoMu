use crate::durable_domain_types::tuple_map_serde;
use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ============================================================================
// AIR NPE Node Graph Runner, Client Node Graphs, Expert Systems & Certificates
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AirNpeRunnerState {
    pub char_id: i64,
    pub state_id: i64,
    pub active_server_graph_id: u32,
    pub executed_server_graphs: Vec<u32>,
    pub active_act: u8,
    pub scene_index: u8,
    pub checkpoint_id: u32,
    pub completed_checkpoints: Vec<u32>,
    pub master_client_graph_id: i64,
    pub master_client_instance_id: Option<i64>,
    pub act_client_graph_id: Option<i64>,
    pub act_client_instance_id: Option<i64>,
    pub scene_client_graph_id: Option<i64>,
    pub scene_client_instance_id: Option<i64>,
    pub pocket_instance_id: Option<i64>,
    pub pocket_dungeon_id: Option<u32>,
    pub pocket_solar_system_id: Option<u32>,
    pub origin_station_id: Option<i64>,
    pub origin_solar_system_id: Option<u32>,
    pub intro_cinematic_completed: bool,
    pub moved_into_pocket: bool,
    pub completed: bool,
    pub skipped: bool,
    pub blackboard: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveClientNodeGraph {
    pub instance_id: i64,
    pub node_graph_id: i64,
    pub category_id: i64,
    pub operation_id: i64,
    pub blackboard: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertSystemRecord {
    pub type_id: i64,
    pub inserted_at_ft: i64,
    pub expires_at_ft: i64,
    #[serde(default)]
    pub skills_granted: BTreeMap<i32, i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterCertificateState {
    #[serde(default)]
    pub certificates: HashMap<u32, u8>,
    #[serde(default)]
    pub flags: HashMap<u32, u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeGraphAndProgressionStore {
    pub air_npe_runners: HashMap<i64, AirNpeRunnerState>,
    pub client_node_graphs: HashMap<i64, BTreeMap<i64, ActiveClientNodeGraph>>,
    pub client_node_graph_seq: i64,
    #[serde(with = "tuple_map_serde")]
    pub node_graph_blackboards: HashMap<(i64, i64), BTreeMap<String, String>>,
    pub expert_systems: HashMap<i64, BTreeMap<i64, ExpertSystemRecord>>,
    pub certificates: HashMap<i64, CharacterCertificateState>,
}

// ============================================================================
// Seasons, Hacking Wins, Milestones, Login Campaigns, Project Discovery, Activity, Fast Checkout
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonalChallengeDef {
    pub challenge_id: i64,
    pub season_id: i64,
    pub event_type: String,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub max_progress: i64,
    pub client_event_driven: bool,
    pub reward_type_id: u32,
    pub reward_quantity: i32,
    #[serde(default)]
    pub is_dormant: bool,
    #[serde(default)]
    pub expiration_time_minutes: i64,
    #[serde(default)]
    pub isk_awarded: i64,
    #[serde(default)]
    pub next_challenge: Option<i64>,
    #[serde(default)]
    pub should_be_active_on_season_start: bool,
    #[serde(default)]
    pub should_not_expire_at_downtime: bool,
    #[serde(default)]
    pub should_only_appear_once_per_season: bool,
    #[serde(default)]
    pub event_conditions: BTreeMap<String, Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeasonConfig {
    pub season_id: i64,
    pub template_id: i64,
    pub max_active_challenges: i64,
    pub max_season_points: i64,
    pub season_name: String,
    pub title_message_id: i64,
    pub description_message_id: i64,
    pub hint_message_id: i64,
    pub large_selection_title_id: i64,
    pub navigation_card_picture_path: String,
    pub background_no_logo: String,
    pub start_time_ft: i64,
    pub end_time_ft: i64,
    pub multiple_season_running: bool,
}

impl Default for SeasonConfig {
    fn default() -> Self {
        Self {
            season_id: 1,
            template_id: 1,
            max_active_challenges: 4,
            max_season_points: 100,
            season_name: "Crimson Harvest".into(),
            title_message_id: 583_333,
            description_message_id: 583_334,
            hint_message_id: 583_335,
            large_selection_title_id: 583_380,
            navigation_card_picture_path: "res:/UI/Texture/classes/agency/navigationCards/CrimsonHarvest2026_Agency_Navicons_400x666.png".into(),
            background_no_logo: "res:/UI/Texture/classes/NewFeatureNotify/triglavianInvasion.png".into(),
            start_time_ft: 133_700_000_000_000_000,
            end_time_ft: 133_730_240_000_000_000,
            multiple_season_running: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterChallenge {
    pub challenge_id: i64,
    pub season_id: i64,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub progress: i64,
    pub max_progress: i64,
    pub completed: bool,
    pub claimed: bool,
    pub is_dormant: bool,
    pub expiration_date: i64,
    #[serde(default)]
    pub client_event_driven: bool,
    #[serde(default)]
    pub reward_type_id: u32,
    #[serde(default)]
    pub reward_quantity: i32,
    #[serde(default)]
    pub baseline_progress: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterSeasonState {
    #[serde(default)]
    pub selected_season_id: i64,
    #[serde(default)]
    pub season_points: i64,
    #[serde(default)]
    pub challenges: HashMap<i64, CharacterChallenge>,
    #[serde(default)]
    pub claimed_challenges: HashMap<i64, CharacterChallenge>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub completed_once_per_season_ids: HashSet<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterMilestoneState {
    pub first_logon_unix: u64,
    pub last_activity_unix: u64,
    pub accumulated_playtime_secs: u64,
    #[serde(default)]
    pub active_session: bool,
}

impl CharacterMilestoneState {
    /// Total accumulated play time in seconds as of `now_unix`, including elapsed time
    /// only when the character currently has an active online session.
    pub fn accumulated_playtime_now(&self, now_unix: u64) -> u64 {
        if self.active_session && self.last_activity_unix > 0 {
            self.accumulated_playtime_secs
                .saturating_add(now_unix.saturating_sub(self.last_activity_unix))
        } else {
            self.accumulated_playtime_secs
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledDailyCampaign {
    pub campaign_id: i64,
    pub start_unix: u64,
    #[serde(default)]
    pub end_unix: Option<u64>,
    #[serde(default)]
    pub is_rookie: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonalCampaignSchedule {
    pub campaign_id: i64,
    pub start_unix: u64,
    pub end_unix: u64,
    #[serde(default)]
    pub turnover_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDiscoveryTaskData {
    pub task_id: String,
    pub task_number: i64,
    pub project_id: String,
    pub url: String,
    pub samples: i64,
    #[serde(default)]
    pub fetch_count: u64,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub payload_json: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectDiscoveryStore {
    #[serde(default)]
    pub tasks: HashMap<i64, ProjectDiscoveryTaskData>,
    #[serde(default)]
    pub char_tasks: HashMap<String, ProjectDiscoveryTaskData>,
    #[serde(default)]
    pub xp_curve_by_rank: HashMap<i64, f64>,
    #[serde(default)]
    pub xp_per_rank: f64,
    #[serde(default)]
    pub tutorial_xp_reward: f64,
    #[serde(default)]
    pub task_consensus_by_index: HashMap<i64, bool>,
    #[serde(default)]
    pub classification_xp_accurate: f64,
    #[serde(default)]
    pub classification_xp_inaccurate: f64,
    #[serde(default)]
    pub classification_ak_reward: i64,
    #[serde(default)]
    pub max_bonus_samples: Option<i64>,
    #[serde(default)]
    pub mmos_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerProjectState {
    pub rank: i64,
    pub experience: f64,
    pub accuracy: f64,
    pub score: f64,
    pub tasks_submitted: i64,
    pub analysis_kredits: i64,
    pub tutorial_level: i64,
    pub tutorial_complete: bool,
    pub current_task_index: i64,
    pub bonus_samples_left: i64,
    #[serde(default)]
    pub tutorial_reward_claimed: bool,
    #[serde(default)]
    pub tutorial_skipped: bool,
    #[serde(default = "default_max_bonus_samples")]
    pub max_bonus_samples: i64,
    #[serde(default = "default_project_id")]
    pub project_id: String,
    #[serde(default)]
    pub last_submitted_task_index: i64,
    #[serde(default)]
    pub last_bonus_refill_unix: u64,
    #[serde(default)]
    pub last_submission_unix: u64,
}

fn default_max_bonus_samples() -> i64 {
    10
}

fn default_project_id() -> String {
    "mcgill-polyseg".into()
}

impl Default for PlayerProjectState {
    fn default() -> Self {
        Self {
            rank: 1,
            experience: 0.0,
            accuracy: 0.0,
            score: 0.0,
            tasks_submitted: 0,
            analysis_kredits: 0,
            tutorial_level: 1,
            tutorial_complete: false,
            tutorial_skipped: false,
            current_task_index: 0,
            bonus_samples_left: 10,
            tutorial_reward_claimed: false,
            max_bonus_samples: 10,
            project_id: default_project_id(),
            last_submitted_task_index: 0,
            last_bonus_refill_unix: 0,
            last_submission_unix: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskVoteBucket {
    pub submissions: Vec<(i64, [f64; 2], bool)>,
    pub consensus_centroid: Option<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CharacterActivityState {
    #[serde(default)]
    pub seen_activities: Vec<i64>,
    #[serde(default)]
    pub pop_state: Vec<(PyRep, PyRep)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FastCheckoutProductRecord {
    pub type_id: u32,
    pub quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FastCheckoutOfferRecord {
    pub offer_id: i64,
    pub plex_amount: i64,
    pub price_usd: f64,
    #[serde(default)]
    pub price_plex: i64,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub products: Vec<FastCheckoutProductRecord>,
    #[serde(default)]
    pub category_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserFastCheckoutState {
    pub enabled: bool,
    #[serde(default)]
    pub cached_session_token: Option<String>,
    #[serde(default)]
    pub cached_offers: Option<Vec<FastCheckoutOfferRecord>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeasonsAndCampaignsStore {
    pub season_states: HashMap<i64, CharacterSeasonState>,
    pub active_seasons: HashMap<i64, i64>,
    pub season_configuration: Option<SeasonConfig>,
    pub seasons_catalog: BTreeMap<i64, SeasonConfig>,
    pub challenge_definitions: Vec<SeasonalChallengeDef>,
    pub season_loot_tables: HashMap<u32, Vec<(u32, i64, f64)>>,
    pub announced_season_init: bool,
    pub announced_season_states: BTreeMap<i64, String>,
    pub hacking_wins: HashMap<i64, u64>,
    pub milestone_playtime: HashMap<i64, CharacterMilestoneState>,
    pub login_campaign_enrollments: HashMap<i64, i64>,
    pub daily_login_campaign_schedule: Option<ScheduledDailyCampaign>,
    pub seasonal_login_schedule: Option<SeasonalCampaignSchedule>,
    pub auto_injected_rewards: HashMap<i64, Vec<u32>>,
    pub login_reward_expiry: HashMap<i64, i64>,
    pub project_discovery_store: ProjectDiscoveryStore,
    pub project_discovery_players: HashMap<i64, PlayerProjectState>,
    pub project_discovery_votes: HashMap<i64, TaskVoteBucket>,
    pub character_activities: HashMap<i64, CharacterActivityState>,
    pub fast_checkout_states: HashMap<u64, UserFastCheckoutState>,
}
