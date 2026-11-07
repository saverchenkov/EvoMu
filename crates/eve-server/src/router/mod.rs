//! Authenticated RPC and Quasar protobuf router (`M1`–`M8`).
//!
//! Encapsulates `ServerContext` initialization, remote object binding (`MachoBindObject`),
//! session validation, and central RPC dispatch across the 12 modular domain services.

pub mod binding;
mod binding_auth;
mod binding_effects;
pub mod cache_notifications;
pub mod client_compat;
pub mod context;
mod context_charges;
pub mod dispatch;
pub mod session;

#[cfg(test)]
mod binding_deep_extra_unit_tests;
#[cfg(test)]
mod binding_hooks_and_skills_extra_unit_tests;
#[cfg(test)]
mod cache_notifications_deep_extra_unit_tests;
#[cfg(test)]
mod dispatch_deep_extra_unit_tests;
#[cfg(test)]
mod r7_ta_p0_security_tests;
#[cfg(test)]
mod router_advanced_branches_tests;
#[cfg(test)]
mod session_context_deep_extra_unit_tests;
#[cfg(test)]
mod tests;

pub use cache_notifications::*;
pub use context::*;

/// Canonical MachoNet error code returned for unimplemented RPC methods.
pub const ERR_UNIMPLEMENTED: &str = "UnimplementedRPC";
