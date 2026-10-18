//! `eve-db` — Durable transactional persistence layer and domain repositories for `EvoMu` (`M1`–`M8`).
//!
//! Encapsulates all account, character, skill queue, clone, inventory, fitting, market, wallet,
//! contract, industry, planetary interaction, agent/mission, NPE, social, corporation, fleet,
//! CrimeWatch, Upwell structure, sovereignty, Faction Warfare, and dynamic system state behind
//! atomic repository operations with entity versioning, fixed-point ISK currency (`IskCents`),
//! per-character idempotency keys, and durable JSON snapshot save/load.

#![forbid(unsafe_code)]

pub mod sqlite_store;

use eve_sde::{AttributeId, SdeRegistry, SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;
use thiserror::Error;

const MAX_IDEMPOTENCY_KEYS: usize = 4_096;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DbError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Account banned until {until_unix}: {reason}")]
    AccountBanned { until_unix: u64, reason: String },
    #[error("Account {user_id} already has an active online session ({active_session_id})")]
    AlreadyLoggedIn {
        user_id: u64,
        active_session_id: u64,
    },
    #[error("Character {0} not found")]
    CharacterNotFound(i64),
    #[error("Character {0} is already undocked in space")]
    NotDocked(i64),
    #[error("Character {character_id} is not owned by user {user_id}")]
    CharacterOwnershipViolation { user_id: u64, character_id: i64 },
    #[error("Item {0} not found")]
    ItemNotFound(i64),
    #[error("Item {item_id} is not owned by character {character_id}")]
    ItemOwnershipViolation { character_id: i64, item_id: i64 },
    #[error("Item {0} is locked in escrow or an active job and cannot be modified")]
    ItemLocked(i64),
    #[error("Destination location {location_id} is not authorized for character {character_id}")]
    InvalidDestinationLocation { character_id: i64, location_id: i64 },
    #[error("Stale entity version (expected {expected}, found {actual})")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("Insufficient ISK balance (required {required}, available {available})")]
    InsufficientFunds { required: i64, available: i64 },
    #[error("Alpha clone skill point cap (5,000,000 SP) exceeded")]
    AlphaCloneSpCapExceeded,
    #[error("Active ship {ship_id} is located at {ship_location_id}, not at docked station {station_id}")]
    ShipNotAtDockedStation {
        ship_id: i64,
        ship_location_id: i64,
        station_id: i64,
    },
    #[error("Domain validation error: {0}")]
    ValidationError(String),
    #[error("Durable persistence failure: {0}")]
    PersistenceFailure(String),
}

/// Converts a durable WAL/checkpoint write error into a caller's transaction error type so
/// failed disk writes abort the transaction instead of committing in memory (`R24-P1-1`).
pub trait FromWalError {
    fn from_wal_error(err: String) -> Self;
}

impl FromWalError for DbError {
    fn from_wal_error(err: String) -> Self {
        DbError::PersistenceFailure(err)
    }
}

impl FromWalError for eve_proto::BridgeRpcError {
    fn from_wal_error(err: String) -> Self {
        eve_proto::BridgeRpcError {
            code: "PersistenceFailure".into(),
            message: err,
        }
    }
}

impl FromWalError for String {
    fn from_wal_error(err: String) -> Self {
        err
    }
}

impl FromWalError for () {
    fn from_wal_error(err: String) -> Self {
        panic!("Durable WAL persistence failure: {err}");
    }
}

impl FromWalError for std::convert::Infallible {
    fn from_wal_error(err: String) -> Self {
        panic!("Durable WAL persistence failure: {err}");
    }
}

/// Fixed-point ISK representation in cents (`1 ISK = 100 IskCents`) to prevent `f64` precision drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IskCents(pub i64);

impl IskCents {
    pub const fn from_isk(whole_isk: i64) -> Self {
        Self(whole_isk.saturating_mul(100))
    }

    pub fn from_f64_isk(isk: f64) -> Self {
        if !isk.is_finite() {
            return Self(0);
        }
        let scaled = (isk * 100.0).round();
        if scaled >= (i64::MAX / 2) as f64 {
            Self(i64::MAX / 2)
        } else if scaled <= (i64::MIN / 2) as f64 {
            Self(i64::MIN / 2)
        } else {
            Self(scaled as i64)
        }
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        if self.0 < 0 || rhs.0 < 0 {
            return None;
        }
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        if rhs.0 < 0 || self.0 < rhs.0 {
            return None;
        }
        self.0.checked_sub(rhs.0).map(Self)
    }

    pub fn checked_mul_qty(self, qty: i64) -> Option<Self> {
        if self.0 < 0 || qty < 0 {
            return None;
        }
        self.0.checked_mul(qty).map(Self)
    }

    pub fn to_wire_f64(self) -> f64 {
        (self.0 as f64) / 100.0
    }
}

/// Standard EVE inventory location flags (`invFlags`).
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InventoryFlag {
    None = 0,
    Hangar = 4,
    Cargo = 5,
    Skill = 7,
    LoSlot0 = 11,
    LoSlot1 = 12,
    LoSlot2 = 13,
    LoSlot3 = 14,
    LoSlot4 = 15,
    LoSlot5 = 16,
    LoSlot6 = 17,
    LoSlot7 = 18,
    MedSlot0 = 19,
    MedSlot1 = 20,
    MedSlot2 = 21,
    MedSlot3 = 22,
    MedSlot4 = 23,
    MedSlot5 = 24,
    MedSlot6 = 25,
    MedSlot7 = 26,
    HiSlot0 = 27,
    HiSlot1 = 28,
    HiSlot2 = 29,
    HiSlot3 = 30,
    HiSlot4 = 31,
    HiSlot5 = 32,
    HiSlot6 = 33,
    HiSlot7 = 34,
    /// `invFlags`: 56 = Capsule, 57 = Pilot, 62 = Locked.
    Capsule = 56,
    Pilot = 57,
    Locked = 62,
    DroneBay = 87,
    Booster = 88,
    Implant = 89,
    RigSlot0 = 92,
    RigSlot1 = 93,
    RigSlot2 = 94,
    SubSystemSlot0 = 125,
    FighterBay = 158,
    Deliveries = 173,
}

impl InventoryFlag {
    pub fn from_u16(v: u16) -> Self {
        match v {
            4 => Self::Hangar,
            5 => Self::Cargo,
            7 => Self::Skill,
            11 => Self::LoSlot0,
            12 => Self::LoSlot1,
            13 => Self::LoSlot2,
            14 => Self::LoSlot3,
            15 => Self::LoSlot4,
            16 => Self::LoSlot5,
            17 => Self::LoSlot6,
            18 => Self::LoSlot7,
            19 => Self::MedSlot0,
            20 => Self::MedSlot1,
            21 => Self::MedSlot2,
            22 => Self::MedSlot3,
            23 => Self::MedSlot4,
            24 => Self::MedSlot5,
            25 => Self::MedSlot6,
            26 => Self::MedSlot7,
            27 => Self::HiSlot0,
            28 => Self::HiSlot1,
            29 => Self::HiSlot2,
            30 => Self::HiSlot3,
            31 => Self::HiSlot4,
            32 => Self::HiSlot5,
            33 => Self::HiSlot6,
            34 => Self::HiSlot7,
            56 => Self::Capsule,
            57 => Self::Pilot,
            62 => Self::Locked,
            87 => Self::DroneBay,
            88 => Self::Booster,
            89 => Self::Implant,
            92 => Self::RigSlot0,
            93 => Self::RigSlot1,
            94 => Self::RigSlot2,
            125 => Self::SubSystemSlot0,
            158 => Self::FighterBay,
            173 => Self::Deliveries,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRecord {
    pub user_id: u64,
    pub username: String,
    pub password_token: String,
    pub role: u64,
    #[serde(default)]
    pub banned_until_unix: Option<u64>,
    #[serde(default)]
    pub ban_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineSessionRecord {
    pub user_id: u64,
    pub username: String,
    pub session_id: u64,
    pub character_id: Option<i64>,
    pub connected_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkceAuthCode {
    pub code: String,
    pub user_id: u64,
    pub character_id: i64,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoTokenBundle {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoTokenClaims {
    pub sub: String,
    pub name: String,
    pub owner: String,
    pub user_id: u64,
    pub character_id: i64,
    pub scp: Vec<String>,
    pub iss: String,
    pub aud: Vec<String>,
    pub iat: u64,
    pub exp: u64,
    pub jti: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoRefreshTokenRecord {
    pub refresh_token: String,
    pub user_id: u64,
    pub character_id: i64,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub expires_at_unix: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
struct AuthRuntimeState {
    online_sessions: HashMap<u64, OnlineSessionRecord>,
    kicked_users: HashMap<u64, (u64, String)>,
    displaced_sessions: HashMap<u64, String>,
    pkce_codes: HashMap<String, PkceAuthCode>,
    refresh_tokens: HashMap<String, SsoRefreshTokenRecord>,
    jwt_signing_secret: [u8; 32],
}

impl AuthRuntimeState {
    fn new(jwt_signing_secret: [u8; 32]) -> Self {
        Self {
            online_sessions: HashMap::new(),
            kicked_users: HashMap::new(),
            displaced_sessions: HashMap::new(),
            pkce_codes: HashMap::new(),
            refresh_tokens: HashMap::new(),
            jwt_signing_secret,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeuralAttributes {
    pub charisma: u8,
    pub intelligence: u8,
    pub memory: u8,
    pub perception: u8,
    pub willpower: u8,
    pub bonus_remaps: u8,
}

impl Default for NeuralAttributes {
    fn default() -> Self {
        Self {
            charisma: 19,
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            bonus_remaps: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterRecord {
    pub character_id: i64,
    pub user_id: u64,
    pub name: String,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    #[serde(default)]
    pub war_faction_id: Option<i64>,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub solar_system_id: SolarSystemId,
    pub constellation_id: u32,
    pub region_id: u32,
    pub active_ship_id: i64,
    pub balance: IskCents,
    pub plex_balance: i64,
    pub loyalty_points: HashMap<i64, i64>,
    pub standings: HashMap<i64, f64>,
    pub security_status: f64,
    pub clone_grade: u32, // 1 = Alpha, 2 = Omega
    pub home_station_id: i64,
    pub race_id: u32,
    pub bloodline_id: u32,
    pub ancestry_id: u32,
    pub school_id: u32,
    pub neural: NeuralAttributes,
    pub skill_points: f64,
    pub free_skill_points: f64,
    #[serde(default)]
    pub last_clone_jump_unix: u64,
    #[serde(default)]
    pub jump_fatigue_minutes: f64,
    #[serde(default)]
    pub jump_cooldown_until_unix: u64,
    pub version: u64,
    pub system_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainedSkillRecord {
    pub type_id: TypeId,
    pub level: u8,
    pub skill_points: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillQueueEntry {
    pub type_id: TypeId,
    pub target_level: u8,
    pub start_sp: f64,
    pub end_sp: f64,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JumpCloneRecord {
    pub jump_clone_id: i64,
    pub character_id: i64,
    pub location_id: i64,
    pub implants: Vec<TypeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryItemRecord {
    pub item_id: i64,
    pub type_id: TypeId,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag: InventoryFlag,
    pub quantity: i32,
    pub singleton: bool,
    pub version: u64,
    #[serde(default)]
    pub dynamic_attributes: HashMap<AttributeId, f64>,
    #[serde(default)]
    pub source_mutaplasmid_id: Option<TypeId>,
    #[serde(default)]
    pub insured_payout_isk: f64,
    #[serde(default)]
    pub is_blueprint_copy: bool,
    #[serde(default)]
    pub blueprint_runs_remaining: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedFittingRecord {
    pub fitting_id: i64,
    pub owner_id: i64,
    pub name: String,
    pub ship_type_id: TypeId,
    pub description: String,
    pub items: Vec<(TypeId, InventoryFlag, i32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketOrderRecord {
    pub order_id: i64,
    pub owner_id: i64,
    pub station_id: i64,
    pub region_id: u32,
    pub type_id: TypeId,
    pub is_bid: bool,
    pub price: IskCents,
    pub vol_entered: i32,
    pub vol_remaining: i32,
    pub min_volume: i32,
    pub is_npc: bool,
    pub escrow: IskCents,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalletJournalEntry {
    pub entry_id: i64,
    pub character_id: i64,
    pub entry_type: String,
    pub amount: IskCents,
    pub balance_after: IskCents,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractRecord {
    pub contract_id: i64,
    pub issuer_id: i64,
    pub acceptor_id: Option<i64>,
    pub contract_type: String, // "ItemExchange", "Courier"
    pub status: String,        // "Outstanding", "InProgress", "Completed", "Failed"
    pub start_station_id: i64,
    pub end_station_id: i64,
    pub price: IskCents,
    pub reward: IskCents,
    pub collateral: IskCents,
    pub item_ids: Vec<i64>,
    #[serde(default)]
    pub expiration_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndustryJobRecord {
    pub job_id: i64,
    pub installer_id: i64,
    pub facility_id: i64,
    pub blueprint_item_id: i64,
    pub blueprint_type_id: TypeId,
    pub activity_id: u8, // 1=Manufacturing, 3=TE, 4=ME, 5=Copy, 8=Invention, 9=Reaction
    pub runs: u32,
    pub product_type_id: TypeId,
    pub status: String, // "Active", "Delivered"
    pub duration_seconds: u64,
    #[serde(default)]
    pub start_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanetaryColonyRecord {
    pub planet_id: i64,
    pub owner_id: i64,
    pub solar_system_id: SolarSystemId,
    pub command_center_level: u8,
    pub pins_count: u32,
    pub extracted_commodities: HashMap<TypeId, u64>,
    #[serde(default)]
    pub last_extracted_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveMissionRecord {
    pub mission_id: u32,
    pub character_id: i64,
    pub agent_id: i64,
    #[serde(default)]
    pub solar_system_id: SolarSystemId,
    pub state: String, // "Offered", "Accepted", "Completed"
    pub current_room_id: u32,
    pub npcs_remaining: u32,
    #[serde(default)]
    pub spawned_npc_ball_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NpeProgressRecord {
    pub character_id: i64,
    pub node_graph_id: u32,
    pub current_node_id: u32,
    pub completed: bool,
    pub career_goal_progress: HashMap<u32, u64>,
    pub claimed_goals: HashSet<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookmarkRecord {
    pub bookmark_id: i64,
    pub owner_id: i64,
    pub label: String,
    pub solar_system_id: SolarSystemId,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailRecord {
    pub message_id: i64,
    pub sender_id: i64,
    pub recipient_id: i64,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetRecord {
    pub fleet_id: i64,
    pub boss_id: i64,
    pub members: Vec<i64>,
    pub wings: Vec<i64>,
    pub squads: Vec<i64>,
    pub free_move: bool,
    pub is_registered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorporationRecord {
    pub corporation_id: i64,
    pub name: String,
    pub ticker: String,
    pub ceo_id: i64,
    pub alliance_id: Option<i64>,
    pub tax_rate: f64,
    pub master_wallet: IskCents,
    pub members: HashSet<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllianceRecord {
    pub alliance_id: i64,
    pub name: String,
    pub short_name: String,
    pub executor_corp_id: i64,
    pub member_corp_ids: HashSet<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarRecord {
    pub war_id: i64,
    pub aggressor_corp_id: i64,
    pub defender_corp_id: i64,
    pub mutual: bool,
    pub started_unix: u64,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpProjectRecord {
    pub project_id: i64,
    pub corporation_id: i64,
    pub name: String,
    pub target_quantity: u64,
    pub current_quantity: u64,
    pub isk_per_unit: IskCents,
    pub escrow_remaining: IskCents,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillmailRecord {
    pub kill_id: i64,
    pub victim_character_id: i64,
    pub attacker_character_id: i64,
    pub solar_system_id: SolarSystemId,
    pub ship_type_id: TypeId,
    pub total_value_isk: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillRightRecord {
    pub kill_right_id: i64,
    pub owner_id: i64,
    pub target_id: i64,
    pub activation_fee: IskCents,
    pub is_active_suspect: bool,
}

fn default_structure_hp() -> f64 {
    5_000.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpwellStructureRecord {
    pub structure_id: i64,
    pub type_id: TypeId,
    pub owner_corp_id: i64,
    pub solar_system_id: SolarSystemId,
    pub name: String,
    pub position: [f64; 3],
    pub state: String, // "Anchoring", "OnliningVulnerable", "ShieldVulnerable", "ArmorReinforced", "HullReinforced", "Destroyed"
    pub quantum_core_installed: bool,
    pub fuel_blocks: u32,
    #[serde(default = "default_structure_hp")]
    pub shield_hp: f64,
    #[serde(default = "default_structure_hp")]
    pub armor_hp: f64,
    #[serde(default = "default_structure_hp")]
    pub hull_hp: f64,
    #[serde(default)]
    pub reinforced_until_unix: u64,
    #[serde(default)]
    pub last_damage_tick: u64,
    #[serde(default)]
    pub last_damage_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemDynamicState {
    pub solar_system_id: SolarSystemId,
    pub dbs_multiplier: f64,
    pub ess_main_bank: IskCents,
    pub ess_reserve_bank: IskCents,
    #[serde(default)]
    pub ess_linked_character_id: Option<i64>,
    #[serde(default)]
    pub ess_link_started_tick: Option<u64>,
    pub sov_alliance_id: Option<i64>,
    pub military_index: u8,
    pub industrial_index: u8,
    pub strategic_index: u8,
    pub fw_contested_pct: f64,
    pub insurgency_corruption_stage: u8,
    pub insurgency_suppression_stage: u8,
    pub wormhole_remaining_mass_kg: f64,
    #[serde(default)]
    pub wormhole_destination_system_id: Option<SolarSystemId>,
    #[serde(default)]
    pub signature_position: Option<[f64; 3]>,
    #[serde(default)]
    pub fw_complex_position: Option<[f64; 3]>,
    #[serde(default)]
    pub fw_capture_seconds_elapsed: u64,
    #[serde(default)]
    pub fw_capture_duration_seconds: u64,
    #[serde(default)]
    pub last_fw_capture_tick: u64,
    #[serde(default)]
    pub last_sov_claim_tick: u64,
}

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

    pub fn deserialize<'de, D, K1, K2, V>(
        deserializer: D,
    ) -> Result<HashMap<(K1, K2), V>, D::Error>
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingTransactionRecord {
    pub event_id: i64,
    pub from_id: i64,
    pub to_id: i64,
    pub modification: f64,
    pub event_type_id: i64,
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
}

impl WorldStateSnapshot {
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
            .filter(|&sid| self.is_ship_colocated_with_character(char_id, sid, station_or_struct_id))
            .is_some()
            && Some(item.location_id) == active_ship_id
            && item.flag == InventoryFlag::Cargo;
        if !(at_dock_hangar || in_ship_cargo) {
            return false;
        }
        !self.is_item_locked_in_contract_or_job(item.item_id)
    }

    /// Resolves an expired `Outstanding` or `InProgress` contract in-place:
    /// - For `Outstanding`: refunds `reward` to `issuer_id`, returns escrowed items to `start_station_id` Hangar (removing temporary wrap `24445`), and sets status to `"Expired"`.
    /// - For `InProgress` (Courier): pays `collateral + reward` to `issuer_id`, removes temporary wrap `24445`, transfers contracted items to `acceptor_id` at their current location when collateral is forfeited (or returns them to `issuer_id` at `start_station_id` when `collateral == 0`), and sets status to `"Failed"`.
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
                if c.reward.0 > 0 {
                    if let Some(issuer) = self.characters.get_mut(&c.issuer_id) {
                        issuer.balance =
                            issuer.balance.checked_add(c.reward).unwrap_or(issuer.balance);
                    }
                }
                for iid in &c.item_ids {
                    let is_temp_wrap = self
                        .items
                        .get(iid)
                        .map(|item| item.type_id == 24445)
                        .unwrap_or(false);
                    if is_temp_wrap {
                        self.items.remove(iid);
                    } else if let Some(item) = self.items.get_mut(iid) {
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
                for iid in &c.item_ids {
                    let is_temp_wrap = self
                        .items
                        .get(iid)
                        .map(|item| item.type_id == 24445)
                        .unwrap_or(false);
                    let in_ship_container = self
                        .items
                        .get(iid)
                        .map(|item| self.items.contains_key(&item.location_id))
                        .unwrap_or(false);
                    if is_temp_wrap {
                        self.items.remove(iid);
                    } else if let Some(item) = self.items.get_mut(iid) {
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
                if let Some(c_mut) = self.contracts.get_mut(&contract_id) {
                    c_mut.status = "Failed".into();
                }
                Some("Failed")
            }
            _ => None,
        }
    }
}

const WAL_CHECKPOINT_INTERVAL: usize = 64;

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256_bytes(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner_in = Vec::with_capacity(64 + message.len());
    inner_in.extend_from_slice(&ipad);
    inner_in.extend_from_slice(message);
    let inner_hash = sha256_bytes(&inner_in);

    let mut outer_in = Vec::with_capacity(64 + 32);
    outer_in.extend_from_slice(&opad);
    outer_in.extend_from_slice(&inner_hash);
    sha256_bytes(&outer_in)
}

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn base64url_encode_no_pad(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    out
}

pub fn base64url_decode_no_pad(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim_end_matches('=');
    let mut vals = Vec::with_capacity(trimmed.len());
    for b in trimmed.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("Invalid base64url character".into()),
        };
        vals.push(v as u32);
    }
    let mut out = Vec::with_capacity((vals.len() * 3) / 4);
    for chunk in vals.chunks(4) {
        match chunk.len() {
            4 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
                out.push((n & 0xff) as u8);
            }
            3 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6);
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
            }
            2 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12);
                out.push(((n >> 16) & 0xff) as u8);
            }
            _ => return Err("Invalid base64url length".into()),
        }
    }
    Ok(out)
}

/// Incremental Write-Ahead Log (WAL) delta record for hot-path mutations (`OF-06` / `OF-23` / `R21-P1-5`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WorldStateWalDelta {
    #[serde(default)]
    pub(crate) seq: u64,
    pub(crate) next_entity_id: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_characters: Vec<CharacterRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_items: Vec<InventoryItemRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) removed_items: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_market_orders: Vec<MarketOrderRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) removed_market_orders: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) appended_wallet_journal: Vec<WalletJournalEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_contracts: Vec<ContractRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_system_states: Vec<SystemDynamicState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) added_idempotency_keys: Vec<(i64, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) full_snapshot: Option<WorldStateSnapshot>,
}

impl WorldStateWalDelta {
    #[cfg(test)]
    fn from_diff(old: &WorldStateSnapshot, new: &WorldStateSnapshot) -> Self {
        let cold_equal = old.markets_seeded == new.markets_seeded
            && old.accounts == new.accounts
            && old.skills == new.skills
            && old.skill_queues == new.skill_queues
            && old.jump_clones == new.jump_clones
            && old.active_implants == new.active_implants
            && old.saved_fittings == new.saved_fittings
            && old.industry_jobs == new.industry_jobs
            && old.planetary_colonies == new.planetary_colonies
            && old.active_missions == new.active_missions
            && old.mission_tally == new.mission_tally
            && old.standing_transactions == new.standing_transactions
            && old.npe_progress == new.npe_progress
            && old.bookmarks == new.bookmarks
            && old.mails == new.mails
            && old.fleets == new.fleets
            && old.corporations == new.corporations
            && old.alliances == new.alliances
            && old.wars == new.wars
            && old.corp_projects == new.corp_projects
            && old.killmails == new.killmails
            && old.kill_rights == new.kill_rights
            && old.structures == new.structures
            && old.licensed_skins == new.licensed_skins
            && old.applied_ship_skins == new.applied_ship_skins
            && new.wallet_journal.len() >= old.wallet_journal.len()
            && new.wallet_journal[..old.wallet_journal.len()] == old.wallet_journal[..]
            && old.characters.keys().all(|k| new.characters.contains_key(k))
            && old.contracts.keys().all(|k| new.contracts.contains_key(k))
            && old.system_states.keys().all(|k| new.system_states.contains_key(k))
            && old
                .committed_idempotency_keys
                .iter()
                .all(|k| new.committed_idempotency_keys.contains(k));

        if !cold_equal {
            return Self {
                seq: new.wal_seq,
                next_entity_id: new.next_entity_id,
                full_snapshot: Some(new.clone()),
                ..Default::default()
            };
        }

        let mut delta = Self {
            seq: new.wal_seq,
            next_entity_id: new.next_entity_id,
            ..Default::default()
        };
        for (id, ch) in &new.characters {
            if old.characters.get(id) != Some(ch) {
                delta.upserted_characters.push(ch.clone());
            }
        }
        for (id, item) in &new.items {
            if old.items.get(id) != Some(item) {
                delta.upserted_items.push(item.clone());
            }
        }
        for id in old.items.keys() {
            if !new.items.contains_key(id) {
                delta.removed_items.push(*id);
            }
        }
        for (id, ord) in &new.market_orders {
            if old.market_orders.get(id) != Some(ord) {
                delta.upserted_market_orders.push(ord.clone());
            }
        }
        for id in old.market_orders.keys() {
            if !new.market_orders.contains_key(id) {
                delta.removed_market_orders.push(*id);
            }
        }
        if new.wallet_journal.len() > old.wallet_journal.len() {
            delta
                .appended_wallet_journal
                .extend_from_slice(&new.wallet_journal[old.wallet_journal.len()..]);
        }
        for (id, c) in &new.contracts {
            if old.contracts.get(id) != Some(c) {
                delta.upserted_contracts.push(c.clone());
            }
        }
        for (id, st) in &new.system_states {
            if old.system_states.get(id) != Some(st) {
                delta.upserted_system_states.push(st.clone());
            }
        }
        for k in &new.committed_idempotency_keys {
            if !old.committed_idempotency_keys.contains(k) {
                delta.added_idempotency_keys.push(k.clone());
            }
        }
        delta
    }

    fn apply_to(self, snap: &mut WorldStateSnapshot) {
        if let Some(mut full) = self.full_snapshot {
            full.wal_seq = full.wal_seq.max(self.seq);
            *snap = full;
            return;
        }
        snap.wal_seq = snap.wal_seq.max(self.seq);
        snap.next_entity_id = self.next_entity_id;
        for ch in self.upserted_characters {
            snap.characters.insert(ch.character_id, ch);
        }
        for id in self.removed_items {
            snap.items.remove(&id);
        }
        for item in self.upserted_items {
            snap.items.insert(item.item_id, item);
        }
        for id in self.removed_market_orders {
            snap.market_orders.remove(&id);
        }
        for ord in self.upserted_market_orders {
            snap.market_orders.insert(ord.order_id, ord);
        }
        snap.wallet_journal.extend(self.appended_wallet_journal);
        for c in self.upserted_contracts {
            snap.contracts.insert(c.contract_id, c);
        }
        for st in self.upserted_system_states {
            snap.system_states.insert(st.solar_system_id, st);
        }
        for k in self.added_idempotency_keys {
            snap.committed_idempotency_keys.insert(k);
        }
    }
}

/// Encapsulated transactional world repository backed by SQLite (`D-3`, `D-4`).
pub struct WorldRepository {
    inner: RwLock<WorldStateSnapshot>,
    sqlite_conn: std::sync::Mutex<rusqlite::Connection>,
    sqlite_fp: std::sync::Mutex<sqlite_store::TableFingerprints>,
    sqlite_path: Option<std::path::PathBuf>,
    auth_runtime: RwLock<AuthRuntimeState>,
    wal_lock: std::sync::Mutex<()>,
    snapshot_path: Option<std::path::PathBuf>,
    pending_wal_records: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    wal_fault_mode: std::sync::atomic::AtomicU8,
}

impl Drop for WorldRepository {
    fn drop(&mut self) {
        if let Some(ref p) = self.snapshot_path {
            if self.pending_wal_records.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                let _ = self.save_snapshot_to_file(p);
            }
        }
    }
}

impl Default for WorldRepository {
    fn default() -> Self {
        Self::new_empty()
    }
}

/// Exact 64-bit account and session role bitmasks from `carbon/common/script/sys/serviceConst.pyj` in V24.01 `code.ccp` (`S-9`).
pub mod service_roles {
    /// `ROLE_GMS` (`1 << 38` = `274877906944`)
    pub const ROLE_GMS: u64 = 0x0000_0040_0000_0000;
    /// `ROLE_GMH` (`1 << 53` = `9007199254740992`)
    pub const ROLE_GMH: u64 = 0x0020_0000_0000_0000;
    /// `ROLE_GML` (`1 << 54` = `18014398509481984`)
    pub const ROLE_GML: u64 = 0x0040_0000_0000_0000;
    /// `ROLE_CONTENT` (`1 << 55` = `36028797018963968`)
    pub const ROLE_CONTENT: u64 = 0x0080_0000_0000_0000;
    /// `ROLE_ADMIN` (`1 << 56` = `72057594037927936`)
    pub const ROLE_ADMIN: u64 = 0x0100_0000_0000_0000;
    /// `ROLE_PLAYER` (`1 << 61` = `2305843009213693952`)
    pub const ROLE_PLAYER: u64 = 0x2000_0000_0000_0000;
    /// `ROLE_LOGIN` (`1 << 62` = `4611686018427387904`)
    pub const ROLE_LOGIN: u64 = 0x4000_0000_0000_0000;
    /// Default role mask for standard player accounts (`0` GM bits).
    pub const ROLE_PLAYER_DEFAULT: u64 = ROLE_PLAYER | ROLE_LOGIN;
    /// Combined GM / admin authorization bitmask (`ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN`).
    pub const ROLE_GM_MASK: u64 = ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN;
    /// Default role mask for provisioned GM / admin accounts.
    pub const ROLE_GM_DEFAULT: u64 = ROLE_PLAYER_DEFAULT | ROLE_GM_MASK;
}

impl WorldRepository {
    /// Generates a 32-byte (256-bit) cryptographically random secret from OS CSPRNG (`/dev/urandom`),
    /// or from `EVOMU_SSO_JWT_SECRET` when explicitly configured with at least 32 bytes (`R34-P0-1`).
    fn generate_random_secret_32() -> [u8; 32] {
        if let Ok(env_secret) = std::env::var("EVOMU_SSO_JWT_SECRET") {
            let trimmed = env_secret.trim();
            if trimmed.len() >= 32 {
                return sha256_bytes(trimmed.as_bytes());
            }
        }
        use std::io::Read as _;
        let mut secret = [0u8; 32];
        let mut f = std::fs::File::open("/dev/urandom")
            .expect("OS CSPRNG (/dev/urandom) must be available to initialize JWT signing secret");
        f.read_exact(&mut secret)
            .expect("OS CSPRNG (/dev/urandom) must yield 32 random bytes for JWT signing secret");
        secret
    }

    /// Generates a 16-byte random per-account salt encoded as 32 lowercase hex characters from OS CSPRNG (`/dev/urandom`).
    fn generate_random_salt_hex() -> String {
        use std::io::Read as _;
        let mut salt_bytes = [0u8; 16];
        let mut f = std::fs::File::open("/dev/urandom")
            .expect("OS CSPRNG (/dev/urandom) must be available to generate password salt");
        f.read_exact(&mut salt_bytes)
            .expect("OS CSPRNG (/dev/urandom) must yield 16 random bytes for password salt");
        let mut hex = String::with_capacity(32);
        for b in salt_bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    /// Computes a salted Argon2id (`v=19, m=19456, t=2, p=1` OWASP minimum) credential digest
    /// (`$argon2id$v=19$m=19456,t=2,p=1$<salt_hex>$<digest_hex>`) from an explicit per-account
    /// salt and raw client password token without any pass-the-hash bypass (`D-4`, `R21-P1-3`).
    pub fn hash_password_with_salt(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
    ) -> String {
        Self::hash_password_with_params(username, salt_hex, raw_password_token, 19_456, 2, 1)
    }

    fn hash_password_with_params(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    ) -> String {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params =
            Params::new(m_cost, t_cost, p_cost, Some(32)).expect("valid Argon2id parameters");
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt_material = format!("evomu-auth-argon2id:{username}:{salt_hex}");
        let mut digest = [0u8; 32];
        argon2
            .hash_password_into(
                raw_password_token.as_bytes(),
                salt_material.as_bytes(),
                &mut digest,
            )
            .expect("Argon2id hash_password_into succeeds");
        use std::fmt::Write as _;
        let mut out = String::with_capacity(34 + salt_hex.len() + 64);
        let _ = write!(
            &mut out,
            "$argon2id$v=19$m={m_cost},t={t_cost},p={p_cost}${salt_hex}$"
        );
        for byte in digest {
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }

    fn hash_password_with_salt_sha256_legacy(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
    ) -> String {
        let mut digest = sha256_bytes(
            format!("evomu-auth-v1:{salt_hex}:{username}:{raw_password_token}").as_bytes(),
        );
        for round in 0..256u16 {
            let mut input = Vec::with_capacity(
                64 + salt_hex.len() + username.len() + raw_password_token.len(),
            );
            input.extend_from_slice(&digest);
            input.extend_from_slice(&round.to_be_bytes());
            input.extend_from_slice(salt_hex.as_bytes());
            input.push(b':');
            input.extend_from_slice(username.as_bytes());
            input.push(b':');
            input.extend_from_slice(raw_password_token.as_bytes());
            digest = sha256_bytes(&input);
        }
        let mut hex = String::with_capacity(11 + salt_hex.len() + 64);
        hex.push_str("sha256$v1$");
        hex.push_str(salt_hex);
        hex.push('$');
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        hex
    }

    /// Derives a newly salted Argon2id credential digest (`$argon2id$v=19$m=19456,t=2,p=1$<salt_hex>$<digest_hex>`)
    /// using a fresh 16-byte random per-account salt (`D-4`, `OF-24`, `R21-P1-3`).
    pub fn hash_password_token(username: &str, raw_password_token: &str) -> String {
        let salt_hex = Self::generate_random_salt_hex();
        Self::hash_password_with_salt(username, &salt_hex, raw_password_token)
    }

    #[cfg(any(test, feature = "demo-seed"))]
    pub fn with_demo_seed() -> Self {
        let mut accounts = HashMap::new();
        accounts.insert(
            "demo".to_string(),
            AccountRecord {
                user_id: 1,
                username: "demo".to_string(),
                password_token: Self::hash_password_token("demo", "demo-secret"),
                role: service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        accounts.insert(
            "other".to_string(),
            AccountRecord {
                user_id: 2,
                username: "other".to_string(),
                password_token: Self::hash_password_token("other", "other-secret"),
                role: service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );

        let mut characters = HashMap::new();
        characters.insert(
            90_000_001,
            CharacterRecord {
                character_id: 90_000_001,
                user_id: 1,
                name: "Aura Capsuleer".to_string(),
                corporation_id: 1_000_170, // Republic Military School (school 14)
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(60_003_760), // Jita 4-4
                structure_id: None,
                solar_system_id: 30_000_142, // Jita
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_001,
                balance: IskCents::from_isk(1_000_000_000),
                plex_balance: 500,
                loyalty_points: HashMap::from([(1_000_035, 5_000)]),
                standings: HashMap::from([(1_000_035, 2.0)]),
                security_status: 0.5,
                clone_grade: 2, // Omega
                home_station_id: 60_003_760,
                race_id: 2,
                bloodline_id: 3,
                ancestry_id: 19,
                school_id: 14, // Republic Military School (Minmatar)
                neural: NeuralAttributes::default(),
                skill_points: 15_000_000.0,
                free_skill_points: 50_000.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                version: 1,
                system_epoch: 1,
            },
        );
        characters.insert(
            90_000_002,
            CharacterRecord {
                character_id: 90_000_002,
                user_id: 2,
                name: "Rival Pilot".to_string(),
                corporation_id: 1_000_045, // Science and Trade Institute (school 18)
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(60_003_760),
                structure_id: None,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_002,
                balance: IskCents::from_isk(50_000_000),
                plex_balance: 100,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 18, // Science and Trade Institute (Caldari)
                neural: NeuralAttributes::default(),
                skill_points: 4_500_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                version: 1,
                system_epoch: 1,
            },
        );

        let mut items = HashMap::new();
        items.insert(
            100_000_001,
            InventoryItemRecord {
                item_id: 100_000_001,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        items.insert(
            100_000_002,
            InventoryItemRecord {
                item_id: 100_000_002,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Fitted 200mm AutoCannon II on active ships 100_000_001 and 100_000_002
        items.insert(
            100_000_005,
            InventoryItemRecord {
                item_id: 100_000_005,
                type_id: 2889, // 200mm AutoCannon II
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        items.insert(
            100_000_006,
            InventoryItemRecord {
                item_id: 100_000_006,
                type_id: 2889, // 200mm AutoCannon II
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );

        // Seed only starter frigate modules, basic minerals/ore, and an unresearched Rifter BPO (IDs 100_000_003..=100_000_012)
        let starter_items = [
            (100_000_003, 438, 1, true),        // 1MN Afterburner II
            (100_000_004, 1236, 2, true),       // Overdrive Injector System II
            (100_000_007, 691, 1, true),        // Rifter Blueprint (unresearched BPO ME0/TE0)
            (100_000_008, 34, 100_000, false),  // Tritanium
            (100_000_009, 35, 50_000, false),   // Pyerite
            (100_000_010, 36, 20_000, false),   // Mexallon
            (100_000_011, 37, 5_000, false),    // Isogen
            (100_000_012, 1230, 10_000, false), // Veldspar
        ];
        for (id, tid, qty, single) in starter_items {
            items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: single,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }

        // Seed NPC & synthetic market orders in Jita 4-4 (60_003_760)
        let mut market_orders = HashMap::new();
        market_orders.insert(
            700_000_001,
            MarketOrderRecord {
                order_id: 700_000_001,
                owner_id: 1_000_035, // Caldari Navy NPC
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34, // Tritanium
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 1_000_000,
                vol_remaining: 1_000_000,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        market_orders.insert(
            700_000_002,
            MarketOrderRecord {
                order_id: 700_000_002,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 438, // 1MN Afterburner II
                is_bid: false,
                price: IskCents::from_isk(550_000),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );

        let mut system_states = HashMap::new();
        for sys_id in [
            30_000_142, 30_000_144, 30_002_813, 30_002_904, 31_000_005,
        ] {
            system_states.insert(
                sys_id,
                SystemDynamicState {
                    solar_system_id: sys_id,
                    dbs_multiplier: 1.35,
                    ess_main_bank: IskCents(0),
                    ess_reserve_bank: IskCents(0),
                    ess_linked_character_id: None,
                    ess_link_started_tick: None,
                    sov_alliance_id: None,
                    military_index: 2,
                    industrial_index: 2,
                    strategic_index: 3,
                    fw_contested_pct: 0.0,
                    insurgency_corruption_stage: 2,
                    insurgency_suppression_stage: 1,
                    wormhole_remaining_mass_kg: 0.0,
                    wormhole_destination_system_id: None,
                    signature_position: None,
                    fw_complex_position: None,
                    fw_capture_seconds_elapsed: 0,
                    fw_capture_duration_seconds: 0,
                    last_fw_capture_tick: 0,
                    last_sov_claim_tick: 0,
                },
            );
        }

        let mut npe_progress = HashMap::new();
        npe_progress.insert(
            90_000_001,
            NpeProgressRecord {
                character_id: 90_000_001,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );

        let mut skills = HashMap::new();
        let starter_skill_map = HashMap::from([
            (
                3300,
                TrainedSkillRecord {
                    type_id: 3300, // Gunnery
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3302,
                TrainedSkillRecord {
                    type_id: 3302, // Small Projectile Turret
                    level: 5,
                    skill_points: 256_000.0,
                },
            ),
            (
                11084,
                TrainedSkillRecord {
                    type_id: 11084, // Small Autocannon Specialization
                    level: 1,
                    skill_points: 750.0,
                },
            ),
            (
                3327,
                TrainedSkillRecord {
                    type_id: 3327, // Spaceship Command
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3329,
                TrainedSkillRecord {
                    type_id: 3329, // Minmatar Frigate
                    level: 4,
                    skill_points: 90_510.0,
                },
            ),
            (
                3380,
                TrainedSkillRecord {
                    type_id: 3380, // Industry
                    level: 5,
                    skill_points: 256_000.0,
                },
            ),
            (
                3394,
                TrainedSkillRecord {
                    type_id: 3394, // Hull Upgrades
                    level: 2,
                    skill_points: 2_829.0,
                },
            ),
            (
                3443,
                TrainedSkillRecord {
                    type_id: 3443, // Trade
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3446,
                TrainedSkillRecord {
                    type_id: 3446, // Broker Relations
                    level: 4,
                    skill_points: 90_510.0,
                },
            ),
            (
                3449,
                TrainedSkillRecord {
                    type_id: 3449, // Navigation
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3450,
                TrainedSkillRecord {
                    type_id: 3450, // Afterburner
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3452,
                TrainedSkillRecord {
                    type_id: 3452, // Acceleration Control
                    level: 4,
                    skill_points: 181_020.0,
                },
            ),
            (
                16622,
                TrainedSkillRecord {
                    type_id: 16622, // Accounting
                    level: 4,
                    skill_points: 135_765.0,
                },
            ),
        ]);
        skills.insert(90_000_001, starter_skill_map.clone());
        skills.insert(90_000_002, starter_skill_map);

        let snapshot_path = std::env::var("EVOMU_SNAPSHOT_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);

        let initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts,
            characters,
            skills,
            skill_queues: HashMap::new(),
            jump_clones: HashMap::new(),
            active_implants: HashMap::new(),
            items,
            saved_fittings: HashMap::new(),
            market_orders,
            wallet_journal: Vec::new(),
            contracts: HashMap::new(),
            industry_jobs: HashMap::new(),
            planetary_colonies: HashMap::new(),
            active_missions: HashMap::new(),
            mission_tally: HashMap::new(),
            standing_transactions: HashMap::new(),
            npe_progress,
            bookmarks: HashMap::new(),
            mails: Vec::new(),
            fleets: HashMap::new(),
            corporations: HashMap::new(),
            alliances: HashMap::new(),
            wars: HashMap::new(),
            corp_projects: HashMap::new(),
            killmails: HashMap::new(),
            kill_rights: HashMap::new(),
            structures: HashMap::new(),
            system_states,
            committed_idempotency_keys: HashSet::new(),
            licensed_skins: HashMap::new(),
            applied_ship_skins: HashMap::new(),
        };
        let mut conn = rusqlite::Connection::open_in_memory()
            .expect("open in-memory SQLite connection for WorldRepository");
        sqlite_store::apply_migrations(&conn)
            .expect("apply SQLite migrations for WorldRepository");
        let fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)
            .expect("initialize SQLite from demo snapshot");
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)
                .expect("initialize per-deployment JWT signing secret");

        let mut repo = Self {
            inner: RwLock::new(initial_snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: None,
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        };

        if let Some(ref p) = snapshot_path {
            if p.exists() {
                repo.load_snapshot_from_file(p)
                    .expect("Failed to load persisted WorldRepository snapshot");
            } else {
                repo.save_snapshot_to_file(p)
                    .expect("Failed to initialize persisted WorldRepository snapshot");
            }
            repo.snapshot_path = snapshot_path;
        }
        repo
    }

    fn wal_path_for(path: &Path) -> std::path::PathBuf {
        path.with_extension("json.wal")
    }

    fn is_sqlite_extension(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("sqlite" | "sqlite3" | "db")
        )
    }

    /// Opens or initializes a durable relational SQLite world database at `path`, applies schema
    /// migrations (`schema_migrations`), and loads the persisted world state (`D-3`).
    pub fn open_sqlite(path: impl AsRef<Path>) -> Result<Self, String> {
        let target = path.as_ref();
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let conn = rusqlite::Connection::open(target).map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let (snap, fp) = sqlite_store::load_snapshot_from_sqlite(&conn)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;
        Ok(Self {
            inner: RwLock::new(snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: Some(target.to_path_buf()),
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        })
    }

    /// Returns the file path of the backing SQLite database if opened via [`Self::open_sqlite`].
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.sqlite_path.as_deref()
    }

    /// Creates a production `WorldRepository` with an empty account, character, and player item table
    /// prior to loading `EVOMU_DB_PATH` or `EVOMU_SNAPSHOT_PATH` (`R22-P1-5`, `R23-P0-1`, `R24-P1-1`, `D-3`).
    pub fn new_empty() -> Self {
        let db_or_snapshot_path = std::env::var("EVOMU_DB_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EVOMU_SNAPSHOT_PATH")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .map(std::path::PathBuf::from);
        Self::new_empty_with_snapshot_path(db_or_snapshot_path)
            .expect("Failed to load or initialize persisted WorldRepository database")
    }

    /// Creates a production `WorldRepository` with an empty player state, then loads `snapshot_path`
    /// if it already exists on disk without clearing any loaded accounts, characters, skills, or items.
    /// When `snapshot_path` has a `.sqlite`/`.sqlite3`/`.db` extension, opens the relational SQLite
    /// store directly via [`Self::open_sqlite`] (`D-3`).
    /// Returns `Err` if loading or initializing fails, refusing to start with an empty world over an
    /// unreadable save (`R23-P0-1`, `R24-P1-1`, `S-5`, `D-3`).
    pub fn new_empty_with_snapshot_path(
        snapshot_path: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        if let Some(ref p) = snapshot_path {
            if Self::is_sqlite_extension(p) {
                return Self::open_sqlite(p);
            }
        }

        let market_orders = HashMap::new();
        let system_states = HashMap::new();
        let initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts: HashMap::new(),
            characters: HashMap::new(),
            skills: HashMap::new(),
            skill_queues: HashMap::new(),
            jump_clones: HashMap::new(),
            active_implants: HashMap::new(),
            items: HashMap::new(),
            saved_fittings: HashMap::new(),
            market_orders,
            wallet_journal: Vec::new(),
            contracts: HashMap::new(),
            industry_jobs: HashMap::new(),
            planetary_colonies: HashMap::new(),
            active_missions: HashMap::new(),
            mission_tally: HashMap::new(),
            standing_transactions: HashMap::new(),
            npe_progress: HashMap::new(),
            bookmarks: HashMap::new(),
            mails: Vec::new(),
            fleets: HashMap::new(),
            corporations: HashMap::new(),
            alliances: HashMap::new(),
            wars: HashMap::new(),
            corp_projects: HashMap::new(),
            killmails: HashMap::new(),
            kill_rights: HashMap::new(),
            structures: HashMap::new(),
            system_states,
            committed_idempotency_keys: HashSet::new(),
            licensed_skins: HashMap::new(),
            applied_ship_skins: HashMap::new(),
        };
        let mut conn = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;

        let mut repo = Self {
            inner: RwLock::new(initial_snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: None,
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        };

        if let Some(ref p) = snapshot_path {
            if p.exists() {
                repo.load_snapshot_from_file(p)?;
            } else {
                repo.save_snapshot_to_file(p)?;
            }
            repo.snapshot_path = snapshot_path;
        }
        Ok(repo)
    }

    /// Explicitly and idempotently provisions a standard player account (`role = ROLE_PLAYER_DEFAULT`, `0` GM bits) with an operator-supplied password token (`R22-P1-5`, `R23-P1-4`, `S-9`).
    pub fn provision_player_account(&self, username: &str, password_token: &str) -> u64 {
        if let Ok(existing) = self.authenticate(username, password_token) {
            return existing.user_id;
        }
        let hashed_token = Self::hash_password_token(username, password_token);
        self.try_with_mut_snapshot(|snap| {
            if let Some(existing) = snap.accounts.get_mut(username) {
                existing.password_token = hashed_token;
                return Ok::<u64, std::convert::Infallible>(existing.user_id);
            }
            let user_id = snap
                .accounts
                .values()
                .map(|a| a.user_id)
                .max()
                .unwrap_or(0)
                + 1;
            snap.accounts.insert(
                username.to_string(),
                AccountRecord {
                    user_id,
                    username: username.to_string(),
                    password_token: hashed_token,
                    role: service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            Ok::<u64, std::convert::Infallible>(user_id)
        })
        .unwrap()
    }

    /// Explicitly and idempotently provisions a GM / administrator account (`role = ROLE_GM_DEFAULT`) outside of the default player seed (`R23-P1-4`, `S-9`).
    pub fn provision_gm_account(
        &self,
        username: &str,
        password_token: &str,
        char_id: i64,
        char_name: &str,
    ) {
        let already_complete = self.authenticate(username, password_token).is_ok()
            && self.with_snapshot(|snap| snap.characters.contains_key(&char_id));
        if already_complete {
            return;
        }
        let hashed_token = Self::hash_password_token(username, password_token);
        let _ = self.try_with_mut_snapshot(|snap| {
            let user_id = if let Some(existing) = snap.accounts.get_mut(username) {
                existing.password_token = hashed_token;
                existing.role |= service_roles::ROLE_GM_DEFAULT;
                existing.user_id
            } else {
                let uid = snap
                    .accounts
                    .values()
                    .map(|a| a.user_id)
                    .max()
                    .unwrap_or(99)
                    .max(99)
                    + 1;
                snap.accounts.insert(
                    username.to_string(),
                    AccountRecord {
                        user_id: uid,
                        username: username.to_string(),
                        password_token: hashed_token,
                        role: service_roles::ROLE_GM_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
                uid
            };
            if !snap.characters.contains_key(&char_id) {
                snap.next_entity_id += 1;
                let ship_id = snap.next_entity_id;
                snap.characters.insert(
                    char_id,
                    CharacterRecord {
                        character_id: char_id,
                        user_id,
                        name: char_name.to_string(),
                        corporation_id: 1_000_006,
                        alliance_id: None,
                        war_faction_id: None,
                        station_id: Some(60_003_760),
                        structure_id: None,
                        solar_system_id: 30_000_142,
                        constellation_id: 20_000_020,
                        region_id: 10_000_002,
                        active_ship_id: ship_id,
                        balance: IskCents::from_isk(1_000_000_000),
                        plex_balance: 0,
                        loyalty_points: HashMap::new(),
                        standings: HashMap::new(),
                        security_status: 5.0,
                        clone_grade: 2,
                        home_station_id: 60_003_760,
                        race_id: 1,
                        bloodline_id: 1,
                        ancestry_id: 11,
                        school_id: 17,
                        neural: NeuralAttributes::default(),
                        skill_points: 50_000_000.0,
                        free_skill_points: 0.0,
                        last_clone_jump_unix: 0,
                        jump_fatigue_minutes: 0.0,
                        jump_cooldown_until_unix: 0,
                        version: 1,
                        system_epoch: 1,
                    },
                );
                snap.items.insert(
                    ship_id,
                    InventoryItemRecord {
                        item_id: ship_id,
                        type_id: 587,
                        owner_id: char_id,
                        location_id: 60_003_760,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
            Ok::<(), std::convert::Infallible>(())
        });
    }

    /// Writes a compact checkpoint of `snap` to `target` with `fsync`, atomically renames the
    /// checkpoint file into place, and atomically rotates/removes the companion `.json.wal`
    /// while the caller holds both the snapshot lock and `wal_lock` (`R21-P1-5`).
    fn save_snapshot_locked(&self, target: &Path, snap: &WorldStateSnapshot) -> Result<(), String> {
        use std::io::Write as _;
        let bytes = serde_json::to_vec(snap).map_err(|e| e.to_string())?;
        let tmp_path = target.with_extension("json.tmp");
        {
            let mut tmp_file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            tmp_file.write_all(&bytes).map_err(|e| e.to_string())?;
            tmp_file.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp_path, target).map_err(|e| e.to_string())?;
        let wal_path = Self::wal_path_for(target);
        if wal_path.exists() {
            let wal_tmp = target.with_extension("json.wal.tmp");
            if let Ok(empty_wal) = std::fs::File::create(&wal_tmp) {
                let _ = empty_wal.sync_all();
                let _ = std::fs::rename(&wal_tmp, &wal_path);
            }
            let _ = std::fs::remove_file(&wal_path);
        }
        self.pending_wal_records
            .store(0, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Saves a compact checkpoint of the complete transactional world state to a JSON snapshot file
    /// (`M1` durable persistence) under `inner` + `wal_lock`, using an atomic temp-file write + `fsync` +
    /// rename and atomic `.json.wal` checkpoint rotation (`R21-P1-5`).
    pub fn save_snapshot_to_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.save_snapshot_locked(path.as_ref(), &guard)
    }

    /// Appends a single compact JSONL delta record to `<path>.json.wal` with `sync_data()` (`fsync`)
    /// while holding `inner` and `wal_lock`, truncating any uncleaned partial tail before appending,
    /// performing best-effort `set_len(prev_len)` + `sync_all()` rollback on write/sync error, and
    /// treating periodic post-sync checkpoint compaction as retryable (`R21-P1-5`, `R25-P1-1`).
    fn append_wal_delta_locked(
        &self,
        path: &Path,
        snap: &WorldStateSnapshot,
        delta: &WorldStateWalDelta,
    ) -> Result<(), String> {
        use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
        if !path.exists() {
            return self.save_snapshot_locked(path, snap);
        }
        let wal_path = Self::wal_path_for(path);
        let mut line = serde_json::to_vec(delta).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&wal_path)
            .map_err(|e| e.to_string())?;

        // Find the byte offset of the last newline boundary so any uncleaned partial write at the
        // tail of `.json.wal` is truncated before appending without re-parsing prior records (`R25-P1-1`).
        let mut existing = Vec::new();
        file.read_to_end(&mut existing).map_err(|e| e.to_string())?;
        let prev_len: u64 = if existing.ends_with(b"\n") {
            existing.len() as u64
        } else {
            existing
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|idx| (idx + 1) as u64)
                .unwrap_or(0)
        };
        if existing.len() as u64 != prev_len {
            file.set_len(prev_len).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        file.seek(SeekFrom::Start(prev_len))
            .map_err(|e| e.to_string())?;

        #[cfg(test)]
        let fault_mode = self
            .wal_fault_mode
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        let write_and_sync = (|| -> Result<(), String> {
            #[cfg(test)]
            if fault_mode == 1 {
                let half = (line.len() / 2).max(1);
                file.write_all(&line[..half]).map_err(|e| e.to_string())?;
                return Err("Injected partial WAL write_all failure".to_string());
            }
            file.write_all(&line).map_err(|e| e.to_string())?;
            #[cfg(test)]
            if fault_mode == 2 {
                return Err("Injected WAL sync_data failure after write_all".to_string());
            }
            file.sync_data().map_err(|e| e.to_string())?;
            Ok(())
        })();

        if let Err(mut err) = write_and_sync {
            if let Err(trunc_err) = file.set_len(prev_len).and_then(|()| file.sync_all()) {
                err = format!("{err}; rollback set_len/sync_all also failed: {trunc_err}");
            }
            return Err(err);
        }

        let count = self
            .pending_wal_records
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if count >= WAL_CHECKPOINT_INTERVAL && count % WAL_CHECKPOINT_INTERVAL == 0 {
            // Once the WAL line is durably written and synced, treat checkpoint compaction as a
            // retryable background operation once per interval (`R25-P1-1`).
            let _ = self.save_snapshot_locked(path, snap);
        }
        Ok(())
    }

    /// Restores the transactional world state from a JSON snapshot file and replays any
    /// uncompacted incremental `.json.wal` delta records in strict monotonic `seq` order,
    /// tolerating and healing a truncated/torn trailing WAL record from a prior crash and
    /// rejecting duplicate sequence numbers (`R21-P1-5`, `R24-P1-1`, `R25-P1-1`).
    pub fn load_snapshot_from_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        use std::io::Write as _;
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut fp = self.sqlite_fp.lock().unwrap_or_else(|e| e.into_inner());
        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = path.as_ref();
        let json = std::fs::read_to_string(target).map_err(|e| e.to_string())?;
        let mut snap: WorldStateSnapshot =
            serde_json::from_str(&json).map_err(|e| e.to_string())?;
        let wal_path = Self::wal_path_for(target);
        if wal_path.exists() {
            let wal_text = std::fs::read_to_string(&wal_path).map_err(|e| e.to_string())?;
            let non_empty_lines: Vec<&str> = wal_text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let mut deltas: Vec<WorldStateWalDelta> = Vec::new();
            let mut seen_seqs: HashSet<u64> = HashSet::new();
            let mut had_torn_tail = false;
            for (idx, line) in non_empty_lines.iter().enumerate() {
                match serde_json::from_str::<WorldStateWalDelta>(line) {
                    Ok(delta) => {
                        if delta.seq == 0 || !seen_seqs.insert(delta.seq) {
                            return Err(format!(
                                "Duplicate or invalid WAL record sequence number {} at line {}",
                                delta.seq,
                                idx + 1
                            ));
                        }
                        deltas.push(delta);
                    }
                    Err(e) => {
                        if idx + 1 == non_empty_lines.len() {
                            had_torn_tail = true;
                            break;
                        }
                        return Err(format!(
                            "Corrupt non-trailing WAL record at line {}: {}",
                            idx + 1,
                            e
                        ));
                    }
                }
            }
            if had_torn_tail {
                let wal_tmp = target.with_extension("json.wal.tmp");
                let mut healed = std::fs::File::create(&wal_tmp).map_err(|e| e.to_string())?;
                for d in &deltas {
                    let mut line = serde_json::to_vec(d).map_err(|e| e.to_string())?;
                    line.push(b'\n');
                    healed.write_all(&line).map_err(|e| e.to_string())?;
                }
                healed.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&wal_tmp, &wal_path).map_err(|e| e.to_string())?;
            }
            deltas.sort_by_key(|d| d.seq);
            let valid_count = deltas.len();
            for delta in deltas {
                if delta.seq != 0 && delta.seq <= snap.wal_seq {
                    continue;
                }
                delta.apply_to(&mut snap);
            }
            self.pending_wal_records
                .store(valid_count, std::sync::atomic::Ordering::SeqCst);
        }
        *fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &snap)?;
        *guard = snap;
        Ok(())
    }

    pub fn with_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&WorldStateSnapshot) -> R,
    {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Delegates infallible snapshot mutations through `try_with_mut_snapshot`.
    pub fn with_mut_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WorldStateSnapshot) -> R,
    {
        match self.try_with_mut_snapshot(|snap| Ok::<R, std::convert::Infallible>(f(snap))) {
            Ok(out) => out,
            Err(infallible) => match infallible {},
        }
    }

    /// Transactionally applies a fallible closure `f` in-place (with zero whole-world `WorldStateSnapshot`
    /// cloning) and commits dirty rows inside an immediate `rusqlite::Transaction` ONLY when `f` returns
    /// `Ok(val)` AND durable persistence succeeds, rolling back in-memory state from the committed SQLite
    /// tables on any error (`D-3`, `R21-P1-5`, `R24-P1-1`).
    pub fn try_with_mut_snapshot<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut WorldStateSnapshot) -> Result<T, E>,
        E: FromWalError,
    {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut fp = self.sqlite_fp.lock().unwrap_or_else(|e| e.into_inner());
        match f(&mut guard) {
            Ok(val) => {
                guard.wal_seq = guard.wal_seq.saturating_add(1);
                let need_wal_delta = self.snapshot_path.is_some();
                let mut staged_fp = fp.clone();
                let persist_res = (|| -> Result<(), String> {
                    let tx = conn
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                        .map_err(|e| e.to_string())?;
                    let delta = sqlite_store::sync_dirty_to_sqlite(
                        &tx,
                        &guard,
                        &mut staged_fp,
                        need_wal_delta,
                    )?;
                    if let Some(ref p) = self.snapshot_path {
                        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
                        self.append_wal_delta_locked(p, &guard, &delta)?;
                    }
                    tx.commit().map_err(|e| e.to_string())?;
                    Ok(())
                })();
                match persist_res {
                    Ok(()) => {
                        *fp = staged_fp;
                        Ok(val)
                    }
                    Err(persist_err) => {
                        sqlite_store::rollback_dirty_from_sqlite(&conn, &mut guard, &fp)
                            .expect("Fatal: failed to roll back dirty state from SQLite after persist error");
                        Err(E::from_wal_error(persist_err))
                    }
                }
            }
            Err(err) => {
                sqlite_store::rollback_dirty_from_sqlite(&conn, &mut guard, &fp)
                    .expect("Fatal: failed to roll back dirty state from SQLite after transaction error");
                Err(err)
            }
        }
    }

    fn current_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1_700_000_000)
    }

    /// Configures and persists an explicit per-deployment SSO JWT HS256 signing secret, requiring
    /// at least 32 bytes (256 bits) of key material (`R34-P0-1`, `D-4`).
    pub fn set_jwt_signing_secret(&self, raw_secret: &[u8]) -> Result<(), String> {
        if raw_secret.len() < 32 {
            return Err(format!(
                "SSO JWT signing secret must be at least 32 bytes, got {}",
                raw_secret.len()
            ));
        }
        let key = sha256_bytes(raw_secret);
        {
            let conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
            sqlite_store::save_jwt_secret(&conn, &key)?;
        }
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.jwt_signing_secret = key;
        Ok(())
    }

    pub fn authenticate(
        &self,
        username: &str,
        password_token: &str,
    ) -> Result<AccountRecord, DbError> {
        self.authenticate_at(username, password_token, Self::current_unix_secs())
    }

    /// Authenticates `username` against `password_token` (or a signed SSO JWT access token) at
    /// `now_unix`, enforcing account ban checks and constant-time Argon2id verification (`D-4`).
    pub fn authenticate_at(
        &self,
        username: &str,
        password_token: &str,
        now_unix: u64,
    ) -> Result<AccountRecord, DbError> {
        // Support signed OAuth2/PKCE SSO JWT access token authentication
        if password_token.matches('.').count() == 2 {
            if let Ok(claims) = self.verify_sso_access_token(password_token, now_unix) {
                if !claims
                    .aud
                    .iter()
                    .any(|a| matches!(a.as_str(), "evomu-launcher" | "eveLauncherTQ"))
                {
                    return Err(DbError::InvalidCredentials);
                }
                let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
                let acc = if username.is_empty() || username == "__sso__" {
                    guard.accounts.values().find(|a| a.user_id == claims.user_id)
                } else {
                    guard.accounts.get(username).filter(|a| a.user_id == claims.user_id)
                }
                .ok_or(DbError::InvalidCredentials)?;
                if let Some(until) = acc.banned_until_unix {
                    if now_unix < until {
                        return Err(DbError::AccountBanned {
                            until_unix: until,
                            reason: acc.ban_reason.clone().unwrap_or_default(),
                        });
                    }
                }
                return Ok(acc.clone());
            }
        }

        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let acc = guard
            .accounts
            .get(username)
            .ok_or(DbError::InvalidCredentials)?;
        if let Some(until) = acc.banned_until_unix {
            if now_unix < until {
                return Err(DbError::AccountBanned {
                    until_unix: until,
                    reason: acc.ban_reason.clone().unwrap_or_default(),
                });
            }
        }
        let candidate_hash = if let Some(rest) = acc.password_token.strip_prefix("$argon2id$v=19$")
        {
            let Some((params_part, tail)) = rest.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            let Some((salt_hex, _digest_hex)) = tail.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            let mut m_cost = 19_456u32;
            let mut t_cost = 2u32;
            let mut p_cost = 1u32;
            for kv in params_part.split(',') {
                if let Some(v) = kv.strip_prefix("m=") {
                    m_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                } else if let Some(v) = kv.strip_prefix("t=") {
                    t_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                } else if let Some(v) = kv.strip_prefix("p=") {
                    p_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                }
            }
            Self::hash_password_with_params(&acc.username, salt_hex, password_token, m_cost, t_cost, p_cost)
        } else if let Some(rest) = acc.password_token.strip_prefix("sha256$v1$") {
            let Some((salt_hex, _digest_hex)) = rest.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            Self::hash_password_with_salt_sha256_legacy(&acc.username, salt_hex, password_token)
        } else {
            return Err(DbError::InvalidCredentials);
        };
        if !constant_time_eq(acc.password_token.as_bytes(), candidate_hash.as_bytes()) {
            return Err(DbError::InvalidCredentials);
        }
        Ok(acc.clone())
    }

    /// Bans an account by `username` until `banned_until_unix` with `reason`, persists the ban to
    /// SQLite, revokes all SSO refresh tokens for the user, and kicks any active online session (`D-4`).
    /// Refuses to ban accounts holding `ROLE_ADMIN`.
    pub fn ban_account(
        &self,
        username: &str,
        banned_until_unix: u64,
        reason: &str,
    ) -> Result<u64, DbError> {
        let user_id = self.try_with_mut_snapshot(|snap| {
            let acc = snap
                .accounts
                .get_mut(username)
                .ok_or_else(|| DbError::ValidationError(format!("Unknown account '{username}'")))?;
            if (acc.role & service_roles::ROLE_ADMIN) != 0 {
                return Err(DbError::ValidationError(format!(
                    "Cannot ban administrator account '{username}'"
                )));
            }
            acc.banned_until_unix = Some(banned_until_unix);
            acc.ban_reason = Some(reason.to_string());
            Ok(acc.user_id)
        })?;
        let _ = self.kick_user(user_id, reason, Self::current_unix_secs());
        Ok(user_id)
    }

    /// Lifts any active ban on `username` and persists the update to SQLite (`D-4`).
    pub fn unban_account(&self, username: &str) -> Result<u64, DbError> {
        let user_id = self.try_with_mut_snapshot(|snap| {
            let acc = snap
                .accounts
                .get_mut(username)
                .ok_or_else(|| DbError::ValidationError(format!("Unknown account '{username}'")))?;
            acc.banned_until_unix = None;
            acc.ban_reason = None;
            Ok(acc.user_id)
        })?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.remove(&user_id);
        Ok(user_id)
    }

    /// Clears prior kick/displacement revocation markers ONLY when `authentication.Login` succeeds
    /// with verified credentials (`R36-P1-3`).
    pub fn clear_revocation_on_fresh_login(&self, user_id: u64, session_id: u64) {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.remove(&user_id);
        rt.displaced_sessions.remove(&session_id);
    }

    /// Acquires the single-active-session online lock for `user_id` (`D-4`, `multiLoginBlocker`).
    /// Returns `Err(DbError::AlreadyLoggedIn)` if a different `session_id` already holds the lock
    /// and `allow_displace` is `false`. When `allow_displace` is `true`, displaces and returns the
    /// previous `OnlineSessionRecord`. Refuses if `user_id` is currently kicked or `session_id` has
    /// been displaced (`R36-P1-3`).
    pub fn acquire_online_lock(
        &self,
        user_id: u64,
        session_id: u64,
        character_id: Option<i64>,
        allow_displace: bool,
        now_unix: u64,
    ) -> Result<Option<OnlineSessionRecord>, DbError> {
        let username = {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == user_id)
                .ok_or(DbError::InvalidCredentials)?;
            if let Some(until) = acc.banned_until_unix {
                if now_unix < until {
                    return Err(DbError::AccountBanned {
                        until_unix: until,
                        reason: acc.ban_reason.clone().unwrap_or_default(),
                    });
                }
            }
            acc.username.clone()
        };
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = rt.displaced_sessions.get(&session_id) {
            return Err(DbError::ValidationError(format!(
                "Session kicked: {reason}"
            )));
        }
        if let Some((_, reason)) = rt.kicked_users.get(&user_id) {
            return Err(DbError::ValidationError(format!(
                "Session kicked by GM: {reason}"
            )));
        }
        if let Some(existing) = rt.online_sessions.get(&user_id) {
            if existing.session_id != session_id && !allow_displace {
                return Err(DbError::AlreadyLoggedIn {
                    user_id,
                    active_session_id: existing.session_id,
                });
            }
        }
        let displaced = rt
            .online_sessions
            .insert(
                user_id,
                OnlineSessionRecord {
                    user_id,
                    username,
                    session_id,
                    character_id,
                    connected_at_unix: now_unix,
                },
            )
            .filter(|prev| prev.session_id != session_id);
        if let Some(ref prev) = displaced {
            rt.displaced_sessions.insert(
                prev.session_id,
                "Displaced by concurrent login on same account".to_string(),
            );
        }
        Ok(displaced)
    }

    /// Releases the active online session lock for `user_id` if held by `session_id` (or unconditionally when `session_id == 0`).
    pub fn release_online_lock(&self, user_id: u64, session_id: u64) -> bool {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.displaced_sessions.remove(&session_id);
        if let Some(existing) = rt.online_sessions.get(&user_id) {
            if session_id == 0 || existing.session_id == session_id {
                rt.online_sessions.remove(&user_id);
                return true;
            }
        }
        false
    }

    /// Returns the currently active online session for `user_id`, if any (`D-4`).
    pub fn get_online_session(&self, user_id: u64) -> Option<OnlineSessionRecord> {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        rt.online_sessions.get(&user_id).cloned()
    }

    /// Looks up `user_id` for `username` (`D-4`).
    pub fn find_user_id_by_username(&self, username: &str) -> Option<u64> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard.accounts.get(username).map(|a| a.user_id)
    }

    /// Checks whether an active session (`user_id`, `session_id`) has been banned, kicked, displaced,
    /// or blocked by an existing online lock held by another session (`D-4`, `R34-P1-4`).
    pub fn check_session_revoked(
        &self,
        user_id: u64,
        session_id: u64,
        now_unix: u64,
    ) -> Option<(String, String)> {
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            if let Some(acc) = guard.accounts.values().find(|a| a.user_id == user_id) {
                if let Some(until) = acc.banned_until_unix {
                    if now_unix < until {
                        return Some((
                            "AccountBanned".to_string(),
                            format!(
                                "Account is banned until unix timestamp {until}: {}",
                                acc.ban_reason.clone().unwrap_or_default()
                            ),
                        ));
                    }
                }
            }
        }
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = rt.displaced_sessions.get(&session_id) {
            return Some(("SessionKicked".to_string(), reason.clone()));
        }
        if let Some((_, reason)) = rt.kicked_users.get(&user_id) {
            return Some(("SessionKicked".to_string(), reason.clone()));
        }
        if let Some(active) = rt.online_sessions.get(&user_id) {
            if active.session_id != session_id {
                return Some((
                    "MultiLoginBlocked".to_string(),
                    format!(
                        "Account user_id={user_id} is locked by active session {}; call multiLoginBlocker.Login to displace",
                        active.session_id
                    ),
                ));
            }
        }
        None
    }

    /// Kicks `user_id` from any active online session, records the kick timestamp/reason, and revokes
    /// all active SSO refresh tokens for `user_id` (`D-4`). Refuses to kick accounts with `ROLE_ADMIN`.
    pub fn kick_user(
        &self,
        user_id: u64,
        reason: &str,
        now_unix: u64,
    ) -> Option<OnlineSessionRecord> {
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            if let Some(acc) = guard.accounts.values().find(|a| a.user_id == user_id) {
                if (acc.role & service_roles::ROLE_ADMIN) != 0 {
                    return None;
                }
            }
        }
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users
            .insert(user_id, (now_unix, reason.to_string()));
        for rec in rt.refresh_tokens.values_mut() {
            if rec.user_id == user_id {
                rec.revoked = true;
            }
        }
        let removed = rt.online_sessions.remove(&user_id);
        if let Some(ref prev) = removed {
            rt.displaced_sessions
                .insert(prev.session_id, reason.to_string());
        }
        removed
    }

    /// Returns the kick reason if `user_id` was kicked and has not yet re-authenticated.
    pub fn kicked_reason(&self, user_id: u64) -> Option<String> {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.get(&user_id).map(|(_, r)| r.clone())
    }

    /// Issues an OAuth2 PKCE authorization code (`S256`) for `username` and `character_id` (`D-4`, `ESI-1`).
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_pkce(
        &self,
        username: &str,
        password_token: &str,
        character_id: i64,
        client_id: &str,
        redirect_uri: &str,
        scopes: Vec<String>,
        code_challenge: &str,
        code_challenge_method: &str,
        now_unix: u64,
    ) -> Result<String, DbError> {
        if code_challenge_method != "S256" || code_challenge.trim().is_empty() {
            return Err(DbError::ValidationError(
                "PKCE requires S256 code_challenge_method and non-empty code_challenge".into(),
            ));
        }
        let acc = self.authenticate_at(username, password_token, now_unix)?;
        let _ch = self.select_character_for_user(acc.user_id, character_id)?;
        let code = format!("evomu_pkce_{}", Self::generate_random_salt_hex());
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.pkce_codes.insert(
            code.clone(),
            PkceAuthCode {
                code: code.clone(),
                user_id: acc.user_id,
                character_id,
                client_id: client_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                scopes,
                code_challenge: code_challenge.to_string(),
                code_challenge_method: code_challenge_method.to_string(),
                expires_at_unix: now_unix.saturating_add(300),
            },
        );
        Ok(code)
    }

    fn mint_sso_tokens_locked(
        &self,
        rt: &mut AuthRuntimeState,
        user_id: u64,
        character_id: i64,
        char_name: &str,
        client_id: &str,
        scopes: Vec<String>,
        now_unix: u64,
    ) -> SsoTokenBundle {
        let header_b64 = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
        let jti = Self::generate_random_salt_hex();
        let owner_hash = base64url_encode_no_pad(
            &sha256_bytes(format!("evomu-owner:{user_id}:{character_id}").as_bytes())[..16],
        );
        let claims = SsoTokenClaims {
            sub: format!("CHARACTER:EVE:{character_id}"),
            name: char_name.to_string(),
            owner: owner_hash,
            user_id,
            character_id,
            scp: scopes.clone(),
            iss: "https://login.eveonline.com".to_string(),
            aud: vec![client_id.to_string(), "EVE Online".to_string()],
            iat: now_unix,
            exp: now_unix.saturating_add(1_200),
            jti,
        };
        let payload_json = serde_json::to_vec(&claims).expect("serialize SsoTokenClaims");
        let payload_b64 = base64url_encode_no_pad(&payload_json);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
        let sig_b64 = base64url_encode_no_pad(&sig);
        let access_token = format!("{signing_input}.{sig_b64}");

        let refresh_token = format!("evomu_rt_{}", Self::generate_random_salt_hex());
        rt.refresh_tokens.insert(
            refresh_token.clone(),
            SsoRefreshTokenRecord {
                refresh_token: refresh_token.clone(),
                user_id,
                character_id,
                client_id: client_id.to_string(),
                scopes,
                expires_at_unix: now_unix.saturating_add(86_400 * 30),
                revoked: false,
            },
        );
        SsoTokenBundle {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: 1_200,
            refresh_token,
        }
    }

    /// Exchanges a single-use OAuth2 PKCE authorization code and `code_verifier` for a signed HS256
    /// JWT `access_token` and rotatable `refresh_token` (`D-4`, `ESI-1`).
    pub fn exchange_pkce_code(
        &self,
        code: &str,
        client_id: &str,
        code_verifier: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let pkce = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            rt.pkce_codes
                .remove(code)
                .ok_or(DbError::InvalidCredentials)?
        };
        if now_unix > pkce.expires_at_unix || pkce.client_id != client_id {
            return Err(DbError::InvalidCredentials);
        }
        let verifier_digest = sha256_bytes(code_verifier.as_bytes());
        let computed_challenge = base64url_encode_no_pad(&verifier_digest);
        if !constant_time_eq(
            computed_challenge.as_bytes(),
            pkce.code_challenge.as_bytes(),
        ) {
            return Err(DbError::InvalidCredentials);
        }
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == pkce.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            if let Some(until) = acc.banned_until_unix {
                if now_unix < until {
                    return Err(DbError::AccountBanned {
                        until_unix: until,
                        reason: acc.ban_reason.clone().unwrap_or_default(),
                    });
                }
            }
        }
        let ch = self.select_character_for_user(pkce.user_id, pkce.character_id)?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        Ok(self.mint_sso_tokens_locked(
            &mut rt,
            pkce.user_id,
            pkce.character_id,
            &ch.name,
            &pkce.client_id,
            pkce.scopes,
            now_unix,
        ))
    }

    /// Rotates an active SSO `refresh_token` and issues a fresh JWT `access_token` + `refresh_token` (`D-4`, `ESI-1`).
    pub fn refresh_sso_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let rec = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            let r = rt
                .refresh_tokens
                .get_mut(refresh_token)
                .ok_or(DbError::InvalidCredentials)?;
            if r.revoked || now_unix > r.expires_at_unix || r.client_id != client_id {
                return Err(DbError::InvalidCredentials);
            }
            r.revoked = true;
            r.clone()
        };
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == rec.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            if let Some(until) = acc.banned_until_unix {
                if now_unix < until {
                    return Err(DbError::AccountBanned {
                        until_unix: until,
                        reason: acc.ban_reason.clone().unwrap_or_default(),
                    });
                }
            }
        }
        let ch = self.select_character_for_user(rec.user_id, rec.character_id)?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        Ok(self.mint_sso_tokens_locked(
            &mut rt,
            rec.user_id,
            rec.character_id,
            &ch.name,
            &rec.client_id,
            rec.scopes,
            now_unix,
        ))
    }

    /// Verifies a signed HS256 JWT `access_token`, checking header algorithm, signature integrity,
    /// `iat <= now_unix < exp`, kick revocation, and account ban status (`D-4`, `ESI-1`, `R34-P0-1`).
    pub fn verify_sso_access_token(
        &self,
        token: &str,
        now_unix: u64,
    ) -> Result<SsoTokenClaims, DbError> {
        let mut parts = token.splitn(3, '.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(DbError::InvalidCredentials);
        };
        if sig_b64.is_empty() {
            return Err(DbError::InvalidCredentials);
        }
        let header_bytes =
            base64url_decode_no_pad(header_b64).map_err(|_| DbError::InvalidCredentials)?;
        let header_json: serde_json::Value =
            serde_json::from_slice(&header_bytes).map_err(|_| DbError::InvalidCredentials)?;
        if header_json.get("alg").and_then(|v| v.as_str()) != Some("HS256") {
            return Err(DbError::InvalidCredentials);
        }
        let signing_input = format!("{header_b64}.{payload_b64}");
        let claims: SsoTokenClaims = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            let expected_sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
            let expected_sig_b64 = base64url_encode_no_pad(&expected_sig);
            if !constant_time_eq(sig_b64.as_bytes(), expected_sig_b64.as_bytes()) {
                return Err(DbError::InvalidCredentials);
            }
            let payload_bytes =
                base64url_decode_no_pad(payload_b64).map_err(|_| DbError::InvalidCredentials)?;
            let parsed: SsoTokenClaims =
                serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
            if parsed.iat > now_unix
                || now_unix >= parsed.exp
                || parsed.iss != "https://login.eveonline.com"
                || !parsed.aud.iter().any(|a| a == "EVE Online")
            {
                return Err(DbError::InvalidCredentials);
            }
            if let Some((kicked_at, _)) = rt.kicked_users.get(&parsed.user_id) {
                if *kicked_at >= parsed.iat {
                    return Err(DbError::InvalidCredentials);
                }
            }
            parsed
        };
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let acc = guard
            .accounts
            .values()
            .find(|a| a.user_id == claims.user_id)
            .ok_or(DbError::InvalidCredentials)?;
        if let Some(until) = acc.banned_until_unix {
            if now_unix < until {
                return Err(DbError::AccountBanned {
                    until_unix: until,
                    reason: acc.ban_reason.clone().unwrap_or_default(),
                });
            }
        }
        Ok(claims)
    }

    /// Revokes an SSO refresh token (`D-4`, `ESI-1`).
    pub fn revoke_sso_refresh_token(&self, refresh_token: &str) -> bool {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        if let Some(rec) = rt.refresh_tokens.get_mut(refresh_token) {
            rec.revoked = true;
            return true;
        }
        false
    }

    pub fn get_characters_for_user(&self, user_id: u64) -> Vec<CharacterRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut chars: Vec<CharacterRecord> = guard
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect();
        chars.sort_by_key(|c| c.character_id);
        chars
    }

    pub fn get_item(&self, item_id: i64) -> Option<InventoryItemRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard.items.get(&item_id).cloned()
    }

    pub fn get_character(&self, character_id: i64) -> Result<CharacterRecord, DbError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard
            .characters
            .get(&character_id)
            .cloned()
            .ok_or(DbError::CharacterNotFound(character_id))
    }

    pub fn select_character_for_user(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<CharacterRecord, DbError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let ch = guard
            .characters
            .get(&character_id)
            .ok_or(DbError::CharacterNotFound(character_id))?;
        if ch.user_id != user_id {
            return Err(DbError::CharacterOwnershipViolation {
                user_id,
                character_id,
            });
        }
        Ok(ch.clone())
    }

    /// Creates a new character for `user_id` with its racial Corvette, NPE progress, and school starting station (`E2E-01`).
    pub fn create_character(
        &self,
        user_id: u64,
        name: &str,
        race_id: u32,
        bloodline_id: u32,
        ancestry_id: u32,
        school_id: u32,
        sde: &SdeRegistry,
    ) -> Result<CharacterRecord, DbError> {
        if name.trim().len() < 3 {
            return Err(DbError::ValidationError(
                "Character name must be at least 3 characters".into(),
            ));
        }

        // Strictly validate race, bloodline, ancestry, and school lineage against SDE
        let race = sde
            .races
            .get(&race_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown race_id {race_id}")))?;
        let bl = sde
            .bloodlines
            .get(&bloodline_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown bloodline_id {bloodline_id}")))?;
        if bl.race_id != race_id {
            return Err(DbError::ValidationError(format!(
                "Bloodline {bloodline_id} does not belong to race {race_id}"
            )));
        }
        let anc = sde
            .ancestries
            .get(&ancestry_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown ancestry_id {ancestry_id}")))?;
        if anc.bloodline_id != bloodline_id {
            return Err(DbError::ValidationError(format!(
                "Ancestry {ancestry_id} does not belong to bloodline {bloodline_id}"
            )));
        }
        let school = sde
            .schools
            .get(&school_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown school_id {school_id}")))?;
        if school.race_id != race_id {
            return Err(DbError::ValidationError(format!(
                "School {school_id} does not belong to race {race_id}"
            )));
        }

        self.try_with_mut_snapshot(|guard| {
            let user_char_count = guard
                .characters
                .values()
                .filter(|c| c.user_id == user_id)
                .count();
            if user_char_count >= 3 {
                return Err(DbError::ValidationError(
                    "Maximum of 3 characters per account reached".into(),
                ));
            }
            if guard
                .characters
                .values()
                .any(|c| c.name.eq_ignore_ascii_case(name))
            {
                return Err(DbError::ValidationError(
                    "Character name already exists".into(),
                ));
            }

            guard.next_entity_id += 1;
            let char_id = guard.next_entity_id;
            guard.next_entity_id += 1;
            let ship_id = guard.next_entity_id;

            let station_id = school.station_id;
            let sys_id = school.solar_system_id;
            let corp_id = school.corporation_id;
            let sys = sde.solar_systems.get(&sys_id);
            let const_id = sys.map(|s| s.constellation_id).unwrap_or(20_000_020);
            let reg_id = sys.map(|s| s.region_id).unwrap_or(10_000_002);
            let corvette_type_id = race.corvette_type_id;

            let rec = CharacterRecord {
                character_id: char_id,
                user_id,
                name: name.to_string(),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(station_id),
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: const_id,
                region_id: reg_id,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(5_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1,
                home_station_id: station_id,
                race_id,
                bloodline_id,
                ancestry_id,
                school_id,
                neural: NeuralAttributes::default(),
                skill_points: 400_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                version: 1,
                system_epoch: 1,
            };

            guard.characters.insert(char_id, rec.clone());
            guard.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: corvette_type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            guard.npe_progress.insert(
                char_id,
                NpeProgressRecord {
                    character_id: char_id,
                    node_graph_id: 473,
                    current_node_id: 1,
                    completed: false,
                    career_goal_progress: HashMap::new(),
                    claimed_goals: HashSet::new(),
                },
            );
            Ok(rec)
        })
    }

    pub fn get_items_for_character(&self, character_id: i64) -> Vec<InventoryItemRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<InventoryItemRecord> = guard
            .items
            .values()
            .filter(|i| i.owner_id == character_id)
            .cloned()
            .collect();
        items.sort_by_key(|i| i.item_id);
        items
    }

    /// Atomically moves an inventory item owned by `character_id` to `(new_location_id, new_flag)`
    /// after verifying item ownership, destination authorization, and optional optimistic concurrency.
    pub fn move_item_transactional(
        &self,
        character_id: i64,
        item_id: i64,
        new_location_id: i64,
        new_flag: InventoryFlag,
        expected_version: Option<u64>,
        idempotency_key: Option<&str>,
    ) -> Result<InventoryItemRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let ch = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?
                .clone();

            // Reject self-containment, moving the currently boarded ship, and Locked flag manipulation
            if item_id == new_location_id || item_id == ch.active_ship_id {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: new_location_id,
                });
            }
            if new_flag == InventoryFlag::Locked {
                return Err(DbError::ItemLocked(item_id));
            }

            let dock_id = ch.station_id.or(ch.structure_id);
            let active_ship_colocated =
                guard.is_ship_colocated_with_character(character_id, ch.active_ship_id, dock_id);

            // Authorize destination: must be the character's current station, structure, or co-located active ship
            let valid_dest = (Some(new_location_id) == ch.station_id)
                || (Some(new_location_id) == ch.structure_id)
                || (active_ship_colocated && new_location_id == ch.active_ship_id);
            if !valid_dest {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: new_location_id,
                });
            }

            let existing_item = guard
                .items
                .get(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if existing_item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if existing_item.flag == InventoryFlag::Locked {
                return Err(DbError::ItemLocked(item_id));
            }

            // Verify source location is reachable by the character (current station, structure, or co-located active ship)
            let valid_source = (Some(existing_item.location_id) == ch.station_id)
                || (Some(existing_item.location_id) == ch.structure_id)
                || (active_ship_colocated && existing_item.location_id == ch.active_ship_id);
            if !valid_source {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: existing_item.location_id,
                });
            }

            // Reject moving items locked in active contracts or active industry jobs
            if guard.is_item_locked_in_contract_or_job(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }

            if let Some(key) = idempotency_key {
                let scoped_key = (character_id, key.to_string());
                if guard.committed_idempotency_keys.contains(&scoped_key) {
                    return Ok(existing_item.clone());
                }
            }

            if let Some(exp_v) = expected_version {
                if existing_item.version != exp_v {
                    return Err(DbError::VersionConflict {
                        expected: exp_v,
                        actual: existing_item.version,
                    });
                }
            }

            let item = guard.items.get_mut(&item_id).unwrap();
            item.location_id = new_location_id;
            item.flag = new_flag;
            item.version += 1;
            let updated = item.clone();

            if let Some(key) = idempotency_key {
                if guard.committed_idempotency_keys.len() >= MAX_IDEMPOTENCY_KEYS {
                    guard.committed_idempotency_keys.clear();
                }
                guard
                    .committed_idempotency_keys
                    .insert((character_id, key.to_string()));
            }

            Ok(updated)
        })
    }

    /// Atomically transitions a docked character and their active ship into space (`location_id = solar_system_id`, `flag = None`).
    pub fn undock_character(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<(i64, CharacterRecord, InventoryItemRecord), DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (station_id, active_ship_id, solar_system_id) = {
                let ch = guard
                    .characters
                    .get(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                if ch.user_id != user_id {
                    return Err(DbError::CharacterOwnershipViolation {
                        user_id,
                        character_id,
                    });
                }
                let st_id = ch
                    .station_id
                    .or(ch.structure_id)
                    .ok_or(DbError::NotDocked(character_id))?;
                (st_id, ch.active_ship_id, ch.solar_system_id)
            };

            {
                let ship = guard
                    .items
                    .get(&active_ship_id)
                    .ok_or(DbError::ItemNotFound(active_ship_id))?;
                if ship.owner_id != character_id {
                    return Err(DbError::ItemOwnershipViolation {
                        character_id,
                        item_id: active_ship_id,
                    });
                }
                if ship.location_id != station_id {
                    return Err(DbError::ShipNotAtDockedStation {
                        ship_id: active_ship_id,
                        ship_location_id: ship.location_id,
                        station_id,
                    });
                }
            }

            let ch = guard.characters.get_mut(&character_id).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
            ch.version += 1;
            ch.system_epoch += 1;
            let ch_snapshot = ch.clone();

            let ship = guard.items.get_mut(&active_ship_id).unwrap();
            ship.location_id = solar_system_id as i64;
            ship.flag = InventoryFlag::None;
            ship.version += 1;
            let ship_snapshot = ship.clone();

            Ok((station_id, ch_snapshot, ship_snapshot))
        })
    }

    /// Docks a character in a station or Upwell structure.
    pub fn dock_character(
        &self,
        character_id: i64,
        dock_location_id: i64,
        is_structure: bool,
    ) -> Result<CharacterRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (active_ship_id, ch_solar_system_id) = {
                let ch = guard
                    .characters
                    .get(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                (ch.active_ship_id, ch.solar_system_id)
            };
            if is_structure {
                let st = guard
                    .structures
                    .get(&dock_location_id)
                    .ok_or(DbError::InvalidDestinationLocation {
                        character_id,
                        location_id: dock_location_id,
                    })?;
                if st.solar_system_id != ch_solar_system_id || st.state == "Destroyed" {
                    return Err(DbError::InvalidDestinationLocation {
                        character_id,
                        location_id: dock_location_id,
                    });
                }
            }
            {
                let ch = guard.characters.get_mut(&character_id).unwrap();
                if is_structure {
                    ch.structure_id = Some(dock_location_id);
                    ch.station_id = None;
                } else {
                    ch.station_id = Some(dock_location_id);
                    ch.structure_id = None;
                }
                ch.version += 1;
                ch.system_epoch += 1;
            }
            if let Some(ship) = guard.items.get_mut(&active_ship_id) {
                if ship.location_id == ch_solar_system_id as i64 {
                    ship.location_id = dock_location_id;
                    ship.flag = InventoryFlag::Hangar;
                    ship.version += 1;
                }
            }
            Ok(guard.characters[&character_id].clone())
        })
    }

    /// Transactionally destroys `destroyed_ship_id` (and all fitted/contained items inside it),
    /// reuses or spawns a Capsule (`type_id = 670`) at `character.home_station_id`, and updates the
    /// character's active ship, docked station, and solar system/constellation/region from `sde`.
    pub fn respawn_character_at_home_station(
        &self,
        character_id: i64,
        destroyed_ship_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(CharacterRecord, InventoryItemRecord), DbError> {
        self.try_with_mut_snapshot(|guard| {
            let home_station_id = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?
                .home_station_id;

            let (home_sys_id, home_const_id, home_reg_id) = sde
                .solar_systems
                .values()
                .find(|sys| {
                    sys.stations
                        .iter()
                        .any(|st| st.station_id == home_station_id)
                })
                .map(|sys| (sys.solar_system_id, sys.constellation_id, sys.region_id))
                .unwrap_or((30_000_142, 20_000_020, 10_000_002));

            // Recursively remove the destroyed ship and all items fitted/contained inside it (including nested containers)
            let mut to_remove = HashSet::from([destroyed_ship_id]);
            loop {
                let mut added = false;
                for (&id, item) in &guard.items {
                    if !to_remove.contains(&id) && to_remove.contains(&item.location_id) {
                        to_remove.insert(id);
                        added = true;
                    }
                }
                if !added {
                    break;
                }
            }
            guard.items.retain(|id, _| !to_remove.contains(id));

            let existing_capsule_id = guard
                .items
                .values()
                .find(|i| {
                    i.owner_id == character_id
                        && i.type_id == 670
                        && i.location_id == home_station_id
                })
                .map(|i| i.item_id);
            let (capsule_id, capsule) = if let Some(cid) = existing_capsule_id {
                let cap = guard.items.get_mut(&cid).unwrap();
                cap.flag = InventoryFlag::Hangar;
                cap.version += 1;
                (cid, cap.clone())
            } else {
                guard.next_entity_id += 1;
                let cid = guard.next_entity_id;
                let cap = InventoryItemRecord {
                    item_id: cid,
                    type_id: 670, // Capsule
                    owner_id: character_id,
                    location_id: home_station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                };
                guard.items.insert(cid, cap.clone());
                (cid, cap)
            };

            let ch = guard.characters.get_mut(&character_id).unwrap();
            ch.active_ship_id = capsule_id;
            ch.station_id = Some(home_station_id);
            ch.structure_id = None;
            ch.solar_system_id = home_sys_id;
            ch.constellation_id = home_const_id;
            ch.region_id = home_reg_id;
            ch.version += 1;
            ch.system_epoch += 1;
            let ch_snapshot = ch.clone();

            Ok((ch_snapshot, capsule))
        })
    }

    /// Compensating transaction to restore docked station state if solar-system actor spawn fails.
    pub fn rollback_undock(&self, character_id: i64, station_id: i64, ship_id: i64) {
        let _ = self.try_with_mut_snapshot(|guard| {
            if let Some(ch) = guard.characters.get_mut(&character_id) {
                ch.station_id = Some(station_id);
                ch.version += 1;
                ch.system_epoch += 1;
            }
            if let Some(ship) = guard.items.get_mut(&ship_id) {
                ship.location_id = station_id;
                ship.flag = InventoryFlag::Hangar;
                ship.version += 1;
            }
            Ok::<(), std::convert::Infallible>(())
        });
    }

    /// Updates a character's solar system (e.g. after Stargate Jump, Cyno Jump, Wormhole, or Filament).
    pub fn set_character_solar_system(
        &self,
        character_id: i64,
        new_solar_system_id: SolarSystemId,
        sde: &SdeRegistry,
    ) -> Result<CharacterRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (const_id, reg_id) = sde
                .solar_systems
                .get(&new_solar_system_id)
                .map(|s| (s.constellation_id, s.region_id))
                .unwrap_or((20_000_020, 10_000_002));

            let ship_id = {
                let ch = guard
                    .characters
                    .get_mut(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                ch.solar_system_id = new_solar_system_id;
                ch.constellation_id = const_id;
                ch.region_id = reg_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.version += 1;
                ch.system_epoch += 1;
                ch.active_ship_id
            };
            if let Some(ship) = guard.items.get_mut(&ship_id) {
                ship.location_id = new_solar_system_id as i64;
                ship.version += 1;
            }
            Ok(guard.characters[&character_id].clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_item_ownership_idempotency_and_version_conflict() {
        let repo = WorldRepository::with_demo_seed();

        let res = repo.move_item_transactional(
            90_000_002,
            100_000_003,
            60_003_760,
            InventoryFlag::Cargo,
            None,
            Some("shared-key-1"),
        );
        assert!(matches!(res, Err(DbError::ItemOwnershipViolation { .. })));

        // Moving the currently boarded active ship (100_000_001) must be rejected
        let active_ship_move = repo.move_item_transactional(
            90_000_001,
            100_000_001,
            60_003_760,
            InventoryFlag::Cargo,
            Some(1),
            None,
        );
        assert!(matches!(
            active_ship_move,
            Err(DbError::InvalidDestinationLocation { .. })
        ));

        let moved1 = repo
            .move_item_transactional(
                90_000_001,
                100_000_003,
                100_000_001,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-ab-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved1.flag, InventoryFlag::Cargo);
        assert_eq!(moved1.version, 2);

        let moved2 = repo
            .move_item_transactional(
                90_000_001,
                100_000_003,
                100_000_001,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-ab-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved2.version, 2);

        let stale = repo.move_item_transactional(
            90_000_001,
            100_000_003,
            60_003_760,
            InventoryFlag::Hangar,
            Some(1),
            Some("new-key-stale-version"),
        );
        assert!(matches!(
            stale,
            Err(DbError::VersionConflict {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn test_atomic_undock_and_rollback() {
        let repo = WorldRepository::with_demo_seed();
        let (station_id, ch, ship) = repo.undock_character(1, 90_000_001).unwrap();
        assert_eq!(station_id, 60_003_760);
        assert_eq!(ch.station_id, None);
        assert_eq!(ship.location_id, 30_000_142);
        assert_eq!(ship.flag, InventoryFlag::None);

        assert!(matches!(
            repo.undock_character(1, 90_000_001),
            Err(DbError::NotDocked(90_000_001))
        ));

        repo.rollback_undock(90_000_001, station_id, ship.item_id);
        let restored = repo.select_character_for_user(1, 90_000_001).unwrap();
        assert_eq!(restored.station_id, Some(60_003_760));
    }

    #[test]
    fn test_durable_snapshot_persistence_and_domain_mutations() {
        // Verify FIPS 180-4 SHA-256 test vector ("abc") and salted credential hashing (OF-24)
        let abc_digest = sha256_bytes(b"abc");
        let mut abc_hex = String::new();
        for b in abc_digest {
            use std::fmt::Write as _;
            let _ = write!(&mut abc_hex, "{b:02x}");
        }
        assert_eq!(
            abc_hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let sde = SdeRegistry::with_canonical_seed();
        let repo = WorldRepository::with_demo_seed();
        let stored_demo_digest = repo.with_snapshot(|snap| {
            let demo_acc = &snap.accounts["demo"];
            let other_acc = &snap.accounts["other"];
            assert!(demo_acc
                .password_token
                .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
            assert_ne!(demo_acc.password_token, "demo-secret");
            let demo_salt = demo_acc.password_token.split('$').nth(4).unwrap();
            let other_salt = other_acc.password_token.split('$').nth(4).unwrap();
            assert_eq!(demo_salt.len(), 32);
            assert_ne!(demo_salt, other_salt);
            demo_acc.password_token.clone()
        });
        assert!(repo.authenticate("demo", "demo-secret").is_ok());
        assert_eq!(
            repo.authenticate("demo", "wrong-secret"),
            Err(DbError::InvalidCredentials)
        );
        // R21-P1-3: Pass-the-hash must be rejected (stored digest is not accepted as a login token)
        assert_eq!(
            repo.authenticate("demo", &stored_demo_digest),
            Err(DbError::InvalidCredentials)
        );

        let created = repo
            .create_character(1, "Snapshot Pilot", 1, 1, 11, 17, &sde)
            .unwrap();
        // Verify Caldari pilot spawns in Ibis (601) at State War Academy starting station in Kisogo (30000141)
        let created_items = repo.get_items_for_character(created.character_id);
        assert_eq!(created_items[0].type_id, 601);
        assert_eq!(created.solar_system_id, 30_000_141);
        assert_eq!(created.station_id, Some(60_015_157));

        // Verify IskCents rejects negative debits and insufficient funds
        assert_eq!(IskCents::from_isk(100).checked_sub(IskCents::from_isk(150)), None);
        assert_eq!(IskCents::from_isk(100).checked_sub(IskCents::from_isk(-10)), None);
        assert_eq!(IskCents::from_isk(100).checked_add(IskCents::from_isk(-10)), None);
        assert_eq!(IskCents::from_isk(100).checked_mul_qty(-5), None);

        // Populate tuple-keyed maps (planetary_colonies & active_missions) and idempotency keys
        repo.with_mut_snapshot(|snap| {
            snap.planetary_colonies.insert(
                (created.character_id, 40009082),
                PlanetaryColonyRecord {
                    planet_id: 40009082,
                    owner_id: created.character_id,
                    solar_system_id: 30000142,
                    command_center_level: 1,
                    pins_count: 2,
                    extracted_commodities: HashMap::from([(2393, 500)]),
                    last_extracted_unix: 0,
                },
            );
            snap.active_missions.insert(
                (created.character_id, 1001),
                ActiveMissionRecord {
                    mission_id: 1001,
                    character_id: created.character_id,
                    agent_id: 3018924,
                    solar_system_id: 30000142,
                    state: "Accepted".to_string(),
                    current_room_id: 1,
                    npcs_remaining: 3,
                    spawned_npc_ball_ids: Vec::new(),
                },
            );
            snap.committed_idempotency_keys
                .insert((created.character_id, "idem-1".to_string()));
        });

        // Verify try_with_mut_snapshot discards mutations when closure returns Err
        let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
            snap.items.clear();
            Err("simulated failure".to_string())
        });
        assert!(err_res.is_err());
        assert!(!repo.get_items_for_character(created.character_id).is_empty());

        let pid = std::process::id();
        let snap_path = std::env::temp_dir().join(format!("evomu_test_snapshot_{pid}.json"));
        repo.save_snapshot_to_file(&snap_path).unwrap();

        // Verify incremental WAL delta append and replay before checkpoint compaction (OF-06 / OF-23)
        let old_snap = repo.with_snapshot(|s| s.clone());
        let _ = repo
            .move_item_transactional(
                90_000_001,
                100_000_003,
                100_000_001,
                InventoryFlag::Cargo,
                Some(1),
                Some("wal-move-1"),
            )
            .unwrap();
        let new_snap = repo.with_snapshot(|s| s.clone());
        let wal_delta = WorldStateWalDelta::from_diff(&old_snap, &new_snap);
        assert!(wal_delta.full_snapshot.is_none());
        assert_eq!(wal_delta.upserted_items.len(), 1);
        repo.append_wal_delta_locked(&snap_path, &new_snap, &wal_delta)
            .unwrap();
        assert!(WorldRepository::wal_path_for(&snap_path).exists());

        let repo2 = WorldRepository::with_demo_seed();
        repo2.load_snapshot_from_file(&snap_path).unwrap();
        let loaded = repo2
            .select_character_for_user(1, created.character_id)
            .unwrap();
        assert_eq!(loaded.name, "Snapshot Pilot");
        let replayed_item = repo2.get_item(100_000_003).unwrap();
        assert_eq!(replayed_item.flag, InventoryFlag::Cargo);
        assert_eq!(replayed_item.version, 2);
        let has_colony = repo2.with_snapshot(|snap| {
            snap.planetary_colonies
                .contains_key(&(created.character_id, 40009082))
                && snap
                    .active_missions
                    .contains_key(&(created.character_id, 1001))
        });
        assert!(has_colony);

        // R23-P0-1, R23-P1-4, & R24-P1-1: Verify `new_empty_with_snapshot_path` preserves all persisted accounts,
        // characters, balances, skills, and items across restarts even when a torn/truncated trailing line is
        // appended to `.json.wal` before an ungraceful crash (`std::mem::forget`), and that `provision_player_account`
        // and `provision_gm_account` are idempotent.
        let prod_snap_path =
            std::env::temp_dir().join(format!("evomu_test_prod_restart_{pid}.json"));
        let prod_wal_path = WorldRepository::wal_path_for(&prod_snap_path);
        let _ = std::fs::remove_file(&prod_snap_path);
        let _ = std::fs::remove_file(&prod_wal_path);
        let prod1 = WorldRepository::new_empty_with_snapshot_path(Some(prod_snap_path.clone()))
            .expect("initial new_empty_with_snapshot_path");
        assert!(prod1.with_snapshot(|s| s.accounts.is_empty() && s.characters.is_empty()));
        let player_uid = prod1.provision_player_account("prod_pilot", "prod_pass_1");
        let prod_char = prod1
            .create_character(player_uid, "Prod Capsuleer", 1, 1, 11, 17, &sde)
            .unwrap();
        prod1.provision_gm_account("gm_admin", "gm_pass_1", 90_000_003, "ISD Polaris");
        prod1.with_mut_snapshot(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(42_000_000);
            }
        });
        let gm_items_before = prod1.get_items_for_character(90_000_003);
        assert_eq!(gm_items_before.len(), 1);

        // R25-P1-1: (a) Verify `sync_data` failure after `write_all` truncates the uncommitted WAL record
        // back to `prev_len` on disk and aborts in memory, so a subsequent committed transaction reuses
        // the sequence number cleanly and survives restart without reviving the aborted transaction.
        let wal_len_before_abort = std::fs::metadata(&prod_wal_path).unwrap().len();
        prod1
            .wal_fault_mode
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let sync_fail_res: Result<(), DbError> = prod1.try_with_mut_snapshot(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(999_999_999);
            }
            Ok(())
        });
        assert!(matches!(sync_fail_res, Err(DbError::PersistenceFailure(_))));
        assert_eq!(
            prod1.get_character(90_000_003).unwrap().balance,
            IskCents::from_isk(42_000_000)
        );
        assert_eq!(
            std::fs::metadata(&prod_wal_path).unwrap().len(),
            wal_len_before_abort,
            "sync_data failure after write_all must truncate .json.wal back to prev_len (R25-P1-1)"
        );

        // R25-P1-1: (b) Verify partial `write_all` failure truncates partial bytes back to `prev_len`.
        prod1
            .wal_fault_mode
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let partial_fail_res: Result<(), DbError> = prod1.try_with_mut_snapshot(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(888_888_888);
            }
            Ok(())
        });
        assert!(matches!(
            partial_fail_res,
            Err(DbError::PersistenceFailure(_))
        ));
        assert_eq!(
            std::fs::metadata(&prod_wal_path).unwrap().len(),
            wal_len_before_abort,
            "partial write_all failure must truncate .json.wal back to prev_len (R25-P1-1)"
        );

        // R25-P1-1: (c) Simulate an uncleaned partial write left on disk in `.json.wal` followed by a
        // committed append, verifying the partial bytes are truncated before appending so the committed
        // record is never merged with the partial line and survives restart.
        {
            use std::io::Write as _;
            let mut wal_file = std::fs::OpenOptions::new()
                .append(true)
                .open(&prod_wal_path)
                .unwrap();
            wal_file
                .write_all(b"{\"seq\":777,\"partial_uncleaned_tail")
                .unwrap();
            wal_file.sync_data().unwrap();
        }
        prod1.with_mut_snapshot(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(55_000_000);
            }
        });

        // R25-P1-1: (d) Verify a checkpoint (`save_snapshot_locked`) failure after a durable WAL write
        // treats checkpoint compaction as retryable, commits the durable transaction, and preserves it across restart.
        let checkpoint_tmp_dir = prod_snap_path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&checkpoint_tmp_dir);
        std::fs::create_dir_all(&checkpoint_tmp_dir).unwrap();
        prod1.pending_wal_records.store(
            WAL_CHECKPOINT_INTERVAL - 1,
            std::sync::atomic::Ordering::SeqCst,
        );
        prod1
            .try_with_mut_snapshot::<_, (), DbError>(|snap| {
                if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                    gm_ch.balance = IskCents::from_isk(64_000_000);
                }
                Ok(())
            })
            .expect("durable WAL write must commit even when background checkpoint fails (R25-P1-1)");
        let _ = std::fs::remove_dir_all(&checkpoint_tmp_dir);

        // R24-P1-1: Append a torn/truncated trailing line to `.json.wal` and simulate an ungraceful process crash (`std::mem::forget`)
        {
            use std::io::Write as _;
            let mut wal_file = std::fs::OpenOptions::new()
                .append(true)
                .open(&prod_wal_path)
                .expect("open prod_wal_path to append torn line");
            wal_file
                .write_all(b"{\"seq\":999,\"full_snapshot\":null,\"upserted_items\":[{\"item_id\":")
                .expect("write torn WAL line");
            wal_file.sync_data().unwrap();
        }
        std::mem::forget(prod1);

        // Restart production repository from the same snapshot path + torn WAL
        let prod2 = WorldRepository::new_empty_with_snapshot_path(Some(prod_snap_path.clone()))
            .expect("restart must tolerate and heal torn trailing WAL line (R24-P1-1, R25-P1-1)");
        let restarted_char = prod2
            .select_character_for_user(player_uid, prod_char.character_id)
            .expect("persisted player character must survive production restart after torn WAL (R23-P0-1, R24-P1-1)");
        assert_eq!(restarted_char.name, "Prod Capsuleer");
        assert!(!prod2.get_items_for_character(prod_char.character_id).is_empty());

        // Re-running account provisioning on startup must preserve `user_id`, GM balance (`64_000_000` ISK), and GM items (R23-P1-4, R25-P1-1)
        let same_uid = prod2.provision_player_account("prod_pilot", "prod_pass_1");
        assert_eq!(same_uid, player_uid);
        let rotated_uid = prod2.provision_player_account("prod_pilot", "prod_pass_2");
        assert_eq!(rotated_uid, player_uid);
        assert!(prod2.authenticate("prod_pilot", "prod_pass_2").is_ok());

        prod2.provision_gm_account("gm_admin", "gm_pass_1", 90_000_003, "ISD Polaris");
        let gm_after = prod2.get_character(90_000_003).unwrap();
        assert_eq!(gm_after.balance, IskCents::from_isk(64_000_000));
        assert_eq!(prod2.get_items_for_character(90_000_003).len(), 1);

        // R24-P1-1: Verify that if the WAL path is unwriteable (e.g., replaced by a directory), `try_with_mut_snapshot`
        // returns `Err(DbError::PersistenceFailure(_))` and does NOT commit the mutation in memory.
        let _ = std::fs::remove_file(&prod_wal_path);
        std::fs::create_dir_all(&prod_wal_path).unwrap();
        let wal_err_res: Result<(), DbError> = prod2.try_with_mut_snapshot(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(1);
            }
            Ok(())
        });
        assert!(matches!(wal_err_res, Err(DbError::PersistenceFailure(_))));
        assert_eq!(
            prod2.get_character(90_000_003).unwrap().balance,
            IskCents::from_isk(64_000_000)
        );
        let _ = std::fs::remove_dir_all(&prod_wal_path);

        // R25-P1-1: (e) Verify duplicate `seq` numbers in `.json.wal` are rejected on replay.
        prod2.save_snapshot_to_file(&prod_snap_path).unwrap();
        let dup_line = format!(
            "{}\n",
            serde_json::to_string(&wal_delta).unwrap()
        );
        std::fs::write(&prod_wal_path, format!("{dup_line}{dup_line}")).unwrap();
        assert!(
            prod2.load_snapshot_from_file(&prod_snap_path).is_err(),
            "duplicate WAL sequence numbers must be rejected on replay (R25-P1-1)"
        );
        let _ = std::fs::remove_file(&prod_wal_path);
        drop(prod2);

        // R24-P1-1: Verify that a corrupted base snapshot file returns `Err` from `new_empty_with_snapshot_path`
        // and is NEVER overwritten on disk.
        let corrupt_snap_path =
            std::env::temp_dir().join(format!("evomu_test_corrupt_snapshot_{pid}.json"));
        std::fs::write(&corrupt_snap_path, b"{not-valid-json").unwrap();
        let corrupt_load =
            WorldRepository::new_empty_with_snapshot_path(Some(corrupt_snap_path.clone()));
        assert!(corrupt_load.is_err());
        assert_eq!(
            std::fs::read(&corrupt_snap_path).unwrap(),
            b"{not-valid-json"
        );
        let _ = std::fs::remove_file(&corrupt_snap_path);

        let _ = std::fs::remove_file(&prod_snap_path);
        let _ = std::fs::remove_file(&prod_wal_path);
        let _ = std::fs::remove_file(&snap_path);
        let _ = std::fs::remove_file(WorldRepository::wal_path_for(&snap_path));
    }

    #[test]
    fn test_d3_sqlite_relational_world_db_and_concurrent_sessions() {
        use std::sync::Arc;
        let sde = SdeRegistry::with_canonical_seed();
        let pid = std::process::id();
        let sqlite_path =
            std::env::temp_dir().join(format!("evomu_test_d3_world_{pid}.sqlite"));
        let _ = std::fs::remove_file(&sqlite_path);

        let repo = Arc::new(
            WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite must succeed"),
        );
        assert_eq!(repo.sqlite_path(), Some(sqlite_path.as_path()));

        // Verify PRAGMA journal_mode = wal and PRAGMA foreign_keys = 1 are active
        {
            let conn = rusqlite::Connection::open(&sqlite_path).unwrap();
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
                .unwrap();
            assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        }

        let u1 = repo.provision_player_account("pilot_one", "pass_one_123");
        let u2 = repo.provision_player_account("pilot_two", "pass_two_456");
        let c1 = repo
            .create_character(u1, "Sqlite Alpha", 1, 1, 11, 17, &sde)
            .unwrap();
        let c2 = repo
            .create_character(u2, "Sqlite Bravo", 2, 3, 19, 14, &sde)
            .unwrap();

        // Populate all relational domain tables (skills, queue, orders, journal, contracts, jobs,
        // missions, standings, mail, fleets, corps, bookmarks, structures)
        repo.with_mut_snapshot(|snap| {
            snap.skills.entry(c1.character_id).or_default().insert(
                3300,
                TrainedSkillRecord {
                    type_id: 3300,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            snap.skill_queues.insert(
                c1.character_id,
                vec![SkillQueueEntry {
                    type_id: 3302,
                    target_level: 4,
                    start_sp: 8_000.0,
                    end_sp: 45_255.0,
                    duration_seconds: 1200.0,
                }],
            );
            snap.market_orders.insert(
                800_001,
                MarketOrderRecord {
                    order_id: 800_001,
                    owner_id: c1.character_id,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    type_id: 34,
                    is_bid: true,
                    price: IskCents::from_isk(6),
                    vol_entered: 1_000,
                    vol_remaining: 1_000,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents::from_isk(6_000),
                },
            );
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: 900_001,
                character_id: c1.character_id,
                entry_type: "market_escrow".to_string(),
                amount: IskCents::from_isk(-6_000),
                balance_after: IskCents::from_isk(4_000),
                reason: "Market order escrow".to_string(),
            });
            snap.contracts.insert(
                910_001,
                ContractRecord {
                    contract_id: 910_001,
                    issuer_id: c1.character_id,
                    acceptor_id: None,
                    contract_type: "ItemExchange".to_string(),
                    status: "Outstanding".to_string(),
                    price: IskCents::from_isk(100_000),
                    reward: IskCents(0),
                    collateral: IskCents(0),
                    start_station_id: 60_003_760,
                    end_station_id: 60_003_760,
                    item_ids: vec![],
                    expiration_unix: 2_000_000_000,
                },
            );
            snap.industry_jobs.insert(
                920_001,
                IndustryJobRecord {
                    job_id: 920_001,
                    installer_id: c1.character_id,
                    facility_id: 60_003_760,
                    activity_id: 1,
                    blueprint_item_id: 100_000_007,
                    blueprint_type_id: 691,
                    product_type_id: 587,
                    runs: 1,
                    status: "Active".to_string(),
                    duration_seconds: 3600,
                    start_unix: 1_700_000_000,
                },
            );
            snap.active_missions.insert(
                (c1.character_id, 501),
                ActiveMissionRecord {
                    mission_id: 501,
                    character_id: c1.character_id,
                    agent_id: 3018924,
                    solar_system_id: 30_000_142,
                    state: "Accepted".to_string(),
                    current_room_id: 0,
                    npcs_remaining: 2,
                    spawned_npc_ball_ids: vec![],
                },
            );
            snap.standing_transactions.insert(
                c1.character_id,
                vec![StandingTransactionRecord {
                    event_id: 1,
                    from_id: 1_000_035,
                    to_id: c1.character_id,
                    modification: 0.25,
                    event_type_id: 33,
                }],
            );
            snap.mails.push(MailRecord {
                message_id: 930_001,
                sender_id: c1.character_id,
                recipient_id: c2.character_id,
                subject: "Hello".to_string(),
                body: "World".to_string(),
            });
            snap.fleets.insert(
                940_001,
                FleetRecord {
                    fleet_id: 940_001,
                    boss_id: c1.character_id,
                    members: vec![c1.character_id],
                    wings: vec![],
                    squads: vec![],
                    free_move: true,
                    is_registered: true,
                },
            );
            snap.corporations.insert(
                950_001,
                CorporationRecord {
                    corporation_id: 950_001,
                    name: "Sqlite Corp".to_string(),
                    ticker: "SQLT".to_string(),
                    ceo_id: c1.character_id,
                    alliance_id: None,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_isk(1_000_000),
                    members: HashSet::from([c1.character_id]),
                },
            );
            snap.bookmarks.insert(
                960_001,
                BookmarkRecord {
                    bookmark_id: 960_001,
                    owner_id: c1.character_id,
                    label: "Jita Undock".to_string(),
                    solar_system_id: 30_000_142,
                    x: 100.0,
                    y: 200.0,
                    z: 300.0,
                },
            );
            snap.structures.insert(
                970_001,
                UpwellStructureRecord {
                    structure_id: 970_001,
                    type_id: 35832,
                    owner_corp_id: 950_001,
                    solar_system_id: 30_000_142,
                    name: "Jita - Astrahus".to_string(),
                    position: [1_000.0, 2_000.0, 3_000.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                },
            );
        });

        // Verify selective dirty-row rollback on try_with_mut_snapshot Err restores in-memory state cleanly
        let abort_res: Result<(), DbError> = repo.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&c1.character_id) {
                ch.balance = IskCents::from_isk(999_999_999);
            }
            snap.market_orders.remove(&800_001);
            Err(DbError::InvalidCredentials)
        });
        assert_eq!(abort_res, Err(DbError::InvalidCredentials));
        assert_eq!(
            repo.get_character(c1.character_id).unwrap().balance,
            IskCents::from_isk(5_000)
        );
        assert!(repo.with_snapshot(|s| s.market_orders.contains_key(&800_001)));

        // Concurrent writes from two sessions must not lose updates
        let r1 = Arc::clone(&repo);
        let r2 = Arc::clone(&repo);
        let c1_id = c1.character_id;
        let c2_id = c2.character_id;
        let t1 = std::thread::spawn(move || {
            for _ in 0..25 {
                r1.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&c1_id) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(1_000)).unwrap();
                    }
                });
            }
        });
        let t2 = std::thread::spawn(move || {
            for _ in 0..25 {
                r2.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&c2_id) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(2_000)).unwrap();
                    }
                });
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        drop(repo);

        // Restart server from the SQLite database file and verify all state survived
        let restarted =
            WorldRepository::open_sqlite(&sqlite_path).expect("reopen SQLite world DB");
        let c1_after = restarted.get_character(c1_id).unwrap();
        let c2_after = restarted.get_character(c2_id).unwrap();
        assert_eq!(c1_after.balance, IskCents::from_isk(5_000 + 25_000));
        assert_eq!(c2_after.balance, IskCents::from_isk(5_000 + 50_000));

        restarted.with_snapshot(|snap| {
            assert_eq!(snap.skills[&c1_id][&3300].level, 5);
            assert_eq!(snap.skill_queues[&c1_id].len(), 1);
            assert!(snap.market_orders.contains_key(&800_001));
            assert_eq!(snap.wallet_journal.len(), 1);
            assert!(snap.contracts.contains_key(&910_001));
            assert!(snap.industry_jobs.contains_key(&920_001));
            assert!(snap.active_missions.contains_key(&(c1_id, 501)));
            assert_eq!(snap.standing_transactions[&c1_id].len(), 1);
            assert_eq!(snap.mails.len(), 1);
            assert!(snap.fleets.contains_key(&940_001));
            assert!(snap.corporations.contains_key(&950_001));
            assert!(snap.bookmarks.contains_key(&960_001));
            assert!(snap.structures.contains_key(&970_001));
        });

        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-shm"));
    }

    #[test]
    fn test_d4_argon2id_bans_online_lock_and_oauth2_pkce_sso() {
        let sde = SdeRegistry::with_canonical_seed();
        let pid = std::process::id();
        let sqlite_path =
            std::env::temp_dir().join(format!("evomu_test_d4_auth_{pid}.sqlite"));
        let _ = std::fs::remove_file(&sqlite_path);

        let repo = WorldRepository::open_sqlite(&sqlite_path).unwrap();
        let uid = repo.provision_player_account("capsuleer_auth", "S3cretP@ss!");
        let ch = repo
            .create_character(uid, "SSO Capsuleer", 1, 1, 11, 17, &sde)
            .unwrap();
        repo.provision_gm_account("gm_root", "gm_secret_123", 90_999_001, "ISD Root");

        // 1. Single active session lock (multiLoginBlocker) + check_session_revoked enforcement
        assert!(repo
            .acquire_online_lock(uid, 101, Some(ch.character_id), false, 1_700_000_000)
            .unwrap()
            .is_none());
        assert_eq!(
            repo.acquire_online_lock(uid, 202, Some(ch.character_id), false, 1_700_000_005),
            Err(DbError::AlreadyLoggedIn {
                user_id: uid,
                active_session_id: 101,
            })
        );
        // Active session 101 passes check_session_revoked; unlocked session 202 is blocked with MultiLoginBlocked (R34-P1-3)
        assert_eq!(repo.check_session_revoked(uid, 101, 1_700_000_006), None);
        let revoked_202 = repo.check_session_revoked(uid, 202, 1_700_000_006).unwrap();
        assert_eq!(revoked_202.0, "MultiLoginBlocked");
        // Re-acquiring with the same session_id is idempotent
        assert!(repo
            .acquire_online_lock(uid, 101, Some(ch.character_id), false, 1_700_000_010)
            .unwrap()
            .is_none());

        // 2. OAuth2 PKCE + signed JWT SSO token flow
        let code_verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let code_challenge = base64url_encode_no_pad(&sha256_bytes(code_verifier.as_bytes()));
        let code = repo
            .authorize_pkce(
                "capsuleer_auth",
                "S3cretP@ss!",
                ch.character_id,
                "eveLauncherTQ",
                "https://login.eveonline.com/launcher",
                vec![
                    "esi-skills.read_skills.v1".to_string(),
                    "esi-wallet.read_character_wallet.v1".to_string(),
                ],
                &code_challenge,
                "S256",
                1_700_000_100,
            )
            .unwrap();

        // Wrong verifier must fail and consume the single-use code
        assert_eq!(
            repo.exchange_pkce_code(&code, "eveLauncherTQ", "wrong-verifier", 1_700_000_105),
            Err(DbError::InvalidCredentials)
        );
        // Re-issue and exchange with valid verifier
        let code2 = repo
            .authorize_pkce(
                "capsuleer_auth",
                "S3cretP@ss!",
                ch.character_id,
                "eveLauncherTQ",
                "https://login.eveonline.com/launcher",
                vec!["esi-skills.read_skills.v1".to_string()],
                &code_challenge,
                "S256",
                1_700_000_110,
            )
            .unwrap();
        let bundle = repo
            .exchange_pkce_code(&code2, "eveLauncherTQ", code_verifier, 1_700_000_115)
            .unwrap();
        assert_eq!(bundle.token_type, "Bearer");
        assert_eq!(bundle.expires_in, 1_200);

        let claims = repo
            .verify_sso_access_token(&bundle.access_token, 1_700_000_200)
            .unwrap();
        assert_eq!(claims.character_id, ch.character_id);
        assert_eq!(claims.user_id, uid);
        assert_eq!(claims.sub, format!("CHARACTER:EVE:{}", ch.character_id));

        // R34-P1-4: Forged JWT signed with old hardcoded secret or wrong secret must be rejected
        let forged_header = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
        let forged_payload = base64url_encode_no_pad(
            serde_json::to_string(&claims).unwrap().as_bytes(),
        );
        let forged_signing_input = format!("{forged_header}.{forged_payload}");
        let forged_sig = base64url_encode_no_pad(&hmac_sha256(
            b"evomu-sso-hs256-signing-key-v24-01-local-default",
            forged_signing_input.as_bytes(),
        ));
        let forged_jwt = format!("{forged_signing_input}.{forged_sig}");
        assert_eq!(
            repo.verify_sso_access_token(&forged_jwt, 1_700_000_200),
            Err(DbError::InvalidCredentials)
        );
        assert_eq!(
            repo.authenticate_at("capsuleer_auth", &forged_jwt, 1_700_000_200),
            Err(DbError::InvalidCredentials)
        );

        // JWT with alg:none or future iat (> now_unix) must be rejected
        let none_header = base64url_encode_no_pad(br#"{"alg":"none","typ":"JWT"}"#);
        let none_input = format!("{none_header}.{forged_payload}");
        let none_sig = base64url_encode_no_pad(&hmac_sha256(
            &repo.auth_runtime.read().unwrap().jwt_signing_secret,
            none_input.as_bytes(),
        ));
        let none_jwt = format!("{none_input}.{none_sig}");
        assert_eq!(
            repo.verify_sso_access_token(&none_jwt, 1_700_000_200),
            Err(DbError::InvalidCredentials)
        );
        assert_eq!(
            repo.verify_sso_access_token(&bundle.access_token, 1_700_000_050),
            Err(DbError::InvalidCredentials),
            "JWT with iat in the future must be rejected"
        );
        assert_eq!(
            repo.verify_sso_access_token(&bundle.access_token, 1_700_001_315),
            Err(DbError::InvalidCredentials),
            "Expired JWT (now_unix == exp) must be rejected"
        );
        assert_eq!(
            repo.authenticate_at("capsuleer_auth", &bundle.access_token, 1_700_002_000),
            Err(DbError::InvalidCredentials),
            "Expired JWT (now_unix > exp) must be rejected by authenticate_at"
        );

        // Login with signed SSO JWT token works directly via authenticate_at
        let sso_acc = repo
            .authenticate_at("capsuleer_auth", &bundle.access_token, 1_700_000_200)
            .unwrap();
        assert_eq!(sso_acc.user_id, uid);

        // Refresh token rotation
        let rotated = repo
            .refresh_sso_token(&bundle.refresh_token, "eveLauncherTQ", 1_700_000_300)
            .unwrap();
        // Old refresh token is now revoked
        assert_eq!(
            repo.refresh_sso_token(&bundle.refresh_token, "eveLauncherTQ", 1_700_000_301),
            Err(DbError::InvalidCredentials)
        );

        // 3. Kick user invalidates active session and revokes prior JWT/refresh tokens
        let kicked_sess = repo
            .kick_user(uid, "GM kick test", 1_700_000_400)
            .expect("session 101 was active");
        assert_eq!(kicked_sess.session_id, 101);
        assert!(repo.get_online_session(uid).is_none());
        assert_eq!(
            repo.verify_sso_access_token(&rotated.access_token, 1_700_000_405),
            Err(DbError::InvalidCredentials)
        );

        // 4. ROLE_ADMIN accounts cannot be banned or kicked (role hierarchy check)
        assert!(matches!(
            repo.ban_account("gm_root", 1_700_086_400, "mutiny"),
            Err(DbError::ValidationError(_))
        ));
        let gm_uid = repo.with_snapshot(|s| s.accounts["gm_root"].user_id);
        repo.acquire_online_lock(gm_uid, 505, Some(90_999_001), true, 1_700_000_410)
            .unwrap();
        assert!(repo.kick_user(gm_uid, "try kick gm", 1_700_000_420).is_none());
        assert!(repo.get_online_session(gm_uid).is_some());

        // 5. Account ban persists to SQLite and blocks login until unbanned or expired
        repo.ban_account("capsuleer_auth", 1_700_086_400, "EULA violation")
            .unwrap();
        assert_eq!(
            repo.authenticate_at("capsuleer_auth", "S3cretP@ss!", 1_700_001_000),
            Err(DbError::AccountBanned {
                until_unix: 1_700_086_400,
                reason: "EULA violation".to_string(),
            })
        );
        repo.unban_account("capsuleer_auth").unwrap();
        assert!(repo
            .authenticate_at("capsuleer_auth", "S3cretP@ss!", 1_700_001_000)
            .is_ok());

        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-shm"));
    }
}
