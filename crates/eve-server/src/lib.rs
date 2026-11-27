//! `eve-server` — Library exports for the `EvoMu` EVE Online server emulator.

#![forbid(unsafe_code)]
#![deny(clippy::large_futures)]

pub mod bug_reporting;
pub mod combat;
pub mod config;
pub mod critical_log;
pub mod domain;
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
pub mod ops_metrics;
pub mod outbound_scene_scope;
pub mod outbound_sequencer;
pub mod router;
pub mod sequencer_invariants;
pub mod services;
pub mod sp6_lifecycle;
pub mod sso;
pub mod starter_characters;
pub mod static_data;
pub mod system_actor;
pub mod world_seeder;
pub mod lifecycle;
pub mod ops_telemetry;
pub mod warn_limiter;

pub use services::inventory_fitting::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};

#[cfg(any(test, feature = "testkit"))]
pub mod probe_test_helpers;

#[cfg(any(test, feature = "testkit"))]
pub(crate) use eve_db::{lock_test_env_read, TestEnvReadGuard};
#[cfg(test)]
pub(crate) use eve_db::{lock_test_env_write, TestEnvWriteGuard};

/// Resolves `eve_sde::SdeRegistry::default_db_path()` and invokes `f` under a shared test-env
/// read guard so concurrent `EVOMU_SDE_PATH` poisoning tests cannot race with fallback loaders.
#[inline]
pub(crate) fn with_default_sde_db_path<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
    #[cfg(any(test, feature = "testkit"))]
    let _env_guard = lock_test_env_read();
    let path = eve_sde::SdeRegistry::default_db_path();
    f(&path)
}

#[cfg(test)]
mod config_starter_state_deep_extra_unit_tests;
#[cfg(test)]
mod fast_checkout_plex_tests;
#[cfg(test)]
mod fast_checkout_offers_and_tokens_tests;
#[cfg(test)]
mod flight_recorder_misc_deep_extra_unit_tests;
#[cfg(test)]
mod formulas_extra_unit_tests;
#[cfg(test)]
mod living_world_seeder_deep_extra_unit_tests;
#[cfg(test)]
mod lifecycle_bug_reporting_deep_extra_unit_tests;
#[cfg(test)]
mod macho_listener_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_gateway_deep_extra_unit_tests;
#[cfg(test)]
mod quasar_grpc_extra_unit_tests;
#[cfg(test)]
mod quasar_grpc_payload_and_jwt_unit_tests;
#[cfg(test)]
mod quasar_grpc_unit_tests;
#[cfg(test)]
mod quasar_grpc_notifications_and_errors_unit_tests;
#[cfg(test)]
mod bug_reporting_cluster_salvage_security_tests;
#[cfg(test)]
mod rng_extra_unit_tests;
#[cfg(test)]
mod sp6_system_actor_edge_unit_tests;
#[cfg(test)]
mod starter_characters_deep_extra_unit_tests;
#[cfg(test)]
mod server_core_cov_unit_tests;
#[cfg(test)]
mod combat_ship_fit_edge_unit_tests;
