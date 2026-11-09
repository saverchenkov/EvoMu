//! `eve-db` — Durable transactional persistence layer and domain repositories for `EvoMu` (`M1`–`M8`).
//!
//! Encapsulates all account, character, skill queue, clone, inventory, fitting, market, wallet,
//! contract, industry, planetary interaction, agent/mission, NPE, social, corporation, fleet,
//! CrimeWatch, Upwell structure, sovereignty, Faction Warfare, and dynamic system state behind
//! atomic repository operations with entity versioning, fixed-point ISK currency (`IskCents`),
//! per-character idempotency keys, and durable JSON snapshot save/load.

#![forbid(unsafe_code)]

pub mod accounts;
pub mod characters;
pub mod clock;
pub mod durable_domain_types;
pub use durable_domain_types as d6_types;
pub mod fw_seed;
pub mod gateway_tokens;
pub mod inventory;
pub mod ledger;
pub mod persistence;
pub mod seed;
pub mod service_domains;
pub mod snapshot_ext;
pub mod sqlite_store;
pub mod types;

pub use service_domains::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod accounts_sso_session_tests;
#[cfg(test)]
mod characters_inventory_ledger_tests;
#[cfg(test)]
mod sqlite_persistence_roundtrip_tests;
#[cfg(test)]
mod world_snapshot_sync_rollback_tests;
#[cfg(test)]
mod world_snapshot_extended_sync_tests;
#[cfg(test)]
mod world_snapshot_extended_sync_roundtrip_tests;
#[cfg(test)]
mod runtime_states_sync_and_helpers_tests;
#[cfg(test)]
mod domain_edges_and_persistence_tests;
#[cfg(test)]
mod sqlite_reload_and_contract_edges_tests;
#[cfg(test)]
mod sqlite_error_and_wal_delta_tests;

pub use accounts::service_roles;
pub use accounts::{base64url_decode_no_pad, base64url_encode_no_pad};
pub use clock::{ServerClock, VirtualServerClock};
pub use fw_seed::DEFAULT_FW_WARZONE_SYSTEMS;
pub use gateway_tokens::QuasarVerifiedToken;
pub use inventory::{ConsumeExactOptions, TransferItemSpec};
pub use ledger::{JournalEntryInput, JournalMeta};
pub use persistence::{
    record_last_wal_error, set_wal_error_hook, set_wal_poison_check_hook, WorldRepository,
};
pub use snapshot_ext::{
    CalendarEventRecord, ChatChannelAclRecord, CorpVoteBallotRecord, CorpVoteCaseRecord,
    LpRewardLogEntry, MailingListRecord, MarketDailySummary, MarketHistoryDayRecord,
    PersistedInsuranceContractRecord, SocialContactRecord, StarbaseDetailSettings,
};
pub use durable_domain_types::{
    DetailedKillmail, KillmailAttackerEntry, KillmailItemEntry, PersistedFwStatEvent,
    SocialContactNotificationRecord,
};
pub use types::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SafeLogoffEntry {
    pub char_id: i64,
    pub ship_id: i64,
    pub solar_system_id: u32,
    pub started_unix: u64,
    pub complete_at_unix: u64,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisconnectedShipEntry {
    pub char_id: i64,
    pub ship_id: i64,
    pub ship_type_id: u32,
    pub solar_system_id: u32,
    pub disconnected_at_unix: u64,
    pub remove_at_unix: u64,
    pub emergency_warp_dest: Option<[f64; 3]>,
    pub last_position: [f64; 3],
    pub removed_from_space: bool,
    #[serde(default)]
    pub pre_warp_position: Option<[f64; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfDestructPersistedEntry {
    pub ship_id: i64,
    pub char_id: i64,
    pub solar_system_id: u32,
    pub started_unix: u64,
    pub detonate_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbyssalPersistedSession {
    pub char_id: i64,
    pub ship_id: i64,
    pub pocket_system_id: u32,
    pub origin_system_id: u32,
    pub entered_unix: u64,
    pub collapse_at_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct PersistedCombatHp {
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PersistedSpaceObjectMeta {
    #[serde(default)]
    pub created_unix: Option<u64>,
    #[serde(default)]
    pub is_npc_wreck: bool,
    #[serde(default)]
    pub anchor_start_unix: Option<u64>,
    #[serde(default)]
    pub anchoring_delay_secs: f64,
    #[serde(default)]
    pub anchor_completed: bool,
    #[serde(default)]
    pub bubble_radius_m: Option<f64>,
    #[serde(default)]
    pub drone_ball_id: Option<i64>,
    #[serde(default)]
    pub planet_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedScannerProbe {
    pub probe_id: i64,
    pub probe_type_id: u32,
    pub owner_char_id: i64,
    pub ship_id: i64,
    pub launcher_module_id: i64,
    pub solar_system_id: u32,
    pub position: [f64; 3],
    pub destination: [f64; 3],
    pub range_step: i32,
    pub scan_range_m: f64,
    pub base_sensor_strength: f64,
    pub max_flight_time_s: f64,
    pub launched_at_unix: u64,
    pub state: i32,
    #[serde(default)]
    pub warp_end_ft: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sp6PersistedState {
    #[serde(default)]
    pub safe_logoffs: HashMap<i64, SafeLogoffEntry>,
    #[serde(default)]
    pub disconnected_ships: HashMap<i64, DisconnectedShipEntry>,
    #[serde(default)]
    pub self_destructs: HashMap<i64, SelfDestructPersistedEntry>,
    #[serde(default)]
    pub abyssal_sessions: HashMap<i64, AbyssalPersistedSession>,
    #[serde(default)]
    pub pvp_timers_until: HashMap<i64, u64>,
    #[serde(default)]
    pub npc_timers_until: HashMap<i64, u64>,
    #[serde(default)]
    pub weapon_timers_until: HashMap<i64, u64>,
    #[serde(default)]
    pub criminal_timers_until: HashMap<i64, u64>,
    #[serde(default)]
    pub last_downtime_day_idx: Option<u64>,
    #[serde(default)]
    pub ship_combat_hp: HashMap<i64, PersistedCombatHp>,
    #[serde(default)]
    pub space_object_meta: HashMap<i64, PersistedSpaceObjectMeta>,
    #[serde(default)]
    pub missions_awaiting_downtime_respawn: Vec<(i64, u32)>,
    #[serde(default)]
    pub mission_initial_npc_counts: HashMap<String, u32>,
    #[serde(default)]
    pub mission_initial_npc_types: HashMap<String, Vec<u32>>,
    #[serde(default)]
    pub active_scanner_probes: HashMap<i64, PersistedScannerProbe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_runtime_json: Option<String>,
}

#[cfg(test)] mod db_lib_extra_tests;
#[cfg(test)] mod db_deep_extra_unit_tests;
#[cfg(test)]
mod gateway_tokens_cov_unit_tests;
#[cfg(test)]
mod gateway_and_inventory_edge_tests;
#[cfg(test)]
mod pkce_and_sso_edge_tests;

