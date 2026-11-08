//! `eve-server` — Library exports for the `EvoMu` EVE Online server emulator.

#![forbid(unsafe_code)]

pub mod bug_reporting;
pub mod combat;
pub mod config;
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
pub mod router;
pub mod services;
pub mod sp6_lifecycle;
pub mod sso;
pub mod starter_characters;
pub mod system_actor;
pub mod world_seeder;

pub use services::inventory_fitting::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};

#[cfg(test)]
static TEST_ENV_RW: std::sync::RwLock<()> = std::sync::RwLock::new(());
#[cfg(test)]
thread_local! {
    static TEST_ENV_WRITE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) struct TestEnvWriteGuard(Option<std::sync::RwLockWriteGuard<'static, ()>>);
#[cfg(test)]
impl Drop for TestEnvWriteGuard {
    fn drop(&mut self) {
        self.0.take();
        TEST_ENV_WRITE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}
#[cfg(test)]
pub(crate) fn lock_test_env_write() -> TestEnvWriteGuard {
    let depth = TEST_ENV_WRITE_DEPTH.with(|d| d.get());
    let g = (depth == 0).then(|| TEST_ENV_RW.write().unwrap_or_else(|e| e.into_inner()));
    TEST_ENV_WRITE_DEPTH.with(|d| d.set(depth + 1));
    TestEnvWriteGuard(g)
}
#[cfg(test)]
pub(crate) fn lock_test_env_read() -> Option<std::sync::RwLockReadGuard<'static, ()>> {
    TEST_ENV_WRITE_DEPTH
        .with(|d| d.get() == 0)
        .then(|| TEST_ENV_RW.read().unwrap_or_else(|e| e.into_inner()))
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
mod starter_characters_deep_extra_unit_tests;
