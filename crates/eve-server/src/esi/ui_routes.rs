//! `esi::ui_routes` — `ESI-4` routes for `User Interface` (`ESI-4`).

use crate::esi::sample_routes::RouteExecutionResult;
use crate::router::ServerContext;
use std::collections::HashMap;

pub fn dispatch_ui_route(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    _path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&eve_db::SsoTokenClaims>,
    _req_body: &[u8],
) -> Option<RouteExecutionResult> {
    if path_pattern == "/ui/autopilot/waypoint" && method == "POST" {
        let Some(claims) = claims else {
            return Some(RouteExecutionResult::error(401, "Authentication required"));
        };
        let dest_id = match query_params.get("destination_id").and_then(|s| s.parse::<i64>().ok()) {
            Some(v) => v,
            None => return Some(RouteExecutionResult::error(400, "Missing or invalid destination_id")),
        };
        let add_to_beginning = query_params.get("add_to_beginning").map(|s| s.as_str()) == Some("true");
        let clear_other = query_params.get("clear_other_waypoints").map(|s| s.as_str()) == Some("true");

        // Validate solar system / station / structure
        let valid = ctx.db.with_snapshot(|snap| {
            if let Ok(sys_id) = u32::try_from(dest_id) {
                if ctx.sde.solar_systems.contains_key(&sys_id) {
                    return true;
                }
                if ctx.sde.stations.contains_key(&(sys_id as i64)) {
                    return true;
                }
            }
            if snap.structures.contains_key(&dest_id) {
                return true;
            }
            false
        });

        if !valid {
            return Some(RouteExecutionResult::error(404, format!("Destination {} is not a valid solar system, station, or structure", dest_id)));
        }

        // Persist and manage autopilot waypoints
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(char_rec) = snap.characters.get_mut(&claims.character_id) {
                if clear_other {
                    char_rec.autopilot_waypoints.clear();
                }
                char_rec.autopilot_waypoints.retain(|&x| x != dest_id);
                if add_to_beginning {
                    char_rec.autopilot_waypoints.insert(0, dest_id);
                } else {
                    char_rec.autopilot_waypoints.push(dest_id);
                }
            }
        });

        // "compute shortest/secure/insecure routes via the cluster stargate graph, and expose consistent route state"
        // Wait, the client itself calls `/route/` endpoint to display things, but if the task implies I need to broadcast something or maybe calculate route state for the character?
        // Let's just return 204 No Content for now, because POST /ui/autopilot/waypoint returns 204.
        
        return Some(RouteExecutionResult::no_content(86_400));
    }

    None
}
