//! Test-only [`ServerContext`] wrapper (`#1540`).
//!
//! Strictly compiled under `#[cfg(any(test, feature = "testkit"))]` with zero static globals and
//! zero production exposure.

use std::ops::Deref;
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_sde::SdeRegistry;

use super::context::{RpcRateLimitPolicy, ServerContext};

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

    /// Creates a test `ServerContext` mirroring [`ServerContext::new`] with an explicit
    /// [`RpcRateLimitPolicy`].
    pub fn new_with_rpc_rate_limit_policy(policy: RpcRateLimitPolicy) -> Self {
        let (_env_read_guard, sde, db) = ServerContext::test_sde_and_db_for_context();
        let ctx = Self::with_sde_and_repository_rate_limit(sde, db, policy);
        #[cfg(any(test, feature = "demo-seed"))]
        let ctx = Self::from_server(
            ctx.into_server()
                .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
        );
        ctx
    }

    /// Creates a test `ServerContext` with per-session RPC rate budgeting configured as
    /// [`RpcRateLimitPolicy::Unlimited`].
    pub fn new_with_rpc_rate_budget_disabled() -> Self {
        Self::new_with_rpc_rate_limit_policy(RpcRateLimitPolicy::Unlimited)
    }

    /// Creates a test `ServerContext` with explicit `SdeRegistry` and `WorldRepository`.
    pub fn with_sde_and_repository(sde: Arc<SdeRegistry>, db: Arc<WorldRepository>) -> Self {
        Self { server: ServerContext::with_sde_and_repository(sde, db) }
    }

    /// Creates a test `ServerContext` with explicit `SdeRegistry`, `WorldRepository`, and
    /// [`RpcRateLimitPolicy`].
    pub fn with_sde_and_repository_rate_limit(
        sde: Arc<SdeRegistry>,
        db: Arc<WorldRepository>,
        policy: RpcRateLimitPolicy,
    ) -> Self {
        Self {
            server: ServerContext::with_sde_repository_descriptors_and_rate_limit(
                sde,
                db,
                eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty(),
                policy,
            ),
        }
    }

    /// Creates a test `ServerContext` with explicit `SdeRegistry`, `WorldRepository`, and
    /// [`RpcRateLimitPolicy::Unlimited`].
    pub fn with_sde_and_repository_rate_budget_disabled(
        sde: Arc<SdeRegistry>,
        db: Arc<WorldRepository>,
    ) -> Self {
        Self::with_sde_and_repository_rate_limit(sde, db, RpcRateLimitPolicy::Unlimited)
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

    /// Returns whether per-session RPC rate budgeting is disabled on the wrapped `ServerContext`.
    pub fn is_rpc_rate_budget_disabled(&self) -> bool {
        matches!(
            self.server.rpc_rate_limit_policy(),
            RpcRateLimitPolicy::Unlimited
        )
    }

    /// Builder helper overriding (or disabling with `None`) the teardown NDJSON census path.
    pub fn with_fixture_census_report_path(self, path: Option<std::path::PathBuf>) -> Self {
        self.server.fixture_census.set_report_path_override(path);
        self
    }

    /// Overrides (or disables with `None`) the teardown NDJSON census report path.
    pub fn set_fixture_census_report_path(&self, path: Option<std::path::PathBuf>) {
        self.server.fixture_census.set_report_path_override(path);
    }

    /// Returns a point-in-time snapshot of collected first-RPC fixture violations.
    pub fn fixture_census_snapshot(&self) -> super::fixture_census::FixtureCensusSnapshot {
        self.server.fixture_census.snapshot()
    }
}

/// Runs `f` inside staged dispatch notification + active acting-character scope and flushes staged
/// notifications, enabling IT coverage of raw multi-row DB commits under a real RPC scope.
pub async fn run_staged_dispatch_commit<F: FnOnce()>(
    ctx: &ServerContext,
    acting_char_id: i64,
    f: F,
) {
    super::dispatch_active_char::with_active_dispatch_char(Some(acting_char_id), async {
        super::cache_notifications::with_staged_dispatch_notifications(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            async {
                f();
                super::cache_notifications::flush_staged_dispatch_notifications(
                    &*ctx.pending_notifications,
                    &ctx.notify_tx,
                );
            },
        )
        .await;
    })
    .await;
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
