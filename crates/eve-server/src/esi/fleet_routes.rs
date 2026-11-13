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

use crate::domain::fleets::{
    self, InviteFleetMemberInput, MoveFleetMemberInput, UpdateFleetSettingsInput,
};
use crate::esi::domain_err_to_esi;
use crate::esi::sample_routes::{format_iso8601_utc, parse_path_i64, RouteExecutionResult};
use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_notify as fleet_notify;
use eve_db::SsoTokenClaims;
use eve_proto::PyRep;
use serde_json::{json, Value};
use std::collections::HashMap;

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
            Some(match fleets::get_character_fleet(ctx, char_id) {
                Ok(m) => {
                    let (role_str, _) = fleets::fleet_role_to_esi_str(m.role);
                    RouteExecutionResult::ok_json(
                        json!({
                            "fleet_id": m.fleet_id,
                            "fleet_boss_id": m.fleet_boss_id,
                            "wing_id": m.wing_id.unwrap_or(-1),
                            "squad_id": m.squad_id.unwrap_or(-1),
                            "role": role_str,
                        }),
                        60,
                    )
                }
                Err(e) => domain_err_to_esi(e),
            })
        }

        ("GET", "/fleets/{fleet_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(
                match fleets::get_fleet_settings(ctx, fleet_id, claims.map(|c| c.character_id)) {
                    Ok(s) => RouteExecutionResult::ok_json(
                        json!({
                            "motd": s.motd,
                            "is_free_move": s.is_free_move,
                            "is_registered": s.is_registered,
                            "is_voice_enabled": s.is_voice_enabled,
                        }),
                        5,
                    ),
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            let new_motd = payload.get("motd").and_then(Value::as_str).map(str::to_string);
            let new_free_move = payload.get("is_free_move").and_then(Value::as_bool);
            if new_motd.is_none() && new_free_move.is_none() {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fleet update must specify 'motd' and/or 'is_free_move'",
                ));
            }
            Some(
                match fleets::update_fleet_settings(
                    ctx,
                    UpdateFleetSettingsInput {
                        fleet_id,
                        caller_char_id: claims.map(|c| c.character_id),
                        require_boss_only: false,
                        motd: new_motd,
                        is_free_move: new_free_move,
                        is_registered: None,
                        is_voice_enabled: None,
                        is_loot_logging: None,
                    },
                ) {
                    Ok(out) => {
                        if let Some(ref motd) = out.updated_motd {
                            fleet_notify::emit_motd(
                                ctx,
                                NotificationTarget::Fleet(fleet_id),
                                motd,
                                true,
                            );
                        }
                        if out.old_free_move != out.new_free_move {
                            fleet_notify::sync_fleet_options_to_db(ctx);
                            fleet_notify::emit_options_changed(
                                ctx,
                                fleet_id,
                                fleet_notify::options_keyval(
                                    out.old_free_move,
                                    out.old_registered,
                                    out.auto_join_squad_id,
                                ),
                                fleet_notify::options_keyval(
                                    out.new_free_move,
                                    out.new_registered,
                                    out.auto_join_squad_id,
                                ),
                            );
                        }
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
        }

        ("GET", "/fleets/{fleet_id}/members") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(
                match fleets::list_fleet_members(ctx, fleet_id, claims.map(|c| c.character_id)) {
                    Ok(members) => {
                        let out: Vec<Value> = members
                            .into_iter()
                            .map(|m| {
                                let (role_str, _) = fleets::fleet_role_to_esi_str(m.role);
                                let mut obj = json!({
                                    "character_id": m.character_id,
                                    "ship_type_id": m.ship_type_id,
                                    "wing_id": m.wing_id,
                                    "squad_id": m.squad_id,
                                    "role": role_str,
                                    "role_name": m.role_name,
                                    "join_time": format_iso8601_utc(m.join_time_unix),
                                    "takes_fleet_warp": m.takes_fleet_warp,
                                    "solar_system_id": m.solar_system_id,
                                });
                                if let Some(station_id) = m.station_id {
                                    obj["station_id"] = json!(station_id);
                                }
                                obj
                            })
                            .collect();
                        RouteExecutionResult::ok_json(json!(out), 5)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            let Some(role) = fleets::parse_esi_fleet_role(role_str) else {
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
            let inviter_override = claims.map(|c| c.character_id).filter(|&id| id > 0);
            Some(
                match fleets::invite_fleet_member(
                    ctx,
                    InviteFleetMemberInput {
                        fleet_id,
                        caller_char_id: claims.map(|c| c.character_id),
                        inviter_id_override: inviter_override,
                        invitee_id,
                        role,
                        wing_id,
                        squad_id,
                        enforce_online_and_capacity_checks: false,
                        validate_hierarchy_strict: true,
                        now_unix: ctx.now_unix(),
                    },
                ) {
                    Ok(out) => {
                        ctx.emit_typed(
                            NotificationTarget::Character(out.invitee_id),
                            crate::router::notify_registry::OnFleetInvite {
                                fleet_id: out.fleet_id,
                                invite_id: out.inviter_id,
                                msg_name: PyRep::Integer(out.wing_id),
                                msg_dict: PyRep::Integer(out.squad_id),
                                role_num: Some(out.role_num),
                            },
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            let Some(role) = fleets::parse_esi_fleet_role(role_str) else {
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
            Some(
                match fleets::move_fleet_member(
                    ctx,
                    MoveFleetMemberInput {
                        fleet_id,
                        caller_char_id: claims.map(|c| c.character_id),
                        target_id: member_id,
                        new_role: role,
                        wing_id,
                        squad_id,
                        strict_esi_wing_squad: true,
                        allow_fallback_missing_wing_squad: true,
                        enforce_session_change_timer: false,
                        now_unix: ctx.now_unix(),
                    },
                ) {
                    Ok(out) => {
                        fleet_notify::emit_member_changed(
                            ctx,
                            fleet_id,
                            &out.before,
                            &out.after,
                            out.is_only_member,
                        );
                        ctx.emit_typed(
                            NotificationTarget::Character(member_id),
                            crate::router::notify_registry::OnFleetMove,
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::kick_or_remove_fleet_member(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    member_id,
                    true,
                ) {
                    Ok(outcome) => {
                        fleet_notify::emit_fleet_member_removed(ctx, &outcome);
                        if outcome.was_member {
                            fleet_notify::emit_fleet_session_cleared(
                                ctx,
                                fleet_id,
                                member_id,
                                outcome.old_role_num,
                            );
                        }
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
        }

        ("GET", "/fleets/{fleet_id}/wings") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(
                match fleets::list_fleet_wings(ctx, fleet_id, claims.map(|c| c.character_id)) {
                    Ok(wings) => {
                        let out: Vec<Value> = wings
                            .into_iter()
                            .map(|w| {
                                let squads: Vec<Value> = w
                                    .squads
                                    .into_iter()
                                    .map(|s| json!({ "id": s.squad_id, "name": s.name }))
                                    .collect();
                                json!({ "id": w.wing_id, "name": w.name, "squads": squads })
                            })
                            .collect();
                        RouteExecutionResult::ok_json(json!(out), 5)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
        }

        ("POST", "/fleets/{fleet_id}/wings") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(
                match fleets::create_fleet_wing(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    None,
                    false,
                    2001,
                    false,
                ) {
                    Ok(created) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetWingAdded {
                                wing_id: created.wing_id,
                            },
                        );
                        RouteExecutionResult::created_json(json!({ "wing_id": created.wing_id }))
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::rename_fleet_wing(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    wing_id,
                    name,
                    true,
                ) {
                    Ok(renamed) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetWingNameChanged {
                                wing_id,
                                name: renamed,
                            },
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::delete_fleet_wing(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    wing_id,
                    false,
                ) {
                    Ok(()) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetWingDeleted { wing_id },
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::create_fleet_squad(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    wing_id,
                    None,
                    false,
                    3001,
                    false,
                ) {
                    Ok(created) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetSquadAdded {
                                wing_id,
                                squad_id: created.squad_id,
                            },
                        );
                        RouteExecutionResult::created_json(json!({ "squad_id": created.squad_id }))
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::rename_fleet_squad(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    squad_id,
                    name,
                    true,
                ) {
                    Ok(renamed) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetSquadNameChanged {
                                squad_id,
                                name: renamed,
                            },
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            Some(
                match fleets::delete_fleet_squad(
                    ctx,
                    fleet_id,
                    claims.map(|c| c.character_id),
                    squad_id,
                    false,
                ) {
                    Ok(()) => {
                        ctx.emit_typed(
                            NotificationTarget::Fleet(fleet_id),
                            crate::router::notify_registry::OnFleetSquadDeleted { squad_id },
                        );
                        RouteExecutionResult::no_content(0)
                    }
                    Err(e) => domain_err_to_esi(e),
                },
            )
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
            ctx.emit_typed(
                NotificationTarget::Character(caller_id),
                crate::router::notify_registry::OnShowContract { contract_id },
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
            // The only V24 server push that opens an info window is `OnShowOwnerDetailsWindow(targetID)`
            // (svc.info `__notifyevents__`, eve/client/script/ui/shared/info/infosvc.pyj:207). Its handler
            // (infosvc.pyj:4455-4461) only opens characters, corporations and alliances.
            let is_owner = ctx.db.with_snapshot(|s| {
                s.characters.contains_key(&target_id)
                    || s.corporations.contains_key(&target_id)
                    || s.alliances.contains_key(&target_id)
            }) || ctx.sde.npc_corporations.contains_key(&target_id)
                || ctx.sde.agents.contains_key(&target_id);
            if is_owner {
                ctx.emit_typed(
                    NotificationTarget::Character(caller_id),
                    crate::router::notify_registry::OnShowOwnerDetailsWindow { target_id },
                );
                return Some(RouteExecutionResult::no_content(0));
            }
            let exists = ctx.db.with_snapshot(|s| {
                s.structures.contains_key(&target_id) || s.items.contains_key(&target_id)
            }) || ctx.sde.stations.contains_key(&target_id)
                || u32::try_from(target_id).ok().is_some_and(|tid| {
                    ctx.sde.types.contains_key(&tid)
                        || ctx.sde.solar_systems.contains_key(&tid)
                        || ctx.sde.factions.contains_key(&tid)
                });
            if !exists {
                return Some(RouteExecutionResult::error(404, format!("Target {target_id} not found")));
            }
            // No V24 client push entry point opens info windows for non-owner targets.
            Some(RouteExecutionResult::error(
                501,
                format!("Opening the information window for non-owner target {target_id} has no V24 client push entry point"),
            ))
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
            ctx.emit_typed(
                NotificationTarget::Character(caller_id),
                crate::router::notify_registry::OnShowMarketDetails { type_id },
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
            let mut recip_ids = Vec::new();
            for r in recipients {
                let Some(rid) = r.as_i64().filter(|&v| v > 0) else {
                    return Some(RouteExecutionResult::error(400, "Each recipient in 'recipients' must be a positive integer"));
                };
                recip_ids.push(rid);
            }
            if payload.get("to_corp_or_alliance_id").is_some() && payload.get("to_mailing_list_id").is_some() {
                return Some(RouteExecutionResult::error(422, "Cannot specify both 'to_corp_or_alliance_id' and 'to_mailing_list_id'"));
            }
            let corp_or_alliance = payload.get("to_corp_or_alliance_id").and_then(Value::as_i64).map(PyRep::Integer).unwrap_or(PyRep::None);
            let to_list_id = payload.get("to_mailing_list_id").and_then(Value::as_i64).map(PyRep::Integer).unwrap_or(PyRep::None);
            ctx.emit_typed(
                NotificationTarget::Character(caller_id),
                crate::router::notify_registry::OnCreateNewMailWindow {
                    recipients: recip_ids,
                    corp_or_alliance_recipient: corp_or_alliance,
                    to_list_id,
                    subject: subject.to_string(),
                    body: body.to_string(),
                },
            );
            Some(RouteExecutionResult::no_content(0))
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "fleet_routes_unit_tests.rs"]
mod fleet_routes_unit_tests;

