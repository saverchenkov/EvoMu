//! `eve-server` — Library exports for the `EvoMu` EVE Online server emulator.

#![forbid(unsafe_code)]

pub mod bug_reporting;
pub mod combat;
pub mod config;
pub mod esi;
pub mod flight_recorder;
pub mod formulas;
pub mod living_world;
pub mod living_world_haulers;
pub mod log_ring_buffer;
pub mod quasar_gateway;
pub mod quasar_grpc;
pub mod rng;
pub mod macho_listener;
pub mod router;
pub mod services;
pub mod sp6_lifecycle;
pub mod sso;
pub mod starter_characters;
pub mod system_actor;
pub mod world_seeder;

pub use services::inventory_fitting::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};

#[cfg(test)]
mod config_starter_state_deep_extra_unit_tests;
#[cfg(test)]
mod fast_checkout_plex_tests;
#[cfg(test)]
mod flight_recorder_misc_deep_extra_unit_tests;
#[cfg(test)]
mod formulas_extra_unit_tests;
#[cfg(test)]
mod living_world_seeder_deep_extra_unit_tests;
mod lifecycle_bug_reporting_deep_extra_unit_tests;
#[cfg(test)]
mod macho_listener_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_gateway_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_grpc_extra_unit_tests;
#[cfg(test)]
mod quasar_grpc_unit_tests;
#[cfg(test)]
mod rng_extra_unit_tests;
#[cfg(test)]
mod starter_characters_deep_extra_unit_tests;

