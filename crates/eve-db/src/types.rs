//! Domain models, record types, currency fixed-point types, and world state snapshots.

pub use eve_sde::{AttributeId, SolarSystemId, TypeId};
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

impl FromWalError for (u32, String) {
    fn from_wal_error(err: String) -> Self {
        (500, err)
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

    pub fn to_f64_isk(self) -> f64 {
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
    /// `invFlags`: 56 = Capsule, 57 = Pilot, 62 = CorpDeliveries, 63 = Locked.
    Capsule = 56,
    Pilot = 57,
    CorpDeliveries = 62,
    Locked = 63,
    Unlocked = 64,
    Impounded = 72,
    DroneBay = 87,
    Booster = 88,
    Implant = 89,
    ShipHangar = 90,
    RigSlot0 = 92,
    RigSlot1 = 93,
    RigSlot2 = 94,
    CorpSAG1 = 116,
    CorpSAG2 = 117,
    CorpSAG3 = 118,
    CorpSAG4 = 119,
    CorpSAG5 = 120,
    CorpSAG6 = 121,
    CorpSAG7 = 122,
    SubSystemSlot0 = 125,
    SubSystemSlot1 = 126,
    SubSystemSlot2 = 127,
    SubSystemSlot3 = 128,
    SubSystemSlot4 = 129,
    SpecializedFuelBay = 133,
    SpecializedOreHold = 134,
    SpecializedGasHold = 135,
    SpecializedMineralHold = 136,
    SpecializedSalvageHold = 137,
    SpecializedShipHold = 138,
    SpecializedAmmoHold = 143,
    SpecializedCommandCenterHold = 148,
    SpecializedPlanetaryCommoditiesHold = 149,
    SpecializedMaterialBay = 151,
    FleetHangar = 155,
    FighterBay = 158,
    ServiceSlot0 = 164,
    ServiceSlot1 = 165,
    ServiceSlot2 = 166,
    ServiceSlot3 = 167,
    ServiceSlot4 = 168,
    ServiceSlot5 = 169,
    ServiceSlot6 = 170,
    ServiceSlot7 = 171,
    Deliveries = 173,
    SpecializedMobileDepotHold = 179,
    SpecializedIceHold = 181,
    SpecializedAsteroidHold = 182,
    SpecializedInfrastructureHold = 185,
}

impl InventoryFlag {
    #[allow(non_upper_case_globals)]
    pub const OreHold: InventoryFlag = InventoryFlag::SpecializedOreHold;
    #[allow(non_upper_case_globals)]
    pub const GasHold: InventoryFlag = InventoryFlag::SpecializedGasHold;
    #[allow(non_upper_case_globals)]
    pub const IceHold: InventoryFlag = InventoryFlag::SpecializedIceHold;
    #[allow(non_upper_case_globals)]
    pub const MobileDepotHold: InventoryFlag = InventoryFlag::SpecializedMobileDepotHold;
    #[allow(non_upper_case_globals)]
    pub const InfrastructureHold: InventoryFlag = InventoryFlag::SpecializedInfrastructureHold;
    #[allow(non_upper_case_globals)]
    pub const Wardrobe: InventoryFlag = InventoryFlag::Pilot;
    #[allow(non_upper_case_globals)]
    pub const CorpHangar: InventoryFlag = InventoryFlag::CorpSAG1;

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
            62 => Self::CorpDeliveries,
            63 => Self::Locked,
            64 => Self::Unlocked,
            72 => Self::Impounded,
            87 => Self::DroneBay,
            88 => Self::Booster,
            89 => Self::Implant,
            90 => Self::ShipHangar,
            92 => Self::RigSlot0,
            93 => Self::RigSlot1,
            94 => Self::RigSlot2,
            116 => Self::CorpSAG1,
            117 => Self::CorpSAG2,
            118 => Self::CorpSAG3,
            119 => Self::CorpSAG4,
            120 => Self::CorpSAG5,
            121 => Self::CorpSAG6,
            122 => Self::CorpSAG7,
            125 => Self::SubSystemSlot0,
            126 => Self::SubSystemSlot1,
            127 => Self::SubSystemSlot2,
            128 => Self::SubSystemSlot3,
            129 => Self::SubSystemSlot4,
            133 => Self::SpecializedFuelBay,
            134 => Self::SpecializedOreHold,
            135 => Self::SpecializedGasHold,
            136 => Self::SpecializedMineralHold,
            137 => Self::SpecializedSalvageHold,
            138 => Self::SpecializedShipHold,
            143 => Self::SpecializedAmmoHold,
            148 => Self::SpecializedCommandCenterHold,
            149 => Self::SpecializedPlanetaryCommoditiesHold,
            151 => Self::SpecializedMaterialBay,
            155 => Self::FleetHangar,
            158 => Self::FighterBay,
            164 => Self::ServiceSlot0,
            165 => Self::ServiceSlot1,
            166 => Self::ServiceSlot2,
            167 => Self::ServiceSlot3,
            168 => Self::ServiceSlot4,
            169 => Self::ServiceSlot5,
            170 => Self::ServiceSlot6,
            171 => Self::ServiceSlot7,
            173 => Self::Deliveries,
            179 => Self::SpecializedMobileDepotHold,
            181 => Self::SpecializedIceHold,
            182 => Self::SpecializedAsteroidHold,
            185 => Self::SpecializedInfrastructureHold,
            _ => Self::None,
        }
    }

    pub fn is_service_slot(&self) -> bool {
        matches!(
            self,
            Self::ServiceSlot0
                | Self::ServiceSlot1
                | Self::ServiceSlot2
                | Self::ServiceSlot3
                | Self::ServiceSlot4
                | Self::ServiceSlot5
                | Self::ServiceSlot6
                | Self::ServiceSlot7
        )
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
    #[serde(default)]
    pub activity_status: Option<String>,
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
    #[serde(default)]
    pub jump_fatigue_until_unix: u64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub gender: u32,
    #[serde(default)]
    pub create_date_unix: u64,
    #[serde(default)]
    pub fleet_id: Option<i64>,
    #[serde(default)]
    pub autopilot_waypoints: Vec<i64>,
    pub version: u64,
    pub system_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnerNoteRecord {
    pub note_id: i64,
    pub character_id: i64,
    pub item_id: i64,
    pub label: String,
    pub note: String,
    pub timestamp_unix: u64,
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
    #[serde(default)]
    pub started_at_unix: Option<u64>,
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

impl Default for InventoryItemRecord {
    fn default() -> Self {
        Self {
            item_id: 0,
            type_id: 0,
            owner_id: 0,
            location_id: 0,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    }
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
    pub contract_type: String, // "ItemExchange", "Courier", "Auction"
    pub status: String,        // "Outstanding", "InProgress", "Completed", "Failed", "Expired", "Deleted", "Rejected"
    pub start_station_id: i64,
    pub end_station_id: i64,
    pub price: IskCents,
    pub reward: IskCents,
    pub collateral: IskCents,
    pub item_ids: Vec<i64>,
    #[serde(default)]
    pub expiration_unix: u64,
    #[serde(default)]
    pub issuer_corp_id: i64,
    #[serde(default)]
    pub assignee_id: i64,
    #[serde(default)]
    pub availability: String, // "Public", "Personal", "Corporation", "Alliance"
    #[serde(default)]
    pub buyout: IskCents,
    #[serde(default)]
    pub current_bid: IskCents,
    #[serde(default)]
    pub current_bidder_id: Option<i64>,
    #[serde(default)]
    pub requested_items: Vec<(TypeId, i32)>,
    #[serde(default)]
    pub bid_history: Vec<(i64, IskCents, u64)>,
    #[serde(default)]
    pub date_issued_unix: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub for_corp: bool,
    #[serde(default = "default_contract_num_days")]
    pub num_days: u32,
}

fn default_contract_num_days() -> u32 {
    1
}

impl Default for ContractRecord {
    fn default() -> Self {
        Self {
            contract_id: 0,
            issuer_id: 0,
            acceptor_id: None,
            contract_type: String::new(),
            status: String::new(),
            start_station_id: 0,
            end_station_id: 0,
            price: IskCents(0),
            reward: IskCents(0),
            collateral: IskCents(0),
            item_ids: Vec::new(),
            expiration_unix: 0,
            issuer_corp_id: 0,
            assignee_id: 0,
            availability: String::new(),
            buyout: IskCents(0),
            current_bid: IskCents(0),
            current_bidder_id: None,
            requested_items: Vec::new(),
            bid_history: Vec::new(),
            date_issued_unix: 0,
            title: String::new(),
            for_corp: false,
            num_days: 1,
        }
    }
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
    #[serde(default)]
    pub item_id: Option<i64>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub folder_id: Option<i64>,
    #[serde(default)]
    pub is_shared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailLabelRecord {
    pub label_id: i64,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookmarkFolderRecord {
    pub folder_id: i64,
    pub owner_id: i64,
    pub folder_name: String,
    #[serde(default)]
    pub is_shared: bool,
    #[serde(default)]
    pub corporation_id: Option<i64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub admin_group_id: Option<i64>,
    #[serde(default)]
    pub manage_group_id: Option<i64>,
    #[serde(default)]
    pub use_group_id: Option<i64>,
    #[serde(default)]
    pub view_group_id: Option<i64>,
}

impl Default for BookmarkFolderRecord {
    fn default() -> Self {
        Self {
            folder_id: 0,
            owner_id: 0,
            folder_name: String::new(),
            is_shared: false,
            corporation_id: None,
            description: String::new(),
            admin_group_id: None,
            manage_group_id: None,
            use_group_id: None,
            view_group_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailRecord {
    pub message_id: i64,
    pub sender_id: i64,
    pub recipient_id: i64,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FleetRole {
    FleetCommander = 1,
    WingCommander = 2,
    SquadCommander = 3,
    SquadMember = 4,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetMemberRecord {
    pub character_id: i64,
    pub role: FleetRole,
    #[serde(default)]
    pub wing_id: Option<i64>,
    #[serde(default)]
    pub squad_id: Option<i64>,
    #[serde(default)]
    pub takes_fleet_warp: bool,
    #[serde(default = "default_true")]
    pub accepts_regroup: bool,
    #[serde(default = "default_true")]
    pub accepts_conduit_jumps: bool,
    #[serde(default)]
    pub job: u32,
    #[serde(default)]
    pub role_name: String,
    #[serde(default)]
    pub join_time_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetWingRecord {
    pub wing_id: i64,
    pub name: String,
    #[serde(default)]
    pub commander_id: Option<i64>,
    #[serde(default)]
    pub squad_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetSquadRecord {
    pub squad_id: i64,
    pub wing_id: i64,
    pub name: String,
    #[serde(default)]
    pub commander_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetBroadcastRecord {
    pub broadcast_id: i64,
    pub broadcast_type: String,
    pub sender_id: i64,
    pub target_id: Option<i64>,
    pub solar_system_id: Option<SolarSystemId>,
    pub item_id: Option<i64>,
    pub created_at_ft: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetRecord {
    pub fleet_id: i64,
    pub boss_id: i64,
    pub members: Vec<i64>,
    pub wings: Vec<i64>,
    pub squads: Vec<i64>,
    pub free_move: bool,
    pub is_registered: bool,
    #[serde(default)]
    pub wing_details: HashMap<i64, FleetWingRecord>,
    #[serde(default)]
    pub squad_details: HashMap<i64, FleetSquadRecord>,
    #[serde(default)]
    pub member_details: HashMap<i64, FleetMemberRecord>,
    #[serde(default)]
    pub invites: HashSet<i64>,
    #[serde(default)]
    pub motd: String,
}

pub mod corp_roles {
    pub const ROLE_DIRECTOR: u64 = 1 << 0;
    pub const ROLE_PERSONNEL_MANAGER: u64 = 1 << 7;
    pub const ROLE_ACCOUNTANT: u64 = 1 << 8;
    pub const ROLE_SECURITY_OFFICER: u64 = 1 << 9;
    pub const ROLE_FACTORY_MANAGER: u64 = 1 << 10;
    pub const ROLE_STATION_MANAGER: u64 = 1 << 11;
    pub const ROLE_AUDITOR: u64 = 1 << 12;
    pub const ROLE_RENT_FACTORY_FACILITY: u64 = 1 << 17;
    pub const ROLE_RENT_RESEARCH_FACILITY: u64 = 1 << 18;
    pub const ROLE_DIPLOMAT: u64 = 1 << 34;
    pub const ROLE_EQUIPMENT_CONFIG: u64 = 1 << 41;
    pub const ROLE_CONFIG_EQUIPMENT: u64 = ROLE_EQUIPMENT_CONFIG;
    pub const ROLE_STARBASE_CONFIG: u64 = 1 << 53;
    pub const ROLE_STARBASE_OPERATOR: u64 = 1 << 40;
    /// `corpRoleRentOffice` (V24 client `eve/common/lib/appConst`).
    pub const ROLE_RENT_OFFICE: u64 = 1 << 49;
    /// `corpRoleJuniorAccountant` (V24 client `eve/common/lib/appConst`).
    pub const ROLE_JUNIOR_ACCOUNTANT: u64 = 1 << 52;
    /// `corpRoleTrader` (V24 client `eve/common/lib/appConst` / Crucible `EVE_Roles.h`: 1 << 20).
    pub const ROLE_TRADER: u64 = 1 << 20;
    /// `corpRoleContractManager` (V24 client `eve/common/lib/appConst` / Crucible `EVE_Roles.h:163`: 72057594037927936).
    pub const ROLE_CONTRACT_MANAGER: u64 = 1 << 56;
    /// `corpRoleProjectManager` (Corporation freelance & projects management).
    pub const ROLE_PROJECT_MANAGER: u64 = 1 << 6;
    /// `corpRoleAccountCanTake1..7` (V24 client `appConst`): withdraw from wallet division 1..7.
    pub const ROLE_ACCOUNT_CAN_TAKE: [u64; 7] = [
        1 << 27,
        1 << 28,
        1 << 29,
        1 << 30,
        1 << 31,
        1 << 32,
        1 << 33,
    ];

    pub fn has_role(role_mask: u64, required: u64) -> bool {
        role_mask == u64::MAX || (role_mask & ROLE_DIRECTOR) != 0 || (role_mask & required) == required
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpApplicationRecord {
    pub application_id: i64,
    pub character_id: i64,
    pub corporation_id: i64,
    pub message: String,
    pub created_unix: u64,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpWalletDivisionRecord {
    pub division_id: u8,
    pub name: String,
    pub balance: IskCents,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CorporationRecord {
    pub corporation_id: i64,
    pub name: String,
    pub ticker: String,
    pub ceo_id: i64,
    pub alliance_id: Option<i64>,
    pub tax_rate: f64,
    pub master_wallet: IskCents,
    pub members: HashSet<i64>,
    #[serde(default)]
    pub member_roles: HashMap<i64, u64>,
    #[serde(default)]
    pub applications: HashMap<i64, CorpApplicationRecord>,
    #[serde(default)]
    pub wallet_divisions: Vec<CorpWalletDivisionRecord>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub hq_station_id: Option<i64>,
}

impl CorporationRecord {
    pub fn default_wallet_divisions(master_wallet: IskCents) -> Vec<CorpWalletDivisionRecord> {
        (1..=7)
            .map(|d| CorpWalletDivisionRecord {
                division_id: d,
                name: if d == 1 {
                    "Master Wallet".to_string()
                } else {
                    format!("Division {d}")
                },
                balance: if d == 1 { master_wallet } else { IskCents(0) },
            })
            .collect()
    }

    pub fn ensure_wallet_divisions(&mut self) {
        for d in 1..=7u8 {
            if !self.wallet_divisions.iter().any(|w| w.division_id == d) {
                self.wallet_divisions.push(CorpWalletDivisionRecord {
                    division_id: d,
                    name: if d == 1 {
                        "Master Wallet".to_string()
                    } else {
                        format!("Division {d}")
                    },
                    balance: if d == 1 {
                        self.master_wallet
                    } else {
                        IskCents(0)
                    },
                });
            }
        }
        self.wallet_divisions.sort_by_key(|w| w.division_id);
        if let Some(div1) = self.wallet_divisions.iter_mut().find(|w| w.division_id == 1) {
            if div1.balance.0 == 0 && self.master_wallet.0 != 0 {
                div1.balance = self.master_wallet;
            } else {
                self.master_wallet = div1.balance;
            }
        }
    }

    pub fn division_balance(&self, division_id: u8) -> IskCents {
        if let Some(div) = self
            .wallet_divisions
            .iter()
            .find(|w| w.division_id == division_id)
        {
            if division_id == 1 && div.balance.0 == 0 && self.master_wallet.0 != 0 {
                return self.master_wallet;
            }
            return div.balance;
        }
        if division_id == 1 {
            self.master_wallet
        } else {
            IskCents(0)
        }
    }

    pub fn set_division_balance(&mut self, division_id: u8, balance: IskCents) {
        self.ensure_wallet_divisions();
        if let Some(div) = self
            .wallet_divisions
            .iter_mut()
            .find(|w| w.division_id == division_id)
        {
            div.balance = balance;
        }
        if division_id == 1 {
            self.master_wallet = balance;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatChannelRecord {
    pub channel_id: String,
    pub name: String,
    pub motd: String,
    pub owner_id: Option<i64>,
    pub members: HashSet<i64>,
    pub is_system_local: bool,
    pub solar_system_id: Option<SolarSystemId>,
    pub is_private: bool,
    #[serde(default)]
    pub operators: HashSet<i64>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub is_delayed_local: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageRecord {
    pub message_id: i64,
    pub channel_id: String,
    pub sender_id: i64,
    pub sender_name: String,
    pub message: String,
    pub timestamp_ft: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CosmicSignatureRecord {
    pub signature_id: String,
    pub solar_system_id: SolarSystemId,
    pub sig_group: String,
    pub type_name: String,
    pub position: [f64; 3],
    pub signal_strength: f64,
    pub wormhole_type_id: Option<TypeId>,
    pub destination_system_id: Option<SolarSystemId>,
    pub created_unix: u64,
    pub lifetime_seconds: u64,
    pub max_mass_kg: f64,
    pub remaining_mass_kg: f64,
    pub max_jump_mass_kg: f64,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CosmicAnomalyRecord {
    pub anomaly_id: String,
    pub solar_system_id: SolarSystemId,
    pub name: String,
    pub anomaly_type: String, // "Ore", "Combat"
    pub position: [f64; 3],
    pub signal_strength: f64, // 1.0 (always visible without probing)
    pub ore_types: Vec<TypeId>,
    pub remaining_volume: f64,
    pub created_unix: u64,
    pub lifetime_seconds: u64,
    pub cleared: bool,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KillmailRecord {
    pub kill_id: i64,
    pub victim_character_id: i64,
    pub attacker_character_id: i64,
    pub solar_system_id: SolarSystemId,
    pub ship_type_id: TypeId,
    pub total_value_isk: f64,
    /// Server time of the kill (`0` for legacy records written before it was recorded).
    #[serde(default)]
    pub killed_unix: u64,
    /// Victim / final-blow attacker corporation and alliance *at kill time*, so later corp
    /// changes never move a kill into or out of a war (`0` / `None` when unknown).
    #[serde(default)]
    pub victim_corp_id: i64,
    #[serde(default)]
    pub victim_alliance_id: Option<i64>,
    #[serde(default)]
    pub attacker_corp_id: i64,
    #[serde(default)]
    pub attacker_alliance_id: Option<i64>,
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
    #[serde(default)]
    pub self_destruct_at_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingTransactionRecord {
    pub event_id: i64,
    pub from_id: i64,
    pub to_id: i64,
    pub modification: f64,
    pub event_type_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsteroidRecord {
    pub asteroid_id: i64,
    pub solar_system_id: u32,
    pub belt_id: i64,
    pub type_id: u32,
    pub quantity: i64,
    pub initial_quantity: i64,
    pub position: (f64, f64, f64),
    pub radius: f64,
}


pub use crate::snapshot_ext::*;
