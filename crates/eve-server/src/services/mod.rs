//! `services` — Modular 12-Domain RPC & Quasar Service Handlers (`M1`–`M8`).
//!
//! Organizes all 216 cataloged `machoNet` RPC services (`193` V24.01 active + `23` legacy-only)
//! and all 135 Quasar Protobuf domains (`69` `eve_public.*` + `66` `eve.*` internal) into 12 cohesive
//! domain modules matching `PLAN.md` §3.12 and `docs/spec/01..05`.

#![forbid(unsafe_code)]

pub mod account_services;
pub mod agents_missions;
pub mod auth_cache;
pub mod char_selection;
pub mod character_lifecycle;
pub mod character_skills;
pub mod client_login;
pub mod combat_crimewatch;
pub mod config_service;
pub mod contracts_service;
pub use contracts_service as contracts;
pub mod corp_registry_ops;
pub mod cosmetics_events;
pub mod cosmetics_ui_ops;
pub mod fleet_social;
pub mod fw_enlistment;
pub mod fw_services;
pub mod implant_plugging;
pub mod industry_pi;
pub mod insurgency_services;
pub mod inventory_fitting;
pub mod inventory_stacking;
pub mod jump_clone_names;
pub mod lp_store;
pub mod market_contracts;
pub mod market_trading;
pub mod mining_advanced;
pub mod mining_sites;
pub mod operations_manager;
pub mod pos_manager;
pub mod pos_starbase;
pub mod project_discovery;
pub mod pve_dungeon_cache;
pub mod pve_exploration;
pub mod pve_hacking_sites;
pub mod pve_special_sites;
pub mod quasar_achievements;
pub mod quasar_assetholding_entitlements;
pub mod quasar_auth_user;
pub mod quasar_career_goals;
pub mod quasar_cosmetics_services;
pub mod quasar_daily_goals;
pub mod quasar_economy_planetary_industry;
pub mod quasar_entitlement_membership;
pub mod quasar_inventory_asset;
pub mod quasar_market;
pub mod quasar_payment_store;
pub mod quasar_progression;
pub mod quasar_progression_extra;
pub mod quasar_skills_clones;
pub mod quasar_social;
pub mod quasar_social_chat;
pub mod quasar_social_community;
pub mod quasar_social_corp;
pub mod quasar_social_freelance;
pub mod quasar_sov_services;
pub mod quasar_space_services;
pub mod ship_flight_ops;
pub mod social_corp_fleet;
pub mod sov_fw_insurgency;
pub mod sov_services;
pub mod space_destiny;
pub mod space_entity_mining;
pub mod station_structure;
pub mod tactical_modules;
pub mod wallet_ledger;
pub mod wallet_serverops;
pub mod wallet_services_ext;
