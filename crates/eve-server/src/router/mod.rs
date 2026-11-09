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
mod context_runtime_persistence;
pub mod dispatch;
pub mod rpc_args;
pub mod session;
pub mod session_transition;

#[cfg(test)]
mod binding_deep_extra_unit_tests;
#[cfg(test)]
mod binding_hooks_and_skills_extra_unit_tests;
#[cfg(test)]
mod cache_notifications_deep_extra_unit_tests;
#[cfg(test)]
mod dispatch_deep_extra_unit_tests;
#[cfg(test)]
mod session_sync_object_binding_security_tests;
#[cfg(test)]
mod router_advanced_branches_tests;
#[cfg(test)]
mod runtime_persistence_tests;
#[cfg(test)]
mod session_context_deep_extra_unit_tests;
#[cfg(test)]
mod tests;

pub use cache_notifications::*;
pub use context::*;
pub use dispatch::box_fut;
pub use rpc_args::RpcArgs;
pub use session_transition::*;

/// Canonical MachoNet error code returned for unimplemented RPC methods.
pub const ERR_UNIMPLEMENTED: &str = "UnimplementedRPC";
