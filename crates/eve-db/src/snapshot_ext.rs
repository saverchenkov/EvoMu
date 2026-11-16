//! `WorldStateSnapshot` container and dynamic world/ESI event and campaign records (`ESI-3`).

pub use crate::cow::*;
pub use crate::durable_domain_types::*;
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemJumpEvent {
    pub solar_system_id: SolarSystemId,
    pub character_id: i64,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemKillEvent {
    pub solar_system_id: SolarSystemId,
    pub ship_kills: u32,
    pub npc_kills: u32,
    pub pod_kills: u32,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IncursionRecord {
    pub constellation_id: u32,
    pub staging_solar_system_id: SolarSystemId,
    pub infested_solar_systems: Vec<SolarSystemId>,
    pub faction_id: i64,
    pub has_boss: bool,
    pub influence: f64,
    pub state: String,
    pub incursion_type: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SovCampaignScoreRecord {
    pub structure_id: i64,
    pub attackers_score: f64,
    pub defender_score: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FwWindowedStat {
    pub yesterday: i64,
    pub last_week: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShipFitInputsRecord {
    pub active_stance_type_id: Option<TypeId>,
    pub last_stance_switch_ms: Option<u64>,
    pub overloaded_modules: std::collections::BTreeSet<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FwCharacterStatsRecord {
    pub character_id: i64,
    pub faction_id: Option<i64>,
    pub enlisted_on_unix: Option<u64>,
    pub current_rank: i64,
    pub highest_rank: i64,
    pub kills: FwWindowedStat,
    pub victory_points: FwWindowedStat,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FwSystemOccupationRecord {
    pub solar_system_id: SolarSystemId,
    pub owner_faction_id: i64,
    pub occupier_faction_id: i64,
    pub victory_points: i64,
    pub victory_points_threshold: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WarAllyRecord {
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WarLifecycleMeta {
    pub declared_unix: Option<u64>,
    pub started_unix: Option<u64>,
    pub retracted_unix: Option<u64>,
    pub finished_unix: Option<u64>,
    pub open_for_allies: Option<bool>,
    pub allies: Vec<WarAllyRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KillmailAttackerDetail {
    pub character_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub ship_type_id: TypeId,
    pub weapon_type_id: TypeId,
    pub damage_done: i64,
    pub final_blow: bool,
    pub security_status: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KillmailDetailRecord {
    pub kill_id: i64,
    pub killmail_time_unix: u64,
    pub damage_taken: i64,
    pub victim_position: Option<[f64; 3]>,
    pub attackers: Vec<KillmailAttackerDetail>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AllianceLifecycleMeta {
    pub creator_id: i64,
    pub creator_corporation_id: i64,
    pub date_founded_unix: u64,
    pub faction_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureEsiTimerMeta {
    pub profile_id: i64,
    pub fuel_expires_unix: Option<u64>,
    pub state_timer_start_unix: Option<u64>,
    pub state_timer_end_unix: Option<u64>,
    pub unanchors_at_unix: Option<u64>,
    pub next_reinforce_hour: Option<i64>,
    pub next_reinforce_apply_unix: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpAllianceHistoryEntry {
    pub record_id: i64,
    pub alliance_id: Option<i64>,
    pub start_date_unix: u64,
    pub is_deleted: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpContainerLogEntry {
    pub logged_at_unix: u64,
    pub container_id: i64,
    pub container_type_id: i32,
    pub character_id: i64,
    pub location_id: i64,
    pub location_flag: String,
    pub action: String,
    pub password_type: Option<String>,
    pub type_id: Option<i32>,
    pub quantity: Option<i32>,
    pub old_config_bitmask: Option<i32>,
    pub new_config_bitmask: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpMedalEntry {
    pub medal_id: i64,
    pub title: String,
    pub description: String,
    pub creator_id: i64,
    pub created_at_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpIssuedMedalEntry {
    pub medal_id: i64,
    pub character_id: i64,
    pub reason: String,
    pub status: String,
    pub issuer_id: i64,
    pub issued_at_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpTitleEntry {
    pub title_id: i64,
    pub name: String,
    pub roles: Vec<String>,
    pub grantable_roles: Vec<String>,
    pub roles_at_hq: Vec<String>,
    pub grantable_roles_at_hq: Vec<String>,
    pub roles_at_base: Vec<String>,
    pub grantable_roles_at_base: Vec<String>,
    pub roles_at_other: Vec<String>,
    pub grantable_roles_at_other: Vec<String>,
}

/// Maps a corporation `titleID` to its `titleMask` bit (`#787`).
///
/// V24.01 `bco_titles.pyj` and `corp_ui_member_titles.pyj` use power-of-two `titleID` bitmasks
/// (`1, 2, 4, 8, 16, 32, ..., 32768`, i.e. `1i64 << i` for `i in 0..16`). Non-power-of-two
/// legacy 1-based ordinal IDs (`3, 5, ... <= 64`) fall back to `1u64 << (title_id - 1)`.
pub fn title_id_to_bit(title_id: i64) -> u64 {
    if title_id <= 0 {
        0
    } else if (title_id as u64).is_power_of_two() {
        title_id as u64
    } else if (1..=64).contains(&title_id) {
        1u64 << ((title_id - 1) as u32)
    } else {
        0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpMemberRoleAssignment {
    pub character_id: i64,
    pub roles: u64,
    pub grantable_roles: u64,
    pub roles_at_hq: u64,
    pub grantable_roles_at_hq: u64,
    pub roles_at_base: u64,
    pub grantable_roles_at_base: u64,
    pub roles_at_other: u64,
    pub grantable_roles_at_other: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpRoleHistoryEntry {
    pub character_id: i64,
    pub changed_at_unix: u64,
    pub issuer_id: i64,
    pub role_type: String,
    pub old_roles: Vec<String>,
    pub new_roles: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpShareholderEntry {
    pub shareholder_id: i64,
    pub shareholder_type: String,
    pub share_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StarbaseDetailSettings {
    pub allow_corporation_members: bool,
    pub allow_alliance_members: bool,
    pub use_alliance_standings: bool,
    pub attack_if_other_security_status_dropping: bool,
    pub attack_if_at_war: bool,
    pub fuel_bay_view: String,
    pub fuel_bay_take: String,
    pub anchor: String,
    pub unanchor: String,
    pub online: String,
    pub offline: String,
    pub attack_standing_threshold: Option<f64>,
    pub attack_security_status_threshold: Option<f64>,
    #[serde(default)]
    pub active_controller_id: Option<i64>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub notify_fuel: bool,
    #[serde(default)]
    pub notify_calendar: bool,
    #[serde(default)]
    pub notify_shield: Option<f64>,
    #[serde(default)]
    pub notify_armor: Option<f64>,
    #[serde(default)]
    pub notify_structure: Option<f64>,
    #[serde(default)]
    pub linked_resources: Vec<i64>,
    #[serde(default)]
    pub last_moon_process_unix: u64,
}

impl Default for StarbaseDetailSettings {
    fn default() -> Self {
        Self {
            allow_corporation_members: true,
            allow_alliance_members: true,
            use_alliance_standings: false,
            attack_if_other_security_status_dropping: false,
            attack_if_at_war: true,
            fuel_bay_view: "corp".into(),
            fuel_bay_take: "corp".into(),
            anchor: "corp".into(),
            unanchor: "corp".into(),
            online: "corp".into(),
            offline: "corp".into(),
            attack_standing_threshold: None,
            attack_security_status_threshold: None,
            active_controller_id: None,
            password: None,
            notify_fuel: false,
            notify_calendar: false,
            notify_shield: None,
            notify_armor: None,
            notify_structure: None,
            linked_resources: Vec::new(),
            last_moon_process_unix: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpVoteCaseRecord {
    pub case_id: i64,
    pub corporation_id: i64,
    pub vote_type: i64,
    pub text: String,
    pub description: String,
    pub parameter: i64,
    pub start_ts: u64,
    pub end_ts: u64,
    pub executed: bool,
    #[serde(default)]
    pub options: Vec<(i64, String, i64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpVoteBallotRecord {
    pub case_id: i64,
    pub voter_id: i64,
    pub option_id: i64,
    pub weight: i64,
    pub voted_at_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpProjectContributorEntry {
    pub character_id: i64,
    pub contributed_quantity: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterResearchAgentRecord {
    pub agent_id: i64,
    pub skill_type_id: TypeId,
    pub started_at_unix: u64,
    pub points_per_day: f64,
    pub remainder_points: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterMedalGraphicRecord {
    pub part: i64,
    pub layer: i64,
    pub graphic: String,
    pub color: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterMedalRecord {
    pub medal_id: i64,
    pub title: String,
    pub description: String,
    pub corporation_id: i64,
    pub issuer_id: i64,
    pub date_unix: u64,
    pub reason: String,
    pub status: String,
    pub graphics: Vec<CharacterMedalGraphicRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterScopedRolesRecord {
    pub roles: Vec<String>,
    pub roles_at_hq: Vec<String>,
    pub roles_at_base: Vec<String>,
    pub roles_at_other: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoyaltyStoreOfferItemRecord {
    pub type_id: TypeId,
    pub quantity: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoyaltyStoreOfferRecord {
    pub offer_id: i64,
    pub type_id: TypeId,
    pub quantity: i64,
    pub lp_cost: i64,
    pub isk_cost: i64,
    #[serde(default)]
    pub ak_cost: Option<i64>,
    #[serde(default)]
    pub required_items: Vec<LoyaltyStoreOfferItemRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletTransactionRecord {
    pub transaction_id: i64,
    pub date_unix: u64,
    pub location_id: i64,
    pub type_id: TypeId,
    pub unit_price: f64,
    pub quantity: i64,
    pub client_id: i64,
    pub is_buy: bool,
    pub is_personal: bool,
    pub journal_ref_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpWalletJournalRecord {
    pub id: i64,
    pub date_unix: u64,
    pub ref_type: String,
    pub description: String,
    pub amount: f64,
    pub balance: f64,
    #[serde(default)]
    pub first_party_id: Option<i64>,
    #[serde(default)]
    pub second_party_id: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketOrderMeta {
    pub issued_unix: u64,
    pub duration_days: i64,
    pub range: String,
    #[serde(default)]
    pub issued_by: Option<i64>,
    #[serde(default)]
    pub wallet_division: Option<i64>,
    #[serde(default)]
    pub is_corporation: bool,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketHistoryDayRecord {
    pub date: String,
    pub average: f64,
    pub highest: f64,
    pub lowest: f64,
    pub order_count: i64,
    pub volume: i64,
    #[serde(default)]
    pub open_price: Option<f64>,
    #[serde(default)]
    pub close_price: Option<f64>,
}

pub type MarketDailySummary = MarketHistoryDayRecord;

impl MarketHistoryDayRecord {
    pub fn open_isk(&self) -> f64 { self.open_price.unwrap_or(self.average) }
    pub fn high_isk(&self) -> f64 { self.highest }
    pub fn low_isk(&self) -> f64 { self.lowest }
    pub fn close_isk(&self) -> f64 { self.close_price.unwrap_or(self.average) }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanetaryPinContentRecord {
    pub type_id: TypeId,
    pub amount: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanetaryPinRecord {
    pub pin_id: i64,
    pub type_id: TypeId,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub schematic_id: Option<i64>,
    #[serde(default)]
    pub install_time_unix: Option<u64>,
    #[serde(default)]
    pub expiry_time_unix: Option<u64>,
    #[serde(default)]
    pub last_cycle_start_unix: Option<u64>,
    #[serde(default)]
    pub contents: Vec<PlanetaryPinContentRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanetaryLinkRecord {
    pub source_pin_id: i64,
    pub destination_pin_id: i64,
    pub link_level: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanetaryRouteRecord {
    pub route_id: i64,
    pub source_pin_id: i64,
    pub destination_pin_id: i64,
    pub content_type_id: TypeId,
    pub quantity: i64,
    #[serde(default)]
    pub waypoints: Vec<i64>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanetaryColonyLayoutRecord {
    pub pins: Vec<PlanetaryPinRecord>,
    pub links: Vec<PlanetaryLinkRecord>,
    pub routes: Vec<PlanetaryRouteRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomsOfficeRecord {
    pub office_id: i64,
    pub system_id: SolarSystemId,
    pub reinforce_exit_start: i64,
    pub reinforce_exit_end: i64,
    pub allow_alliance_access: bool,
    pub allow_access_with_standings: bool,
    #[serde(default)]
    pub standing_level: Option<String>,
    #[serde(default)]
    pub corporation_tax_rate: Option<f64>,
    #[serde(default)]
    pub alliance_tax_rate: Option<f64>,
    #[serde(default)]
    pub excellent_standing_tax_rate: Option<f64>,
    #[serde(default)]
    pub good_standing_tax_rate: Option<f64>,
    #[serde(default)]
    pub neutral_standing_tax_rate: Option<f64>,
    #[serde(default)]
    pub bad_standing_tax_rate: Option<f64>,
    #[serde(default)]
    pub terrible_standing_tax_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SocialContactRecord {
    pub contact_id: i64,
    pub contact_type: String,
    pub standing: f64,
    pub is_watched: bool,
    pub is_blocked: bool,
    #[serde(default)]
    pub label_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailingListRecord {
    pub list_id: i64,
    pub name: String,
    pub owner_id: i64,
    pub default_access: i64,
    pub default_role: i64,
    pub cost: i64,
    pub members: HashMap<i64, i64>,
    #[serde(default)]
    pub entity_access: HashMap<i64, i64>,
    #[serde(default)]
    pub welcome_mail: Option<(String, String)>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub mail_ids: HashSet<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarEventRecord {
    pub event_id: i64,
    pub owner_id: i64,
    pub creator_id: i64,
    pub event_date_time: i64,
    pub duration: i64,
    pub importance: i64,
    pub title: String,
    pub description: String,
    pub scope: String,
    pub responses: HashMap<i64, i64>,
    #[serde(default)]
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatChannelAclRecord {
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub allow_list: HashSet<i64>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub deny_list: HashSet<i64>,
    #[serde(default)]
    pub muted_until: HashMap<i64, (u64, String)>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldStateSnapshot {
    pub next_entity_id: i64,
    #[serde(default)]
    pub wal_seq: u64,
    #[serde(default)]
    pub markets_seeded: bool,
    #[serde(default)]
    pub service_state_blobs: CowCell<HashMap<String, String>>,
    #[serde(skip, default)]
    pub sessions: CowCell<HashMap<u64, OnlineSessionRecord>>,
    pub accounts: CowCell<HashMap<String, AccountRecord>>,
    pub characters: CowMap<i64, CharacterRecord>,
    pub skills: CowMap<i64, HashMap<TypeId, TrainedSkillRecord>>,
    pub skill_queues: CowMap<i64, Vec<SkillQueueEntry>>,
    pub jump_clones: CowCell<HashMap<i64, Vec<JumpCloneRecord>>>,
    pub active_implants: CowCell<HashMap<i64, Vec<TypeId>>>,
    pub items: CowMap<i64, InventoryItemRecord>,
    #[serde(skip, default)] pub transient_items: CowMap<i64, InventoryItemRecord>,
    #[serde(skip, default)] pub transient_item_attributes: HashMap<i64, HashMap<AttributeId, f64>>,
    pub saved_fittings: CowCell<HashMap<i64, SavedFittingRecord>>,
    pub market_orders: CowMap<i64, MarketOrderRecord>,
    pub wallet_journal: CowIdVec<WalletJournalEntry>,
    pub contracts: CowCell<HashMap<i64, ContractRecord>>,
    pub industry_jobs: CowCell<HashMap<i64, IndustryJobRecord>>,
    #[serde(with = "cow_cell_tuple_map_serde")]
    pub planetary_colonies: CowCell<HashMap<(i64, i64), PlanetaryColonyRecord>>,
    #[serde(with = "crate::cow_collections::cow_active_missions_tuple_serde")]
    pub active_missions: CowActiveMissions,
    pub mission_tally: CowCell<HashMap<i64, u32>>,
    #[serde(default)]
    pub standing_transactions: CowCell<HashMap<i64, Vec<StandingTransactionRecord>>>,
    pub npe_progress: CowCell<HashMap<i64, NpeProgressRecord>>,
    pub bookmarks: CowCell<HashMap<i64, BookmarkRecord>>,
    pub mails: CowIdVec<MailRecord>,
    pub fleets: CowCell<HashMap<i64, FleetRecord>>,
    pub corporations: CowCell<HashMap<i64, CorporationRecord>>,
    #[serde(default)]
    pub quasar_state_blobs: CowCell<HashMap<String, String>>,
    #[serde(default)]
    pub alliances: CowCell<HashMap<i64, AllianceRecord>>,
    #[serde(default)]
    pub wars: CowCell<HashMap<i64, WarRecord>>,
    pub corp_projects: CowCell<HashMap<i64, CorpProjectRecord>>,
    pub killmails: CowCell<HashMap<i64, KillmailRecord>>,
    pub kill_rights: CowCell<HashMap<i64, KillRightRecord>>,
    pub structures: CowMap<i64, UpwellStructureRecord>,
    pub system_states: CowCell<HashMap<SolarSystemId, SystemDynamicState>>,
    #[serde(default, with = "crate::durable_domain_types::cow_cell_sorted_set_serde")]
    pub committed_idempotency_keys: CowCell<HashSet<(i64, String)>>,
    #[serde(default)]
    pub licensed_skins: CowCell<HashMap<i64, Vec<u32>>>,
    #[serde(default)]
    pub applied_ship_skins: CowCell<HashMap<i64, u32>>,
    #[serde(default)]
    pub chat_channels: CowCell<HashMap<String, ChatChannelRecord>>,
    #[serde(default)]
    pub cosmic_signatures: CowCell<HashMap<SolarSystemId, Vec<CosmicSignatureRecord>>>,
    #[serde(default, with = "crate::durable_domain_types::cow_cell_sorted_set_serde")]
    pub read_mails: CowCell<HashSet<(i64, i64)>>,
    #[serde(default, with = "crate::durable_domain_types::cow_cell_sorted_set_serde")]
    pub deleted_mails: CowCell<HashSet<(i64, i64)>>,
    #[serde(default)]
    pub mail_labels: CowCell<HashMap<i64, Vec<MailLabelRecord>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub mail_assigned_labels: CowCell<HashMap<(i64, i64), Vec<i64>>>,
    #[serde(default)]
    pub bookmark_folders: CowCell<HashMap<i64, BookmarkFolderRecord>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub wh_polarizations: CowCell<HashMap<(i64, i64), u64>>,
    #[serde(default)]
    pub mining_ledger: CowCell<Vec<MiningLedgerRecord>>,
    pub module_online_states: HashMap<i64, bool>,
    /// Fitted modules currently cycling, set by `dogmaIM.Activate` and cleared by `dogmaIM.Deactivate`.
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub module_active_states: HashSet<i64>,
    /// Charge type loaded into an active burst module when it was activated (`dogmaIM.Activate`).
    #[serde(default)]
    pub module_active_charges: HashMap<i64, u32>,
    #[serde(default)]
    pub ship_fit_inputs: CowCell<HashMap<i64, ShipFitInputsRecord>>,
    #[serde(default)]
    pub weapon_banks: CowCell<HashMap<i64, HashMap<i64, Vec<i64>>>>,
    #[serde(default)]
    pub asteroids: CowCell<HashMap<i64, AsteroidRecord>>,
    #[serde(default)]
    pub moon_extractions: CowCell<HashMap<i64, MoonExtractionRecord>>,
    #[serde(default)]
    pub auto_moon_miners: CowCell<HashMap<i64, AutoMoonMinerRecord>>,
    #[serde(default)]
    pub observer_mining_ledger: CowCell<HashMap<i64, Vec<ObserverMiningRecord>>>,
    #[serde(default)]
    pub owner_notes: crate::cow::CowMap<i64, OwnerNoteRecord>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub paused_skill_queues: HashSet<i64>,
    #[serde(default)]
    pub cosmic_anomalies: CowCell<HashMap<SolarSystemId, Vec<CosmicAnomalyRecord>>>,
    #[serde(default)]
    pub sov_prospecting_levels: CowCell<HashMap<SolarSystemId, u32>>,
    #[serde(default)]
    pub last_mission_decline_times: CowCell<HashMap<i64, u64>>,
    #[serde(default)]
    pub character_last_jump_unix: CowCell<HashMap<i64, u64>>,
    #[serde(default)]
    pub character_cspa_charges: CowCell<HashMap<i64, IskCents>>,
    #[serde(default)]
    pub mail_timestamps: CowMap<i64, u64>,
    #[serde(default)]
    pub character_login_stats: CowCell<HashMap<i64, CharacterLoginStats>>,
    #[serde(default)]
    pub item_custom_names: CowMap<i64, String>,
    #[serde(default)]
    pub item_positions: CowCell<HashMap<i64, [f64; 3]>>,
    #[serde(default)]
    pub structure_vulnerability_windows: CowCell<HashMap<i64, (u64, u64)>>,
    #[serde(default)]
    pub skill_history: CowCell<HashMap<i64, Vec<SkillHistoryEntry>>>,
    #[serde(default)]
    pub character_doll_info: CowCell<HashMap<i64, (String, String)>>,
    #[serde(default)]
    pub system_jump_events: CowCell<Vec<SystemJumpEvent>>,
    #[serde(default)]
    pub system_kill_events: CowCell<Vec<SystemKillEvent>>,
    #[serde(default)]
    pub incursions: CowCell<HashMap<u32, IncursionRecord>>,
    #[serde(default)]
    pub sov_campaign_scores: CowCell<HashMap<i64, SovCampaignScoreRecord>>,
    #[serde(default)]
    pub fw_character_stats: CowCell<HashMap<i64, FwCharacterStatsRecord>>,
    #[serde(default)]
    pub fw_corp_enlisted_on: CowCell<HashMap<i64, u64>>,
    #[serde(default)]
    pub fw_system_occupations: CowCell<HashMap<SolarSystemId, FwSystemOccupationRecord>>,
    #[serde(default)]
    pub fw_wars: CowCell<Vec<(i64, i64)>>,
    #[serde(default)]
    pub war_lifecycle_meta: CowCell<HashMap<i64, WarLifecycleMeta>>,
    #[serde(default)]
    pub killmail_details: CowCell<HashMap<i64, KillmailDetailRecord>>,
    #[serde(default)]
    pub station_office_rental_costs: CowCell<HashMap<i64, f64>>,
    #[serde(default)]
    pub alliance_lifecycle_meta: CowCell<HashMap<i64, AllianceLifecycleMeta>>,
    #[serde(default)]
    pub corp_hangar_divisions: CowCell<HashMap<i64, HashMap<u8, String>>>,
    #[serde(default)]
    pub structure_esi_timer_meta: CowCell<HashMap<i64, StructureEsiTimerMeta>>,
    #[serde(default)]
    pub corp_alliance_history: CowCell<HashMap<i64, Vec<CorpAllianceHistoryEntry>>>,
    #[serde(default)]
    pub corp_container_logs: CowCell<HashMap<i64, Vec<CorpContainerLogEntry>>>,
    #[serde(default)]
    pub corp_medals: CowCell<HashMap<i64, Vec<CorpMedalEntry>>>,
    #[serde(default)]
    pub corp_issued_medals: CowCell<HashMap<i64, Vec<CorpIssuedMedalEntry>>>,
    #[serde(default)]
    pub corp_member_limits: CowCell<HashMap<i64, i64>>,
    #[serde(default)]
    pub corp_titles: CowCell<HashMap<i64, Vec<CorpTitleEntry>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub corp_member_titles: CowCell<HashMap<(i64, i64), Vec<i64>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub corp_role_assignments: CowCell<HashMap<(i64, i64), CorpMemberRoleAssignment>>,
    #[serde(default)]
    pub corp_role_history: CowCell<HashMap<i64, Vec<CorpRoleHistoryEntry>>>,
    #[serde(default)]
    pub corp_shareholders: CowCell<HashMap<i64, Vec<CorpShareholderEntry>>>,
    #[serde(default)]
    pub starbase_settings: CowCell<HashMap<i64, StarbaseDetailSettings>>,
    #[serde(default)]
    pub corp_project_contributors: CowCell<HashMap<i64, Vec<CorpProjectContributorEntry>>>,
    #[serde(default)]
    pub char_research_agents: CowCell<HashMap<i64, Vec<CharacterResearchAgentRecord>>>,
    #[serde(default)]
    pub char_medals: CowCell<HashMap<i64, Vec<CharacterMedalRecord>>>,
    #[serde(default)]
    pub char_scoped_roles: CowCell<HashMap<i64, CharacterScopedRolesRecord>>,
    #[serde(default)]
    pub loyalty_store_offers: CowCell<HashMap<i64, Vec<LoyaltyStoreOfferRecord>>>,
    #[serde(default)]
    pub wallet_journal_timestamps: CowMap<i64, u64>,
    #[serde(default)]
    pub char_wallet_transactions: CowCell<HashMap<i64, Vec<WalletTransactionRecord>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub corp_wallet_journals: CowCell<HashMap<(i64, i64), Vec<CorpWalletJournalRecord>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub corp_wallet_transactions: CowCell<HashMap<(i64, i64), Vec<WalletTransactionRecord>>>,
    #[serde(default)]
    pub market_order_meta: CowCell<HashMap<i64, MarketOrderMeta>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub market_history: CowCell<HashMap<(u32, TypeId), Vec<MarketHistoryDayRecord>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub planetary_colony_layouts: CowCell<HashMap<(i64, i64), PlanetaryColonyLayoutRecord>>,
    #[serde(default)]
    pub corp_customs_offices: CowCell<HashMap<i64, Vec<CustomsOfficeRecord>>>,
    /// Journal parties/context metadata keyed by journal entry ID (`crate::ledger`).
    #[serde(default)]
    pub wallet_journal_meta: crate::cow::CowMap<i64, crate::ledger::JournalMeta>,
    /// Market fills recorded at fill time (`crate::ledger`).
    #[serde(default, with = "market_transactions_cow_serde")]
    pub market_transactions: crate::cow::CowMap<i64, crate::ledger::MarketTransactionRecord>,
    /// Account-wide PLEX Vault balances keyed by `user_id`.
    #[serde(default)]
    pub account_plex_vault: crate::cow::CowMap<u64, i64>,
    #[serde(default)]
    pub corp_vote_cases: CowCell<HashMap<i64, CorpVoteCaseRecord>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub corp_vote_ballots: CowCell<HashMap<(i64, i64), CorpVoteBallotRecord>>,
    #[serde(default)]
    pub orbital_provision_types: CowCell<HashMap<i64, i64>>,
    #[serde(default, with = "crate::durable_domain_types::cow_tuple_map_serde")]
    pub corp_lp_balances: crate::cow::CowMap<(i64, i64), i64>,
    #[serde(default)]
    pub char_lp_logs: CowLpLogs,
    #[serde(default)]
    pub next_alliance_id: i64,
    #[serde(default)]
    pub next_corporation_id: i64,
    #[serde(default)]
    pub next_notification_id: i64,
    #[serde(default)]
    pub next_report_id: i64,
    #[serde(default, with = "crate::durable_domain_types::cow_cell_sorted_set_serde")]
    pub trashed_mails: CowCell<HashSet<(i64, i64)>>,
    #[serde(default)]
    pub mailing_lists: CowCell<HashMap<i64, MailingListRecord>>,
    #[serde(default)]
    pub calendar_events: CowCell<HashMap<i64, CalendarEventRecord>>,
    #[serde(default)]
    pub char_contacts: CowCell<HashMap<i64, Vec<SocialContactRecord>>>,
    #[serde(default)]
    pub char_contact_notifications: CowCell<HashMap<i64, Vec<crate::durable_domain_types::SocialContactNotificationRecord>>>,
    #[serde(default)]
    pub char_contact_labels: CowCell<HashMap<i64, Vec<(i64, String)>>>,
    #[serde(default)]
    pub corp_contacts: CowCell<HashMap<i64, Vec<SocialContactRecord>>>,
    #[serde(default)]
    pub corp_contact_labels: CowCell<HashMap<i64, Vec<(i64, String, u32)>>>,
    #[serde(default)]
    pub alliance_contacts: CowCell<HashMap<i64, Vec<SocialContactRecord>>>,
    #[serde(default)]
    pub alliance_contact_labels: CowCell<HashMap<i64, Vec<(i64, String)>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub char_notes: CowCell<HashMap<(i64, i64), String>>,
    #[serde(default)]
    pub bookmark_subfolders: CowCell<HashMap<i64, i64>>,
    #[serde(default)]
    pub bookmark_subfolder_parents: CowCell<HashMap<i64, i64>>,
    #[serde(default)]
    pub bookmark_expiries: CowCell<HashMap<i64, u64>>,
    #[serde(default)]
    pub bookmark_folder_acls: CowCell<HashMap<i64, Vec<i64>>>,
    #[serde(default)]
    pub access_groups: CowCell<HashMap<i64, Vec<i64>>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub bookmark_folder_known_by: CowCell<HashMap<(i64, i64), bool>>,
    #[serde(default)]
    pub chat_channel_acl: CowCell<HashMap<String, ChatChannelAclRecord>>,
    #[serde(default)]
    pub corp_governance: CowCell<HashMap<i64, CorpGovState>>,
    #[serde(default)]
    pub employment_history: CowCell<HashMap<i64, Vec<(i64, u64, Option<u64>)>>>,
    #[serde(default)]
    pub alliance_war_state: CowCell<PersistedAllianceWarState>,
    #[serde(default)]
    pub war_state: CowCell<PersistedWarState>,
    #[serde(default)]
    pub fw_runtime: CowCell<PersistedFwState>,
    #[serde(default)]
    pub sov_runtime: CowCell<PersistedSovState>,
    #[serde(default)]
    pub insurgency_runtime: CowCell<PersistedInsurgencyState>,
    #[serde(default)]
    pub crimewatch_states: CowCell<HashMap<i64, PersistedCrimeWatchState>>,
    #[serde(default, with = "cow_cell_tuple_map_serde")]
    pub crimewatch_aggressions: CowCell<HashMap<(i64, i64), PersistedAggressionRecord>>,
    #[serde(default)]
    pub combat_crimewatch_state: CowCell<PersistedCombatCrimewatchState>,
    #[serde(default)]
    pub zarzakh_gate_locks: CowCell<HashMap<i64, (i64, u64)>>,
    #[serde(default, with = "crate::durable_domain_types::cow_cell_sorted_map_of_sets_serde")]
    pub structure_docking_acl_denied: CowCell<HashMap<i64, HashSet<i64>>>,
    #[serde(default)]
    pub sp6_lifecycle: Sp6PersistedState,
    #[serde(default)]
    pub pve_special_sites: crate::service_domains::PveSpecialSitesState,
    #[serde(default)]
    pub crab_beacons: CowCell<crate::service_domains::CrabBeaconStore>,
    #[serde(default)]
    pub abyssal_state: crate::service_domains::AbyssalRuntimeStore,
    #[serde(default)]
    pub dungeon_runtime: crate::service_domains::DungeonRuntimeStore,
    #[serde(default)]
    pub dungeon_editor: crate::service_domains::DungeonEditorStore,
    #[serde(default)]
    pub fob_observatory_state: CowCell<crate::service_domains::FobObservatoryStore>,
    #[serde(default)]
    pub mining_and_ess_state: CowCell<crate::service_domains::MiningAndEssRuntimeStore>,
    #[serde(default)]
    pub pvp_filament_state: CowCell<crate::service_domains::PvpFilamentStore>,
    #[serde(default)]
    pub tourney_state: CowCell<crate::service_domains::TourneyStore>,
    #[serde(default)]
    pub deployable_state: CowCell<crate::service_domains::MobileDeployableRuntimeState>,
    #[serde(default)]
    pub superweapon_runtime: CowCell<crate::service_domains::PersistedSuperweaponRuntime>,
    #[serde(default)]
    pub insurgency_complex_ops: CowCell<crate::service_domains::InsurgencyComplexOpsState>,
    #[serde(default)]
    pub node_graph_state: CowCell<crate::service_domains::NodeGraphAndProgressionStore>,
    #[serde(default)]
    pub seasons_and_campaigns: CowCell<crate::service_domains::SeasonsAndCampaignsStore>,
    #[serde(default)]
    pub quasar_commerce: CowCell<crate::service_domains::QuasarCommerceStore>,
    #[serde(default)]
    pub fleet_and_social_ops: CowCell<crate::service_domains::FleetAndSocialOpsStore>,
    #[serde(default)]
    pub cluster_and_runtime: CowCell<crate::service_domains::ClusterAndRuntimeStore>,
    #[serde(default, with = "crate::service_domains::entity_kv_map_serde")]
    pub entity_kv: std::collections::BTreeMap<
        crate::service_domains::EntityKvKey,
        crate::service_domains::EntityKvRow,
    >,
    #[serde(default)]
    pub insurance_contracts: CowCell<HashMap<i64, PersistedInsuranceContractRecord>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PersistedInsuranceContractRecord {
    pub ship_id: i64,
    pub ship_type_id: TypeId,
    pub owner_id: i64,
    pub level_name: String,
    pub level_fraction: f64,
    pub cost_isk: f64,
    pub payout_isk: f64,
    pub start_unix: u64,
    pub end_unix: u64,
}

pub use crate::{
    AbyssalPersistedSession, DisconnectedShipEntry, PersistedCombatHp, PersistedSpaceObjectMeta,
    SafeLogoffEntry, SelfDestructPersistedEntry, Sp6PersistedState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpRewardLogEntry {
    pub log_id: i64,
    pub corp_id: i64,
    pub amount: i64,
    pub reason: String,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterLoginStats {
    pub last_login_unix: u64,
    pub last_logout_unix: u64,
    pub logins_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillHistoryEntry {
    pub log_date_ft: i64,
    pub event_type_id: i64,
    pub skill_type_id: TypeId,
    pub skill_level: u8,
    pub absolute_points: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiningLedgerRecord {
    pub character_id: i64,
    pub solar_system_id: u32,
    pub type_id: u32,
    pub quantity: i64,
    pub date: String,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoonExtractionRecord {
    pub structure_id: i64,
    pub moon_id: i64,
    pub extraction_start_unix: u64,
    pub chunk_arrival_unix: u64,
    pub natural_decay_unix: u64,
    pub status: String, // "Started", "Ready", "Fractured", "Cancelled"
    pub auto_fracture: bool,
    #[serde(default)]
    pub event_id: i64,
    #[serde(default)]
    pub moon_ores: Vec<(TypeId, f64)>,
    #[serde(default)]
    pub spawned_asteroid_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoMoonMinerRecord {
    pub structure_id: i64,
    pub fuel_units_remaining: u32,
    pub magmatic_gas_remaining: u32,
    pub efficiency: f64,
    pub cycle_duration_seconds: u64,
    pub last_cycle_unix: u64,
    pub ore_type_id: TypeId,
    pub hourly_yield: u64,
    pub status: String, // "Active", "OutOfFuel", "Idle"
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserverMiningRecord {
    pub observer_id: i64,
    pub character_id: i64,
    pub recorded_corporation_id: i64,
    pub type_id: TypeId,
    pub quantity: i64,
    pub last_updated_date: String,
    pub timestamp_unix: u64,
}

mod market_transactions_cow_serde {
    use crate::cow::CowMap;
    use crate::ledger::MarketTransactionRecord;
    use serde::de::{MapAccess, SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(
        map: &CowMap<i64, MarketTransactionRecord>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(map.len()))?;
        for rec in map.values() {
            seq.serialize_element(rec)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CowMap<i64, MarketTransactionRecord>, D::Error> {
        struct MarketTxVisitor;

        impl<'de> Visitor<'de> for MarketTxVisitor {
            type Value = CowMap<i64, MarketTransactionRecord>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence or map of MarketTransactionRecord")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut map = CowMap::new();
                while let Some(rec) = seq.next_element::<MarketTransactionRecord>()? {
                    map.insert(rec.transaction_id, rec);
                }
                Ok(map)
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut map = CowMap::new();
                while let Some((k, rec)) = access.next_entry::<i64, MarketTransactionRecord>()? {
                    map.insert(k, rec);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_any(MarketTxVisitor)
    }
}

impl WorldStateSnapshot {
    // Transitional no-op shim for `sp6_lifecycle.rs` (owned by another track; `#915` follow-up cleanup).
    #[doc(hidden)] #[inline] pub fn mark_other_domains_dirty(&mut self) {}

    /// Persists a killmail stamped with the kill time and the victim's and final-blow attacker's
    /// corporation/alliance as they are at the moment of the kill. Affiliations already set on
    /// `km` (e.g. a structure's owning corporation) are kept when the id is not a known pilot.
    pub fn record_killmail(&mut self, mut km: KillmailRecord, killed_unix: u64) {
        km.killed_unix = killed_unix;
        if let Some(c) = self.characters.get(&km.victim_character_id) {
            km.victim_corp_id = c.corporation_id;
            km.victim_alliance_id = c.alliance_id;
        }
        if let Some(c) = self.characters.get(&km.attacker_character_id) {
            km.attacker_corp_id = c.corporation_id;
            km.attacker_alliance_id = c.alliance_id;
        }
        self.killmails.insert(km.kill_id, km);
    }

    pub fn record_system_jump(
        &mut self,
        solar_system_id: SolarSystemId,
        character_id: i64,
        timestamp_unix: u64,
    ) {
        self.system_jump_events
            .retain(|e| timestamp_unix.saturating_sub(e.timestamp_unix) <= 86_400);
        self.system_jump_events.push(SystemJumpEvent {
            solar_system_id,
            character_id,
            timestamp_unix,
        });
    }

    pub fn record_system_kill(
        &mut self,
        solar_system_id: SolarSystemId,
        ship_kills: u32,
        npc_kills: u32,
        pod_kills: u32,
        timestamp_unix: u64,
    ) {
        self.system_kill_events
            .retain(|e| timestamp_unix.saturating_sub(e.timestamp_unix) <= 86_400);
        self.system_kill_events.push(SystemKillEvent {
            solar_system_id,
            ship_kills,
            npc_kills,
            pod_kills,
            timestamp_unix,
        });
    }

    /// Computes `(is_ceo, effective_roles)` for `char_id` in `corp_id` at `location_id` and `now_unix`.
    /// Applies CEO immunity (`(true, u64::MAX)`), direct `corp.member_roles`, `corp_role_assignments`
    /// (global + HQ/base/other location scopes), `gov.titles` / `corp_titles`, and 24-hour maturation
    /// delays (`pending_role_grants`, `pending_scoped_role_grants`, `pending_title_role_grants`).
    /// Falls back to `(false, session_roles)` when `corp_id` is absent from `self.corporations`.
    pub fn effective_corp_roles_at(
        &self,
        corp_id: i64,
        char_id: i64,
        location_id: Option<i64>,
        session_roles: u64,
        now_unix: u64,
    ) -> (bool, u64) {
        let Some(corp) = self.corporations.get(&corp_id) else {
            let mut roles = session_roles;
            if let Some(ra) = self.corp_role_assignments.get(&(corp_id, char_id)) {
                roles |= ra.roles | ra.roles_at_hq | ra.roles_at_base | ra.roles_at_other;
            }
            return (false, roles);
        };
        if corp.ceo_id == char_id {
            return (true, u64::MAX);
        }
        let has_ceo_roles_entry = corp.member_roles.contains_key(&corp.ceo_id);
        let is_corp_member = corp.members.contains(&char_id)
            || (!has_ceo_roles_entry
                && self
                    .characters
                    .get(&char_id)
                    .is_some_and(|ch| ch.corporation_id == corp_id))
            || (corp.members.is_empty() && !self.characters.contains_key(&char_id));
        let mut roles = if let Some(&db_roles) = corp.member_roles.get(&char_id) {
            db_roles
        } else if self.corp_role_assignments.contains_key(&(corp_id, char_id))
            || !is_corp_member
            || session_roles == u64::MAX
        {
            0
        } else {
            session_roles
        };

        let char_home = self
            .characters
            .get(&char_id)
            .and_then(|ch| {
                ch.station_id
                    .or(ch.structure_id)
                    .or(Some(ch.home_station_id))
                    .filter(|&id| id > 0)
            })
            .unwrap_or(0);
        let hq_station_id = corp.hq_station_id.unwrap_or(char_home);
        let mut loc_station_id = location_id
            .or_else(|| {
                self.characters
                    .get(&char_id)
                    .and_then(|ch| ch.station_id.or(ch.structure_id))
            })
            .unwrap_or(0);
        for _ in 0..8 {
            if loc_station_id <= 0
                || (60_000_000..=69_999_999).contains(&loc_station_id)
                || loc_station_id >= 1_000_000_000_000
                || self.structures.contains_key(&loc_station_id)
            {
                break;
            }
            let Some(it) = self.items.get(&loc_station_id) else {
                break;
            };
            if it.location_id <= 0 || it.location_id == loc_station_id {
                break;
            }
            loc_station_id = it.location_id;
        }
        let gov_opt = self.corp_governance.get(&corp_id);
        let base_id = gov_opt
            .and_then(|g| g.member_bases.get(&char_id).copied())
            .unwrap_or(0);

        if let Some(ra) = self.corp_role_assignments.get(&(corp_id, char_id)) {
            roles |= ra.roles;
            if loc_station_id > 0 {
                if loc_station_id == hq_station_id {
                    roles |= ra.roles_at_hq;
                }
                if base_id > 0 && loc_station_id == base_id {
                    roles |= ra.roles_at_base;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    roles |= ra.roles_at_other;
                }
            }
        }

        let mut unmatured_role_bits = 0u64;
        let mut unmatured_title_bits = 0u64;
        if let Some(gov) = gov_opt {
            if let Some(&(p_mask, matured_at)) = gov.pending_role_grants.get(&char_id) {
                if now_unix >= matured_at {
                    roles |= p_mask;
                } else {
                    // Only roles subject to 24h stasis (`CORP_DELAYED_DIVISION_ROLE_MASK`, Director,
                    // and Container Take bits) are suppressed while pending; `UpdateMember` also packs
                    // immediate roles like Accountant/Trader into `pending_role_grants`.
                    const DELAYED_STASIS_MASK: u64 =
                        crate::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK
                            | crate::corp_roles::ROLE_DIRECTOR
                            | (0x7Fu64 << 42);
                    unmatured_role_bits |= p_mask & DELAYED_STASIS_MASK;
                }
            }
            if let Some(&(hq_p, base_p, other_p, matured_at)) =
                gov.pending_scoped_role_grants.get(&char_id)
            {
                if loc_station_id > 0 {
                    let mut scoped_p = 0u64;
                    if loc_station_id == hq_station_id {
                        scoped_p |= hq_p;
                    }
                    if base_id > 0 && loc_station_id == base_id {
                        scoped_p |= base_p;
                    }
                    if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id)
                    {
                        scoped_p |= other_p;
                    }
                    if now_unix >= matured_at {
                        roles |= scoped_p;
                    } else {
                        unmatured_role_bits |= scoped_p;
                    }
                }
            }
            if let Some(&(t_mask, matured_at)) = gov.pending_title_role_grants.get(&char_id) {
                if now_unix >= matured_at {
                    roles |= t_mask;
                } else {
                    unmatured_title_bits |= t_mask;
                }
            }
            if let Some(&tmask) = gov.member_titles.get(&char_id) {
                for (&tid, &(_, troles)) in &gov.titles {
                    let bit = title_id_to_bit(tid);
                    if (tmask & bit) != 0 {
                        roles |= troles & !unmatured_title_bits;
                    }
                }
            }
        }

        let member_tmask = self
            .corp_governance
            .get(&corp_id)
            .and_then(|g| g.member_titles.get(&char_id).copied())
            .unwrap_or(0);
        let tids_opt = self.corp_member_titles.get(&(corp_id, char_id));
        if let Some(titles) = self.corp_titles.get(&corp_id) {
            for t in titles.iter().filter(|t| {
                tids_opt.is_some_and(|tids| tids.contains(&t.title_id))
                    || (member_tmask & title_id_to_bit(t.title_id)) != 0
            }) {
                roles |= crate::corp_roles::encode_corp_role_names(&t.roles) & !unmatured_title_bits;
                if loc_station_id > 0 {
                    if loc_station_id == hq_station_id {
                        roles |= crate::corp_roles::encode_corp_role_names(&t.roles_at_hq)
                            & !unmatured_title_bits;
                    }
                    if base_id > 0 && loc_station_id == base_id {
                        roles |= crate::corp_roles::encode_corp_role_names(&t.roles_at_base)
                            & !unmatured_title_bits;
                    }
                    if loc_station_id != hq_station_id
                        && (base_id <= 0 || loc_station_id != base_id)
                    {
                        roles |= crate::corp_roles::encode_corp_role_names(&t.roles_at_other)
                            & !unmatured_title_bits;
                    }
                }
            }
        }

        roles &= !(unmatured_role_bits | unmatured_title_bits);
        (false, roles)
    }

    /// Ensures the 16 canonical V24 power-of-two corporation title slots (`1i64 << i` for `i in 0..16`,
    /// i.e. `1, 2, 4, 8, ..., 32768`) exist in `corp_governance[corp_id].titles` and `corp_titles[corp_id]`.
    pub fn seed_default_corp_titles(&mut self, corp_id: i64) {
        let state = self.corp_governance.entry(corp_id).or_default();
        let entries = self.corp_titles.entry(corp_id).or_default();
        for i in 0..16u32 {
            let tid = 1i64 << i;
            state
                .titles
                .entry(tid)
                .or_insert_with(|| (String::new(), 0));
            if !entries.iter().any(|t| t.title_id == tid) {
                entries.push(CorpTitleEntry {
                    title_id: tid,
                    name: String::new(),
                    ..Default::default()
                });
            }
        }
        entries.sort_by_key(|t| t.title_id);
    }

    /// Returns `true` if `char_id` is within the 24-hour (`86_400s`) role-drop stasis window in `corp_id`.
    pub fn is_in_role_drop_stasis(&self, corp_id: i64, char_id: i64, now_unix: u64) -> bool {
        self.corp_governance
            .get(&corp_id)
            .and_then(|g| g.role_drop_timestamps.get(&char_id).copied())
            .is_some_and(|drop_ts| now_unix < drop_ts.saturating_add(86_400))
    }

    /// Returns `true` if `item_id` is escrowed in an active contract, industry job, direct trade, or HyperNet raffle.
    pub fn is_item_locked_in_contract_or_job(&self, item_id: i64) -> bool {
        self.is_item_locked_excluding_trade(item_id, None)
    }

    /// Returns `true` if `item_id` (or an ancestor container up to 8 hops) is escrowed in an active contract, industry job, direct trade (optionally excluding `exempt_trade_id`), or HyperNet raffle.
    pub fn is_item_locked_excluding_trade(&self, item_id: i64, exempt_trade_id: Option<i64>) -> bool {
        #[derive(serde::Deserialize)]
        struct TradeRef { #[serde(default)] trade_id: i64, #[serde(default)] char_a: i64, #[serde(default)] char_b: i64, #[serde(default)] station_id: i64, #[serde(default)] items_a: Vec<(i64, i32)>, #[serde(default)] items_b: Vec<(i64, i32)>, #[serde(default)] completed: bool, #[serde(default)] cancelled: bool }
        #[derive(serde::Deserialize)]
        struct RaffleRef { item_id: i64, #[serde(default)] winner_id: Option<i64>, #[serde(default)] expired: bool }
        let is_direct_lock = |check_id: i64| -> bool {
            self.contracts.values().any(|c| matches!(c.status.as_str(), "Outstanding" | "InProgress") && c.item_ids.contains(&check_id))
                || self.industry_jobs.values().any(|j| matches!(j.status.as_str(), "Active" | "Paused") && j.blueprint_item_id == check_id)
                || self.owner_notes.values().any(|n| match n.character_id {
                    -73 => serde_json::from_str::<TradeRef>(&n.note).is_ok_and(|t| {
                        let both_docked = t.station_id <= 0 || (
                            self.characters.get(&t.char_a).is_none_or(|c| c.station_id.or(c.structure_id) == Some(t.station_id))
                            && self.characters.get(&t.char_b).is_none_or(|c| c.station_id.or(c.structure_id) == Some(t.station_id))
                        );
                        exempt_trade_id != Some(t.trade_id) && !t.completed && !t.cancelled && both_docked && (t.items_a.iter().any(|(id, _)| *id == check_id) || t.items_b.iter().any(|(id, _)| *id == check_id))
                    }),
                    -74 => serde_json::from_str::<RaffleRef>(&n.note).is_ok_and(|r| !r.expired && r.winner_id.is_none() && r.item_id == check_id),
                    _ => false,
                })
        };
        let mut cur_id = item_id;
        let mut visited = Vec::with_capacity(9);
        for _ in 0..=8 {
            visited.push(cur_id);
            if is_direct_lock(cur_id) {
                return true;
            }
            let Some(item) = self.items.get(&cur_id) else {
                break;
            };
            let next_id = item.location_id;
            if next_id <= 0 || visited.contains(&next_id) || self.structures.contains_key(&next_id) {
                break;
            }
            cur_id = next_id;
        }
        false
    }

    /// Returns `true` if `item_id` is saved in `StructureExtendedState.previous_ship_by_structure`
    /// (`item_custom_names[-7_700_001]`) for a structure currently controlled in `active_pilot_by_structure`.
    pub fn is_structure_controller_saved_ship(&self, item_id: i64) -> bool {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct CtrlShips { active_pilot_by_structure: std::collections::HashMap<i64, i64>, previous_ship_by_structure: std::collections::HashMap<i64, i64> }
        item_id > 0 && self.item_custom_names.get(&-7_700_001)
            .and_then(|raw| serde_json::from_str::<CtrlShips>(raw).ok())
            .is_some_and(|st| st.previous_ship_by_structure.iter().any(|(sid, &sh)| sh == item_id && st.active_pilot_by_structure.contains_key(sid)))
    }

    /// Returns `true` if `item_id` is `char_id`'s `active_ship_id`, matches `session_ship_id`, or is saved in `previous_ship_by_structure`.
    pub fn is_character_boarded_or_saved_ship(&self, char_id: i64, session_ship_id: Option<i64>, item_id: i64) -> bool {
        item_id > 0 && (session_ship_id == Some(item_id)
            || self.characters.get(&char_id).is_some_and(|c| c.active_ship_id == item_id)
            || self.is_structure_controller_saved_ship(item_id))
    }

    /// Verifies that `ship_id` exists, is owned by `char_id`, and is physically co-located with the character
    /// (in `station_or_struct_id` when docked, or in `ch.solar_system_id` when in space).
    pub fn is_ship_colocated_with_character(
        &self,
        char_id: i64,
        ship_id: i64,
        station_or_struct_id: Option<i64>,
    ) -> bool {
        let Some(ship) = self.items.get(&ship_id) else {
            return false;
        };
        if ship.owner_id != char_id {
            return false;
        }
        if let Some(dock_id) = station_or_struct_id {
            ship.location_id == dock_id
        } else if let Some(ch) = self.characters.get(&char_id) {
            ship.location_id == ch.solar_system_id as i64
        } else {
            false
        }
    }

    /// Shared predicate verifying that `item` is owned by `char_id`, is unlocked and un-impounded
    /// across its entire ancestor chain, is not the character's currently boarded or structure-controlled
    /// ship, is not fitted, is not a non-empty container, is not escrowed in an active contract/job/trade,
    /// and is physically located in the caller's current docked Hangar or co-located active ship Cargo.
    pub fn is_item_consumable_by(
        &self,
        item: &InventoryItemRecord,
        char_id: i64,
        station_or_struct_id: Option<i64>,
        active_ship_id: Option<i64>,
    ) -> bool {
        self.is_item_consumable_by_inner(item, char_id, station_or_struct_id, active_ship_id, true)
    }

    pub fn is_item_consumable_by_ignoring_contents(
        &self,
        item: &InventoryItemRecord,
        char_id: i64,
        station_or_struct_id: Option<i64>,
        active_ship_id: Option<i64>,
    ) -> bool {
        self.is_item_consumable_by_inner(item, char_id, station_or_struct_id, active_ship_id, false)
    }

    fn is_item_consumable_by_inner(
        &self,
        item: &InventoryItemRecord,
        char_id: i64,
        station_or_struct_id: Option<i64>,
        active_ship_id: Option<i64>,
        require_empty: bool,
    ) -> bool {
        if item.owner_id != char_id || item.quantity <= 0 {
            return false;
        }
        if self.is_item_or_ancestor_locked_or_impounded(item.item_id) {
            return false;
        }
        if Self::is_fitted_slot_flag(item.flag)
            || (require_empty && self.is_container_non_empty(item.item_id))
        {
            return false;
        }
        if self.is_character_boarded_or_saved_ship(char_id, active_ship_id, item.item_id)
            || self.is_structure_controller_saved_ship(item.item_id)
        {
            return false;
        }
        if let Some(loc_id) = station_or_struct_id {
            if !self.has_structure_acl_access(loc_id, char_id) {
                return false;
            }
        }
        let at_dock_hangar = station_or_struct_id.is_some()
            && Some(item.location_id) == station_or_struct_id
            && item.flag == InventoryFlag::Hangar;
        let in_ship_cargo = active_ship_id
            .filter(|&sid| {
                !self.is_item_locked_in_contract_or_job(sid)
                    && !self.is_item_or_ancestor_locked_or_impounded(sid)
                    && self.is_ship_colocated_with_character(char_id, sid, station_or_struct_id)
            })
            .is_some()
            && Some(item.location_id) == active_ship_id
            && item.flag == InventoryFlag::Cargo;
        at_dock_hangar || in_ship_cargo
    }

    /// Credits a contract issuer (corporation master wallet division 1000 when `c.for_corp`,
    /// or character wallet when `!c.for_corp`) (`feat:lifecycle.corp-contract-wallet`, `plan:E-3`).
    pub fn credit_contract_issuer(&mut self, c: &ContractRecord, amount: IskCents) {
        if amount.0 <= 0 {
            return;
        }
        if c.for_corp {
            if let Some(corp) = self.corporations.get_mut(&c.issuer_corp_id) {
                corp.credit_division(1, amount);
                return;
            }
        }
        if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
            issuer.balance = issuer.balance.checked_add(amount).unwrap_or(issuer.balance);
        }
    }

    /// Collects all descendant item IDs contained inside `root_id` (excluding `root_id` itself)
    /// in breadth-first deterministic order with cycle protection.
    pub fn collect_item_descendants(&self, root_id: i64) -> Vec<i64> {
        self.collect_item_descendants_many(&[root_id])
    }

    /// Collects all descendant item IDs nested under any item in `root_ids` (excluding the roots
    /// themselves), in breadth-first deterministic order with cycle protection.
    pub fn collect_item_descendants_many(&self, root_ids: &[i64]) -> Vec<i64> {
        if root_ids.is_empty() {
            return Vec::new();
        }
        let mut by_parent: HashMap<i64, Vec<i64>> = HashMap::new();
        for (&iid, it) in &self.items {
            if it.location_id != iid {
                by_parent.entry(it.location_id).or_default().push(iid);
            }
        }
        for children in by_parent.values_mut() {
            children.sort_unstable();
        }
        let mut result = Vec::new();
        let mut visited: HashSet<i64> = root_ids.iter().copied().collect();
        let mut queue: std::collections::VecDeque<i64> = root_ids.iter().copied().collect();
        while let Some(parent_id) = queue.pop_front() {
            if let Some(children) = by_parent.get(&parent_id) {
                for &cid in children {
                    if visited.insert(cid) {
                        result.push(cid);
                        queue.push_back(cid);
                    }
                }
            }
        }
        result
    }

    /// Transfers ownership of all nested descendants of `root_ids` to `target_owner_id` without
    /// altering their `location_id` or `flag`, clearing any armed self-destruct timers and
    /// returning every descendant ID whose `owner_id` changed.
    pub fn transfer_descendant_item_ownership(
        &mut self,
        root_ids: &[i64],
        target_owner_id: i64,
    ) -> Vec<i64> {
        let descendants = self.collect_item_descendants_many(root_ids);
        let mut transferred = Vec::new();
        for child_id in descendants {
            self.sp6_lifecycle.self_destructs.remove(&child_id);
            if let Some(child) = self.items.get_mut(&child_id) {
                if child.owner_id != target_owner_id {
                    transferred.push(child_id);
                }
                child.owner_id = target_owner_id;
                child.version += 1;
            }
        }
        transferred
    }

    /// Completes escrow settlement of `item_ids` (plus any orphaned items directly at
    /// `contract_id` when `include_contract_location_orphans` is true): moves top-level roots to
    /// `target_location_id` with `target_flag` (or restores each root's saved `ContractOrigFlag`
    /// defaulting to `Hangar` when `target_flag` is `None`), transfers ownership of both top-level
    /// roots and all nested descendants to `target_owner_id`, clears `ContractOrigFlag` entries
    /// and `self_destructs`, and returns every item ID whose `owner_id` changed.
    pub fn settle_contract_escrow_items(
        &mut self,
        contract_id: i64,
        item_ids: &[i64],
        target_owner_id: i64,
        target_location_id: i64,
        target_flag: Option<InventoryFlag>,
        include_contract_location_orphans: bool,
    ) -> Vec<i64> {
        let mut roots: Vec<i64> = item_ids.to_vec();
        if include_contract_location_orphans {
            let mut extra_ids: Vec<i64> = self
                .items
                .values()
                .filter(|item| item.location_id == contract_id && !roots.contains(&item.item_id))
                .map(|item| item.item_id)
                .collect();
            extra_ids.sort_unstable();
            roots.extend(extra_ids);
        }
        let descendants = self.collect_item_descendants_many(&roots);
        let mut transferred_ids = Vec::new();
        for &iid in &roots {
            let iid_sub = iid.to_string();
            let kv_flag = self
                .kv(crate::service_domains::Ns::ContractOrigFlag)
                .get::<u16>(contract_id, &iid_sub);
            self.kv_mut(crate::service_domains::Ns::ContractOrigFlag)
                .remove(contract_id, &iid_sub);
            let blob_flag = self
                .service_state_blobs
                .remove(&format!("contract_orig_flag:{contract_id}:{iid}"))
                .and_then(|s| s.parse::<u16>().ok());
            let resolved_flag = target_flag.unwrap_or_else(|| {
                kv_flag
                    .or(blob_flag)
                    .map(InventoryFlag::from_u16)
                    .filter(|f| *f != InventoryFlag::Locked)
                    .unwrap_or(InventoryFlag::Hangar)
            });
            self.sp6_lifecycle.self_destructs.remove(&iid);
            if let Some(item) = self.items.get_mut(&iid) {
                if item.owner_id != target_owner_id {
                    transferred_ids.push(iid);
                }
                item.owner_id = target_owner_id;
                item.location_id = target_location_id;
                item.flag = resolved_flag;
                item.version += 1;
            }
        }
        for &child_id in &descendants {
            self.sp6_lifecycle.self_destructs.remove(&child_id);
            if let Some(child) = self.items.get_mut(&child_id) {
                if child.owner_id != target_owner_id {
                    transferred_ids.push(child_id);
                }
                child.owner_id = target_owner_id;
                child.version += 1;
            }
        }
        transferred_ids
    }

    /// Resolves an expired `Outstanding` or `InProgress` contract in-place.
    pub fn resolve_expired_contract(
        &mut self,
        contract_id: i64,
        now_unix: u64,
    ) -> Option<&'static str> {
        let c = self.contracts.get(&contract_id)?.clone();
        if c.expiration_unix == 0 || now_unix < c.expiration_unix {
            return None;
        }
        let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 {
            c.issuer_corp_id
        } else {
            c.issuer_id
        };
        match c.status.as_str() {
            "Outstanding" => {
                if c.contract_type == "Auction" {
                    if let Some(winner_id) = c
                        .current_bidder_id
                        .or(c.acceptor_id)
                        .filter(|&id| id > 0 && c.current_bid.0 > 0)
                    {
                        self.credit_contract_issuer(&c, c.current_bid);
                        if let Some(dep_raw) = self
                            .fleet_and_social_ops
                            .contract_deposits
                            .remove(&contract_id)
                            .or_else(|| {
                                self.service_state_blobs
                                    .remove(&format!("contract_deposit:{contract_id}"))
                                    .and_then(|s| s.parse::<i64>().ok())
                            })
                        {
                            self.credit_contract_issuer(&c, IskCents(dep_raw));
                        }
                        let winner_flag = if self.corporations.contains_key(&winner_id) {
                            InventoryFlag::CorpDeliveries
                        } else {
                            InventoryFlag::Hangar
                        };
                        let transferred_ids = self.settle_contract_escrow_items(
                            contract_id,
                            &c.item_ids,
                            winner_id,
                            c.start_station_id,
                            Some(winner_flag),
                            true,
                        );
                        self.kv_mut(crate::service_domains::Ns::ContractBidDiv)
                            .clear_entity(contract_id);
                        self.strip_ship_cosmetics(&transferred_ids);
                        if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                            c_mut.acceptor_id = Some(winner_id);
                            c_mut.status = "Completed".into();
                        }
                        return Some("Completed");
                    }
                }
                self.credit_contract_issuer(&c, c.reward);
                if let Some(dep_raw) = self
                    .fleet_and_social_ops
                    .contract_deposits
                    .remove(&contract_id)
                    .or_else(|| {
                        self.service_state_blobs
                            .remove(&format!("contract_deposit:{contract_id}"))
                            .and_then(|s| s.parse::<i64>().ok())
                    })
                {
                    self.credit_contract_issuer(&c, IskCents(dep_raw));
                }
                let transferred_ids = self.settle_contract_escrow_items(
                    contract_id,
                    &c.item_ids,
                    issuer_entity_id,
                    c.start_station_id,
                    None,
                    true,
                );
                if !transferred_ids.is_empty() {
                    self.strip_ship_cosmetics(&transferred_ids);
                }
                self.kv_mut(crate::service_domains::Ns::ContractBidDiv)
                    .clear_entity(contract_id);
                if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                    c_mut.status = "Expired".into();
                }
                Some("Expired")
            }
            "InProgress" => {
                self.fail_courier_contract(contract_id, now_unix);
                Some("Failed")
            }
            _ => None,
        }
    }

    /// Appends an entry to the wallet journal with auto-incrementing ID.
    pub fn append_journal(
        &mut self,
        character_id: i64,
        entry_type: impl Into<String>,
        amount: IskCents,
        balance_after: IskCents,
        reason: impl Into<String>,
    ) -> i64 {
        let entry_id = self.alloc_entity_id();
        self.wallet_journal.push(WalletJournalEntry {
            entry_id,
            character_id,
            entry_type: entry_type.into(),
            amount,
            balance_after,
            reason: reason.into(),
        });
        entry_id
    }

    /// Fails an active in-progress courier contract, paying collateral + reward to issuer
    /// and unwrapping the goods in-place for the acceptor without teleporting them (`feat:lifecycle.courier-package-destroyed`).
    pub fn fail_courier_contract(&mut self, contract_id: i64, _now_unix: u64) -> bool {
        let Some(c) = self.contracts.get(&contract_id).cloned() else { return false };
        if c.contract_type != "Courier" || c.status != "InProgress" {
            return false;
        }
        let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 { c.issuer_corp_id } else { c.issuer_id };
        let refund_and_collateral = c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
        self.credit_contract_issuer(&c, refund_and_collateral);
        if let Some(dep_raw) = self
            .fleet_and_social_ops
            .contract_deposits
            .remove(&contract_id)
            .or_else(|| {
                self.service_state_blobs
                    .remove(&format!("contract_deposit:{contract_id}"))
                    .and_then(|s| s.parse::<i64>().ok())
            })
        {
            self.credit_contract_issuer(&c, IskCents(dep_raw));
        }
        let target_owner = c.acceptor_id.unwrap_or(issuer_entity_id);
        let mut transferred_ids = self.transfer_descendant_item_ownership(&c.item_ids, target_owner);
        let mut wraps_to_remove = Vec::new();
        for iid in &c.item_ids {
            let parent_loc = self.items.get(iid).map(|it| it.location_id).unwrap_or(0);
            if let Some(wloc) = self.items.get(&parent_loc).filter(|p| p.type_id == 3468).map(|p| p.location_id) {
                wraps_to_remove.push(parent_loc);
                if let Some(item) = self.items.get_mut(iid) {
                    item.location_id = wloc;
                }
            }
            let in_ship_container = self.items.get(iid).is_some_and(|item| self.items.contains_key(&item.location_id));
            if let Some(item) = self.items.get_mut(iid) {
                if item.owner_id != target_owner {
                    transferred_ids.push(*iid);
                }
                item.owner_id = target_owner;
                item.flag = if in_ship_container { InventoryFlag::Cargo } else { InventoryFlag::Hangar };
                item.version += 1;
            }
        }
        if !transferred_ids.is_empty() {
            self.strip_ship_cosmetics(&transferred_ids);
        }
        for wid in wraps_to_remove {
            self.items.remove(&wid);
        }
        if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
            c_mut.status = "Failed".into();
        }
        true
    }

    /// Returns `true` if `candidate` is already referenced in any ID-bearing collection on this
    /// snapshot (`#1087`).
    pub fn is_entity_id_referenced(&self, candidate: i64) -> bool {
        if candidate <= 0 {
            return false;
        }
        if self.characters.contains_key(&candidate)
            || self.items.contains_key(&candidate)
            || self.transient_items.contains_key(&candidate)
            || self.structures.contains_key(&candidate)
            || self.saved_fittings.contains_key(&candidate)
            || self.market_orders.contains_key(&candidate)
            || self.contracts.contains_key(&candidate)
            || self.industry_jobs.contains_key(&candidate)
            || self.bookmarks.contains_key(&candidate)
            || self.bookmark_folders.contains_key(&candidate)
            || self.mailing_lists.contains_key(&candidate)
            || self.calendar_events.contains_key(&candidate)
            || self.fleets.contains_key(&candidate)
            || self.wars.contains_key(&candidate)
            || self.corp_projects.contains_key(&candidate)
            || self.killmails.contains_key(&candidate)
            || self.kill_rights.contains_key(&candidate)
            || self.market_transactions.contains_key(&candidate)
            || self.wallet_journal_meta.contains_key(&candidate)
            || self.asteroids.contains_key(&candidate)
            || self.corporations.contains_key(&candidate)
            || self.alliances.contains_key(&candidate)
            || self.access_groups.contains_key(&candidate)
            || self.mail_timestamps.contains_key(&candidate)
            || self.structure_vulnerability_windows.contains_key(&candidate)
            || self.bookmark_expiries.contains_key(&candidate)
            || self.bookmark_subfolders.contains_key(&candidate)
            || self.item_positions.contains_key(&candidate)
            || self.item_custom_names.contains_key(&candidate)
            || self.applied_ship_skins.contains_key(&candidate)
            || self.owner_notes.contains_key(&candidate)
        {
            return true;
        }
        if let Some(notif_key) = (-7_900_000_000_000_i64).checked_sub(candidate) {
            if self.owner_notes.contains_key(&notif_key) {
                return true;
            }
        }
        if let Some(report_key) = 780_000_000_i64.checked_add(candidate) {
            if self.owner_notes.contains_key(&report_key) {
                return true;
            }
        }
        self.mails.contains_id(candidate)
            || self.wallet_journal.contains_id(candidate)
            || self.char_lp_logs.contains_log_id(candidate)
            || self.active_missions.contains_spawned_npc_id(candidate)
            || crate::service_domains::entity_kv::Ns::ALL.iter().any(|&ns| {
                self.entity_kv
                    .range(crate::service_domains::entity_kv::EntityKvKey {
                        ns,
                        entity_id: candidate,
                        sub_key: String::new(),
                    }..)
                    .next()
                    .is_some_and(|(k, _)| k.ns == ns && k.entity_id == candidate)
            })
    }

    fn entity_id_sources(&self) -> impl Iterator<Item = i64> + '_ {
        self.characters
            .keys()
            .copied()
            .chain(self.items.keys().copied())
            .chain(self.transient_items.keys().copied())
            .chain(self.structures.keys().copied())
            .chain(self.saved_fittings.keys().copied())
            .chain(self.market_orders.keys().copied())
            .chain(self.wallet_journal.iter().map(|e| e.entry_id))
            .chain(self.contracts.keys().copied())
            .chain(self.industry_jobs.keys().copied())
            .chain(self.bookmarks.keys().copied())
            .chain(self.bookmark_folders.keys().copied())
            .chain(self.mailing_lists.keys().copied())
            .chain(self.calendar_events.keys().copied())
            .chain(self.mails.iter().map(|m| m.message_id))
            .chain(self.fleets.keys().copied())
            .chain(self.wars.keys().copied())
            .chain(self.corp_projects.keys().copied())
            .chain(self.killmails.keys().copied())
            .chain(self.kill_rights.keys().copied())
            .chain(self.market_transactions.keys().copied())
            .chain(self.wallet_journal_meta.keys().copied())
            .chain(self.asteroids.keys().copied())
            .chain(self.corporations.keys().copied())
            .chain(self.alliances.keys().copied())
            .chain(self.access_groups.keys().copied())
            .chain(self.mail_timestamps.keys().copied())
            .chain(self.structure_vulnerability_windows.keys().copied())
            .chain(self.bookmark_expiries.keys().copied())
            .chain(self.bookmark_subfolders.keys().copied())
            .chain(self.item_positions.keys().copied())
            .chain(self.item_custom_names.keys().copied())
            .chain(self.applied_ship_skins.keys().copied())
            .chain(self.entity_kv.keys().map(|k| k.entity_id))
            .chain(self.char_lp_logs.values().flatten().map(|l| l.log_id))
            .chain(
                self.active_missions
                    .values()
                    .flat_map(|m| m.spawned_npc_ball_ids.iter().copied()),
            )
            .chain(self.owner_notes.values().filter_map(decode_owner_note_entity_id))
    }

    /// Collects all positive entity IDs referenced across all ID-bearing tables in this snapshot (`#1087`).
    pub fn collect_referenced_entity_ids(&self) -> HashSet<i64> {
        let mut ids: HashSet<i64> = self.entity_id_sources().filter(|&id| id > 0).collect();
        // Also retain raw note_id when positive (even for -78 sentinel records).
        for n in self.owner_notes.values() {
            if n.note_id > 0 {
                ids.insert(n.note_id);
            }
        }
        ids
    }

    /// Allocates a strictly monotonic dynamic entity/ball ID from `self.next_entity_id`
    /// persisted in `world_meta`, skipping any candidate still referenced in any ID-bearing table
    /// (`feat:invariant.ids-monotonic-across-restart`, `#1087`).
    pub fn alloc_entity_id(&mut self) -> i64 {
        let mut next = self.next_entity_id.max(200_000_000).saturating_add(1);
        while self.is_entity_id_referenced(next) {
            next = next.saturating_add(1);
        }
        self.next_entity_id = next;
        next
    }

    /// Allocates a strictly monotonic alliance ID persisted in `self.next_alliance_id` and `world_meta`,
    /// guaranteed strictly greater than any existing alliance in `self.alliances` (`feat:invariant.ids-monotonic-across-restart`).
    pub fn allocate_alliance_id(&mut self) -> i64 {
        self.next_entity_id += 1;
        let max_existing = self.alliances.keys().copied().max().unwrap_or(99_000_000);
        let next = self.next_alliance_id.max(99_000_000).max(max_existing) + 1;
        self.next_alliance_id = next;
        next
    }

    /// Allocates a strictly monotonic corporation ID persisted in `self.next_corporation_id` and `world_meta`,
    /// guaranteed strictly greater than any existing corporation in `self.corporations` (`feat:invariant.ids-monotonic-across-restart`).
    pub fn allocate_corporation_id(&mut self) -> i64 {
        self.next_entity_id += 1;
        let max_existing = self.corporations.keys().copied().max().unwrap_or(98_000_000);
        let next = self
            .next_corporation_id
            .max(98_100_000)
            .max(max_existing)
            + 1;
        self.next_corporation_id = next;
        next
    }

    /// Allocates a strictly monotonic notification ID persisted in `self.next_notification_id` and `world_meta`.
    pub fn allocate_notification_id(&mut self) -> i64 {
        self.allocate_notification_id_above(500_000)
    }

    /// Allocates a strictly monotonic notification ID above `floor` persisted in `self.next_notification_id` and `world_meta`.
    pub fn allocate_notification_id_above(&mut self, floor: i64) -> i64 {
        self.next_entity_id += 1;
        let max_existing = self
            .owner_notes
            .values()
            .filter(|n| n.character_id == -79 && n.note_id < -7_900_000_000_000)
            .map(|n| (-n.note_id) - 7_900_000_000_000)
            .max()
            .unwrap_or(500_000);
        let next = self
            .next_notification_id
            .max(500_000)
            .max(floor)
            .max(max_existing)
            + 1;
        self.next_notification_id = next;
        next
    }

    /// Allocates a strictly monotonic in-game report ID persisted in `self.next_report_id` and `world_meta`,
    /// guaranteed strictly greater than any existing report in `self.owner_notes`.
    pub fn allocate_report_id(&mut self) -> i64 {
        self.next_entity_id += 1;
        let max_existing = self
            .owner_notes
            .values()
            .filter(|n| n.character_id == -78 && n.note_id > 780_000_000)
            .map(|n| n.note_id - 780_000_000)
            .max()
            .unwrap_or(700_000);
        let next = self.next_report_id.max(700_000).max(max_existing) + 1;
        self.next_report_id = next;
        next
    }

    /// Reconciles all ID counters on load so every counter is at least the maximum ID present across
    /// all persisted entity tables (`feat:invariant.ids-monotonic-across-restart`, `#1087`).
    pub fn reconcile_id_counters(&mut self) {
        let max_entity = self
            .entity_id_sources()
            .filter(|&id| id > 0 && id < 900_000_000)
            .max()
            .unwrap_or(200_000_000);
        self.next_entity_id = self.next_entity_id.max(200_000_000).max(max_entity);

        if let Some(max_aid) = self.alliances.keys().copied().max() {
            self.next_alliance_id = self.next_alliance_id.max(99_000_000).max(max_aid);
        }
        if let Some(max_cid) = self.corporations.keys().copied().max() {
            if max_cid > 98_000_000 {
                self.next_corporation_id = self.next_corporation_id.max(max_cid);
            }
        }
        if let Some(max_nid) = self
            .owner_notes
            .values()
            .filter(|n| n.character_id == -79 && n.note_id < -7_900_000_000_000)
            .map(|n| (-n.note_id) - 7_900_000_000_000)
            .max()
        {
            self.next_notification_id = self.next_notification_id.max(500_000).max(max_nid);
        }
        if let Some(max_rid) = self
            .owner_notes
            .values()
            .filter(|n| n.character_id == -78 && n.note_id > 780_000_000)
            .map(|n| n.note_id - 780_000_000)
            .max()
        {
            self.next_report_id = self.next_report_id.max(700_000).max(max_rid);
        }
    }
    /// Removes any applied 1st-party SKINs, 3rd-party SKINs, and ship logos for `ship_ids`
    /// stored in this snapshot (used on ship repackage, destruction, or ownership change).
    pub fn strip_ship_cosmetics(&mut self, ship_ids: &[i64]) {
        if ship_ids.is_empty() {
            return;
        }
        for id in ship_ids {
            self.applied_ship_skins.remove(id);
        }
        let ship_u64s: Vec<String> = ship_ids
            .iter()
            .filter_map(|&id| u64::try_from(id).ok().map(|u| u.to_string()))
            .collect();
        if ship_u64s.is_empty() {
            return;
        }
        if let Some(raw) = self.quasar_state_blobs.get("quasar_cosmetics_skinr").cloned() {
            if let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut changed = false;
                if let Some(map) = root
                    .get_mut("applied_thirdparty_skins")
                    .and_then(|v| v.as_object_mut())
                {
                    for key in &ship_u64s {
                        changed |= map.remove(key).is_some();
                    }
                }
                if changed {
                    if let Ok(json) = serde_json::to_string(&root) {
                        self.quasar_state_blobs
                            .insert("quasar_cosmetics_skinr".into(), json);
                    }
                }
            }
        }
        if let Some(raw) = self.quasar_state_blobs.get("quasar_cosmetics_live").cloned() {
            if let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut changed = false;
                if let Some(map) = root
                    .get_mut("ship_logos")
                    .and_then(|v| v.as_object_mut())
                {
                    for key in &ship_u64s {
                        changed |= map.remove(key).is_some();
                    }
                }
                if changed {
                    if let Ok(json) = serde_json::to_string(&root) {
                        self.quasar_state_blobs
                            .insert("quasar_cosmetics_live".into(), json);
                    }
                }
            }
        }
    }

    /// Validates that `service_state_blobs` strictly holds bounded small opaque metadata.
    pub fn validate_service_state_blobs(&self) -> Result<(), String> {
        crate::service_domains::validate_service_state_blobs(&self.service_state_blobs)
    }

    /// Inserts an opaque metadata key-value entry into `service_state_blobs`, enforcing configured
    /// key-count and payload-size limits.
    pub fn set_service_state_blob(&mut self, key: String, value: String) -> Result<(), String> {
        if let Err(err) = crate::service_domains::validate_service_state_blob_write(
            &self.service_state_blobs,
            &key,
            &value,
        ) {
            crate::service_domains::record_service_state_blob_limit_violation(&err);
            return Err(err);
        }
        self.service_state_blobs.insert(key, value);
        Ok(())
    }

    /// Migrates legacy JSON-encoded `service_state_blobs` and sentinel `owner_notes` entries onto
    /// the typed domain fields on `WorldStateSnapshot`.
    pub fn migrate_legacy_service_state(&mut self) {
        crate::service_domains::migrate_legacy_service_state(self);
    }

    /// Returns a read-only handle to the `entity_kv` namespace `ns` (`#913`).
    #[inline]
    pub fn kv(&self, ns: crate::service_domains::Ns) -> crate::service_domains::EntityKvReader<'_> {
        crate::service_domains::EntityKvReader::new(self, ns)
    }

    /// Returns a mutable handle to the `entity_kv` namespace `ns` (`#913`).
    #[inline]
    pub fn kv_mut(
        &mut self,
        ns: crate::service_domains::Ns,
    ) -> crate::service_domains::EntityKvWriter<'_> {
        crate::service_domains::EntityKvWriter::new(self, ns)
    }

    /// Cascade-deletes all `entity_kv` rows belonging to `(scope, entity_id)` across all namespaces
    /// with `cascade_on_owner_delete == true` (`#913`).
    pub fn cascade_delete_entity_kv(
        &mut self,
        scope: crate::service_domains::EntityKvScope,
        entity_id: i64,
    ) -> usize {
        crate::service_domains::cascade_delete_entity_kv(self, scope, entity_id)
    }

    /// Inserts an item into the non-persisted transient item namespace (`#1087`).
    ///
    /// Items stored here are excluded from JSON snapshots, `ChangeSet::diff`, WAL deltas,
    /// and SQLite persistence unless explicitly materialized via [`Self::materialize_transient_item`].
    #[inline]
    pub fn insert_transient_item(&mut self, item: InventoryItemRecord) -> Option<InventoryItemRecord> {
        self.transient_items.insert(item.item_id, item)
    }

    /// Returns `true` if `item_id` is currently stored in the transient item namespace (`#1087`).
    #[inline]
    pub fn is_transient_item(&self, item_id: i64) -> bool {
        self.transient_items.contains_key(&item_id)
    }

    /// Looks up an item across persistent `items` and non-persisted `transient_items` (`#1087`).
    #[inline]
    pub fn get_any_item(&self, item_id: i64) -> Option<&InventoryItemRecord> {
        self.items
            .get(&item_id)
            .or_else(|| self.transient_items.get(&item_id))
    }

    /// Mutably looks up an item across persistent `items` and non-persisted `transient_items` (`#1087`).
    #[inline]
    pub fn get_any_item_mut(&mut self, item_id: i64) -> Option<&mut InventoryItemRecord> {
        if self.items.contains_key(&item_id) {
            self.items.get_mut(&item_id)
        } else {
            self.transient_items.get_mut(&item_id)
        }
    }

    /// Materializes a transient item into a persistent row in `self.items` (`#1087`).
    ///
    /// Intended for the death/loot pipeline when surviving NPC or scene items become persistent drops.
    /// Returns `None` if `item_id` was not present in `self.transient_items` (or if already in `self.items`,
    /// returns `self.items.get_mut(&item_id)`).
    pub fn materialize_transient_item(&mut self, item_id: i64) -> Option<&mut InventoryItemRecord> {
        if let Some(mut item) = self.transient_items.remove(&item_id) {
            item.version = item.version.saturating_add(1);
            self.items.insert(item_id, item);
            return self.items.get_mut(&item_id);
        }
        self.items.get_mut(&item_id)
    }

    fn persistent_location_in_system(&self, mut loc_id: i64, solar_system_id: SolarSystemId) -> bool {
        let sys_loc = i64::from(solar_system_id);
        for _ in 0..16 {
            if loc_id == sys_loc {
                return true;
            }
            if self
                .structures
                .get(&loc_id)
                .is_some_and(|st| st.solar_system_id == solar_system_id)
            {
                return true;
            }
            if let Some(parent_item) = self.items.get(&loc_id) {
                if parent_item.location_id == loc_id {
                    break;
                }
                loc_id = parent_item.location_id;
            } else {
                break;
            }
        }
        false
    }

    /// Reclaims all transient items located in `solar_system_id` (direct, parented by a
    /// persistent item/structure in `solar_system_id`, and their transient descendants)
    /// when a system scene unloads (`#1087`).
    pub fn reclaim_transient_items_in_system(&mut self, solar_system_id: SolarSystemId) -> usize {
        if self.transient_items.is_empty() {
            return 0;
        }
        let sys_loc = i64::from(solar_system_id);
        let mut by_parent: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut non_transient_parents: Vec<(i64, i64)> = Vec::new();
        for (&iid, item) in self.transient_items.iter() {
            if item.location_id == sys_loc {
                non_transient_parents.push((iid, sys_loc));
            } else {
                by_parent.entry(item.location_id).or_default().push(iid);
                if !self.transient_items.contains_key(&item.location_id) {
                    non_transient_parents.push((iid, item.location_id));
                }
            }
        }
        let mut queue: Vec<i64> = non_transient_parents
            .into_iter()
            .filter_map(|(iid, parent_id)| {
                (parent_id == sys_loc
                    || self.persistent_location_in_system(parent_id, solar_system_id))
                .then_some(iid)
            })
            .collect();
        let mut to_remove: HashSet<i64> = HashSet::with_capacity(queue.len());
        while let Some(iid) = queue.pop() {
            if !to_remove.insert(iid) {
                continue;
            }
            if let Some(children) = by_parent.remove(&iid) {
                queue.extend(children);
            }
        }
        for &iid in &to_remove {
            self.transient_items.remove(&iid);
            self.transient_item_attributes.remove(&iid);
            self.item_positions.remove(&iid);
        }
        to_remove.len()
    }
}

/// Decodes an entity ID stored inside an [`OwnerNoteRecord`], reversing `-79` notification
/// and `-78` combat report sentinel encodings (`#1087`).
#[inline]
pub fn decode_owner_note_entity_id(note: &OwnerNoteRecord) -> Option<i64> {
    if note.character_id == -79 && note.note_id < -7_900_000_000_000 {
        note.note_id
            .checked_neg()
            .and_then(|neg| neg.checked_sub(7_900_000_000_000))
    } else if note.character_id == -78 && note.note_id > 780_000_000 {
        note.note_id.checked_sub(780_000_000)
    } else if note.note_id > 0 {
        Some(note.note_id)
    } else {
        None
    }
}

/// Converts a unix timestamp in seconds to an ISO date string "YYYY-MM-DD".
pub fn unix_secs_to_date_string(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
#[cfg(test)] #[path = "snapshot_ext_unit_tests.rs"] mod snapshot_ext_unit_tests;
#[cfg(test)] #[path = "snapshot_ext_edges_tests.rs"] mod snapshot_ext_edges_tests;

