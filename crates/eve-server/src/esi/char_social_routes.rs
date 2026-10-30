//! `esi::char_social_routes` — Remaining `ESI-4` routes across `Character`, `Calendar`,
//! `Contacts`, and `Mail` labels/lists, plus legacy `Bookmarks` & `Opportunities` endpoints (`ESI-4`).

#![forbid(unsafe_code)]

use crate::esi::openapi::{MatchedOperation, OperationSpec, ResponseSpec, VERSION_PREFIXES};
use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::IskCents;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

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

pub(crate) fn infer_contact_type(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, id: i64) -> &'static str {
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

fn map_notification_type_id(type_id: i32) -> &'static str {
    match type_id {
        5 => "AllWarDeclaredMsg",
        10 => "BountyClaimMsg",
        14 => "CorpAppNewMsg",
        16 => "CorpAppAcceptMsg",
        34 => "InsuranceFirstShipMsg",
        45 => "AllAnchoringMsg",
        138 => "CloneActivationMsg2",
        184 => "StructureUnderAttack",
        _ => "AllWarDeclaredMsg",
    }
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
        required_roles: Vec::new(),
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

#[allow(clippy::too_many_lines)]
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
                let gender = if ch.gender == 0 { "female" } else { "male" };
                let mut obj = json!({
                    "name": ch.name,
                    "corporation_id": ch.corporation_id,
                    "birthday": format_iso8601_utc(ch.create_date_unix),
                    "bloodline_id": ch.bloodline_id as i64,
                    "race_id": ch.race_id as i64,
                    "gender": gender,
                    "security_status": ch.security_status,
                    "description": ch.description,
                });
                if !ch.title.is_empty() {
                    obj["title"] = json!(ch.title);
                }
                if let Some(aid) = ch.alliance_id {
                    obj["alliance_id"] = json!(aid);
                }
                if let Some(fid) = ch.war_faction_id.or_else(|| {
                    ctx.sde
                        .npc_corporations
                        .get(&ch.corporation_id)
                        .and_then(|c| c.faction_id)
                        .map(|f| f as i64)
                }) {
                    obj["faction_id"] = json!(fid);
                }
                RouteExecutionResult::ok_json(obj, 86400)
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
                let mut history = ctx
                    .esi_state
                    .char_corp_history
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                if history.is_empty() {
                    if let Some(emp) = snap.employment_history.get(&char_id) {
                        for (idx, &(corp_id, start_unix, _)) in emp.iter().enumerate() {
                            history.push(crate::esi::runtime_state::EsiCorpHistoryEntry {
                                record_id: (idx + 1) as i64,
                                corporation_id: corp_id,
                                start_date_unix: start_unix,
                                is_deleted: false,
                            });
                        }
                    }
                }
                if history.is_empty() && ch.create_date_unix > 0 {
                    history.push(crate::esi::runtime_state::EsiCorpHistoryEntry {
                        record_id: 1,
                        corporation_id: ch.corporation_id,
                        start_date_unix: ch.create_date_unix,
                        is_deleted: false,
                    });
                }
                history.sort_by(|a, b| b.record_id.cmp(&a.record_id));
                let out: Vec<Value> = history
                    .into_iter()
                    .map(|h| {
                        let mut entry = json!({
                            "corporation_id": h.corporation_id,
                            "record_id": h.record_id,
                            "start_date": format_iso8601_utc(h.start_date_unix),
                        });
                        if h.is_deleted {
                            entry["is_deleted"] = json!(true);
                        }
                        entry
                    })
                    .collect();
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
                let contacts_guard = ctx
                    .esi_state
                    .char_contacts
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let mut total_cents = 0i64;
                for tid in &targets {
                    if *tid == char_id {
                        continue;
                    }
                    let same_corp = snap
                        .characters
                        .get(tid)
                        .is_some_and(|t| t.corporation_id == caller.corporation_id);
                    let is_exempt_contact = contacts_guard.get(tid).is_some_and(|list| {
                        list.iter()
                            .any(|c| c.contact_id == char_id && !c.is_blocked && c.standing > -10.0)
                    });
                    if !same_corp && !is_exempt_contact {
                        let cspa = snap
                            .character_cspa_charges
                            .get(tid)
                            .copied()
                            .unwrap_or(IskCents(0));
                        total_cents = total_cents.saturating_add(cspa.0.max(0));
                    }
                }
                RouteExecutionResult::created_json(json!(IskCents(total_cents).to_wire_f64()))
            }))
        }

        ("GET", "/characters/{character_id}/notifications") => {
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
                let mut recs: Vec<_> = ctx
                    .notification_records
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter(|r| r.receiver_id == char_id && !r.deleted)
                    .cloned()
                    .collect();
                recs.sort_by(|a, b| b.notification_id.cmp(&a.notification_id));
                let out: Vec<Value> = recs
                    .into_iter()
                    .map(|r| {
                        let unix_secs =
                            ((r.created / 10_000_000) - 11_644_473_600).max(0) as u64;
                        let sender_type = infer_contact_type(ctx, snap, r.sender_id);
                        json!({
                            "notification_id": r.notification_id,
                            "type": map_notification_type_id(r.type_id),
                            "sender_id": r.sender_id,
                            "sender_type": sender_type,
                            "timestamp": format_iso8601_utc(unix_secs),
                            "is_read": r.processed,
                            "text": r.data,
                        })
                    })
                    .collect();
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
                let mut notifs = ctx
                    .esi_state
                    .contact_notifications
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                notifs.sort_by(|a, b| b.notification_id.cmp(&a.notification_id));
                let out: Vec<Value> = notifs
                    .into_iter()
                    .map(|n| {
                        json!({
                            "notification_id": n.notification_id,
                            "send_date": format_iso8601_utc(n.send_date_unix),
                            "standing_level": n.standing_level,
                            "message": n.message,
                            "sender_character_id": n.sender_character_id,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 600)
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
                let mut rows = snap
                    .char_research_agents
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                rows.sort_by_key(|r| r.agent_id);
                let out: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        json!({
                            "agent_id": r.agent_id,
                            "skill_type_id": r.skill_type_id as i64,
                            "started_at": format_iso8601_utc(r.started_at_unix),
                            "points_per_day": r.points_per_day,
                            "remainder_points": r.remainder_points,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/characters/{character_id}/medals") => {
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
                let mut rows = snap
                    .char_medals
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                rows.sort_by_key(|m| m.medal_id);
                let out: Vec<Value> = rows
                    .into_iter()
                    .map(|m| {
                        let graphics: Vec<Value> = m
                            .graphics
                            .into_iter()
                            .map(|g| {
                                json!({
                                    "part": g.part,
                                    "layer": g.layer,
                                    "graphic": g.graphic,
                                    "color": g.color,
                                })
                            })
                            .collect();
                        json!({
                            "medal_id": m.medal_id,
                            "title": m.title,
                            "description": m.description,
                            "corporation_id": m.corporation_id,
                            "issuer_id": m.issuer_id,
                            "date": format_iso8601_utc(m.date_unix),
                            "reason": m.reason,
                            "status": m.status,
                            "graphics": graphics,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
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
                if let Some(scoped) = snap.char_scoped_roles.get(&char_id) {
                    return RouteExecutionResult::ok_json(
                        json!({
                            "roles": scoped.roles,
                            "roles_at_hq": scoped.roles_at_hq,
                            "roles_at_base": scoped.roles_at_base,
                            "roles_at_other": scoped.roles_at_other,
                        }),
                        3600,
                    );
                }
                let role_mask = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.member_roles.get(&char_id).copied())
                    .unwrap_or(0);
                let role_table: &[(u64, &str)] = &[
                    (eve_db::corp_roles::ROLE_DIRECTOR, "Director"),
                    (eve_db::corp_roles::ROLE_ACCOUNTANT, "Accountant"),
                    (eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT, "Junior_Accountant"),
                    (eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT, "Config_Equipment"),
                    (eve_db::corp_roles::ROLE_DIPLOMAT, "Diplomat"),
                    (eve_db::corp_roles::ROLE_FACTORY_MANAGER, "Factory_Manager"),
                    (eve_db::corp_roles::ROLE_PERSONNEL_MANAGER, "Personnel_Manager"),
                    (eve_db::corp_roles::ROLE_SECURITY_OFFICER, "Security_Officer"),
                    (eve_db::corp_roles::ROLE_STATION_MANAGER, "Station_Manager"),
                    (eve_db::corp_roles::ROLE_AUDITOR, "Auditor"),
                ];
                let roles: Vec<&str> = role_table
                    .iter()
                    .filter_map(|&(bit, name)| ((role_mask & bit) != 0).then_some(name))
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "roles": roles,
                        "roles_at_hq": [],
                        "roles_at_base": [],
                        "roles_at_other": [],
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
                let mut titles = ctx
                    .esi_state
                    .char_titles
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                if titles.is_empty() {
                    if let Some(member_titles) =
                        snap.corp_member_titles.get(&(ch.corporation_id, char_id))
                    {
                        for &tid in member_titles {
                            let tname = snap
                                .corp_titles
                                .get(&ch.corporation_id)
                                .and_then(|list| list.iter().find(|t| t.title_id == tid))
                                .map(|t| t.name.clone())
                                .or_else(|| {
                                    snap.corp_governance
                                        .get(&ch.corporation_id)
                                        .and_then(|g| g.titles.get(&tid))
                                        .map(|(n, _)| n.clone())
                                })
                                .unwrap_or_else(|| format!("Title {tid}"));
                            titles.push((tid, tname));
                        }
                    }
                }
                if titles.is_empty() && !ch.title.is_empty() {
                    titles.push((1, ch.title.clone()));
                }
                titles.sort_by_key(|&(tid, _)| tid);
                let out: Vec<Value> = titles
                    .into_iter()
                    .map(|(tid, name)| {
                        json!({
                            "title_id": tid,
                            "name": name,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 2. Calendar (ESI-4)
        ("GET", "/characters/{character_id}/calendar") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let from_event: Option<i64> = match query_params.get("from_event") {
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
                let events_guard = ctx
                    .esi_state
                    .calendar_events
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let resp_guard = ctx
                    .esi_state
                    .calendar_responses
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let corp_alliance_id = snap
                    .corporations
                    .get(&ch.corporation_id)
                    .and_then(|c| c.alliance_id);
                let mut visible: Vec<_> = events_guard
                    .values()
                    .filter(|ev| {
                        let invited = ev.owner_id == char_id
                            || ev.attendees.contains(&char_id)
                            || (ev.owner_type == "corporation"
                                && ev.owner_id == ch.corporation_id)
                            || (ev.owner_type == "alliance"
                                && corp_alliance_id == Some(ev.owner_id))
                            || ev.owner_type == "eve_server";
                        invited && from_event.is_none_or(|f| ev.event_id >= f)
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
                let events_guard = ctx
                    .esi_state
                    .calendar_events
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
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
                let invited = ev.owner_id == char_id
                    || ev.attendees.contains(&char_id)
                    || (ev.owner_type == "corporation" && ev.owner_id == ch.corporation_id)
                    || (ev.owner_type == "alliance" && corp_alliance_id == Some(ev.owner_id))
                    || ev.owner_type == "eve_server";
                if !invited {
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
            let (event_exists, is_invited) = {
                let esi_guard = ctx
                    .esi_state
                    .calendar_events
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(ev) = esi_guard.get(&event_id) {
                    let inv = ev.owner_id == char_id
                        || ev.attendees.contains(&char_id)
                        || (ev.owner_type == "corporation" && ev.owner_id == corp_id)
                        || (ev.owner_type == "alliance" && corp_alliance_id == Some(ev.owner_id))
                        || ev.owner_type == "eve_server";
                    (true, inv)
                } else {
                    drop(esi_guard);
                    let s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ev) = s.calendar_events.get(&event_id).filter(|e| !e.is_deleted) {
                        let inv = ev.owner_id == char_id
                            || ev.creator_id == char_id
                            || ev.responses.contains_key(&char_id)
                            || (ev._scope == "Corporation" && ev.owner_id == corp_id)
                            || (ev._scope == "Alliance" && corp_alliance_id == Some(ev.owner_id))
                            || ev._scope == "CCP";
                        (true, inv)
                    } else {
                        (false, false)
                    }
                }
            };
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
            let social_ev = crate::services::fleet_social::update_calendar_response_from_esi(
                ctx,
                char_id,
                event_id,
                response_code,
            );
            let ev_meta = if let Some(meta) = social_ev {
                meta
            } else {
                let mut esi_guard = ctx
                    .esi_state
                    .calendar_events
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let Some(ev) = esi_guard.get_mut(&event_id) else {
                    return Some(RouteExecutionResult::error(
                        404,
                        format!("Calendar event {event_id} not found"),
                    ));
                };
                if !ev.attendees.contains(&char_id) {
                    ev.attendees.push(char_id);
                    ev.attendees.sort_unstable();
                }
                // Convert Unix epoch seconds to BlueOS FILETIME ticks (100 ns since 1601-01-01).
                let filetime = (ev.event_date_unix as i64 + 11_644_473_600) * 10_000_000;
                (
                    ev.owner_id,
                    filetime,
                    ev.duration_minutes,
                    ev.title.clone(),
                    ev.importance,
                )
            };
            ctx.esi_state
                .calendar_responses
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert((char_id, event_id), resp_str.to_string());
            let (owner_id, event_date_time, duration, title, importance) = ev_meta;
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
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let events_guard = ctx
                    .esi_state
                    .calendar_events
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
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
                let invited = ev.owner_id == char_id
                    || ev.attendees.contains(&char_id)
                    || (ev.owner_type == "corporation" && ev.owner_id == ch.corporation_id)
                    || (ev.owner_type == "alliance" && corp_alliance_id == Some(ev.owner_id))
                    || ev.owner_type == "eve_server";
                if !invited {
                    return RouteExecutionResult::error(
                        403,
                        format!("Character {char_id} is not invited to calendar event {event_id}"),
                    );
                }
                let resp_guard = ctx
                    .esi_state
                    .calendar_responses
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
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

        // 5. Legacy / Extended Feature Routes (Bookmarks, Corp Projects, Opportunities)
        ("GET", "/characters/{character_id}/bookmarks") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
                }
                let mut bms: Vec<_> = snap
                    .bookmarks
                    .values()
                    .filter(|b| {
                        b.owner_id == char_id
                            && !snap
                                .bookmark_expiries
                                .get(&b.bookmark_id)
                                .is_some_and(|&exp| exp > 0 && now >= exp)
                    })
                    .collect();
                bms.sort_by_key(|b| b.bookmark_id);
                let out: Vec<Value> = bms
                    .into_iter()
                    .map(|b| {
                        let mut obj = json!({
                            "bookmark_id": b.bookmark_id,
                            "creator_id": b.owner_id,
                            "created": format_iso8601_utc(now),
                            "label": b.label,
                            "notes": b.memo,
                            "location_id": b.solar_system_id as i64,
                            "coordinates": { "x": b.x, "y": b.y, "z": b.z },
                        });
                        if let Some(fid) = b.folder_id {
                            obj["folder_id"] = json!(fid);
                        }
                        if let Some(iid) = b.item_id {
                            obj["item"] = json!({ "item_id": iid, "type_id": 5 });
                        }
                        obj
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
                    return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
                }
                let mut folders: Vec<_> = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        f.owner_id == char_id
                            && f.corporation_id.is_none()
                            && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    })
                    .collect();
                folders.sort_by_key(|f| f.folder_id);
                let mut out: Vec<Value> = folders
                    .into_iter()
                    .map(|f| json!({ "folder_id": f.folder_id, "name": f.folder_name }))
                    .collect();
                let mut known_fids: Vec<i64> = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        f.owner_id == char_id
                            && f.corporation_id.is_none()
                            && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    })
                    .map(|f| f.folder_id)
                    .collect();
                let mut bm_fids: Vec<i64> = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
                    .filter_map(|b| b.folder_id)
                    .filter(|fid| !snap.bookmark_subfolder_parents.contains_key(fid))
                    .collect();
                bm_fids.sort_unstable();
                bm_fids.dedup();
                for fid in bm_fids {
                    if !known_fids.contains(&fid) {
                        known_fids.push(fid);
                        let fname = snap
                            .bookmark_folders
                            .get(&fid)
                            .map(|f| f.folder_name.clone())
                            .unwrap_or_else(|| format!("Folder {fid}"));
                        out.push(json!({ "folder_id": fid, "name": fname }));
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/bookmarks") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(404, format!("Corporation {corp_id} not found"));
                }
                let mut bms: Vec<_> = snap
                    .bookmarks
                    .values()
                    .filter(|b| {
                        let is_corp_folder = b
                            .folder_id
                            .and_then(|fid| snap.bookmark_folders.get(&fid))
                            .is_some_and(|f| f.is_shared || f.corporation_id == Some(corp_id));
                        let belongs_to_corp = b.owner_id == corp_id
                            || ((b.is_shared || is_corp_folder)
                                && snap
                                    .characters
                                    .get(&b.owner_id)
                                    .is_some_and(|c| c.corporation_id == corp_id));
                        belongs_to_corp
                            && !snap
                                .bookmark_expiries
                                .get(&b.bookmark_id)
                                .is_some_and(|&exp| exp > 0 && now >= exp)
                    })
                    .collect();
                bms.sort_by_key(|b| b.bookmark_id);
                let out: Vec<Value> = bms
                    .into_iter()
                    .map(|b| {
                        let mut obj = json!({
                            "bookmark_id": b.bookmark_id,
                            "creator_id": b.owner_id,
                            "created": format_iso8601_utc(now),
                            "label": b.label,
                            "notes": b.memo,
                            "location_id": b.solar_system_id as i64,
                            "coordinates": { "x": b.x, "y": b.y, "z": b.z },
                        });
                        if let Some(fid) = b.folder_id {
                            obj["folder_id"] = json!(fid);
                        }
                        if let Some(iid) = b.item_id {
                            obj["item"] = json!({ "item_id": iid, "type_id": 5 });
                        }
                        obj
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
                    return RouteExecutionResult::error(404, format!("Corporation {corp_id} not found"));
                }
                let mut folders: Vec<_> = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        (f.owner_id == corp_id || f.corporation_id == Some(corp_id))
                            && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    })
                    .collect();
                folders.sort_by_key(|f| f.folder_id);
                let mut out: Vec<Value> = folders
                    .into_iter()
                    .map(|f| json!({ "folder_id": f.folder_id, "name": f.folder_name, "creator_id": f.owner_id }))
                    .collect();
                let mut known_fids: Vec<i64> = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        (f.owner_id == corp_id || f.corporation_id == Some(corp_id))
                            && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    })
                    .map(|f| f.folder_id)
                    .collect();
                let mut bm_fids: Vec<i64> = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.owner_id == corp_id)
                    .filter_map(|b| b.folder_id)
                    .filter(|fid| !snap.bookmark_subfolder_parents.contains_key(fid))
                    .collect();
                bm_fids.sort_unstable();
                bm_fids.dedup();
                for fid in bm_fids {
                    if !known_fids.contains(&fid) {
                        known_fids.push(fid);
                        out.push(json!({ "folder_id": fid, "name": format!("Corp Folder {fid}"), "creator_id": corp_id }));
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/projects") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(404, format!("Corporation {corp_id} not found"));
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
            ids.extend(
                ctx.sde
                    .node_graphs
                    .values()
                    .filter_map(|ng| ng.parent_graph_id.map(|pid| pid as i64)),
            );
            ids.sort_unstable();
            ids.dedup();
            Some(RouteExecutionResult::ok_json(json!(ids), 3600))
        }

        ("GET", "/opportunities/tasks") => {
            let mut ids: Vec<i64> = ctx.sde.node_graphs.keys().map(|&id| id as i64).collect();
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
                    return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
                }
                let mut out = Vec::new();
                if let Some(npe) = snap.npe_progress.get(&char_id) {
                    let mut gids: Vec<u32> = npe.claimed_goals.iter().copied().collect();
                    gids.sort_unstable();
                    for gid in gids {
                        out.push(json!({
                            "task_id": gid as i64,
                            "completed_at": format_iso8601_utc(now),
                        }));
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        _ => super::char_social_mail_routes::dispatch_char_social_mail_route(
            ctx,
            method,
            path,
            path_params,
            query_params,
            req_body,
        ),
    }
}
