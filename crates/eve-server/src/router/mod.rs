//! Authenticated RPC and Quasar protobuf router (`M1`–`M8`).
//!
//! Encapsulates `ServerContext` initialization, remote object binding (`MachoBindObject`),
//! session validation, and central RPC dispatch across the 12 modular domain services.

pub mod binding;
mod binding_auth;
mod binding_effects;
pub(crate) mod binding_transition_policy;
pub mod cache_notifications;
pub mod client_compat;
pub mod client_wire;
pub mod context;
mod context_charges;
mod context_runtime_persistence;
pub mod delta_commit_listener;
pub mod delta_item_compare;
pub mod delta_item_switch;
mod delta_item_switch_ctx;
pub mod delta_projection;
pub mod destiny_delivery_policy;
pub mod dispatch;
pub mod notify_registry;
pub mod notify_types;
pub mod reply_registry;
pub mod rpc_args;
pub mod rpc_registry;
pub mod session;
pub mod session_transition;
pub mod slash_registry;
pub mod teardown_registry;
#[cfg(any(test, feature = "testkit"))]
pub mod fixture_census;
#[cfg(any(test, feature = "testkit"))]
pub mod test_context;

pub use binding::{alloc_transport_session_id, SessionCleanupGuard, NEXT_TRANSPORT_SESSION_ID};
pub use cache_notifications::*;
pub use context::*;
pub use delta_item_compare::{
    flush_item_compare_summary, is_item_compare_enabled, item_compare_summary,
    log_item_compare_summary, CompareFirstSetSummary, CompareOriginScope, HelperCompareCounts,
    HelperDictionaryCounts, ItemCompareByScope, ItemCompareEventSummary, ItemCompareScopeCounts,
    ItemCompareSummary,
};
pub use delta_item_switch::{
    projected_item_switch_summary, ProjectedItemSwitchConfig, ProjectedItemSwitchHandle,
    ProjectedItemSwitchSummary, SwitchTelemetryCounts,
};
#[cfg(any(test, feature = "testkit"))]
pub use test_context::TestContext;
pub use delta_projection::{
    materialize_pending_notifications, project_fit_attribute_diff, resolve_all_notifications,
    resolve_projected_recipients, DefaultDeltaProjection, DefaultGodmaFitHook, DeltaProjection,
    DeltaProjectionHook, DeltaProjectionInput, DeltaSink, DeltaStateCache, FitProjectionDiff,
    FitProjectionSnapshot, OnlineCharacterLookup, PlexDelta, ProjectedNotification,
    ProjectedTarget, ResolvedNotification,
};
pub use dispatch::box_fut;
pub use eve_proto::{
    ClientError, IskCents, UserErrorArg, UserErrorKey, UserErrorPresentation,
    UI_LABEL_SHIP_TOO_LARGE, V24_PROGRAMMATIC_USER_ERROR_KEYS,
};
pub use reply_registry::{into_client_reply, ToClient};
pub use rpc_args::{check_str_bounds, check_str_rep_max, RpcArgs};
pub use session_transition::*;
pub use teardown_registry::*;

/// Canonical MachoNet error code returned for unimplemented RPC methods.
pub const ERR_UNIMPLEMENTED: &str = "UnimplementedRPC";

#[cfg(test)]
mod binding_deep_extra_unit_tests;
#[cfg(test)]
mod binding_hooks_and_skills_extra_unit_tests;
#[cfg(test)]
mod cache_notifications_deep_extra_unit_tests;
#[cfg(test)]
mod delta_item_switch_tests;
#[cfg(test)]
mod delta_item_switch_e2e_tests;
#[cfg(test)]
mod delta_projection_tests;
#[cfg(test)]
mod dispatch_deep_extra_unit_tests;
#[cfg(test)]
mod notify_registry_tests;
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
