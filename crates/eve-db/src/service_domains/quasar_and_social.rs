use crate::durable_domain_types::tuple_map_serde;
use crate::types::IskCents;
use eve_proto::PyRep;
use eve_sde::TypeId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

// ============================================================================
// Quasar Commerce, Payments, NES Catalog, Cosmetics Market, Orchestrator & ACL
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlexTransactionRecord {
    pub id: u64,
    pub user_id: u64,
    pub amount: i64,
    pub balance_after: i64,
    pub timestamp_unix: u64,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreCategory {
    pub id: u32,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreProduct {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub type_id: u32,
    pub quantity: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreOffer {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub price_plex: i64,
    pub product_ids: Vec<u32>,
    pub targeted: bool,
    #[serde(default)]
    pub price_cents: Option<u64>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreCatalog {
    pub categories: Vec<StoreCategory>,
    pub products: Vec<StoreProduct>,
    pub offers: Vec<StoreOffer>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaymentScheduleRecord {
    pub id: u64,
    pub user_id: u64,
    pub schedule_type: u32,
    pub next_payment_unix: u64,
    pub offer_id: String,
    pub interval_days: u32,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaymentTokenRecord {
    pub id: u32,
    pub user_id: u64,
    pub token_str: String,
    pub is_quick_pay: bool,
    #[serde(default)]
    pub consumed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaymentReceiptRecord {
    pub receipt_id: u64,
    pub user_id: u64,
    pub offer_id: String,
    pub amount_in_cents: u32,
    pub currency: String,
    pub timestamp_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListingTargetRecord {
    Public,
    Corporation(u64),
    Alliance(u64),
    Character(u64),
}

impl Default for ListingTargetRecord {
    fn default() -> Self {
        Self::Public
    }
}

impl ListingTargetRecord {
    pub fn is_branded(&self) -> bool {
        !matches!(self, Self::Public)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComponentListingRecord {
    pub uuid: [u8; 16],
    pub sale_end_unix: u64,
    pub price_isk: Option<u64>,
    pub price_plex_cents: Option<u64>,
    pub component_item_type_id: u32,
    pub bundle_size: u64,
    pub discount_percent: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkinListingRecord {
    pub uuid: [u8; 16],
    pub seller_id: u32,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub price_isk: Option<u64>,
    pub price_plex_cents: Option<u64>,
    pub skin_hex: String,
    pub quantity: u32,
    pub target: ListingTargetRecord,
    pub branded: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CosmeticTransactionDetails {
    #[default]
    PurchasingComponent,
    PurchasingSkin,
    CreatingSkinListing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CosmeticTransactionRecord {
    pub uuid: [u8; 16],
    pub last_updated_unix: u64,
    pub status: u32,
    pub buyer_id: Option<u32>,
    pub seller_id: Option<u32>,
    pub details: CosmeticTransactionDetails,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CosmeticsMarketData {
    pub component_listings: BTreeMap<String, ComponentListingRecord>,
    pub skin_listings: BTreeMap<String, SkinListingRecord>,
    #[serde(default)]
    pub transactions: BTreeMap<String, CosmeticTransactionRecord>,
    pub next_seq: u64,
}

impl CosmeticsMarketData {
    pub fn record_tx(
        &mut self,
        key: String,
        uuid: [u8; 16],
        last_updated_unix: u64,
        succeeded: bool,
        buyer_id: Option<u32>,
        seller_id: Option<u32>,
        details: CosmeticTransactionDetails,
    ) {
        self.transactions.insert(
            key,
            CosmeticTransactionRecord {
                uuid,
                last_updated_unix,
                status: if succeeded { 3 } else { 2 },
                buyer_id,
                seller_id,
                details,
            },
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SemVerSpec {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Default for SemVerSpec {
    fn default() -> Self {
        Self {
            major: 1,
            minor: 0,
            patch: 0,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeploymentRecord {
    pub id: u64,
    pub name: String,
    pub short_description: String,
    pub content_group_id: u64,
    pub enabled: bool,
    pub current_content_count: u32,
    #[serde(default = "default_true")]
    pub no_region_limit: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentGroupRecord {
    pub id: u64,
    pub name: String,
    pub content_type_id: u64,
    pub weight: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentTypeRecord {
    pub id: u64,
    pub name: String,
    pub short_description: String,
    pub dungeon_id: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeploymentInstanceRecord {
    pub uuid: Vec<u8>,
    pub deployment_id: u64,
    pub content_type_id: u64,
    pub solar_system_id: u32,
    pub constellation_id: u32,
    pub created_unix: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorState {
    pub default_tenant_version: SemVerSpec,
    pub tenant_version: Option<SemVerSpec>,
    pub deployments: BTreeMap<u64, DeploymentRecord>,
    pub content_groups: BTreeMap<u64, ContentGroupRecord>,
    pub content_types: BTreeMap<u64, ContentTypeRecord>,
    pub instances: Vec<DeploymentInstanceRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailedFulfillmentRecord {
    pub id: u32,
    pub user_id: u64,
    pub plex_units: i64,
    pub retries: u32,
    pub fulfilled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetedOfferAdminRecord {
    pub user_id: u64,
    pub catalogue_entry_name: String,
    pub promoted: bool,
    pub retracted: bool,
    pub earliest_reissue_unix: i64,
    pub partner_code: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuasarAclRecord {
    pub acl_id: u32,
    pub name: String,
    pub description: String,
    pub version: u32,
    pub characters: Vec<(u64, i32)>,
    pub corporations: Vec<(u64, i32)>,
    pub alliances: Vec<(u64, i32)>,
    pub everyone: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuasarCommerceStore {
    pub payment_tokens: Vec<PaymentTokenRecord>,
    pub payment_receipts: Vec<PaymentReceiptRecord>,
    pub payment_schedules: Vec<PaymentScheduleRecord>,
    pub plex_tx_logs: Vec<PlexTransactionRecord>,
    pub nes_store_catalog: Option<StoreCatalog>,
    pub nes_store_offer_tags: HashMap<u32, Vec<String>>,
    pub plex_invoices: Vec<String>,
    pub cosmetics_market: Option<CosmeticsMarketData>,
    pub orchestrator: OrchestratorState,
    pub failed_fulfillments: Vec<FailedFulfillmentRecord>,
    pub targeted_offers: BTreeMap<String, TargetedOfferAdminRecord>,
    pub acl_registry: Option<Vec<QuasarAclRecord>>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub lp_asset_ops_completed: HashSet<String>,
}

// ============================================================================
// Fleet, Corp Ads/Wars, Hypernet, Contracts, Reports, Agents, LP & Scanning
// ============================================================================

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetSetupRecord {
    pub setup_name: String,
    pub motd: String,
    pub is_free_move: bool,
    pub max_size: u16,
    pub default_squad_id: Option<i64>,
    pub wings: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetAdvertRecord {
    pub fleet_id: i64,
    pub leader_id: i64,
    pub fleet_name: String,
    pub description: String,
    pub solar_system_id: u32,
    pub invite_scope: u8,
    pub auto_join: bool,
    pub min_standing: f64,
    pub min_security: f64,
    pub allowed_entities: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpRecruitmentAd {
    pub ad_id: i64,
    pub corp_id: i64,
    pub title: String,
    pub description: String,
    pub type_mask: u64,
    pub recruiter_id: i64,
    pub created_unix: u64,
    pub expiry_unix: u64,
    pub days: u32,
    pub lang_mask: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InGameReportRecord {
    pub report_id: i64,
    pub reporter_char_id: i64,
    pub reporter_corp_id: i64,
    pub reported_entity_id: i64,
    pub category: String,
    pub reason: String,
    pub details: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LPOffer {
    pub offer_id: i64,
    pub corp_id: i64,
    pub type_id: TypeId,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: IskCents,
    pub req_items: Vec<(TypeId, i32)>,
    #[serde(default)]
    pub ak_cost: i64,
    #[serde(default)]
    pub min_standing: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetAndSocialOpsStore {
    pub fleet_saved_setups: HashMap<i64, HashMap<String, FleetSetupRecord>>,
    pub fleet_stored_formations: HashMap<i64, (i64, f64, f64)>,
    pub fleet_adverts: HashMap<i64, FleetAdvertRecord>,
    #[serde(default, with = "crate::durable_domain_types::sorted_map_of_sets_serde")]
    pub fleet_join_requests: HashMap<i64, HashSet<i64>>,
    #[serde(default, with = "crate::durable_domain_types::sorted_map_of_sets_serde")]
    pub fleet_watchlist: HashMap<i64, HashSet<i64>>,
    pub fleet_voice_enabled: HashMap<i64, bool>,
    pub fleet_loot_logging: HashMap<i64, bool>,
    pub corp_recruitment_ads: Vec<CorpRecruitmentAd>,
    pub corp_allow_war: HashMap<i64, bool>,
    pub hypernet_subs: BTreeMap<String, Vec<i64>>,
    pub ingamereport_tickets: Vec<InGameReportRecord>,
    pub contract_deposits: HashMap<i64, i64>,
    pub lp_store_offers: Option<Vec<LPOffer>>,
    pub delayed_rewards: HashMap<i64, HashMap<String, Vec<serde_json::Value>>>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub mission_taxed_journal: HashSet<i64>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub agent_global_disabled_missions: HashSet<u32>,
    #[serde(default, with = "crate::durable_domain_types::sorted_map_of_sets_serde")]
    pub agent_char_disabled_missions: HashMap<i64, HashSet<u32>>,
    #[serde(with = "tuple_map_serde")]
    pub agent_replay_ts: HashMap<(i64, i64), u64>,
    pub signal_tracker_subs: HashMap<u32, Vec<i64>>,
    pub pending_probe_scans: Vec<(i64, Vec<i64>, Vec<PyRep>, Vec<PyRep>, Vec<PyRep>, i64)>,
    #[serde(default, with = "crate::durable_domain_types::sorted_map_of_sets_serde")]
    pub wh_local_witnesses: HashMap<String, HashSet<i64>>,
}

// ============================================================================
// Cluster Runtime, Shipcaster, Defense, Alerts, Portraits & Character Misc
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterEnergyState {
    pub current_energy: i64,
    pub max_energy: i64,
    pub last_update_time: i64,
    pub recharge_rate: f64,
    #[serde(default)]
    pub energy_rem: f64,
}

impl Default for CharacterEnergyState {
    fn default() -> Self {
        Self {
            current_energy: 100,
            max_energy: 100,
            last_update_time: 0,
            recharge_rate: 100.0 / 1_320.0,
            energy_rem: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedLandingPadRecord {
    pub item_id: i64,
    pub type_id: u32,
    #[serde(default)]
    pub built_type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub is_built: bool,
    pub link_timestamp: i64,
    pub is_disrupted: bool,
    pub position: [f64; 3],
    #[serde(default)]
    pub required_materials: BTreeMap<u32, u32>,
    #[serde(default)]
    pub contributed_materials: BTreeMap<u32, u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedLauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub linked_pad_ids: Vec<i64>,
    pub position: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedStarterLauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub staging_system_id: u32,
    #[serde(default)]
    pub staging_station_id: i64,
    pub position: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OfferedLoadSlot {
    pub slot_key: String,
    pub original_system_id: u32,
    pub offered_system_ids: Vec<u32>,
    pub expires_unix: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingZarzakhTollQuote {
    pub from_gate_id: i64,
    pub to_gate_id: i64,
    pub ship_id: i64,
    pub quoted_toll_cents: IskCents,
    pub expires_unix: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HorizonSiegePhase {
    #[default]
    Normal,
    Warning,
    Active,
    Stabilizing,
}

impl HorizonSiegePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Warning => "Warning",
            Self::Active => "Active",
            Self::Stabilizing => "Stabilizing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ZarzakhHorizonSiegeState {
    pub phase: HorizonSiegePhase,
    pub phase_started_unix: u64,
    pub active_start_unix: u64,
    pub active_until_unix: u64,
    pub cooldown_until_unix: u64,
    pub instability_points: u32,
    pub triggered_by_campaign_id: Option<i64>,
    pub fulcrum_restricted_to_pirates: bool,
}

#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedClusterMiscState {
    pub persisted_shipcaster_state: bool,
    pub char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    pub landing_pads: Vec<PersistedLandingPadRecord>,
    pub launchers: Vec<PersistedLauncherRecord>,
    pub starter_launchers: Vec<PersistedStarterLauncherRecord>,
    pub used_starter_shipcaster_chars: BTreeSet<i64>,
    pub offered_load_slots: HashMap<i64, OfferedLoadSlot>,
    pub char_energy: HashMap<i64, CharacterEnergyState>,
    pub target_cooldowns: HashMap<i64, u64>,
    pub target_charges: HashMap<i64, u32>,
    pub zarzakh_toll_quotes: HashMap<i64, PendingZarzakhTollQuote>,
    pub zarzakh_horizon_siege: ZarzakhHorizonSiegeState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloneVatBayState {
    pub active_by_ship: HashMap<i64, (i64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureDefenseSettings {
    pub structure_id: i64,
    pub auto_defense_enabled: bool,
    pub auto_target_attackers: bool,
    pub auto_target_war_targets: bool,
    pub standing_threshold: f64,
    pub target_blocked_acl: bool,
    #[serde(default)]
    pub known_attackers: Vec<i64>,
    #[serde(default)]
    pub known_attacker_timestamps: HashMap<i64, u64>,
}

pub const STRUCTURE_ATTACKER_TTL_SECS: u64 = 900;

impl Default for StructureDefenseSettings {
    fn default() -> Self {
        Self {
            structure_id: 0,
            auto_defense_enabled: false,
            auto_target_attackers: true,
            auto_target_war_targets: true,
            standing_threshold: 0.0,
            target_blocked_acl: false,
            known_attackers: Vec::new(),
            known_attacker_timestamps: HashMap::new(),
        }
    }
}

impl StructureDefenseSettings {
    /// Records `attacker_char_id` as an active aggressor at `now_unix`, pruning expired entries.
    pub fn record_attacker(&mut self, attacker_char_id: i64, now_unix: u64) {
        self.prune_expired_attackers(now_unix);
        if attacker_char_id > 0 {
            if !self.known_attackers.contains(&attacker_char_id) {
                self.known_attackers.push(attacker_char_id);
            }
            self.known_attacker_timestamps.insert(attacker_char_id, now_unix);
        }
    }

    /// Removes attacker entries whose recorded timestamp has exceeded `STRUCTURE_ATTACKER_TTL_SECS`.
    pub fn prune_expired_attackers(&mut self, now_unix: u64) {
        let cutoff = now_unix.saturating_sub(STRUCTURE_ATTACKER_TTL_SECS);
        self.known_attacker_timestamps.retain(|_, &mut ts| ts > cutoff);
        let timestamps = &self.known_attacker_timestamps;
        self.known_attackers
            .retain(|cid| timestamps.contains_key(cid));
    }

    /// Returns `true` if `pilot_id` has an active, non-expired attacker record at `now_unix`.
    pub fn is_active_attacker(&self, pilot_id: i64, now_unix: u64) -> bool {
        let cutoff = now_unix.saturating_sub(STRUCTURE_ATTACKER_TTL_SECS);
        self.known_attacker_timestamps
            .get(&pilot_id)
            .copied()
            .is_some_and(|ts| ts > cutoff)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AllianceSovConfig {
    #[serde(default)]
    pub capital_system_id: Option<i64>,
    #[serde(default)]
    pub pending_capital_system_id: Option<i64>,
    #[serde(default)]
    pub capital_valid_after_unix: u64,
    #[serde(default = "default_alliance_prime_hour")]
    pub current_prime_hour: i64,
    #[serde(default = "default_alliance_prime_hour")]
    pub new_prime_hour: i64,
    #[serde(default)]
    pub prime_valid_after_unix: u64,
}

fn default_alliance_prime_hour() -> i64 {
    18
}

impl Default for AllianceSovConfig {
    fn default() -> Self {
        Self {
            capital_system_id: None,
            pending_capital_system_id: None,
            capital_valid_after_unix: 0,
            current_prime_hour: 18,
            new_prime_hour: 18,
            prime_valid_after_unix: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LoadServiceState {
    pub active: bool,
    pub config: BTreeMap<String, PyRep>,
    pub runs_started: u64,
    pub total_requests: u64,
    pub systems_stepped: u64,
    pub ticks_executed: u64,
    pub sde_types_sampled: u64,
    pub sde_solar_systems_sampled: u64,
    pub world_items_sampled: u64,
    pub world_characters_sampled: u64,
    pub active_sessions_sampled: u64,
    pub last_started_unix: u64,
    pub last_stopped_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedDowntimeState {
    pub emitted_warnings_by_day: BTreeMap<u64, Vec<u64>>,
    pub last_shutdown_epoch_day: Option<u64>,
    pub cluster_restart_epoch: u64,
    pub last_maintenance_unix: u64,
    #[serde(default)]
    pub downtime_until_unix: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SolarSystemVisitRecord {
    pub visits: i64,
    #[serde(default)]
    pub last_visited_ticks: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlaggedSiteEntry {
    pub site_url: String,
    pub site_flag: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserLockdownPolicyState {
    pub lockdown: bool,
    pub default_home_page: String,
    #[serde(default)]
    pub flagged_sites: Vec<FlaggedSiteEntry>,
}

impl Default for BrowserLockdownPolicyState {
    fn default() -> Self {
        Self {
            lockdown: false,
            default_home_page: "https://www.eveonline.com/".to_string(),
            flagged_sites: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterAndRuntimeStore {
    pub cluster_misc_state: Option<PersistedClusterMiscState>,
    pub clone_vat_bay_state: CloneVatBayState,
    pub structure_defense: HashMap<i64, StructureDefenseSettings>,
    pub alliance_sov_cfg: HashMap<i64, AllianceSovConfig>,
    pub load_service_state: LoadServiceState,
    pub active_sessions: Vec<i64>,
    pub session_registry: HashMap<String, serde_json::Value>,
    pub cluster_node_table: Option<BTreeMap<String, String>>,
    pub cluster_admin_broadcasts: Vec<serde_json::Value>,
    pub cluster_service_states: BTreeMap<String, bool>,
    pub cluster_attacks: Vec<serde_json::Value>,
    pub external_queue_events: Vec<serde_json::Value>,
    pub cluster_downtime_state: PersistedDowntimeState,
    pub macho_client_code_hash: Option<serde_json::Value>,
    pub macho_global_config: HashMap<String, PyRep>,
    pub user_sessions: Vec<serde_json::Value>,
    pub gm_audit_log: Vec<serde_json::Value>,
    pub alert_bean_counts: HashMap<String, i64>,
    pub alert_log_modes: HashMap<String, String>,
    pub alert_client_traces: Option<serde_json::Value>,
    pub alert_proxy_traces: Option<serde_json::Value>,
    pub alert_group_bean_bytes: Option<serde_json::Value>,
    pub browser_lockdown_policy: Option<BrowserLockdownPolicyState>,
    pub localization_qa_wrap_users: BTreeMap<i64, bool>,
    pub client_stat_logger_strings: Vec<String>,
    pub blacklisted_planets: Option<Vec<i64>>,
    pub portraits: HashMap<String, String>,
    pub paperdoll_states: HashMap<i64, String>,
    pub char_system_visits: HashMap<i64, HashMap<u32, SolarSystemVisitRecord>>,
    pub module_reload_until_unix: HashMap<i64, u64>,
    pub auto_reloaded_event: HashMap<i64, (Vec<i64>, u32, f64)>,
    pub warp_vector_cooldown_until: HashMap<i64, u64>,
    pub ship_clone_offers: HashMap<i64, (i64, i64)>,
    pub mct_slots: HashMap<u64, Vec<u64>>,
    pub account_resculpt_tokens: HashMap<String, u32>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub resculpt_pending: HashSet<i64>,
    pub char_cohorts: HashMap<i64, Vec<String>>,
    pub user_cohorts: HashMap<u64, Vec<String>>,
    pub character_locks: HashMap<i64, String>,
    #[serde(default, with = "crate::durable_domain_types::sorted_set_serde")]
    pub omega_downgraded: HashSet<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_runtime_json: Option<String>,
}
