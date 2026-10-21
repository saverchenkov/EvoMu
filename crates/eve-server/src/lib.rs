//! `eve-server` — Library exports for the `EvoMu` EVE Online server emulator.

#![forbid(unsafe_code)]

pub mod combat;
pub mod config;
pub mod esi;
pub mod formulas;
pub mod living_world;
pub mod quasar_gateway;
pub mod rng;
pub mod macho_listener;
pub mod router;
pub mod services;
pub mod sso;
pub mod starter_characters;
pub mod system_actor;
pub mod world_seeder;

pub use services::inventory_fitting::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};
