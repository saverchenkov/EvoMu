//! `eve-db` — Durable transactional persistence layer and domain repositories for `EvoMu` (`M1`–`M8`).
//!
//! Encapsulates all account, character, skill queue, clone, inventory, fitting, market, wallet,
//! contract, industry, planetary interaction, agent/mission, NPE, social, corporation, fleet,
//! CrimeWatch, Upwell structure, sovereignty, Faction Warfare, and dynamic system state behind
//! atomic repository operations with entity versioning, fixed-point ISK currency (`IskCents`),
//! per-character idempotency keys, and durable JSON snapshot save/load.

#![forbid(unsafe_code)]

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
    #[error("Destination location {location_id} is not authorized for character {character_id}")]
    InvalidDestinationLocation { character_id: i64, location_id: i64 },
    #[error("Stale entity version (expected {expected}, found {actual})")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("Insufficient ISK balance (required {required}, available {available})")]
    InsufficientFunds { required: i64, available: i64 },
    #[error("Alpha clone skill point cap (5,000,000 SP) exceeded")]
    AlphaCloneSpCapExceeded,
    #[error("Domain validation error: {0}")]
    ValidationError(String),
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
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }

    pub fn checked_mul_qty(self, qty: i64) -> Option<Self> {
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
    MedSlot0 = 19,
    MedSlot1 = 20,
    MedSlot2 = 21,
    MedSlot3 = 22,
    HiSlot0 = 27,
    HiSlot1 = 28,
    HiSlot2 = 29,
    HiSlot3 = 30,
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
            19 => Self::MedSlot0,
            20 => Self::MedSlot1,
            21 => Self::MedSlot2,
            22 => Self::MedSlot3,
            27 => Self::HiSlot0,
            28 => Self::HiSlot1,
            29 => Self::HiSlot2,
            30 => Self::HiSlot3,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub user_id: u64,
    pub username: String,
    pub password_token: String,
    pub role: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRecord {
    pub character_id: i64,
    pub user_id: u64,
    pub name: String,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
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
    pub version: u64,
    pub system_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainedSkillRecord {
    pub type_id: TypeId,
    pub level: u8,
    pub skill_points: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillQueueEntry {
    pub type_id: TypeId,
    pub target_level: u8,
    pub start_sp: f64,
    pub end_sp: f64,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JumpCloneRecord {
    pub jump_clone_id: i64,
    pub character_id: i64,
    pub location_id: i64,
    pub implants: Vec<TypeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedFittingRecord {
    pub fitting_id: i64,
    pub owner_id: i64,
    pub name: String,
    pub ship_type_id: TypeId,
    pub description: String,
    pub items: Vec<(TypeId, InventoryFlag, i32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletJournalEntry {
    pub entry_id: i64,
    pub character_id: i64,
    pub entry_type: String,
    pub amount: IskCents,
    pub balance_after: IskCents,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractRecord {
    pub contract_id: i64,
    pub issuer_id: i64,
    pub acceptor_id: Option<i64>,
    pub contract_type: String, // "ItemExchange", "Auction", "Courier"
    pub status: String,        // "Outstanding", "InProgress", "Completed"
    pub start_station_id: i64,
    pub end_station_id: i64,
    pub price: IskCents,
    pub reward: IskCents,
    pub collateral: IskCents,
    pub item_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanetaryColonyRecord {
    pub planet_id: i64,
    pub owner_id: i64,
    pub solar_system_id: SolarSystemId,
    pub command_center_level: u8,
    pub pins_count: u32,
    pub extracted_commodities: HashMap<TypeId, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveMissionRecord {
    pub mission_id: u32,
    pub character_id: i64,
    pub agent_id: i64,
    pub state: String, // "Offered", "Accepted", "Completed"
    pub current_room_id: u32,
    pub npcs_remaining: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpeProgressRecord {
    pub character_id: i64,
    pub node_graph_id: u32,
    pub current_node_id: u32,
    pub completed: bool,
    pub career_goal_progress: HashMap<u32, u64>,
    pub claimed_goals: HashSet<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkRecord {
    pub bookmark_id: i64,
    pub owner_id: i64,
    pub label: String,
    pub solar_system_id: SolarSystemId,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailRecord {
    pub message_id: i64,
    pub sender_id: i64,
    pub recipient_id: i64,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRecord {
    pub fleet_id: i64,
    pub boss_id: i64,
    pub members: Vec<i64>,
    pub wings: Vec<i64>,
    pub squads: Vec<i64>,
    pub free_move: bool,
    pub is_registered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpProjectRecord {
    pub project_id: i64,
    pub corporation_id: i64,
    pub name: String,
    pub target_quantity: u64,
    pub current_quantity: u64,
    pub isk_per_unit: IskCents,
    pub escrow_remaining: IskCents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillmailRecord {
    pub kill_id: i64,
    pub victim_character_id: i64,
    pub attacker_character_id: i64,
    pub solar_system_id: SolarSystemId,
    pub ship_type_id: TypeId,
    pub total_value_isk: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillRightRecord {
    pub kill_right_id: i64,
    pub owner_id: i64,
    pub target_id: i64,
    pub activation_fee: IskCents,
    pub is_active_suspect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpwellStructureRecord {
    pub structure_id: i64,
    pub type_id: TypeId,
    pub owner_corp_id: i64,
    pub solar_system_id: SolarSystemId,
    pub name: String,
    pub position: [f64; 3],
    pub state: String, // "Anchoring", "OnliningVulnerable", "ShieldVulnerable", "ArmorReinforced", "HullVulnerable"
    pub quantum_core_installed: bool,
    pub fuel_blocks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldStateSnapshot {
    pub next_entity_id: i64,
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
    pub npe_progress: HashMap<i64, NpeProgressRecord>,
    pub bookmarks: HashMap<i64, BookmarkRecord>,
    pub mails: Vec<MailRecord>,
    pub fleets: HashMap<i64, FleetRecord>,
    pub corporations: HashMap<i64, CorporationRecord>,
    pub corp_projects: HashMap<i64, CorpProjectRecord>,
    pub killmails: HashMap<i64, KillmailRecord>,
    pub kill_rights: HashMap<i64, KillRightRecord>,
    pub structures: HashMap<i64, UpwellStructureRecord>,
    pub system_states: HashMap<SolarSystemId, SystemDynamicState>,
    pub committed_idempotency_keys: HashSet<(i64, String)>,
}

/// Encapsulated transactional world repository.
pub struct WorldRepository {
    inner: RwLock<WorldStateSnapshot>,
    snapshot_path: Option<std::path::PathBuf>,
}

impl Default for WorldRepository {
    fn default() -> Self {
        Self::with_demo_seed()
    }
}

impl WorldRepository {
    pub fn with_demo_seed() -> Self {
        let mut accounts = HashMap::new();
        accounts.insert(
            "demo".to_string(),
            AccountRecord {
                user_id: 1,
                username: "demo".to_string(),
                password_token: "demo-secret".to_string(),
                role: 0x0000_0000_0000_0003,
            },
        );
        accounts.insert(
            "other".to_string(),
            AccountRecord {
                user_id: 2,
                username: "other".to_string(),
                password_token: "other-secret".to_string(),
                role: 0x0000_0000_0000_0003,
            },
        );

        let mut characters = HashMap::new();
        characters.insert(
            90_000_001,
            CharacterRecord {
                character_id: 90_000_001,
                user_id: 1,
                name: "Aura Capsuleer".to_string(),
                corporation_id: 1_000_125,
                alliance_id: None,
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
                ancestry_id: 13,
                school_id: 20, // Republic Military School (Minmatar)
                neural: NeuralAttributes::default(),
                skill_points: 15_000_000.0,
                free_skill_points: 50_000.0,
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
                corporation_id: 1_000_125,
                alliance_id: None,
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
                ancestry_id: 1,
                school_id: 17, // Science and Trade Institute (Caldari)
                neural: NeuralAttributes::default(),
                skill_points: 4_500_000.0,
                free_skill_points: 0.0,
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
            },
        );

        // Seed starter modules, minerals, blueprint, mutaplasmid, skill injector, structure, quantum core, and tags
        let starter_items = [
            (100_000_003, 438, 1, true),      // 1MN Afterburner II
            (100_000_004, 1248, 2, true),     // Overdrive Injector System II
            (100_000_005, 2889, 1, true),     // 200mm AutoCannon II
            (100_000_006, 448, 1, true),      // Warp Scrambler II
            (100_000_007, 687, 1, true),      // Rifter Blueprint
            (100_000_008, 34, 100_000, false), // Tritanium
            (100_000_009, 35, 50_000, false),  // Pyerite
            (100_000_010, 36, 20_000, false),  // Mexallon
            (100_000_011, 37, 5_000, false),   // Isogen
            (100_000_012, 1230, 10_000, false), // Veldspar
            (100_000_013, 47702, 5, false),   // Decayed 1MN Afterburner Mutaplasmid
            (100_000_014, 40520, 2, false),   // Large Skill Injector
            (100_000_015, 35832, 1, false),   // Astrahus
            (100_000_016, 56201, 1, false),   // Astrahus Upwell Quantum Core
            (100_000_017, 35664, 5, false),   // Clone Soldier Negotiator Tag
            (100_000_018, 1230, 5_000, false), // Reserve Veldspar stack for Corp Projects
            (100_000_019, 47763, 5, false),   // Calm Exotic Filament
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
        for sys_id in [30_000_142, 30_000_144, 30_002_813, 30_002_904, 31_000_005] {
            system_states.insert(
                sys_id,
                SystemDynamicState {
                    solar_system_id: sys_id,
                    dbs_multiplier: 1.35,
                    ess_main_bank: IskCents::from_isk(10_000_000),
                    ess_reserve_bank: IskCents::from_isk(25_000_000),
                    ess_linked_character_id: None,
                    ess_link_started_tick: None,
                    sov_alliance_id: None,
                    military_index: 2,
                    industrial_index: 2,
                    strategic_index: 3,
                    fw_contested_pct: 45.0,
                    insurgency_corruption_stage: 2,
                    insurgency_suppression_stage: 1,
                    wormhole_remaining_mass_kg: 2_000_000_000.0,
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
        skills.insert(
            90_000_001,
            HashMap::from([
                (
                    3327,
                    TrainedSkillRecord {
                        type_id: 3327,
                        level: 4,
                        skill_points: 45_255.0,
                    },
                ),
                (
                    3328,
                    TrainedSkillRecord {
                        type_id: 3328,
                        level: 4,
                        skill_points: 90_510.0,
                    },
                ),
                (
                    3443,
                    TrainedSkillRecord {
                        type_id: 3443,
                        level: 4,
                        skill_points: 45_255.0,
                    },
                ),
                (
                    3446,
                    TrainedSkillRecord {
                        type_id: 3446,
                        level: 4,
                        skill_points: 90_510.0,
                    },
                ),
                (
                    16622,
                    TrainedSkillRecord {
                        type_id: 16622,
                        level: 4,
                        skill_points: 135_765.0,
                    },
                ),
            ]),
        );

        let snapshot_path = std::env::var("EVOMU_SNAPSHOT_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);

        let repo = Self {
            inner: RwLock::new(WorldStateSnapshot {
                next_entity_id: 200_000_000,
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
                npe_progress,
                bookmarks: HashMap::new(),
                mails: Vec::new(),
                fleets: HashMap::new(),
                corporations: HashMap::new(),
                corp_projects: HashMap::new(),
                killmails: HashMap::new(),
                kill_rights: HashMap::new(),
                structures: HashMap::new(),
                system_states,
                committed_idempotency_keys: HashSet::new(),
            }),
            snapshot_path: snapshot_path.clone(),
        };

        if let Some(ref p) = snapshot_path {
            if p.exists() {
                let _ = repo.load_snapshot_from_file(p);
            } else {
                let _ = repo.save_snapshot_to_file(p);
            }
        }
        repo
    }

    /// Saves the complete transactional world state to a JSON snapshot file (`M1` durable persistence).
    pub fn save_snapshot_to_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let json = serde_json::to_string_pretty(&*guard).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    /// Restores the transactional world state from a JSON snapshot file.
    pub fn load_snapshot_from_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let snap: WorldStateSnapshot = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = snap;
        Ok(())
    }

    pub fn with_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&WorldStateSnapshot) -> R,
    {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Atomically applies `f` to a staged clone of the snapshot and commits only if `f` completes
    /// without panicking, preventing partial state corruption on unwind.
    pub fn with_mut_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WorldStateSnapshot) -> R,
    {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut staged = guard.clone();
        let out = f(&mut staged);
        *guard = staged;
        if let Some(ref p) = self.snapshot_path {
            if let Ok(json) = serde_json::to_string_pretty(&*guard) {
                let _ = std::fs::write(p, json);
            }
        }
        out
    }

    pub fn authenticate(
        &self,
        username: &str,
        password_token: &str,
    ) -> Result<AccountRecord, DbError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let acc = guard
            .accounts
            .get(username)
            .ok_or(DbError::InvalidCredentials)?;
        if acc.password_token != password_token {
            return Err(DbError::InvalidCredentials);
        }
        Ok(acc.clone())
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

    /// Creates a new character for `user_id` with racial Corvette, starter skills, and home station (`E2E-01`).
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

        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
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
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());

        let ch = guard
            .characters
            .get(&character_id)
            .ok_or(DbError::CharacterNotFound(character_id))?
            .clone();

        // Authorize destination: must be the character's current station, structure, or active ship
        let valid_dest = (Some(new_location_id) == ch.station_id)
            || (Some(new_location_id) == ch.structure_id)
            || (new_location_id == ch.active_ship_id);
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
    }

    /// Atomically transitions a docked character and their active ship into space (`location_id = solar_system_id`, `flag = None`).
    pub fn undock_character(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<(i64, CharacterRecord, InventoryItemRecord), DbError> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());

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
    }

    /// Docks a character in a station or Upwell structure.
    pub fn dock_character(
        &self,
        character_id: i64,
        dock_location_id: i64,
        is_structure: bool,
    ) -> Result<CharacterRecord, DbError> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let active_ship_id = {
            let ch = guard
                .characters
                .get_mut(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?;
            if is_structure {
                ch.structure_id = Some(dock_location_id);
                ch.station_id = None;
            } else {
                ch.station_id = Some(dock_location_id);
                ch.structure_id = None;
            }
            ch.version += 1;
            ch.system_epoch += 1;
            ch.active_ship_id
        };
        if let Some(ship) = guard.items.get_mut(&active_ship_id) {
            ship.location_id = dock_location_id;
            ship.flag = InventoryFlag::Hangar;
            ship.version += 1;
        }
        Ok(guard.characters[&character_id].clone())
    }

    /// Compensating transaction to restore docked station state if solar-system actor spawn fails.
    pub fn rollback_undock(&self, character_id: i64, station_id: i64, ship_id: i64) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
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
    }

    /// Updates a character's solar system (e.g. after Stargate Jump, Cyno Jump, Wormhole, or Filament).
    pub fn set_character_solar_system(
        &self,
        character_id: i64,
        new_solar_system_id: SolarSystemId,
        sde: &SdeRegistry,
    ) -> Result<CharacterRecord, DbError> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
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
            100_000_001,
            60_003_760,
            InventoryFlag::Cargo,
            None,
            Some("shared-key-1"),
        );
        assert!(matches!(res, Err(DbError::ItemOwnershipViolation { .. })));

        let moved1 = repo
            .move_item_transactional(
                90_000_001,
                100_000_001,
                60_003_760,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-rifter-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved1.flag, InventoryFlag::Cargo);
        assert_eq!(moved1.version, 2);

        let moved2 = repo
            .move_item_transactional(
                90_000_001,
                100_000_001,
                60_003_760,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-rifter-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved2.version, 2);

        let stale = repo.move_item_transactional(
            90_000_001,
            100_000_001,
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
        let sde = SdeRegistry::with_canonical_seed();
        let repo = WorldRepository::with_demo_seed();
        let created = repo
            .create_character(1, "Snapshot Pilot", 1, 1, 1, 11, &sde)
            .unwrap();
        // Verify Caldari pilot spawns in Ibis (601), not hardcoded Rifter (587)
        let created_items = repo.get_items_for_character(created.character_id);
        assert_eq!(created_items[0].type_id, 601);

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
                },
            );
            snap.active_missions.insert(
                (created.character_id, 1001),
                ActiveMissionRecord {
                    mission_id: 1001,
                    character_id: created.character_id,
                    agent_id: 3018924,
                    state: "Accepted".to_string(),
                    current_room_id: 1,
                    npcs_remaining: 3,
                },
            );
            snap.committed_idempotency_keys
                .insert((created.character_id, "idem-1".to_string()));
        });

        let snap_path = std::env::temp_dir().join("evomu_test_snapshot.json");
        repo.save_snapshot_to_file(&snap_path).unwrap();

        let repo2 = WorldRepository::with_demo_seed();
        repo2.load_snapshot_from_file(&snap_path).unwrap();
        let loaded = repo2
            .select_character_for_user(1, created.character_id)
            .unwrap();
        assert_eq!(loaded.name, "Snapshot Pilot");
        let has_colony = repo2.with_snapshot(|snap| {
            snap.planetary_colonies
                .contains_key(&(created.character_id, 40009082))
                && snap
                    .active_missions
                    .contains_key(&(created.character_id, 1001))
        });
        assert!(has_colony);
        let _ = std::fs::remove_file(&snap_path);
    }
}
