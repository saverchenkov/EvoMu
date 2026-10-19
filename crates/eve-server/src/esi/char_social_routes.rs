//! `esi::char_social_routes` — Remaining `ESI-4` routes across `Character`, `Calendar`,
//! `Contacts`, and `Mail` labels/lists, plus legacy `Bookmarks` & `Opportunities` endpoints (`ESI-4`).

#![forbid(unsafe_code)]

use crate::esi::openapi::{MatchedOperation, OperationSpec, ResponseSpec, VERSION_PREFIXES};
use crate::esi::runtime_state::{EsiContactEntry, EsiMailLabel, EsiMailMeta};
use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

const VALID_MAIL_LABEL_COLORS: &[&str] = &[
    "#0000fe", "#006634", "#0099ff", "#00ff33", "#01ffff", "#349800", "#660066", "#666666",
    "#999999", "#99ffff", "#9a0000", "#ccff9a", "#e6e6e6", "#fe0000", "#ff6600", "#ffff01",
    "#ffffcd", "#ffffff",
];

fn parse_path_i64(
    path_params: &HashMap<String, String>,
    name: &str,
) -> Result<i64, RouteExecutionResult> {
    let raw = path_params.get(name).ok_or_else(|| {
        RouteExecutionResult::error(400, format!("Missing path parameter '{name}'"))
    })?;
    let val = raw.parse::<i64>().map_err(|_| {
        RouteExecutionResult::error(
            400,
            format!("Invalid integer path parameter '{name}': '{raw}'"),
        )
    })?;
    if val <= 0 {
        return Err(RouteExecutionResult::error(
            400,
            format!("Path parameter '{name}' must be a positive integer"),
        ));
    }
    Ok(val)
}

fn infer_contact_type(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, id: i64) -> &'static str {
    if (500_000..600_000).contains(&id) || ctx.sde.factions.contains_key(&(id as u32)) {
        "faction"
    } else if snap.alliances.contains_key(&id) || (99_000_000..100_000_000).contains(&id) {
        "alliance"
    } else if snap.corporations.contains_key(&id)
        || ctx.sde.npc_corporations.contains_key(&id)
        || (1_000_000..2_000_000).contains(&id)
        || (98_000_000..99_000_000).contains(&id)
    {
        "corporation"
    } else {
        "character"
    }
}

fn parse_csv_i64(raw: &str) -> Result<Vec<i64>, RouteExecutionResult> {
    let mut out = Vec::new();
    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Ok(out);
    }
    for part in trimmed.split(',') {
        let p = part.trim();
        let val = p.parse::<i64>().map_err(|_| {
            RouteExecutionResult::error(400, format!("Invalid integer in list parameter: '{p}'"))
        })?;
        if val <= 0 {
            return Err(RouteExecutionResult::error(
                400,
                "IDs in list parameter must be positive integers",
            ));
        }
        out.push(val);
    }
    Ok(out)
}

fn make_legacy_op(method: &str, path_pattern: &str, op_id: &str, scope: Option<&str>) -> OperationSpec {
    let segments = path_pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    let mut responses = HashMap::new();
    responses.insert(
        "200".to_string(),
        ResponseSpec {
            status_code: "200".to_string(),
            required_headers: vec!["Cache-Control".to_string(), "ETag".to_string()],
            json_schema: None,
        },
    );
    OperationSpec {
        method: method.to_string(),
        path_pattern: path_pattern.to_string(),
        segments,
        operation_id: op_id.to_string(),
        tags: vec!["Legacy".to_string()],
        required_scopes: scope.into_iter().map(ToString::to_string).collect(),
        parameters: Vec::new(),
        responses,
    }
}

static LEGACY_OPS: OnceLock<Vec<OperationSpec>> = OnceLock::new();

fn legacy_ops() -> &'static [OperationSpec] {
    LEGACY_OPS.get_or_init(|| {
        vec![
            make_legacy_op(
                "GET",
                "/characters/{character_id}/bookmarks",
                "GetCharactersCharacterIdBookmarks",
                Some("esi-bookmarks.read_character_bookmarks.v1"),
            ),
            make_legacy_op(
                "GET",
                "/characters/{character_id}/bookmarks/folders",
                "GetCharactersCharacterIdBookmarkFolders",
                Some("esi-bookmarks.read_character_bookmarks.v1"),
            ),
            make_legacy_op(
                "GET",
                "/corporations/{corporation_id}/bookmarks",
                "GetCorporationsCorporationIdBookmarks",
                Some("esi-bookmarks.read_corporation_bookmarks.v1"),
            ),
            make_legacy_op(
                "GET",
                "/corporations/{corporation_id}/bookmarks/folders",
                "GetCorporationsCorporationIdBookmarkFolders",
                Some("esi-bookmarks.read_corporation_bookmarks.v1"),
            ),
            make_legacy_op(
                "GET",
                "/corporations/{corporation_id}/projects",
                "GetCorporationsCorporationIdProjects",
                Some("esi-corporations.read_projects.v1"),
            ),
            make_legacy_op(
                "GET",
                "/opportunities/groups",
                "GetOpportunitiesGroups",
                None,
            ),
            make_legacy_op(
                "GET",
                "/opportunities/tasks",
                "GetOpportunitiesTasks",
                None,
            ),
            make_legacy_op(
                "GET",
                "/characters/{character_id}/opportunities",
                "GetCharactersCharacterIdOpportunities",
                Some("esi-characters.read_opportunities.v1"),
            ),
        ]
    })
}

pub fn try_match_legacy_operation(method: &str, raw_path: &str) -> Option<MatchedOperation<'static>> {
    let mut stripped = raw_path.trim_end_matches('/');
    if stripped.is_empty() {
        stripped = "/";
    }
    let mut version_prefix = None;
    for &vp in VERSION_PREFIXES {
        if let Some(rest) = stripped.strip_prefix(vp) {
            if rest.is_empty() || rest.starts_with('/') {
                version_prefix = Some(vp.to_string());
                stripped = if rest.is_empty() { "/" } else { rest };
                break;
            }
        }
    }
    let req_segs: Vec<&str> = stripped
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let method_uc = method.to_ascii_uppercase();
    for op in legacy_ops() {
        if op.method != method_uc || op.segments.len() != req_segs.len() {
            continue;
        }
        let mut params = HashMap::new();
        let mut ok = true;
        for (pat, actual) in op.segments.iter().zip(req_segs.iter()) {
            if let Some(pname) = pat.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                params.insert(pname.to_string(), (*actual).to_string());
            } else if pat != actual {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(MatchedOperation {
                operation: op,
                path_params: params,
                version_prefix,
            });
        }
    }
    None
}

pub fn dispatch_char_social_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();

    match (method, path) {
        // 1. Character Public & Extended Info
        ("GET", "/characters/{character_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut obj = json!({
                    "name": ch.name,
                    "corporation_id": ch.corporation_id,
                    "birthday": "2024-01-01T00:00:00Z",
                    "bloodline_id": ch.bloodline_id as i64,
                    "race_id": ch.race_id as i64,
                    "gender": "male",
                    "security_status": ch.security_status,
                    "description": format!("Capsuleer {}", ch.name),
                });
                if let Some(aid) = ch.alliance_id {
                    obj.as_object_mut()
                        .unwrap()
                        .insert("alliance_id".to_string(), json!(aid));
                }
                if let Some(fid) = ch.war_faction_id {
                    obj.as_object_mut()
                        .unwrap()
                        .insert("faction_id".to_string(), json!(fid));
                }
                RouteExecutionResult::ok_json(obj, 86400)
            }))
        }

        ("GET", "/characters/{character_id}/agents_research") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let out = vec![json!({
                    "agent_id": 3_008_416,
                    "skill_type_id": 11_446,
                    "started_at": format_iso8601_utc(now.saturating_sub(86400)),
                    "points_per_day": 52.5,
                    "remainder_points": 105.0,
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/characters/{character_id}/corporationhistory") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let out = vec![json!({
                    "corporation_id": ch.corporation_id,
                    "record_id": 1,
                    "start_date": "2024-01-01T00:00:00Z",
                })];
                RouteExecutionResult::ok_json(json!(out), 86400)
            }))
        }

        ("POST", "/characters/{character_id}/cspa") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let targets: Vec<i64> = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid CSPA character ID array: {e}"),
                    ));
                }
            };
            if targets.is_empty() || targets.len() > 100 {
                return Some(RouteExecutionResult::error(
                    400,
                    "CSPA character list must contain 1..=100 IDs",
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                let Some(caller) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut total_charge = 0.0_f64;
                for tid in &targets {
                    if *tid == char_id {
                        continue;
                    }
                    let same_corp = snap
                        .characters
                        .get(tid)
                        .map(|t| t.corporation_id == caller.corporation_id)
                        .unwrap_or(false);
                    if !same_corp {
                        total_charge += 2950.0;
                    }
                }
                RouteExecutionResult::created_json(json!(total_charge))
            }))
        }

        ("GET", "/characters/{character_id}/medals") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let out = vec![json!({
                    "medal_id": 1,
                    "title": "Distinguished Capsuleer Service Medal",
                    "description": "Awarded for exemplary service in New Eden.",
                    "corporation_id": ch.corporation_id,
                    "issuer_id": ch.character_id,
                    "date": "2025-01-01T00:00:00Z",
                    "reason": "Distinguished service",
                    "status": "public",
                    "graphics": [
                        {
                            "part": 1,
                            "layer": 0,
                            "graphic": "caldari.1_1",
                            "color": 1,
                        }
                    ]
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/characters/{character_id}/notifications") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let out = vec![json!({
                    "notification_id": 1,
                    "type": "AllWarDeclaredMsg",
                    "sender_id": ch.corporation_id,
                    "sender_type": "corporation",
                    "timestamp": format_iso8601_utc(now),
                    "is_read": false,
                    "text": format!("againstID: {}", ch.corporation_id),
                })];
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        ("GET", "/characters/{character_id}/notifications/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let contacts = ctx
                    .esi_state
                    .char_contacts
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = contacts
                    .iter()
                    .enumerate()
                    .map(|(idx, c)| {
                        json!({
                            "notification_id": (idx as i64) + 1,
                            "send_date": format_iso8601_utc(now),
                            "standing_level": c.standing,
                            "message": "Added to contacts",
                            "sender_character_id": char_id,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        ("GET", "/characters/{character_id}/roles") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let is_ceo = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .map(|c| c.ceo_id == char_id)
                    .unwrap_or(false);
                let roles: Vec<&str> = if is_ceo {
                    vec![
                        "Director",
                        "Accountant",
                        "Communications_Officer",
                        "Config_Equipment",
                        "Config_Starbase_Equipment",
                        "Contract_Manager",
                        "Diplomat",
                        "Factory_Manager",
                        "Fitting_Manager",
                        "Personnel_Manager",
                        "Project_Manager",
                        "Security_Officer",
                        "Starbase_Defense_Operator",
                        "Starbase_Fuel_Technician",
                        "Station_Manager",
                        "Trader",
                    ]
                } else {
                    Vec::new()
                };
                RouteExecutionResult::ok_json(
                    json!({
                        "roles": roles,
                        "roles_at_hq": roles,
                        "roles_at_base": roles,
                        "roles_at_other": roles,
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/characters/{character_id}/titles") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let is_ceo = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .map(|c| c.ceo_id == char_id)
                    .unwrap_or(false);
                let out = if is_ceo {
                    vec![json!({
                        "title_id": 1,
                        "name": "Chief Executive Officer",
                    })]
                } else {
                    vec![json!({
                        "title_id": 1,
                        "name": "Capsuleer Member",
                    })]
                };
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 2. Calendar (ESI-4)
        ("GET", "/characters/{character_id}/calendar") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let resp = ctx
                    .esi_state
                    .calendar_responses
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(char_id, 1))
                    .cloned()
                    .unwrap_or_else(|| "not_responded".to_string());
                let out = vec![json!({
                    "event_id": 1,
                    "event_date": format_iso8601_utc(now + 3600),
                    "title": "Corporation Fleet Operations Briefing",
                    "importance": 1,
                    "event_response": resp,
                })];
                RouteExecutionResult::ok_json(json!(out), 5)
            }))
        }

        ("GET", "/characters/{character_id}/calendar/{event_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let event_id = match parse_path_i64(path_params, "event_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let resp = ctx
                    .esi_state
                    .calendar_responses
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(char_id, event_id))
                    .cloned()
                    .unwrap_or_else(|| "not_responded".to_string());
                RouteExecutionResult::ok_json(
                    json!({
                        "event_id": event_id,
                        "owner_id": ch.corporation_id,
                        "owner_name": snap.corporations.get(&ch.corporation_id).map(|c| c.name.as_str()).unwrap_or("CONCORD"),
                        "date": format_iso8601_utc(now + 3600),
                        "title": "Corporation Fleet Operations Briefing",
                        "duration": 60,
                        "importance": 1,
                        "response": resp,
                        "text": "Scheduled fleet assembly and strategic briefing.",
                        "owner_type": "corporation",
                    }),
                    5,
                )
            }))
        }

        ("PUT", "/characters/{character_id}/calendar/{event_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let event_id = match parse_path_i64(path_params, "event_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid calendar response JSON: {e}"),
                    ));
                }
            };
            let Some(resp_str) = payload.get("response").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing calendar 'response' field",
                ));
            };
            if !matches!(resp_str, "accepted" | "declined" | "tentative") {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid calendar response '{resp_str}'"),
                ));
            }
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            ctx.esi_state
                .calendar_responses
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert((char_id, event_id), resp_str.to_string());
            Some(RouteExecutionResult::no_content(0))
        }

        ("GET", "/characters/{character_id}/calendar/{event_id}/attendees") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let event_id = match parse_path_i64(path_params, "event_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let resp = ctx
                    .esi_state
                    .calendar_responses
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(char_id, event_id))
                    .cloned()
                    .unwrap_or_else(|| "not_responded".to_string());
                let out = vec![json!({
                    "character_id": char_id,
                    "event_response": resp,
                })];
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        // 3. Contacts (ESI-4)
        ("GET", "/characters/{character_id}/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let list = ctx
                    .esi_state
                    .char_contacts
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = list
                    .into_iter()
                    .map(|c| {
                        let mut obj = json!({
                            "contact_id": c.contact_id,
                            "contact_type": c.contact_type,
                            "standing": c.standing,
                            "is_watched": c.is_watched,
                            "is_blocked": c.is_blocked,
                        });
                        if !c.label_ids.is_empty() {
                            obj.as_object_mut()
                                .unwrap()
                                .insert("label_ids".to_string(), json!(c.label_ids));
                        }
                        obj
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("POST", "/characters/{character_id}/contacts")
        | ("PUT", "/characters/{character_id}/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(raw_standing) = query_params.get("standing") else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required query parameter 'standing'",
                ));
            };
            let Ok(standing) = raw_standing.parse::<f64>() else {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid standing parameter '{raw_standing}'"),
                ));
            };
            if !standing.is_finite() || !(-10.0..=10.0).contains(&standing) {
                return Some(RouteExecutionResult::error(
                    400,
                    "Contact standing must be between -10.0 and 10.0",
                ));
            }
            let watched = query_params
                .get("watched")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            let label_ids = match query_params.get("label_ids") {
                Some(raw) => match parse_csv_i64(raw) {
                    Ok(v) => v,
                    Err(e) => return Some(e),
                },
                None => Vec::new(),
            };
            let contact_ids: Vec<i64> = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid contact IDs JSON array: {e}"),
                    ));
                }
            };
            if contact_ids.is_empty() || contact_ids.len() > 100 || contact_ids.iter().any(|&id| id <= 0)
            {
                return Some(RouteExecutionResult::error(
                    400,
                    "Contact IDs array must contain 1..=100 positive integers",
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut map = ctx
                    .esi_state
                    .char_contacts
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let list = map.entry(char_id).or_default();
                for &cid in &contact_ids {
                    let ctype = infer_contact_type(ctx, snap, cid).to_string();
                    if let Some(existing) = list.iter_mut().find(|e| e.contact_id == cid) {
                        existing.standing = standing;
                        existing.is_watched = watched;
                        existing.label_ids = label_ids.clone();
                    } else {
                        list.push(EsiContactEntry {
                            contact_id: cid,
                            contact_type: ctype,
                            standing,
                            is_watched: watched,
                            is_blocked: standing < -5.0,
                            label_ids: label_ids.clone(),
                        });
                    }
                }
                if method == "POST" {
                    RouteExecutionResult::created_json(json!(contact_ids))
                } else {
                    RouteExecutionResult::no_content(0)
                }
            }))
        }

        ("DELETE", "/characters/{character_id}/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(raw_ids) = query_params.get("contact_ids") else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required query parameter 'contact_ids'",
                ));
            };
            let ids = match parse_csv_i64(raw_ids) {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            if ids.is_empty() || ids.len() > 20 {
                return Some(RouteExecutionResult::error(
                    400,
                    "contact_ids must contain 1..=20 IDs",
                ));
            }
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            let mut map = ctx
                .esi_state
                .char_contacts
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(list) = map.get_mut(&char_id) {
                list.retain(|c| !ids.contains(&c.contact_id));
            }
            Some(RouteExecutionResult::no_content(0))
        }

        ("GET", "/characters/{character_id}/contacts/labels") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let out = vec![
                    json!({ "label_id": 1, "label_name": "Fleet Allies" }),
                    json!({ "label_id": 2, "label_name": "Trade Partners" }),
                ];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/corporations/{corporation_id}/contacts") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let out = vec![json!({
                    "contact_id": corp.ceo_id,
                    "contact_type": "character",
                    "standing": 10.0,
                    "is_watched": true,
                    "label_ids": [1],
                })];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/corporations/{corporation_id}/contacts/labels") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let out = vec![json!({
                    "label_id": 1,
                    "label_name": "Diplomatic Blues",
                })];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/alliances/{alliance_id}/contacts") => {
            let alliance_id = match parse_path_i64(path_params, "alliance_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(alliance) = snap.alliances.get(&alliance_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Alliance {alliance_id} not found"),
                    );
                };
                let out = vec![json!({
                    "contact_id": alliance.executor_corp_id,
                    "contact_type": "corporation",
                    "standing": 10.0,
                    "label_ids": [1],
                })];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/alliances/{alliance_id}/contacts/labels") => {
            let alliance_id = match parse_path_i64(path_params, "alliance_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.alliances.contains_key(&alliance_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Alliance {alliance_id} not found"),
                    );
                };
                let out = vec![json!({
                    "label_id": 1,
                    "label_name": "Coalition Allies",
                })];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        // 4. Mail Update, Labels, and Mailing Lists (ESI-4)
        ("PUT", "/characters/{character_id}/mail/{mail_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let mail_id = match parse_path_i64(path_params, "mail_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid mail update JSON body: {e}"),
                    ));
                }
            };
            let new_read = payload.get("read").and_then(Value::as_bool);
            let new_labels = if let Some(arr) = payload.get("labels").and_then(Value::as_array) {
                if arr.len() > 25 {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Mail 'labels' cannot exceed 25 entries",
                    ));
                }
                let mut lvec = Vec::with_capacity(arr.len());
                for item in arr {
                    let Some(lid) = item.as_i64() else {
                        return Some(RouteExecutionResult::error(
                            400,
                            "Mail label ID must be an integer",
                        ));
                    };
                    if lid <= 0 {
                        return Some(RouteExecutionResult::error(
                            400,
                            "Mail label ID must be > 0",
                        ));
                    }
                    if !lvec.contains(&lid) {
                        lvec.push(lid);
                    }
                }
                Some(lvec)
            } else {
                None
            };
            if new_read.is_none() && new_labels.is_none() {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail update body must specify 'read' and/or 'labels'",
                ));
            }
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                if snap.deleted_mails.contains(&(char_id, mail_id))
                    || ctx.esi_state.is_mail_deleted(char_id, mail_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    );
                }
                let Some(first) = snap.mails.iter().find(|m| {
                    m.message_id == mail_id && (m.recipient_id == char_id || m.sender_id == char_id)
                }) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    );
                };
                let mut meta =
                    ctx.esi_state
                        .get_mail_meta(char_id, mail_id, first.sender_id == char_id);
                if let Some(r) = new_read {
                    meta.is_read = r;
                }
                if let Some(lbls) = new_labels {
                    meta.labels = lbls;
                }
                ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
                RouteExecutionResult::no_content(0)
            }))
        }

        ("GET", "/characters/{character_id}/mail/labels") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let labels = {
                    let mut map = ctx
                        .esi_state
                        .mail_labels
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    map.entry(char_id)
                        .or_insert_with(crate::esi::runtime_state::EsiRuntimeState::default_mail_labels)
                        .clone()
                };
                let mut msg_ids: Vec<i64> = snap
                    .mails
                    .iter()
                    .filter(|m| {
                        (m.recipient_id == char_id || m.sender_id == char_id)
                            && !snap.deleted_mails.contains(&(char_id, m.message_id))
                            && !ctx.esi_state.is_mail_deleted(char_id, m.message_id)
                    })
                    .map(|m| m.message_id)
                    .collect();
                msg_ids.sort_unstable();
                msg_ids.dedup();
                let mut total_unread = 0_i64;
                let mut unread_by_label: HashMap<i64, i64> = HashMap::new();
                for mid in msg_ids {
                    let is_sender = snap
                        .mails
                        .iter()
                        .find(|m| m.message_id == mid)
                        .map(|m| m.sender_id == char_id)
                        .unwrap_or(false);
                    let meta: EsiMailMeta = ctx.esi_state.get_mail_meta(char_id, mid, is_sender);
                    if !meta.is_read {
                        total_unread += 1;
                        for lid in meta.labels {
                            *unread_by_label.entry(lid).or_insert(0) += 1;
                        }
                    }
                }
                let out_labels: Vec<Value> = labels
                    .into_iter()
                    .map(|l| {
                        json!({
                            "label_id": l.label_id,
                            "name": l.name,
                            "color": l.color,
                            "unread_count": unread_by_label.get(&l.label_id).copied().unwrap_or(0),
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "total_unread_count": total_unread,
                        "labels": out_labels,
                    }),
                    30,
                )
            }))
        }

        ("POST", "/characters/{character_id}/mail/labels") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid mail label JSON body: {e}"),
                    ));
                }
            };
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing label 'name'"));
            };
            let trimmed = name.trim();
            if trimmed.is_empty() || trimmed.len() > 40 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail label 'name' must be 1..=40 characters",
                ));
            }
            let color = payload
                .get("color")
                .and_then(Value::as_str)
                .unwrap_or("#ffffff");
            if !VALID_MAIL_LABEL_COLORS.contains(&color) {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid mail label color '{color}'"),
                ));
            }
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            let label_id = ctx.esi_state.alloc_id();
            let mut map = ctx
                .esi_state
                .mail_labels
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let list = map
                .entry(char_id)
                .or_insert_with(crate::esi::runtime_state::EsiRuntimeState::default_mail_labels);
            list.push(EsiMailLabel {
                label_id,
                name: trimmed.to_string(),
                color: color.to_string(),
            });
            Some(RouteExecutionResult::created_json(json!(label_id)))
        }

        ("DELETE", "/characters/{character_id}/mail/labels/{label_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let label_id = match parse_path_i64(path_params, "label_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            if matches!(label_id, 1 | 2 | 4 | 8) {
                return Some(RouteExecutionResult::error(
                    422,
                    "Default system mail labels cannot be deleted",
                ));
            }
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            let mut map = ctx
                .esi_state
                .mail_labels
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let list = map
                .entry(char_id)
                .or_insert_with(crate::esi::runtime_state::EsiRuntimeState::default_mail_labels);
            let before = list.len();
            list.retain(|l| l.label_id != label_id);
            if list.len() == before {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Mail label {label_id} not found"),
                ));
            }
            Some(RouteExecutionResult::no_content(0))
        }

        ("GET", "/characters/{character_id}/mail/lists") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let out = vec![json!({
                    "mailing_list_id": 145_000_000 + ch.corporation_id,
                    "name": "Corporation Announcements",
                })];
                RouteExecutionResult::ok_json(json!(out), 120)
            }))
        }

        // 5. Legacy / Extended Feature Routes (Bookmarks, Corp Projects, Opportunities)
        ("GET", "/characters/{character_id}/bookmarks") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut bms: Vec<_> = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
                    .collect();
                bms.sort_by_key(|b| b.bookmark_id);
                let out: Vec<Value> = bms
                    .into_iter()
                    .map(|b| {
                        json!({
                            "bookmark_id": b.bookmark_id,
                            "creator_id": b.owner_id,
                            "created": format_iso8601_utc(now),
                            "label": b.label,
                            "notes": "",
                            "location_id": b.solar_system_id as i64,
                            "coordinates": { "x": b.x, "y": b.y, "z": b.z },
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/characters/{character_id}/bookmarks/folders") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!([{ "folder_id": 1, "name": "Personal Locations" }]),
                    3600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/bookmarks") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let mut bms: Vec<_> = snap
                    .bookmarks
                    .values()
                    .filter(|b| {
                        b.owner_id == corp_id
                            || snap
                                .characters
                                .get(&b.owner_id)
                                .map(|c| c.corporation_id == corp_id)
                                .unwrap_or(false)
                    })
                    .collect();
                bms.sort_by_key(|b| b.bookmark_id);
                let out: Vec<Value> = bms
                    .into_iter()
                    .map(|b| {
                        json!({
                            "bookmark_id": b.bookmark_id,
                            "creator_id": b.owner_id,
                            "created": format_iso8601_utc(now),
                            "label": b.label,
                            "notes": "",
                            "location_id": b.solar_system_id as i64,
                            "coordinates": { "x": b.x, "y": b.y, "z": b.z },
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/bookmarks/folders") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!([{ "folder_id": 1, "name": "Corporation Tactical Bookmarks", "creator_id": corp_id }]),
                    3600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/projects") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let mut projs: Vec<_> = snap
                    .corp_projects
                    .values()
                    .filter(|p| p.corporation_id == corp_id)
                    .collect();
                projs.sort_by_key(|p| p.project_id);
                let out: Vec<Value> = projs
                    .into_iter()
                    .map(|p| {
                        json!({
                            "project_id": p.project_id,
                            "corporation_id": p.corporation_id,
                            "name": p.name,
                            "target_quantity": p.target_quantity,
                            "current_quantity": p.current_quantity,
                            "isk_per_unit": p.isk_per_unit.to_wire_f64(),
                            "escrow_remaining": p.escrow_remaining.to_wire_f64(),
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 60)
            }))
        }

        ("GET", "/opportunities/groups") => {
            let mut ids: Vec<i64> = ctx.sde.career_goals.keys().map(|&id| id as i64).collect();
            if ids.is_empty() {
                ids.push(100);
            }
            ids.sort_unstable();
            Some(RouteExecutionResult::ok_json(json!(ids), 3600))
        }

        ("GET", "/opportunities/tasks") => {
            let mut ids: Vec<i64> = ctx.sde.node_graphs.keys().map(|&id| id as i64).collect();
            if ids.is_empty() {
                ids.push(1);
            }
            ids.sort_unstable();
            Some(RouteExecutionResult::ok_json(json!(ids), 3600))
        }

        ("GET", "/characters/{character_id}/opportunities") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut out = Vec::new();
                if let Some(npe) = snap.npe_progress.get(&char_id) {
                    for &gid in &npe.claimed_goals {
                        out.push(json!({
                            "task_id": gid as i64,
                            "completed_at": format_iso8601_utc(now),
                        }));
                    }
                }
                if out.is_empty() {
                    out.push(json!({
                        "task_id": 1,
                        "completed_at": format_iso8601_utc(now),
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        _ => None,
    }
}
