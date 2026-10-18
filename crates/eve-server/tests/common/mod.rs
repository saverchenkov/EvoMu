//! Shared integration test utilities and common session helpers.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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
