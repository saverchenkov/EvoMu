//! `esi::fleet_routes` — State-backed ESI Fleets and Remote Client UI endpoints (`ESI-7`):
//! - `GET /characters/{character_id}/fleet`
//! - `GET /fleets/{fleet_id}`, `PUT /fleets/{fleet_id}`
//! - `GET /fleets/{fleet_id}/members`, `POST /fleets/{fleet_id}/members`
//! - `PUT /fleets/{fleet_id}/members/{member_id}`, `DELETE /fleets/{fleet_id}/members/{member_id}`
//! - `GET /fleets/{fleet_id}/wings`, `POST /fleets/{fleet_id}/wings`
//! - `PUT /fleets/{fleet_id}/wings/{wing_id}`, `DELETE /fleets/{fleet_id}/wings/{wing_id}`
//! - `POST /fleets/{fleet_id}/wings/{wing_id}/squads`
//! - `PUT /fleets/{fleet_id}/squads/{squad_id}`, `DELETE /fleets/{fleet_id}/squads/{squad_id}`
//! - `POST /ui/openwindow/contract`, `POST /ui/openwindow/information`
//! - `POST /ui/openwindow/marketdetails`, `POST /ui/openwindow/newmail`

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, parse_path_i64, RouteExecutionResult};
use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use eve_db::{
    FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord, SsoTokenClaims,
};
use eve_proto::PyRep;
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

fn is_fleet_boss_or_fc(f: &FleetRecord, char_id: i64) -> bool {
    f.boss_id == char_id
        || f.member_details
            .get(&char_id)
            .is_some_and(|md| md.role == FleetRole::FleetCommander)
}

fn squad_parent_wing_id(f: &FleetRecord, squad_id: i64) -> Option<i64> {
    f.squad_details
        .get(&squad_id)
        .map(|s| s.wing_id)
        .or_else(|| {
            f.wing_details
                .values()
                .find(|w| w.squad_ids.contains(&squad_id))
                .map(|w| w.wing_id)
        })
}

fn can_manage_wing(f: &FleetRecord, char_id: i64, wing_id: i64) -> bool {
    is_fleet_boss_or_fc(f, char_id)
        || f.member_details.get(&char_id).is_some_and(|md| {
            md.role == FleetRole::WingCommander && md.wing_id == Some(wing_id)
        })
}

fn can_manage_squad(f: &FleetRecord, char_id: i64, squad_id: i64) -> bool {
    if is_fleet_boss_or_fc(f, char_id) {
        return true;
    }
    let Some(md) = f.member_details.get(&char_id) else {
        return false;
    };
    match md.role {
        FleetRole::FleetCommander => true,
        FleetRole::WingCommander => {
            let parent_wing = squad_parent_wing_id(f, squad_id);
            parent_wing.is_some() && md.wing_id == parent_wing
        }
        FleetRole::SquadCommander => md.squad_id == Some(squad_id),
        FleetRole::SquadMember => false,
    }
}

fn member_effective_position(
    f: &FleetRecord,
    member_id: i64,
) -> (FleetRole, Option<i64>, Option<i64>) {
    if let Some(md) = f.member_details.get(&member_id) {
        (md.role, md.wing_id, md.squad_id)
    } else if f.boss_id == member_id {
        (FleetRole::FleetCommander, None, None)
    } else {
        (
            FleetRole::SquadMember,
            f.wings.first().copied(),
            f.squads.first().copied(),
        )
    }
}

fn can_invite_member(
    f: &FleetRecord,
    caller_id: i64,
    role: FleetRole,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
) -> bool {
    if f.boss_id == caller_id {
        return true;
    }
    let Some(md) = f.member_details.get(&caller_id) else {
        return false;
    };
    match md.role {
        FleetRole::FleetCommander => role != FleetRole::FleetCommander,
        FleetRole::WingCommander => {
            matches!(role, FleetRole::SquadCommander | FleetRole::SquadMember)
                && md.wing_id.is_some()
                && wing_id == md.wing_id
        }
        FleetRole::SquadCommander => {
            role == FleetRole::SquadMember
                && md.squad_id.is_some()
                && squad_id == md.squad_id
                && (wing_id.is_none() || wing_id == md.wing_id)
        }
        FleetRole::SquadMember => false,
    }
}

fn can_act_on_member(
    f: &FleetRecord,
    caller_id: i64,
    member_id: i64,
    new_pos: Option<(FleetRole, Option<i64>, Option<i64>)>,
) -> bool {
    if member_id == f.boss_id && caller_id != f.boss_id {
        return false;
    }
    if f.boss_id == caller_id {
        return true;
    }
    let (target_role, target_wing, target_squad) = member_effective_position(f, member_id);
    let Some(caller_md) = f.member_details.get(&caller_id) else {
        return false;
    };
    let can_act_on_current = match caller_md.role {
        FleetRole::FleetCommander => target_role != FleetRole::FleetCommander,
        FleetRole::WingCommander => {
            matches!(
                target_role,
                FleetRole::SquadCommander | FleetRole::SquadMember
            ) && caller_md.wing_id.is_some()
                && target_wing == caller_md.wing_id
        }
        FleetRole::SquadCommander => {
            target_role == FleetRole::SquadMember
                && caller_md.squad_id.is_some()
                && target_squad == caller_md.squad_id
        }
        FleetRole::SquadMember => false,
    };
    if !can_act_on_current {
        return false;
    }
    if let Some((new_role, new_wing, new_squad)) = new_pos {
        match caller_md.role {
            FleetRole::FleetCommander => new_role != FleetRole::FleetCommander,
            FleetRole::WingCommander => {
                matches!(
                    new_role,
                    FleetRole::SquadCommander | FleetRole::SquadMember
                ) && new_wing == caller_md.wing_id
            }
            FleetRole::SquadCommander => {
                new_role == FleetRole::SquadMember
                    && new_squad == caller_md.squad_id
                    && (new_wing.is_none() || new_wing == caller_md.wing_id)
            }
            FleetRole::SquadMember => false,
        }
    } else {
        true
    }
}

fn validate_wing_and_squad(
    f: &FleetRecord,
    fleet_id: i64,
    role: FleetRole,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
    is_move: bool,
) -> Result<(Option<i64>, Option<i64>), RouteExecutionResult> {
    if let Some(wid) = wing_id {
        if !f.wings.contains(&wid) && !f.wing_details.contains_key(&wid) {
            return Err(RouteExecutionResult::error(
                404,
                format!("Wing {wid} not found in fleet {fleet_id}"),
            ));
        }
    }
    if let Some(sid) = squad_id {
        if !f.squads.contains(&sid) && !f.squad_details.contains_key(&sid) {
            return Err(RouteExecutionResult::error(
                404,
                format!("Squad {sid} not found in fleet {fleet_id}"),
            ));
        }
        if let Some(wid) = wing_id {
            if let Some(sq) = f.squad_details.get(&sid) {
                if sq.wing_id != wid {
                    return Err(RouteExecutionResult::error(
                        400,
                        format!("Squad {sid} does not belong to wing {wid} in fleet {fleet_id}"),
                    ));
                }
            } else if let Some(w) = f.wing_details.get(&wid) {
                if !w.squad_ids.is_empty() && !w.squad_ids.contains(&sid) {
                    return Err(RouteExecutionResult::error(
                        400,
                        format!("Squad {sid} does not belong to wing {wid} in fleet {fleet_id}"),
                    ));
                }
            }
        }
    }
    match role {
        FleetRole::FleetCommander => Ok((None, None)),
        FleetRole::WingCommander => {
            let resolved_wing =
                wing_id.or_else(|| if is_move { None } else { f.wings.first().copied() });
            if is_move && resolved_wing.is_none() {
                return Err(RouteExecutionResult::error(
                    400,
                    "Moving a member to wing_commander requires a valid wing_id",
                ));
            }
            Ok((resolved_wing, None))
        }
        FleetRole::SquadCommander | FleetRole::SquadMember => {
            let resolved_squad =
                squad_id.or_else(|| if is_move { None } else { f.squads.first().copied() });
            let resolved_wing = wing_id
                .or_else(|| resolved_squad.and_then(|s| squad_parent_wing_id(f, s)))
                .or_else(|| if is_move { None } else { f.wings.first().copied() });
            if is_move && (resolved_wing.is_none() || resolved_squad.is_none()) {
                return Err(RouteExecutionResult::error(
                    400,
                    "Moving a member to squad_commander or squad_member requires wing_id and squad_id",
                ));
            }
            Ok((resolved_wing, resolved_squad))
        }
    }
}

fn ensure_online_ui_session(
    ctx: &ServerContext,
    caller_id: i64,
) -> Result<(), RouteExecutionResult> {
    let is_online = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&caller_id).map(|ch| ch.user_id))
        .is_some_and(|uid| ctx.db.get_online_session(uid).is_some());
    if !is_online {
        return Err(RouteExecutionResult::error(
            400,
            format!("Character {caller_id} does not have an active online client session"),
        ));
    }
    Ok(())
}

fn emit_ui_window_open(
    ctx: &ServerContext,
    caller_id: i64,
    window_type: &str,
    target_id: i64,
    details: String,
) {
    ctx.esi_state.record_ui_event(
        caller_id,
        json!({
            "window_type": window_type,
            "target_id": target_id,
            "details": details,
        }),
    );
    ctx.notify(
        NotificationTarget::Character(caller_id),
        "OnWindowOpened",
        "charid",
        vec![
            PyRep::String(window_type.to_string()),
            PyRep::Integer(target_id),
            PyRep::String(details),
        ],
    );
}

/// Returns `true` if `(method, path_pattern)` is one of the state-backed Fleet or OpenWindow UI operations.
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
            | ("POST", "/ui/openwindow/contract")
            | ("POST", "/ui/openwindow/information")
            | ("POST", "/ui/openwindow/marketdetails")
            | ("POST", "/ui/openwindow/newmail")
    )
}

/// Dispatches Fleet and OpenWindow UI ESI operations against live `WorldRepository` state.
#[allow(clippy::too_many_lines)]
pub fn dispatch_fleet_routes(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
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
                    if !is_fleet_boss_or_fc(f, c.character_id) {
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
                    return RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found"));
                };
                if let Some(c) = claims {
                    if !is_fleet_member(f, c.character_id) {
                        return RouteExecutionResult::error(
                            403,
                            format!("Character {} is not a member of fleet {fleet_id}", c.character_id),
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
                            ("fleet_commander", "Fleet Commander".to_string(), -1, -1, true, 0)
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
            let wing_id = match payload.get("wing_id") {
                Some(v) if !v.is_null() => match v.as_i64().filter(|&w| w > 0) {
                    Some(w) => Some(w),
                    None => return Some(RouteExecutionResult::error(400, "Invalid 'wing_id'")),
                },
                _ => None,
            };
            let squad_id = match payload.get("squad_id") {
                Some(v) if !v.is_null() => match v.as_i64().filter(|&s| s > 0) {
                    Some(s) => Some(s),
                    None => return Some(RouteExecutionResult::error(400, "Invalid 'squad_id'")),
                },
                _ => None,
            };
            let now = ctx.now_unix();
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                let (resolved_wing, resolved_squad) =
                    validate_wing_and_squad(f, fleet_id, role, wing_id, squad_id, false)?;
                if let Some(c) = claims {
                    if !can_invite_member(f, c.character_id, role, resolved_wing, resolved_squad) {
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
            let wing_id = match payload.get("wing_id") {
                Some(v) if !v.is_null() => match v.as_i64().filter(|&w| w > 0) {
                    Some(w) => Some(w),
                    None => return Some(RouteExecutionResult::error(400, "Invalid 'wing_id'")),
                },
                _ => None,
            };
            let squad_id = match payload.get("squad_id") {
                Some(v) if !v.is_null() => match v.as_i64().filter(|&s| s > 0) {
                    Some(s) => Some(s),
                    None => return Some(RouteExecutionResult::error(400, "Invalid 'squad_id'")),
                },
                _ => None,
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !is_fleet_member(f, member_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Member {member_id} not found in fleet {fleet_id}"),
                    ));
                }
                let (resolved_wing, resolved_squad) =
                    validate_wing_and_squad(f, fleet_id, role, wing_id, squad_id, true)?;
                if let Some(c) = claims {
                    if !can_act_on_member(
                        f,
                        c.character_id,
                        member_id,
                        Some((role, resolved_wing, resolved_squad)),
                    ) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks fleet commander/boss permissions for fleet {fleet_id}",
                                c.character_id
                            ),
                        ));
                    }
                }
                let (_, rname) = fleet_role_to_str(role);
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !is_fleet_member(f, member_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Member {member_id} not found in fleet {fleet_id}"),
                    ));
                }
                if member_id == f.boss_id {
                    return Err(RouteExecutionResult::error(
                        403,
                        "Cannot kick or remove the fleet boss without transferring leadership",
                    ));
                }
                if let Some(c) = claims {
                    if c.character_id != member_id
                        && !can_act_on_member(f, c.character_id, member_id, None)
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
                    return RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found"));
                };
                if let Some(c) = claims {
                    if !is_fleet_member(f, c.character_id) {
                        return RouteExecutionResult::error(
                            403,
                            format!("Character {} is not a member of fleet {fleet_id}", c.character_id),
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
                            json!({ "id": sid, "name": s_name })
                        })
                        .collect();
                    wings.push(json!({ "id": wid, "name": w_name, "squads": squads }));
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if let Some(c) = claims {
                    if !is_fleet_boss_or_fc(f, c.character_id) {
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
                return Some(RouteExecutionResult::error(400, "Wing 'name' must be 1..=10 characters"));
            }
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    ));
                }
                if let Some(c) = claims {
                    if !can_manage_wing(f, c.character_id, wing_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks wing/fleet commander permissions for wing {wing_id}",
                                c.character_id
                            ),
                        ));
                    }
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if let Some(c) = claims {
                    if !is_fleet_boss_or_fc(f, c.character_id) {
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !f.wings.contains(&wing_id) && !f.wing_details.contains_key(&wing_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Wing {wing_id} not found in fleet {fleet_id}"),
                    ));
                }
                if let Some(c) = claims {
                    if !can_manage_wing(f, c.character_id, wing_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks wing/fleet commander permissions for wing {wing_id}",
                                c.character_id
                            ),
                        ));
                    }
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Squad {squad_id} not found in fleet {fleet_id}"),
                    ));
                }
                if let Some(c) = claims {
                    if !can_manage_squad(f, c.character_id, squad_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks squad/wing/fleet commander permissions for squad {squad_id}",
                                c.character_id
                            ),
                        ));
                    }
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
                    return Err(RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found")));
                };
                if !f.squads.contains(&squad_id) && !f.squad_details.contains_key(&squad_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Squad {squad_id} not found in fleet {fleet_id}"),
                    ));
                }
                if let Some(c) = claims {
                    if !can_manage_squad(f, c.character_id, squad_id) {
                        return Err(RouteExecutionResult::error(
                            403,
                            format!(
                                "Character {} lacks squad/wing/fleet commander permissions for squad {squad_id}",
                                c.character_id
                            ),
                        ));
                    }
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

        // Remote Client UI OpenWindow Routes (`ESI-7`)
        ("POST", "/ui/openwindow/contract") => {
            let caller_id = claims.map(|c| c.character_id).unwrap_or(0);
            if let Err(e) = ensure_online_ui_session(ctx, caller_id) {
                return Some(e);
            }
            let Some(raw_cid) = query_params.get("contract_id") else {
                return Some(RouteExecutionResult::error(400, "Missing required query parameter 'contract_id'"));
            };
            let Some(contract_id) = raw_cid.parse::<i64>().ok().filter(|&id| id > 0) else {
                return Some(RouteExecutionResult::error(400, format!("Invalid contract_id '{raw_cid}'")));
            };
            if !ctx.db.with_snapshot(|snap| snap.contracts.contains_key(&contract_id)) {
                return Some(RouteExecutionResult::error(404, format!("Contract {contract_id} not found")));
            }
            emit_ui_window_open(ctx, caller_id, "contract", contract_id, format!("contract:{contract_id}"));
            ctx.notify(
                NotificationTarget::Character(caller_id),
                "OnShowContract",
                "charid",
                vec![PyRep::Integer(contract_id)],
            );
            Some(RouteExecutionResult::no_content(0))
        }

        ("POST", "/ui/openwindow/information") => {
            let caller_id = claims.map(|c| c.character_id).unwrap_or(0);
            if let Err(e) = ensure_online_ui_session(ctx, caller_id) {
                return Some(e);
            }
            let Some(raw_tid) = query_params.get("target_id") else {
                return Some(RouteExecutionResult::error(400, "Missing required query parameter 'target_id'"));
            };
            let Some(target_id) = raw_tid.parse::<i64>().ok().filter(|&id| id > 0) else {
                return Some(RouteExecutionResult::error(400, format!("Invalid target_id '{raw_tid}'")));
            };
            emit_ui_window_open(ctx, caller_id, "information", target_id, format!("information:{target_id}"));
            ctx.notify(
                NotificationTarget::Character(caller_id),
                "OnShowOwnerDetailsWindow",
                "charid",
                vec![PyRep::Integer(target_id)],
            );
            Some(RouteExecutionResult::no_content(0))
        }

        ("POST", "/ui/openwindow/marketdetails") => {
            let caller_id = claims.map(|c| c.character_id).unwrap_or(0);
            if let Err(e) = ensure_online_ui_session(ctx, caller_id) {
                return Some(e);
            }
            let Some(raw_tid) = query_params.get("type_id") else {
                return Some(RouteExecutionResult::error(400, "Missing required query parameter 'type_id'"));
            };
            let Some(type_id) = raw_tid.parse::<i64>().ok().filter(|&id| id > 0) else {
                return Some(RouteExecutionResult::error(400, format!("Invalid type_id '{raw_tid}'")));
            };
            let Ok(tid_u32) = u32::try_from(type_id) else {
                return Some(RouteExecutionResult::error(400, "Query parameter 'type_id' must be a positive integer"));
            };
            if !ctx.sde.types.contains_key(&tid_u32) {
                return Some(RouteExecutionResult::error(404, format!("Inventory type {type_id} not found")));
            }
            emit_ui_window_open(ctx, caller_id, "marketdetails", type_id, format!("marketdetails:{type_id}"));
            ctx.notify(
                NotificationTarget::Character(caller_id),
                "OnShowMarketDetails",
                "charid",
                vec![PyRep::Integer(type_id)],
            );
            Some(RouteExecutionResult::no_content(0))
        }

        ("POST", "/ui/openwindow/newmail") => {
            let caller_id = claims.map(|c| c.character_id).unwrap_or(0);
            if let Err(e) = ensure_online_ui_session(ctx, caller_id) {
                return Some(e);
            }
            let payload: Value = match serde_json::from_slice::<Value>(req_body) {
                Ok(v) => v,
                Err(e) => return Some(RouteExecutionResult::error(400, format!("Invalid newmail JSON body: {e}"))),
            };
            let Some(subject) = payload.get("subject").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing required 'subject' in newmail request"));
            };
            if subject.is_empty() || subject.len() > 1000 {
                return Some(RouteExecutionResult::error(400, "newmail 'subject' must be 1..=1000 characters"));
            }
            let Some(body) = payload.get("body").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing required 'body' in newmail request"));
            };
            if body.is_empty() || body.len() > 10_000 {
                return Some(RouteExecutionResult::error(400, "newmail 'body' must be 1..=10000 characters"));
            }
            let Some(recipients) = payload.get("recipients").and_then(Value::as_array) else {
                return Some(RouteExecutionResult::error(400, "Missing required 'recipients' array in newmail request"));
            };
            if recipients.is_empty() || recipients.len() > 50 {
                return Some(RouteExecutionResult::error(400, "newmail 'recipients' must contain 1..=50 entries"));
            }
            let mut first_recipient = 0i64;
            for r in recipients {
                let Some(rid) = r.as_i64().filter(|&v| v > 0) else {
                    return Some(RouteExecutionResult::error(400, "Each recipient in 'recipients' must be a positive integer"));
                };
                if first_recipient == 0 {
                    first_recipient = rid;
                }
            }
            if payload.get("to_corp_or_alliance_id").is_some() && payload.get("to_mailing_list_id").is_some() {
                return Some(RouteExecutionResult::error(
                    422,
                    "Cannot specify both 'to_corp_or_alliance_id' and 'to_mailing_list_id'",
                ));
            }
            emit_ui_window_open(ctx, caller_id, "newmail", first_recipient, format!("{subject}:{body}"));
            ctx.notify(
                NotificationTarget::Character(caller_id),
                "OnCreateNewMailWindow",
                "charid",
                vec![
                    PyRep::Integer(first_recipient),
                    PyRep::String(subject.to_string()),
                    PyRep::String(body.to_string()),
                ],
            );
            Some(RouteExecutionResult::no_content(0))
        }

        _ => None,
    }
}
