//! `esi::char_calendar_routes` — Unified `ESI-4` character calendar routes (`GET /calendar`,
//! `GET /calendar/{event_id}`, `PUT /calendar/{event_id}`, `GET /calendar/{event_id}/attendees`)
//! backed directly by `calendarMgr` (`social_extras` + persisted `WorldStateSnapshot::calendar_events`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use serde_json::{json, Value};
use std::collections::HashMap;

struct UnifiedCalendarEvent {
    event_id: i64,
    creator_id: i64,
    event_date_unix: u64,
    title: String,
    text: String,
    duration_minutes: i64,
    importance: i64,
    owner_id: i64,
    owner_name: String,
    owner_type: String,
    attendees: Vec<i64>,
}

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

fn load_calendar_events_from_game_state(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
) -> (HashMap<i64, UnifiedCalendarEvent>, HashMap<(i64, i64), String>) {
    let mut merged = snap.calendar_events.clone();
    if let Ok(s) = ctx.social_extras.lock() {
        for (&eid, ev) in &s.calendar_events {
            merged.insert(
                eid,
                eve_db::CalendarEventRecord {
                    event_id: ev.event_id,
                    owner_id: ev.owner_id,
                    creator_id: ev.creator_id,
                    event_date_time: ev.event_date_time,
                    duration: ev.duration,
                    importance: ev.importance,
                    title: ev.title.clone(),
                    description: ev.description.clone(),
                    scope: ev._scope.clone(),
                    responses: ev.responses.clone(),
                    is_deleted: ev.is_deleted,
                },
            );
        }
    }

    let mut events = HashMap::new();
    let mut responses = HashMap::new();
    for (&eid, ev) in &merged {
        if ev.is_deleted {
            continue;
        }
        let event_date_unix = if ev.event_date_time > 100_000_000_000_000_000 {
            ((ev.event_date_time / 10_000_000) - 11_644_473_600).max(0) as u64
        } else {
            ev.event_date_time.max(0) as u64
        };
        let owner_type = match ev.scope.as_str() {
            "Corporation" => "corporation",
            "Alliance" => "alliance",
            "CCP" => "eve_server",
            _ if (500_000..600_000).contains(&ev.owner_id)
                || ctx.sde.factions.contains_key(&(ev.owner_id as u32)) =>
            {
                "faction"
            }
            _ => "character",
        }
        .to_string();
        let owner_name = snap
            .characters
            .get(&ev.owner_id)
            .map(|c| c.name.clone())
            .or_else(|| snap.corporations.get(&ev.owner_id).map(|c| c.name.clone()))
            .or_else(|| snap.alliances.get(&ev.owner_id).map(|a| a.name.clone()))
            .or_else(|| {
                ctx.sde
                    .factions
                    .get(&(ev.owner_id as u32))
                    .map(|f| f.name.clone())
            })
            .or_else(|| {
                ctx.sde
                    .npc_corporations
                    .get(&ev.owner_id)
                    .map(|c| c.name.clone())
            })
            .unwrap_or_else(|| {
                if owner_type == "eve_server" || ev.owner_id == 1 {
                    "EVE System".to_string()
                } else {
                    format!("Owner {}", ev.owner_id)
                }
            });
        let mut attendees: Vec<i64> = ev.responses.keys().copied().collect();
        if ev.creator_id > 0 && !attendees.contains(&ev.creator_id) {
            attendees.push(ev.creator_id);
        }
        attendees.sort_unstable();
        events.insert(
            eid,
            UnifiedCalendarEvent {
                event_id: eid,
                creator_id: ev.creator_id,
                event_date_unix,
                title: ev.title.clone(),
                text: ev.description.clone(),
                duration_minutes: ev.duration,
                importance: ev.importance,
                owner_id: ev.owner_id,
                owner_name,
                owner_type,
                attendees,
            },
        );
        for (&cid, &status) in &ev.responses {
            let resp_str = match status {
                4 => "accepted",
                2 => "declined",
                5 => "tentative",
                _ => "not_responded",
            };
            responses.insert((cid, eid), resp_str.to_string());
        }
    }
    (events, responses)
}

fn is_character_invited(
    ev: &UnifiedCalendarEvent,
    char_id: i64,
    corp_id: i64,
    corp_alliance_id: Option<i64>,
) -> bool {
    ev.owner_id == char_id
        || ev.attendees.contains(&char_id)
        || (ev.owner_type == "corporation" && ev.owner_id == corp_id)
        || (ev.owner_type == "alliance" && corp_alliance_id == Some(ev.owner_id))
        || ev.owner_type == "eve_server"
}

fn can_view_event_attendees(
    snap: &eve_db::WorldStateSnapshot,
    ev: &UnifiedCalendarEvent,
    char_id: i64,
    corp_id: i64,
    corp_alliance_id: Option<i64>,
    now_unix: u64,
) -> bool {
    let officer_roles =
        eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CHAT_MANAGER;
    match ev.owner_type.as_str() {
        "eve_server" | "faction" => false,
        "corporation" => {
            ev.owner_id == corp_id
                && (ev.creator_id == char_id || {
                    let eff = super::router::effective_esi_corp_role_breakdown(
                        snap,
                        corp_id,
                        char_id,
                        now_unix,
                    );
                    eff.is_ceo || (eff.any_active_roles() & officer_roles) != 0
                })
        }
        "alliance" => {
            corp_alliance_id == Some(ev.owner_id)
                && (ev.creator_id == char_id || {
                    let eff = super::router::effective_esi_corp_role_breakdown(
                        snap,
                        corp_id,
                        char_id,
                        now_unix,
                    );
                    eff.is_ceo || (eff.any_active_roles() & officer_roles) != 0
                })
        }
        _ => ev.owner_id == char_id || ev.creator_id == char_id,
    }
}

pub fn dispatch_char_calendar_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    match (method, path) {
        ("GET", "/characters/{character_id}/calendar") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let from_event: Option<i64> = match query_params
                .get("from_event")
                .or_else(|| query_params.get("from_event_id"))
            {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(v) if v > 0 => Some(v),
                    _ => {
                        return Some(RouteExecutionResult::error(
                            400,
                            format!("Invalid from_event '{raw}'"),
                        ));
                    }
                },
                None => None,
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let (events_guard, resp_guard) = load_calendar_events_from_game_state(ctx, snap);
                let corp_alliance_id = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.alliance_id);
                let mut visible: Vec<_> = events_guard
                    .values()
                    .filter(|ev| {
                        is_character_invited(ev, char_id, ch.corporation_id, corp_alliance_id)
                            && from_event.is_none_or(|f| ev.event_id >= f)
                    })
                    .collect();
                visible.sort_by_key(|ev| ev.event_id);
                let out: Vec<Value> = visible
                    .into_iter()
                    .take(50)
                    .map(|ev| {
                        let resp = resp_guard
                            .get(&(char_id, ev.event_id))
                            .cloned()
                            .unwrap_or_else(|| "not_responded".to_string());
                        json!({
                            "event_id": ev.event_id,
                            "event_date": format_iso8601_utc(ev.event_date_unix),
                            "title": ev.title,
                            "importance": ev.importance,
                            "event_response": resp,
                        })
                    })
                    .collect();
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
                let (events_guard, resp_guard) = load_calendar_events_from_game_state(ctx, snap);
                let Some(ev) = events_guard.get(&event_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Calendar event {event_id} not found"),
                    );
                };
                let corp_alliance_id = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.alliance_id);
                if !is_character_invited(ev, char_id, ch.corporation_id, corp_alliance_id) {
                    if !ev.attendees.is_empty() {
                        return RouteExecutionResult::error(
                            403,
                            format!("Character {char_id} is not invited to calendar event {event_id}"),
                        );
                    }
                    return RouteExecutionResult::error(
                        404,
                        format!("Calendar event {event_id} not found for character {char_id}"),
                    );
                }
                let resp = resp_guard
                    .get(&(char_id, event_id))
                    .cloned()
                    .unwrap_or_else(|| "not_responded".to_string());
                RouteExecutionResult::ok_json(
                    json!({
                        "event_id": ev.event_id,
                        "owner_id": ev.owner_id,
                        "owner_name": ev.owner_name,
                        "date": format_iso8601_utc(ev.event_date_unix),
                        "title": ev.title,
                        "duration": ev.duration_minutes,
                        "importance": ev.importance,
                        "response": resp,
                        "text": ev.text,
                        "owner_type": ev.owner_type,
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
            let response_code = match resp_str {
                "accepted" => 4,
                "declined" => 2,
                "tentative" => 5,
                _ => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid calendar response '{resp_str}'"),
                    ));
                }
            };
            let Some((corp_id, corp_alliance_id)) = ctx.db.with_snapshot(|snap| {
                let ch = snap.characters.get(&char_id)?;
                let ally_id = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.alliance_id);
                Some((ch.corporation_id, ally_id))
            }) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            };
            let (event_exists, is_invited) = ctx.db.with_snapshot(|snap| {
                let (events_guard, _) = load_calendar_events_from_game_state(ctx, snap);
                if let Some(ev) = events_guard.get(&event_id) {
                    (
                        true,
                        is_character_invited(ev, char_id, corp_id, corp_alliance_id),
                    )
                } else {
                    (false, false)
                }
            });
            if !event_exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Calendar event {event_id} not found"),
                ));
            }
            if !is_invited {
                return Some(RouteExecutionResult::error(
                    403,
                    format!("Character {char_id} is not invited to calendar event {event_id}"),
                ));
            }
            let Some((owner_id, event_date_time, duration, title, importance)) =
                crate::services::fleet_social::update_calendar_response_from_esi(
                    ctx,
                    char_id,
                    event_id,
                    response_code,
                )
            else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Calendar event {event_id} not found"),
                ));
            };
            crate::services::mail_notify::emit_event_response_by_external(
                ctx,
                char_id,
                event_id,
                owner_id,
                event_date_time,
                duration,
                &title,
                importance,
                response_code,
            );
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
            let now = ctx.now_unix();
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let (events_guard, resp_guard) = load_calendar_events_from_game_state(ctx, snap);
                let Some(ev) = events_guard.get(&event_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Calendar event {event_id} not found"),
                    );
                };
                let corp_alliance_id = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.alliance_id);
                if !can_view_event_attendees(snap, ev, char_id, ch.corporation_id, corp_alliance_id, now) {
                    return RouteExecutionResult::error(
                        403,
                        format!(
                            "Character {char_id} is not authorized to view attendees for calendar event {event_id}"
                        ),
                    );
                }
                let mut attendees = ev.attendees.clone();
                attendees.sort_unstable();
                attendees.dedup();
                let out: Vec<Value> = attendees
                    .into_iter()
                    .map(|cid| {
                        let resp = resp_guard
                            .get(&(cid, event_id))
                            .cloned()
                            .unwrap_or_else(|| "not_responded".to_string());
                        json!({
                            "character_id": cid,
                            "event_response": resp,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "char_calendar_routes_unit_tests.rs"]
mod char_calendar_routes_unit_tests;

