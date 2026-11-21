//! Test-only [`ServerContext`] wrapper (`#1540`).
//!
//! Strictly compiled under `#[cfg(any(test, feature = "testkit"))]` with zero static globals and
//! zero production exposure.

use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_sde::SdeRegistry;

use super::context::ServerContext;

/// Test-harness wrapper around [`ServerContext`] forwarding test-only controls (`#1540`).
#[derive(Clone)]
pub struct TestContext {
    server: ServerContext,
}

impl TestContext {
    /// Creates a new default test `ServerContext`.
    pub fn new() -> Self {
        Self { server: ServerContext::new() }
    }

    /// Creates a test `ServerContext` with explicit `SdeRegistry` and `WorldRepository`.
    pub fn with_sde_and_repository(sde: Arc<SdeRegistry>, db: Arc<WorldRepository>) -> Self {
        Self { server: ServerContext::with_sde_and_repository(sde, db) }
    }

    /// Wraps an existing [`ServerContext`] with test-only helpers.
    pub fn from_server(server: ServerContext) -> Self {
        Self { server }
    }

    /// Borrows the wrapped [`ServerContext`].
    pub fn server(&self) -> &ServerContext {
        &self.server
    }

    /// Consumes the wrapper and returns the inner [`ServerContext`].
    pub fn into_server(self) -> ServerContext {
        self.server
    }

    /// Disables per-session RPC rate-budget enforcement by calling
    /// [`ServerContext::disable_rpc_rate_budget_for_test`].
    pub fn disable_rpc_rate_budget(&self) {
        self.server.disable_rpc_rate_budget_for_test();
    }

    /// Builder helper calling [`ServerContext::disable_rpc_rate_budget_for_test`].
    pub fn with_rpc_rate_budget_disabled(self) -> Self {
        self.server.disable_rpc_rate_budget_for_test();
        self
    }

    /// Returns whether per-session RPC rate budgeting is disabled on the wrapped `ServerContext`.
    pub fn is_rpc_rate_budget_disabled(&self) -> bool {
        self.server.rpc_rate_budget_disabled.load(Ordering::Acquire)
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

impl From<ServerContext> for TestContext {
    fn from(server: ServerContext) -> Self {
        Self::from_server(server)
    }
}

impl Deref for TestContext {
    type Target = ServerContext;

    fn deref(&self) -> &Self::Target {
        &self.server
    }
}
