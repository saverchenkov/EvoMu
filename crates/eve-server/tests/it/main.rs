//! Single integration-test binary for eve-server (S-REFACTOR-TESTS).
//! Each former `tests/<name>.rs` file is a module here; test names are unchanged.

#![forbid(unsafe_code)]

mod common;
mod auth;
mod auth_persistence;
mod character;
mod character_progression;
mod character_skills_clones;
mod client_handshake_v24;
mod client_login_rpcs;
mod client_harness;
mod combat;
mod combat_death_crimewatch;
mod combat_pve;
mod destiny_binary_stream;
mod dogma_combat;
mod dogma_modifier_engine;
mod dogma_modules;
mod e2e_journeys;
mod economy_market;
mod economy_market_industry;
mod esi;
mod esi_corp_alliance;
mod esi_full;
mod esi_routes;
mod esi_universe_character;
mod esi_universe_fw_complete;
mod esi_universe_sov_complete;
mod industry;
mod mining_advanced;
mod mining_complete;
mod mining_reprocessing;
mod pve_mining_sites;
mod quasar_gateway;
mod regressions_b01_b15;
mod regressions_r6;
mod regressions_r8;
mod rpc_notification_closure;
mod security_auditor_flags;
mod seeded_rng;
mod social_fleets;
mod social_fleets_corps;
mod space;
mod space_mining;
mod space_navigation_cyno;
mod structures;
mod system;
mod time_control;
