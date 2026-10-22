//! Shared integration test utilities and common session helpers.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

/// Fixed virtual-clock base (Unix seconds) shared by time-dependent integration tests.
#[allow(dead_code)]
pub const TEST_CLOCK_BASE_UNIX: u64 = 1_800_000_000;

/// Pins `ctx` to a deterministic, non-drifting virtual clock (`T-7`) so assertions on
/// server-produced timestamps never race the host wall clock across a second boundary.
/// Time then only moves via `ctx.advance_secs` / explicit tick deltas.
#[allow(dead_code)]
pub fn pin_clock(ctx: &ServerContext) {
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        TEST_CLOCK_BASE_UNIX,
    )));
}

#[allow(dead_code)]
pub async fn login_pilot(
    ctx: &ServerContext,
    sess: &mut SessionState,
    username: &str,
    password: &str,
) {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String(username.into()),
            PyRep::String(password.into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
}
