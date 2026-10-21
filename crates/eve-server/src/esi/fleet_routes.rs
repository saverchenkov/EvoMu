//! `esi::fleet_routes` — State-backed ESI Fleets endpoints (`ESI-7`):
//! - `GET /characters/{character_id}/fleet`
//! - `GET /fleets/{fleet_id}`, `PUT /fleets/{fleet_id}`
//! - `GET /fleets/{fleet_id}/members`, `POST /fleets/{fleet_id}/members`
//! - `PUT /fleets/{fleet_id}/members/{member_id}`, `DELETE /fleets/{fleet_id}/members/{member_id}`
//! - `GET /fleets/{fleet_id}/wings`, `POST /fleets/{fleet_id}/wings`
//! - `PUT /fleets/{fleet_id}/wings/{wing_id}`, `DELETE /fleets/{fleet_id}/wings/{wing_id}`
//! - `POST /fleets/{fleet_id}/wings/{wing_id}/squads`
//! - `PUT /fleets/{fleet_id}/squads/{squad_id}`, `DELETE /fleets/{fleet_id}/squads/{squad_id}`

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, parse_path_i64, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::{
    FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord, SsoTokenClaims,
};
use serde_json::{json, Value};
use std::collections::HashMap;

fn fleet_role_to_str(role: FleetRole) -> (&'static str, &'static str) {
    match role {
        FleetRole::FleetCommander => ("fleet_commander", "Fleet Commander"),
        FleetRole::WingCommander => ("wing_commander", "Wing Commander"),
        FleetRole::SquadCommander => ("squad_commander", "Squad Commander"),
        FleetRole::SquadMember => ("squad_member", "Squad Member"),
    }
}

fn parse_fleet_role(role_str: &str) -> Option<FleetRole> {
    match role_str {
        "fleet_commander" => Some(FleetRole::FleetCommander),
        "wing_commander" => Some(FleetRole::WingCommander),
        "squad_commander" => Some(FleetRole::SquadCommander),
        "squad_member" => Some(FleetRole::SquadMember),
        _ => None,
    }
}

fn is_fleet_member(f: &FleetRecord, char_id: i64) -> bool {
    f.boss_id == char_id
        || f.members.contains(&char_id)
        || f.member_details.contains_key(&char_id)
}

fn is_fleet_commander_or_boss(f: &FleetRecord, char_id: i64) -> bool {
    f.boss_id == char_id
        || f.member_details.get(&char_id).is_some_and(|md| {
            matches!(
                md.role,
                FleetRole::FleetCommander | FleetRole::WingCommander | FleetRole::SquadCommander
            )
        })
}

/// Returns `true` if `(method, path_pattern)` is one of the 14 state-backed Fleet operations.
pub fn is_fleet_operation_implemented(method: &str, path_pattern: &str) -> bool {
    matches!(
        (method, path_pattern),
        ("GET", "/characters/{character_id}/fleet")
            | ("GET", "/fleets/{fleet_id}")
            | ("PUT", "/fleets/{fleet_id}")
            | ("GET", "/fleets/{fleet_id}/members")
            | ("POST", "/fleets/{fleet_id}/members")
            | ("PUT", "/fleets/{fleet_id}/members/{member_id}")
            | ("DELETE", "/fleets/{fleet_id}/members/{member_id}")
            | ("GET", "/fleets/{fleet_id}/wings")
            | ("POST", "/fleets/{fleet_id}/wings")
            | ("PUT", "/fleets/{fleet_id}/wings/{wing_id}")
            | ("DELETE", "/fleets/{fleet_id}/wings/{wing_id}")
            | ("POST", "/fleets/{fleet_id}/wings/{wing_id}/squads")
            | ("PUT", "/fleets/{fleet_id}/squads/{squad_id}")
            | ("DELETE", "/fleets/{fleet_id}/squads/{squad_id}")
    )
}

/// Dispatches Fleet ESI operations against live `WorldRepository` state.
#[allow(clippy::too_many_lines)]
pub fn dispatch_fleet_routes(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    match (method, path_pattern) {
        ("GET", "/characters/{character_id}/fleet") => {
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
                let fleet = snap
                    .fleets
                    .values()
                    .find(|f| is_fleet_member(f, char_id));
                let Some(f) = fleet else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} is not in a fleet"),
                    );
                };
                let (role, wing_id, squad_id) = if let Some(md) = f.member_details.get(&char_id) {
                    let (r_str, _) = fleet_role_to_str(md.role);
                    (
                        r_str,
                        md.wing_id.unwrap_or(-1),
                        md.squad_id.unwrap_or(-1),
                    )
                } else if f.boss_id == char_id {
                    ("fleet_commander", -1, -1)
                } else {
                    let wid = f.wings.first().copied().unwrap_or(-1);
                    let sid = f.squads.first().copied().unwrap_or(-1);
                    ("squad_member", wid, sid)
                };
                RouteExecutionResult::ok_json(
                    json!({
                        "fleet_id": f.fleet_id,
                        "fleet_boss_id": f.boss_id,
                        "wing_id": wing_id,
                        "squad_id": squad_id,
                        "role": role,
                    }),
                    60,
                )
            }))
        }

        ("GET", "/fleets/{fleet_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    );
                };
                if let Some(c) = claims {
                    if !is_fleet_member(f, c.character_id) {
                        return RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} is not a member of fleet {fleet_id}",
                                c.character_id
                            ),
                        );
                    }
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "motd": f.motd.clone(),
                        "is_free_move": f.free_move,
                        "is_registered": f.is_registered,
                        "is_voice_enabled": false,
                    }),
                    5,
                )
            }))
        }

        ("PUT", "/fleets/{fleet_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) if v.is_object() => v,
                _ => {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Invalid fleet settings JSON object",
                    ));
                }
            };
            if payload.get("motd").is_some_and(|v| !v.is_string())
                || payload.get("is_free_move").is_some_and(|v| !v.is_boolean())
            {
                return Some(RouteExecutionResult::error(
                    400,
                    "Invalid types for 'motd' or 'is_free_move'",
                ));
            }
            let new_motd = payload.get("motd").and_then(Value::as_str);
            let new_free_move = payload.get("is_free_move").and_then(Value::as_bool);
            if new_motd.is_none() && new_free_move.is_none() {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fleet update must specify 'motd' and/or 'is_free_move'",
                ));
            }
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if let Some(m) = new_motd {
                    f.motd = m.to_string();
                }
                if let Some(fm) = new_free_move {
                    f.free_move = fm;
                }
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("GET", "/fleets/{fleet_id}/members") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    );
                };
                if let Some(c) = claims {
                    if !is_fleet_member(f, c.character_id) {
                        return RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} is not a member of fleet {fleet_id}",
                                c.character_id
                            ),
                        );
                    }
                }
                let mut member_ids = f.members.clone();
                if !member_ids.contains(&f.boss_id) {
                    member_ids.insert(0, f.boss_id);
                }
                let mut detail_ids: Vec<i64> = f.member_details.keys().copied().collect();
                detail_ids.sort_unstable();
                for cid in detail_ids {
                    if !member_ids.contains(&cid) {
                        member_ids.push(cid);
                    }
                }
                let mut out = Vec::with_capacity(member_ids.len());
                for cid in member_ids {
                    let Some(ch) = snap.characters.get(&cid) else {
                        continue;
                    };
                    let is_boss = cid == f.boss_id;
                    let (role_str, role_name_str, wing_id, squad_id, takes_warp, join_time_unix) =
                        if let Some(md) = f.member_details.get(&cid) {
                            let (r_str, default_rname) = fleet_role_to_str(md.role);
                            let rname = if md.role_name.is_empty() {
                                default_rname.to_string()
                            } else {
                                md.role_name.clone()
                            };
                            (
                                r_str,
                                rname,
                                md.wing_id.unwrap_or(-1),
                                md.squad_id.unwrap_or(-1),
                                md.takes_fleet_warp,
                                md.join_time_unix,
                            )
                        } else if is_boss {
                            (
                                "fleet_commander",
                                "Fleet Commander".to_string(),
                                -1,
                                -1,
                                true,
                                0,
                            )
                        } else {
                            ("squad_member", "Squad Member".to_string(), -1, -1, true, 0)
                        };
                    // DERIVED: 670 is the SDE Capsule type ID when no ship item is active
                    let ship_type_id = snap
                        .items
                        .get(&ch.active_ship_id)
                        .map(|i| i.type_id as i64)
                        .unwrap_or(670);
                    let solar_system_id = ch.solar_system_id as i64;
                    let mut obj = json!({
                        "character_id": cid,
                        "ship_type_id": ship_type_id,
                        "wing_id": wing_id,
                        "squad_id": squad_id,
                        "role": role_str,
                        "role_name": role_name_str,
                        "join_time": format_iso8601_utc(join_time_unix),
                        "takes_fleet_warp": takes_warp,
                        "solar_system_id": solar_system_id,
                    });
                    if let Some(station_id) = ch.station_id {
                        obj["station_id"] = json!(station_id);
                    }
                    out.push(obj);
                }
                RouteExecutionResult::ok_json(json!(out), 5)
            }))
        }

        ("POST", "/fleets/{fleet_id}/members") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid fleet invitation JSON body: {e}"),
                    ));
                }
            };
            let Some(invitee_id) = payload.get("character_id").and_then(Value::as_i64) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required 'character_id' in fleet invite",
                ));
            };
            let Some(role_str) = payload.get("role").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required 'role' in fleet invite",
                ));
            };
            let Some(role) = parse_fleet_role(role_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid fleet role '{role_str}'"),
                ));
            };
            let wing_id = payload
                .get("wing_id")
                .and_then(Value::as_i64)
                .filter(|&w| w > 0);
            let squad_id = payload
                .get("squad_id")
                .and_then(Value::as_i64)
                .filter(|&s| s > 0);
            let now = ctx.now_unix();
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !snap.characters.contains_key(&invitee_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Invited character {invitee_id} not found"),
                    ));
                }
                let f = snap.fleets.get_mut(&fleet_id).unwrap();
                if !f.members.contains(&invitee_id) {
                    f.members.push(invitee_id);
                }
                f.invites.insert(invitee_id);
                let (_, rname) = fleet_role_to_str(role);
                let resolved_wing = match role {
                    FleetRole::FleetCommander => None,
                    FleetRole::WingCommander
                    | FleetRole::SquadCommander
                    | FleetRole::SquadMember => wing_id.or_else(|| f.wings.first().copied()),
                };
                let resolved_squad = match role {
                    FleetRole::FleetCommander | FleetRole::WingCommander => None,
                    FleetRole::SquadCommander | FleetRole::SquadMember => {
                        squad_id.or_else(|| f.squads.first().copied())
                    }
                };
                f.member_details.insert(
                    invitee_id,
                    FleetMemberRecord {
                        character_id: invitee_id,
                        role,
                        wing_id: resolved_wing,
                        squad_id: resolved_squad,
                        takes_fleet_warp: true,
                        job: 0,
                        role_name: rname.to_string(),
                        join_time_unix: now,
                    },
                );
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("PUT", "/fleets/{fleet_id}/members/{member_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let member_id = match parse_path_i64(path_params, "member_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid fleet member move JSON body: {e}"),
                    ));
                }
            };
            let Some(role_str) = payload.get("role").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required 'role' in member move request",
                ));
            };
            let Some(role) = parse_fleet_role(role_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid fleet role '{role_str}'"),
                ));
            };
            let wing_id = payload
                .get("wing_id")
                .and_then(Value::as_i64)
                .filter(|&w| w > 0);
            let squad_id = payload
                .get("squad_id")
                .and_then(Value::as_i64)
                .filter(|&s| s > 0);
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !is_fleet_member(f, member_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Member {member_id} not found in fleet {fleet_id}"),
                    ));
                }
                let (_, rname) = fleet_role_to_str(role);
                let (resolved_wing, resolved_squad) = match role {
                    FleetRole::FleetCommander => (None, None),
                    FleetRole::WingCommander => (wing_id, None),
                    FleetRole::SquadCommander | FleetRole::SquadMember => (wing_id, squad_id),
                };
                let (takes_fleet_warp, job, join_time_unix) = f
                    .member_details
                    .get(&member_id)
                    .map(|md| (md.takes_fleet_warp, md.job, md.join_time_unix))
                    .unwrap_or((true, 0, 0));
                f.member_details.insert(
                    member_id,
                    FleetMemberRecord {
                        character_id: member_id,
                        role,
                        wing_id: resolved_wing,
                        squad_id: resolved_squad,
                        takes_fleet_warp,
                        job,
                        role_name: rname.to_string(),
                        join_time_unix,
                    },
                );
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("DELETE", "/fleets/{fleet_id}/members/{member_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let member_id = match parse_path_i64(path_params, "member_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if c.character_id != member_id && !is_fleet_commander_or_boss(f, c.character_id)
                    {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks permission to kick member {member_id} from fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !is_fleet_member(f, member_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Member {member_id} not found in fleet {fleet_id}"),
                    ));
                }
                f.members.retain(|&m| m != member_id);
                f.member_details.remove(&member_id);
                f.invites.remove(&member_id);
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("GET", "/fleets/{fleet_id}/wings") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    );
                };
                if let Some(c) = claims {
                    if !is_fleet_member(f, c.character_id) {
                        return RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} is not a member of fleet {fleet_id}",
                                c.character_id
                            ),
                        );
                    }
                }
                let mut wings = Vec::new();
                let mut wing_ids = f.wings.clone();
                for &wid in f.wing_details.keys() {
                    if !wing_ids.contains(&wid) {
                        wing_ids.push(wid);
                    }
                }
                wing_ids.sort_unstable();
                for wid in wing_ids {
                    let w_name = f
                        .wing_details
                        .get(&wid)
                        .map(|w| w.name.clone())
                        .unwrap_or_else(|| format!("Wing {wid}"));
                    let mut squad_ids: Vec<i64> = f
                        .wing_details
                        .get(&wid)
                        .map(|w| w.squad_ids.clone())
                        .unwrap_or_default();
                    for (sid, sq) in &f.squad_details {
                        if sq.wing_id == wid && !squad_ids.contains(sid) {
                            squad_ids.push(*sid);
                        }
                    }
                    if squad_ids.is_empty() && f.squad_details.is_empty() {
                        squad_ids.extend(f.squads.iter().copied());
                    }
                    squad_ids.sort_unstable();
                    let squads: Vec<Value> = squad_ids
                        .into_iter()
                        .map(|sid| {
                            let s_name = f
                                .squad_details
                                .get(&sid)
                                .map(|s| s.name.clone())
                                .unwrap_or_else(|| format!("Squad {sid}"));
                            json!({
                                "id": sid,
                                "name": s_name,
                            })
                        })
                        .collect();
                    wings.push(json!({
                        "id": wid,
                        "name": w_name,
                        "squads": squads,
                    }));
                }
                RouteExecutionResult::ok_json(json!(wings), 5)
            }))
        }

        ("POST", "/fleets/{fleet_id}/wings") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<i64, RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                snap.next_entity_id += 1;
                let wing_id = snap.next_entity_id.max(2001);
                snap.next_entity_id = wing_id;
                let f = snap.fleets.get_mut(&fleet_id).unwrap();
                let name = format!("Wing {}", f.wings.len() + 1);
                f.wings.push(wing_id);
                f.wing_details.insert(
                    wing_id,
                    FleetWingRecord {
                        wing_id,
                        name,
                        commander_id: None,
                        squad_ids: Vec::new(),
                    },
                );
                Ok(wing_id)
            });
            Some(match res {
                Ok(wing_id) => RouteExecutionResult::created_json(json!({ "wing_id": wing_id })),
                Err(e) => e,
            })
        }

        ("PUT", "/fleets/{fleet_id}/wings/{wing_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let wing_id = match parse_path_i64(path_params, "wing_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid wing rename JSON body: {e}"),
                    ));
                }
            };
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing wing 'name'"));
            };
            let trimmed = name.trim();
            if trimmed.is_empty() || trimmed.len() > 10 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Wing 'name' must be 1..=10 characters",
                ));
            }
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    ));
                }
                let entry = f.wing_details.entry(wing_id).or_insert_with(|| FleetWingRecord {
                    wing_id,
                    name: trimmed.to_string(),
                    commander_id: None,
                    squad_ids: Vec::new(),
                });
                entry.name = trimmed.to_string();
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("DELETE", "/fleets/{fleet_id}/wings/{wing_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let wing_id = match parse_path_i64(path_params, "wing_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    ));
                }
                f.wings.retain(|&w| w != wing_id);
                if let Some(removed_wing) = f.wing_details.remove(&wing_id) {
                    for sid in removed_wing.squad_ids {
                        f.squads.retain(|&s| s != sid);
                        f.squad_details.remove(&sid);
                    }
                }
                f.squad_details.retain(|sid, sq| {
                    if sq.wing_id == wing_id {
                        f.squads.retain(|&s| s != *sid);
                        false
                    } else {
                        true
                    }
                });
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("POST", "/fleets/{fleet_id}/wings/{wing_id}/squads") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let wing_id = match parse_path_i64(path_params, "wing_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<i64, RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    ));
                }
                snap.next_entity_id += 1;
                let squad_id = snap.next_entity_id.max(3001);
                snap.next_entity_id = squad_id;
                let f = snap.fleets.get_mut(&fleet_id).unwrap();
                let s_name = format!("Squad {}", f.squads.len() + 1);
                f.squads.push(squad_id);
                f.squad_details.insert(
                    squad_id,
                    FleetSquadRecord {
                        squad_id,
                        wing_id,
                        name: s_name,
                        commander_id: None,
                    },
                );
                let wing = f.wing_details.entry(wing_id).or_insert_with(|| FleetWingRecord {
                    wing_id,
                    name: format!("Wing {wing_id}"),
                    commander_id: None,
                    squad_ids: Vec::new(),
                });
                if !wing.squad_ids.contains(&squad_id) {
                    wing.squad_ids.push(squad_id);
                }
                Ok(squad_id)
            });
            Some(match res {
                Ok(squad_id) => RouteExecutionResult::created_json(json!({ "squad_id": squad_id })),
                Err(e) => e,
            })
        }

        ("PUT", "/fleets/{fleet_id}/squads/{squad_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let squad_id = match parse_path_i64(path_params, "squad_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid squad rename JSON body: {e}"),
                    ));
                }
            };
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing squad 'name'"));
            };
            let trimmed = name.trim();
            if trimmed.is_empty() || trimmed.len() > 10 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Squad 'name' must be 1..=10 characters",
                ));
            }
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Squad {squad_id} not found in fleet {fleet_id}"),
                    ));
                }
                let default_wing = f.wings.first().copied().unwrap_or(1);
                let sq = f
                    .squad_details
                    .entry(squad_id)
                    .or_insert_with(|| FleetSquadRecord {
                        squad_id,
                        wing_id: default_wing,
                        name: trimmed.to_string(),
                        commander_id: None,
                    });
                sq.name = trimmed.to_string();
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        ("DELETE", "/fleets/{fleet_id}/squads/{squad_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let squad_id = match parse_path_i64(path_params, "squad_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fleet {fleet_id} not found"),
                    ));
                };
                if let Some(c) = claims {
                    if !is_fleet_commander_or_boss(f, c.character_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Squad {squad_id} not found in fleet {fleet_id}"),
                    ));
                }
                f.squads.retain(|&s| s != squad_id);
                f.squad_details.remove(&squad_id);
                for wing in f.wing_details.values_mut() {
                    wing.squad_ids.retain(|&s| s != squad_id);
                }
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        _ => None,
    }
}
