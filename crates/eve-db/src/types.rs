//! Domain models, record types, currency fixed-point types, and world state snapshots.

use eve_sde::{AttributeId, SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DbError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Account banned until {until_unix}: {reason}")]
    AccountBanned { until_unix: u64, reason: String },
    #[error("Account suspended until {until_unix}: {reason}")]
    AccountSuspended { until_unix: u64, reason: String },
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

impl AccountRecord {
    /// Checks whether the account is currently banned or suspended at `now_unix`.
    /// Suspensions are stored in `ban_reason` prefixed with `"SUSPENDED:"` so they share SQLite durability.
    pub fn check_active_restriction(&self, now_unix: u64) -> Result<(), DbError> {
        if let Some(until) = self.banned_until_unix {
            if now_unix < until {
                let raw_reason = self.ban_reason.as_deref().unwrap_or_default();
                if let Some(susp_reason) = raw_reason.strip_prefix("SUSPENDED:") {
                    return Err(DbError::AccountSuspended {
                        until_unix: until,
                        reason: susp_reason.to_string(),
                    });
                }
                return Err(DbError::AccountBanned {
                    until_unix: until,
                    reason: raw_reason.to_string(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineSessionRecord {
    pub user_id: u64,
    pub username: String,
    pub session_id: u64,
    pub character_id: Option<i64>,
    pub connected_at_unix: u64,
    #[serde(default)]
    pub computer_hash: Option<String>,
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
    #[serde(default)]
    pub last_access_token_jti: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthRuntimeState {
    pub(crate) online_sessions: HashMap<u64, OnlineSessionRecord>,
    pub(crate) displaced_sessions: HashMap<u64, String>,
    pub(crate) kicked_users: HashMap<u64, (u64, String)>,
    pub(crate) pkce_codes: HashMap<String, PkceAuthCode>,
    pub(crate) refresh_tokens: HashMap<String, SsoRefreshTokenRecord>,
    pub(crate) revoked_jtis: HashSet<String>,
    pub(crate) jwt_signing_secret: [u8; 32],
}

impl AuthRuntimeState {
    pub(crate) fn new(jwt_signing_secret: [u8; 32]) -> Self {
        Self {
            online_sessions: HashMap::new(),
            kicked_users: HashMap::new(),
            displaced_sessions: HashMap::new(),
            pkce_codes: HashMap::new(),
            refresh_tokens: HashMap::new(),
            revoked_jtis: HashSet::new(),
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
    #[serde(default)]
    pub last_remap_unix: Option<u64>,
    #[serde(default)]
    pub remap_cooldown_until_unix: Option<u64>,
}

impl NeuralAttributes {
    pub fn get_attribute_by_id(&self, attr_id: u32) -> f64 {
        match attr_id {
            164 => self.charisma as f64,
            165 => self.intelligence as f64,
            166 => self.memory as f64,
            167 => self.perception as f64,
            168 => self.willpower as f64,
            _ => self.perception as f64,
        }
    }
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
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
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
                        issuer.balance = issuer
                            .balance
                            .checked_add(c.reward)
                            .unwrap_or(issuer.balance);
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
