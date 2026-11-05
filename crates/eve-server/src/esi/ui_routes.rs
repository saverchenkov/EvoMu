//! `esi::ui_routes` — `ESI-4` routes for `User Interface` (`ESI-4`).

use crate::esi::sample_routes::RouteExecutionResult;
use crate::router::ServerContext;
use std::collections::HashMap;

pub fn dispatch_ui_route(
    _ctx: &ServerContext,
    _method: &str,
    _path_pattern: &str,
    _path_params: &HashMap<String, String>,
    _query_params: &HashMap<String, String>,
    _claims: Option<&eve_db::SsoTokenClaims>,
    _req_body: &[u8],
) -> Option<RouteExecutionResult> {
    None
}
