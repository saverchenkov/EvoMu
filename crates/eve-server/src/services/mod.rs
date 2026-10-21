//! `services` — Modular 12-Domain RPC & Quasar Service Handlers (`M1`–`M8`).
//!
//! Organizes all 216 cataloged `machoNet` RPC services (`193` V24.01 active + `23` legacy-only)
//! and all 135 Quasar Protobuf domains (`69` `eve_public.*` + `66` `eve.*` internal) into 12 cohesive
//! domain modules matching `PLAN.md` §3.12 and `docs/spec/01..05`.

#![forbid(unsafe_code)]

pub mod agents_missions;
pub mod auth_cache;
pub mod char_selection;
pub mod character_skills;
pub mod client_login;
pub mod combat_crimewatch;
pub mod config_service;
pub mod contracts_service;
pub use contracts_service as contracts;
pub mod cosmetics_events;
pub mod fitting_mining_lasers;
pub mod fleet_social;
pub mod industry_pi;
pub mod inventory_fitting;
pub mod lp_store;
pub mod market_contracts;
pub mod market_trading;
pub mod mining_advanced;
pub mod mining_sites;
pub mod social_corp_fleet;
pub mod sov_fw_insurgency;
pub mod space_destiny;
pub mod space_entity_mining;
pub mod station_structure;
