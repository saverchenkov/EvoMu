//! Authenticated RPC and Quasar protobuf router (`M1`–`M8`).
//!
//! Encapsulates `ServerContext` initialization, remote object binding (`MachoBindObject`),
//! session validation, and central RPC dispatch across the 12 modular domain services.

pub mod binding;
pub mod cache_notifications;
pub mod context;
mod context_charges;
pub mod dispatch;
pub mod session;

#[cfg(test)]
mod tests;

pub use cache_notifications::*;
pub use context::*;
