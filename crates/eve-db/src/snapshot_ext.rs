//! `WorldStateSnapshot` container and dynamic world/ESI event and campaign records (`ESI-3`).

use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod tuple_map_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use std::hash::Hash;

    pub fn serialize<S, K1, K2, V>(
        map: &HashMap<(K1, K2), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K1: Serialize + Copy,
        K2: Serialize + Copy,
        V: Serialize,
    {
        let entries: Vec<((K1, K2), &V)> = map.iter().map(|(&(k1, k2), v)| ((k1, k2), v)).collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, K1, K2, V>(deserializer: D) -> Result<HashMap<(K1, K2), V>, D::Error>
    where
        D: Deserializer<'de>,
        K1: Deserialize<'de> + Eq + Hash,
        K2: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
    {
        let entries: Vec<((K1, K2), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemJumpEvent {
    pub solar_system_id: SolarSystemId,
    pub character_id: i64,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemKillEvent {
    pub solar_system_id: SolarSystemId,
    pub ship_kills: u32,
    pub npc_kills: u32,
    pub pod_kills: u32,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SovCampaignScoreRecord {
    pub structure_id: i64,
    pub attackers_score: f64,
    pub defender_score: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FwWindowedStat {
    pub yesterday: i64,
    pub last_week: i64,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FwCharacterStatsRecord {
    pub character_id: i64,
    pub faction_id: Option<i64>,
    pub enlisted_on_unix: Option<u64>,
    pub current_rank: i64,
    pub highest_rank: i64,
    pub kills: FwWindowedStat,
    pub victory_points: FwWindowedStat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FwSystemOccupationRecord {
    pub solar_system_id: SolarSystemId,
    pub owner_faction_id: i64,
    pub occupier_faction_id: i64,
    pub victory_points: i64,
    pub victory_points_threshold: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarAllyRecord {
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarLifecycleMeta {
    pub declared_unix: Option<u64>,
    pub started_unix: Option<u64>,
    pub retracted_unix: Option<u64>,
    pub finished_unix: Option<u64>,
    pub open_for_allies: Option<bool>,
    pub allies: Vec<WarAllyRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillmailDetailRecord {
    pub kill_id: i64,
    pub killmail_time_unix: u64,
    pub damage_taken: i64,
    pub victim_position: Option<[f64; 3]>,
    pub attackers: Vec<KillmailAttackerDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllianceLifecycleMeta {
    pub creator_id: i64,
    pub creator_corporation_id: i64,
    pub date_founded_unix: u64,
    pub faction_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureEsiTimerMeta {
    pub profile_id: i64,
    pub fuel_expires_unix: Option<u64>,
    pub state_timer_start_unix: Option<u64>,
    pub state_timer_end_unix: Option<u64>,
    pub unanchors_at_unix: Option<u64>,
    pub next_reinforce_hour: Option<i64>,
    pub next_reinforce_apply_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpAllianceHistoryEntry {
    pub record_id: i64,
    pub alliance_id: Option<i64>,
    pub start_date_unix: u64,
    pub is_deleted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpMedalEntry {
    pub medal_id: i64,
    pub title: String,
    pub description: String,
    pub creator_id: i64,
    pub created_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpIssuedMedalEntry {
    pub medal_id: i64,
    pub character_id: i64,
    pub reason: String,
    pub status: String,
    pub issuer_id: i64,
    pub issued_at_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpRoleHistoryEntry {
    pub character_id: i64,
    pub changed_at_unix: u64,
    pub issuer_id: i64,
    pub role_type: String,
    pub old_roles: Vec<String>,
    pub new_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpShareholderEntry {
    pub shareholder_id: i64,
    pub shareholder_type: String,
    pub share_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpVoteBallotRecord {
    pub case_id: i64,
    pub voter_id: i64,
    pub option_id: i64,
    pub weight: i64,
    pub voted_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpProjectContributorEntry {
    pub character_id: i64,
    pub contributed_quantity: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterResearchAgentRecord {
    pub agent_id: i64,
    pub skill_type_id: TypeId,
    pub started_at_unix: u64,
    pub points_per_day: f64,
    pub remainder_points: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterMedalGraphicRecord {
    pub part: i64,
    pub layer: i64,
    pub graphic: String,
    pub color: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct CharacterScopedRolesRecord {
    pub roles: Vec<String>,
    pub roles_at_hq: Vec<String>,
    pub roles_at_base: Vec<String>,
    pub roles_at_other: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoyaltyStoreOfferItemRecord {
    pub type_id: TypeId,
    pub quantity: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketHistoryDayRecord {
    pub date: String,
    pub average: f64,
    pub highest: f64,
    pub lowest: f64,
    pub order_count: i64,
    pub volume: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanetaryPinContentRecord {
    pub type_id: TypeId,
    pub amount: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanetaryLinkRecord {
    pub source_pin_id: i64,
    pub destination_pin_id: i64,
    pub link_level: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct PlanetaryColonyLayoutRecord {
    pub pins: Vec<PlanetaryPinRecord>,
    pub links: Vec<PlanetaryLinkRecord>,
    pub routes: Vec<PlanetaryRouteRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
pub struct WorldStateSnapshot {
    pub next_entity_id: i64,
    #[serde(default)]
    pub wal_seq: u64,
    #[serde(default)]
    pub markets_seeded: bool,
    pub accounts: HashMap<String, AccountRecord>,
    pub characters: HashMap<i64, CharacterRecord>,
    pub skills: HashMap<i64, HashMap<TypeId, TrainedSkillRecord>>,
    pub skill_queues: HashMap<i64, Vec<SkillQueueEntry>>,
    pub jump_clones: HashMap<i64, Vec<JumpCloneRecord>>,
    pub active_implants: HashMap<i64, Vec<TypeId>>,
    pub items: HashMap<i64, InventoryItemRecord>,
    pub saved_fittings: HashMap<i64, SavedFittingRecord>,
    pub market_orders: HashMap<i64, MarketOrderRecord>,
    pub wallet_journal: Vec<WalletJournalEntry>,
    pub contracts: HashMap<i64, ContractRecord>,
    pub industry_jobs: HashMap<i64, IndustryJobRecord>,
    #[serde(with = "tuple_map_serde")]
    pub planetary_colonies: HashMap<(i64, i64), PlanetaryColonyRecord>,
    #[serde(with = "tuple_map_serde")]
    pub active_missions: HashMap<(i64, u32), ActiveMissionRecord>,
    pub mission_tally: HashMap<i64, u32>,
    #[serde(default)]
    pub standing_transactions: HashMap<i64, Vec<StandingTransactionRecord>>,
    pub npe_progress: HashMap<i64, NpeProgressRecord>,
    pub bookmarks: HashMap<i64, BookmarkRecord>,
    pub mails: Vec<MailRecord>,
    pub fleets: HashMap<i64, FleetRecord>,
    pub corporations: HashMap<i64, CorporationRecord>,
    #[serde(default)]
    pub alliances: HashMap<i64, AllianceRecord>,
    #[serde(default)]
    pub wars: HashMap<i64, WarRecord>,
    pub corp_projects: HashMap<i64, CorpProjectRecord>,
    pub killmails: HashMap<i64, KillmailRecord>,
    pub kill_rights: HashMap<i64, KillRightRecord>,
    pub structures: HashMap<i64, UpwellStructureRecord>,
    pub system_states: HashMap<SolarSystemId, SystemDynamicState>,
    pub committed_idempotency_keys: HashSet<(i64, String)>,
    #[serde(default)]
    pub licensed_skins: HashMap<i64, Vec<u32>>,
    #[serde(default)]
    pub applied_ship_skins: HashMap<i64, u32>,
    #[serde(default)]
    pub chat_channels: HashMap<String, ChatChannelRecord>,
    #[serde(default)]
    pub cosmic_signatures: HashMap<SolarSystemId, Vec<CosmicSignatureRecord>>,
    #[serde(default)]
    pub read_mails: HashSet<(i64, i64)>,
    #[serde(default)]
    pub deleted_mails: HashSet<(i64, i64)>,
    #[serde(default)]
    pub mail_labels: HashMap<i64, Vec<MailLabelRecord>>,
    #[serde(default, with = "tuple_map_serde")]
    pub mail_assigned_labels: HashMap<(i64, i64), Vec<i64>>,
    #[serde(default)]
    pub bookmark_folders: HashMap<i64, BookmarkFolderRecord>,
    #[serde(default, with = "tuple_map_serde")]
    pub wh_polarizations: HashMap<(i64, i64), u64>,
    #[serde(default)]
    pub mining_ledger: Vec<MiningLedgerRecord>,
    pub module_online_states: HashMap<i64, bool>,
    /// Fitted modules currently cycling, set by `dogmaIM.Activate` and cleared by `dogmaIM.Deactivate`.
    #[serde(default)]
    pub module_active_states: HashSet<i64>,
    /// Charge type loaded into an active burst module when it was activated (`dogmaIM.Activate`).
    #[serde(default)]
    pub module_active_charges: HashMap<i64, u32>,
    #[serde(default)]
    pub weapon_banks: HashMap<i64, HashMap<i64, Vec<i64>>>,
    #[serde(default)]
    pub asteroids: HashMap<i64, AsteroidRecord>,
    #[serde(default)]
    pub moon_extractions: HashMap<i64, MoonExtractionRecord>,
    #[serde(default)]
    pub auto_moon_miners: HashMap<i64, AutoMoonMinerRecord>,
    #[serde(default)]
    pub observer_mining_ledger: HashMap<i64, Vec<ObserverMiningRecord>>,
    #[serde(default)]
    pub owner_notes: HashMap<i64, OwnerNoteRecord>,
    #[serde(default)]
    pub paused_skill_queues: HashSet<i64>,
    #[serde(default)]
    pub cosmic_anomalies: HashMap<SolarSystemId, Vec<CosmicAnomalyRecord>>,
    #[serde(default)]
    pub sov_prospecting_levels: HashMap<SolarSystemId, u32>,
    #[serde(default)]
    pub last_mission_decline_times: HashMap<i64, u64>,
    #[serde(default)]
    pub character_last_jump_unix: HashMap<i64, u64>,
    #[serde(default)]
    pub character_cspa_charges: HashMap<i64, IskCents>,
    #[serde(default)]
    pub mail_timestamps: HashMap<i64, u64>,
    #[serde(default)]
    pub character_login_stats: HashMap<i64, CharacterLoginStats>,
    #[serde(default)]
    pub item_custom_names: HashMap<i64, String>,
    #[serde(default)]
    pub item_positions: HashMap<i64, [f64; 3]>,
    #[serde(default)]
    pub structure_vulnerability_windows: HashMap<i64, (u64, u64)>,
    #[serde(default)]
    pub skill_history: HashMap<i64, Vec<SkillHistoryEntry>>,
    #[serde(default)]
    pub character_doll_info: HashMap<i64, (String, String)>,
    #[serde(default)]
    pub system_jump_events: Vec<SystemJumpEvent>,
    #[serde(default)]
    pub system_kill_events: Vec<SystemKillEvent>,
    #[serde(default)]
    pub incursions: HashMap<u32, IncursionRecord>,
    #[serde(default)]
    pub sov_campaign_scores: HashMap<i64, SovCampaignScoreRecord>,
    #[serde(default)]
    pub fw_character_stats: HashMap<i64, FwCharacterStatsRecord>,
    #[serde(default)]
    pub fw_corp_enlisted_on: HashMap<i64, u64>,
    #[serde(default)]
    pub fw_system_occupations: HashMap<SolarSystemId, FwSystemOccupationRecord>,
    #[serde(default)]
    pub fw_wars: Vec<(i64, i64)>,
    #[serde(default)]
    pub war_lifecycle_meta: HashMap<i64, WarLifecycleMeta>,
    #[serde(default)]
    pub killmail_details: HashMap<i64, KillmailDetailRecord>,
    #[serde(default)]
    pub station_office_rental_costs: HashMap<i64, f64>,
    #[serde(default)]
    pub alliance_lifecycle_meta: HashMap<i64, AllianceLifecycleMeta>,
    #[serde(default)]
    pub corp_hangar_divisions: HashMap<i64, HashMap<u8, String>>,
    #[serde(default)]
    pub structure_esi_timer_meta: HashMap<i64, StructureEsiTimerMeta>,
    #[serde(default)]
    pub corp_alliance_history: HashMap<i64, Vec<CorpAllianceHistoryEntry>>,
    #[serde(default)]
    pub corp_container_logs: HashMap<i64, Vec<CorpContainerLogEntry>>,
    #[serde(default)]
    pub corp_medals: HashMap<i64, Vec<CorpMedalEntry>>,
    #[serde(default)]
    pub corp_issued_medals: HashMap<i64, Vec<CorpIssuedMedalEntry>>,
    #[serde(default)]
    pub corp_member_limits: HashMap<i64, i64>,
    #[serde(default)]
    pub corp_titles: HashMap<i64, Vec<CorpTitleEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    pub corp_member_titles: HashMap<(i64, i64), Vec<i64>>,
    #[serde(default, with = "tuple_map_serde")]
    pub corp_role_assignments: HashMap<(i64, i64), CorpMemberRoleAssignment>,
    #[serde(default)]
    pub corp_role_history: HashMap<i64, Vec<CorpRoleHistoryEntry>>,
    #[serde(default)]
    pub corp_shareholders: HashMap<i64, Vec<CorpShareholderEntry>>,
    #[serde(default)]
    pub starbase_settings: HashMap<i64, StarbaseDetailSettings>,
    #[serde(default)]
    pub corp_project_contributors: HashMap<i64, Vec<CorpProjectContributorEntry>>,
    #[serde(default)]
    pub char_research_agents: HashMap<i64, Vec<CharacterResearchAgentRecord>>,
    #[serde(default)]
    pub char_medals: HashMap<i64, Vec<CharacterMedalRecord>>,
    #[serde(default)]
    pub char_scoped_roles: HashMap<i64, CharacterScopedRolesRecord>,
    #[serde(default)]
    pub loyalty_store_offers: HashMap<i64, Vec<LoyaltyStoreOfferRecord>>,
    #[serde(default)]
    pub wallet_journal_timestamps: HashMap<i64, u64>,
    #[serde(default)]
    pub char_wallet_transactions: HashMap<i64, Vec<WalletTransactionRecord>>,
    #[serde(default, with = "tuple_map_serde")]
    pub corp_wallet_journals: HashMap<(i64, i64), Vec<CorpWalletJournalRecord>>,
    #[serde(default, with = "tuple_map_serde")]
    pub corp_wallet_transactions: HashMap<(i64, i64), Vec<WalletTransactionRecord>>,
    #[serde(default)]
    pub market_order_meta: HashMap<i64, MarketOrderMeta>,
    #[serde(default, with = "tuple_map_serde")]
    pub market_history: HashMap<(u32, TypeId), Vec<MarketHistoryDayRecord>>,
    #[serde(default, with = "tuple_map_serde")]
    pub planetary_colony_layouts: HashMap<(i64, i64), PlanetaryColonyLayoutRecord>,
    #[serde(default)]
    pub corp_customs_offices: HashMap<i64, Vec<CustomsOfficeRecord>>,
    /// Journal parties/context metadata keyed by journal entry ID (`crate::ledger`).
    #[serde(default)]
    pub wallet_journal_meta: HashMap<i64, crate::ledger::JournalMeta>,
    /// Market fills recorded at fill time (`crate::ledger`).
    #[serde(default)]
    pub market_transactions: Vec<crate::ledger::MarketTransactionRecord>,
    /// Account-wide PLEX Vault balances keyed by `user_id`.
    #[serde(default)]
    pub account_plex_vault: HashMap<u64, i64>,
    #[serde(default)]
    pub corp_vote_cases: HashMap<i64, CorpVoteCaseRecord>,
    #[serde(default, with = "tuple_map_serde")]
    pub corp_vote_ballots: HashMap<(i64, i64), CorpVoteBallotRecord>,
    #[serde(default)]
    pub orbital_provision_types: HashMap<i64, i64>,
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

impl WorldStateSnapshot {
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
        self.system_kill_events.push(SystemKillEvent {
            solar_system_id,
            ship_kills,
            npc_kills,
            pod_kills,
            timestamp_unix,
        });
    }

    /// Returns `true` if `item_id` is currently escrowed in an active contract (`Outstanding` or `InProgress`)
    /// or locked as the blueprint of an `Active` industry job.
    pub fn is_item_locked_in_contract_or_job(&self, item_id: i64) -> bool {
        let in_contract = self.contracts.values().any(|c| {
            (c.status == "Outstanding" || c.status == "InProgress") && c.item_ids.contains(&item_id)
        });
        let in_job = self
            .industry_jobs
            .values()
            .any(|j| j.status == "Active" && j.blueprint_item_id == item_id);
        in_contract || in_job
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

    /// Shared predicate verifying that `item` is owned by `char_id`, is unlocked (`flag != Locked`),
    /// is not the character's currently boarded ship, is not escrowed in an active contract or active
    /// industry job, and is physically located in the caller's current docked Hangar or co-located active ship Cargo.
    pub fn is_item_consumable_by(
        &self,
        item: &InventoryItemRecord,
        char_id: i64,
        station_or_struct_id: Option<i64>,
        active_ship_id: Option<i64>,
    ) -> bool {
        if item.owner_id != char_id || item.quantity <= 0 {
            return false;
        }
        if item.flag == InventoryFlag::Locked {
            return false;
        }
        if Some(item.item_id) == active_ship_id {
            return false;
        }
        let at_dock_hangar = station_or_struct_id.is_some()
            && Some(item.location_id) == station_or_struct_id
            && item.flag == InventoryFlag::Hangar;
        let in_ship_cargo = active_ship_id
            .filter(|&sid| {
                self.is_ship_colocated_with_character(char_id, sid, station_or_struct_id)
            })
            .is_some()
            && Some(item.location_id) == active_ship_id
            && item.flag == InventoryFlag::Cargo;
        if !(at_dock_hangar || in_ship_cargo) {
            return false;
        }
        !self.is_item_locked_in_contract_or_job(item.item_id)
    }

    /// Resolves an expired `Outstanding` or `InProgress` contract in-place.
    pub fn resolve_expired_contract(
        &mut self,
        contract_id: i64,
        now_unix: u64,
    ) -> Option<&'static str> {
        let c = self.contracts.get(&contract_id)?.clone();
        if c.expiration_unix == 0 || now_unix <= c.expiration_unix {
            return None;
        }
        match c.status.as_str() {
            "Outstanding" => {
                if c.contract_type == "Auction" {
                    if let Some(winner_id) = c.current_bidder_id.filter(|_| c.current_bid.0 > 0) {
                        if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(c.current_bid)
                                .unwrap_or(issuer.balance);
                        }
                        for iid in &c.item_ids {
                            if let Some(item) = self.items.get_mut(iid) {
                                item.owner_id = winner_id;
                                item.location_id = c.start_station_id;
                                item.flag = InventoryFlag::Hangar;
                                item.version += 1;
                            }
                        }
                        if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                            c_mut.acceptor_id = Some(winner_id);
                            c_mut.status = "Completed".into();
                        }
                        return Some("Completed");
                    }
                }
                if c.reward.0 > 0 {
                    if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
                        issuer.balance = issuer
                            .balance
                            .checked_add(c.reward)
                            .unwrap_or(issuer.balance);
                    }
                }
                for iid in &c.item_ids {
                    if let Some(item) = self.items.get_mut(iid) {
                        item.owner_id = c.issuer_id;
                        item.location_id = c.start_station_id;
                        item.flag = InventoryFlag::Hangar;
                        item.version += 1;
                    }
                }
                if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                    c_mut.status = "Expired".into();
                }
                Some("Expired")
            }
            "InProgress" => {
                let refund_and_collateral =
                    c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
                if refund_and_collateral.0 > 0 {
                    if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
                        issuer.balance = issuer
                            .balance
                            .checked_add(refund_and_collateral)
                            .unwrap_or(issuer.balance);
                    }
                }
                let mut wraps_to_remove = Vec::new();
                for iid in &c.item_ids {
                    let parent_loc = self.items.get(iid).map(|it| it.location_id).unwrap_or(0);
                    let wrap_parent_loc = self
                        .items
                        .get(&parent_loc)
                        .filter(|p| p.type_id == 3468)
                        .map(|p| p.location_id);
                    if let Some(wloc) = wrap_parent_loc {
                        wraps_to_remove.push(parent_loc);
                        if let Some(item) = self.items.get_mut(iid) {
                            item.location_id = wloc;
                        }
                    }
                    let in_ship_container = self
                        .items
                        .get(iid)
                        .map(|item| self.items.contains_key(&item.location_id))
                        .unwrap_or(false);
                    if let Some(item) = self.items.get_mut(iid) {
                        if c.collateral.0 > 0 {
                            item.owner_id = c.acceptor_id.unwrap_or(c.issuer_id);
                            item.flag = if in_ship_container {
                                InventoryFlag::Cargo
                            } else {
                                InventoryFlag::Hangar
                            };
                        } else {
                            item.owner_id = c.issuer_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                        }
                        item.version += 1;
                    }
                }
                for wid in wraps_to_remove {
                    self.items.remove(&wid);
                }
                if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                    c_mut.status = "Failed".into();
                }
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
        self.next_entity_id += 1;
        let entry_id = self.next_entity_id;
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
    /// and assigning the wrapped goods (or items) to the acceptor (if collateral was posted)
    /// or returning to issuer.
    pub fn fail_courier_contract(&mut self, contract_id: i64, _now_unix: u64) -> bool {
        let c = match self.contracts.get(&contract_id).cloned() {
            Some(c) => c,
            None => return false,
        };
        if c.contract_type != "Courier" || c.status != "InProgress" {
            return false;
        }
        let refund_and_collateral =
            c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
        if refund_and_collateral.0 > 0 {
            if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
                issuer.balance = issuer
                    .balance
                    .checked_add(refund_and_collateral)
                    .unwrap_or(issuer.balance);
            }
        }
        let mut wraps_to_remove = Vec::new();
        for iid in &c.item_ids {
            let parent_loc = self.items.get(iid).map(|it| it.location_id).unwrap_or(0);
            let wrap_parent_loc = self
                .items
                .get(&parent_loc)
                .filter(|p| p.type_id == 3468)
                .map(|p| p.location_id);
            if let Some(wloc) = wrap_parent_loc {
                wraps_to_remove.push(parent_loc);
                if let Some(item) = self.items.get_mut(iid) {
                    item.location_id = wloc;
                }
            }
            let in_ship_container = self
                .items
                .get(iid)
                .map(|item| self.items.contains_key(&item.location_id))
                .unwrap_or(false);
            if let Some(item) = self.items.get_mut(iid) {
                if c.collateral.0 > 0 {
                    item.owner_id = c.acceptor_id.unwrap_or(c.issuer_id);
                    item.flag = if in_ship_container {
                        InventoryFlag::Cargo
                    } else {
                        InventoryFlag::Hangar
                    };
                } else {
                    item.owner_id = c.issuer_id;
                    item.location_id = c.start_station_id;
                    item.flag = InventoryFlag::Hangar;
                }
                item.version += 1;
            }
        }
        for wid in wraps_to_remove {
            self.items.remove(&wid);
        }
        if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
            c_mut.status = "Failed".into();
        }
        true
    }
}

/// Converts a unix timestamp in seconds to an ISO date string "YYYY-MM-DD".
pub fn unix_secs_to_date_string(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1020 + doe / 1461 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
