//! `eve-server` — Library exports for the `EvoMu` EVE Online server emulator.

#![forbid(unsafe_code)]

pub mod bug_reporting;
pub mod combat;
pub mod config;
pub mod esi;
pub mod flight_recorder;
pub mod formulas;
pub mod living_world;
pub mod log_ring_buffer;
pub mod quasar_gateway;
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
