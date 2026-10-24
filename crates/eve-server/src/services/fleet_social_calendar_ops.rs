//! Calendar event creation, mutation, and response handlers.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use crate::services::fleet_social::fleet_social_chat::{extract_id_list, CalendarEventState};
use crate::services::fleet_social::fleet_social_persistence::persist_social_state;
use crate::services::fleet_social::get_effective_corp_roles;
use crate::services::mail_notify::{
    emit_edit_calendar_event, emit_new_calendar_event, emit_remove_calendar_event,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// V24 `corpRoleChatManager` (Communications Officer) bit (`1 << 55 = 36_028_797_018_963_968`, `EVE_Roles.h:162`).
pub const ROLE_CHAT_MANAGER: u64 = 1u64 << 55;

fn has_calendar_manager_role(is_ceo: bool, roles: u64) -> bool {
    is_ceo
        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR)
        || eve_db::corp_roles::has_role(roles, ROLE_CHAT_MANAGER)
}

pub fn create_ccp_calendar_event(
    ctx: &ServerContext,
    creator_id: i64,
    date_time: i64,
    duration: i64,
    importance: i64,
    title: &str,
    desc: &str,
) -> Result<PyRep, BridgeRpcError> {
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    if date_time < now_ft {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Cannot create a calendar event in the past".into(),
        });
    }
    if !(0..=10_000).contains(&duration) {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Calendar event duration must be between 0 and 10000 minutes".into(),
        });
    }
    let clean_title = title.trim();
    if clean_title.is_empty() {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "CalendarEventMustSpecifyTitle: Event title cannot be empty".into(),
        });
    }
    let max_db_id = ctx
        .db
        .with_snapshot(|snap| snap.calendar_events.keys().copied().max().unwrap_or(0));
    let eid = {
        let mut s = ctx.social_extras.lock().unwrap();
        s.next_id = s.next_id.max(max_db_id) + 1;
        let id = s.next_id;
        s.calendar_events.insert(
            id,
            CalendarEventState {
                event_id: id,
                owner_id: 1,
                creator_id,
                event_date_time: date_time,
                duration,
                importance,
                title: clean_title.to_string(),
                description: desc.trim().to_string(),
                _scope: "CCP".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        id
    };
    persist_social_state(ctx);
    let online_cids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| ctx.is_character_online(c.character_id, c.user_id))
            .map(|c| c.character_id)
            .collect()
    });
    for cid in online_cids {
        emit_new_calendar_event(
            ctx,
            NotificationTarget::Character(cid),
            eid,
            1,
            date_time,
            duration,
            clean_title,
            importance,
            true,
        );
    }
    Ok(PyRep::keyval([
        ("eventID", PyRep::Integer(eid)),
        ("ownerID", PyRep::Integer(1)),
        ("title", PyRep::String(clean_title.to_string())),
        ("scope", PyRep::String("CCP".into())),
    ]))
}

pub(crate) fn handle_calendar_create(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (char_corp, char_ally, max_db_id) = ctx.db.with_snapshot(|snap| {
        let (cc, ca) = snap
            .characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id));
        (cc, ca, snap.calendar_events.keys().copied().max().unwrap_or(0))
    });
    let corp_id = session.corporation_id.unwrap_or(char_corp);
    let ally_id = session.alliance_id.or(char_ally).unwrap_or(corp_id);

    if method == "CreateCorporationEvent" {
        let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
        if !has_calendar_manager_role(is_ceo, roles) {
            return Err(BridgeRpcError {
                code: "CalendarAccessDenied".into(),
                message: "Creating corporation calendar events requires Communications Officer or Director role".into(),
            });
        }
    } else if method == "CreateAllianceEvent" {
        if session.alliance_id.or(char_ally).is_none() {
            return Err(BridgeRpcError {
                code: "CalendarAccessDenied".into(),
                message: "Creating alliance calendar events requires membership in an alliance".into(),
            });
        }
        let is_executor = ctx.db.with_snapshot(|snap| {
            snap.alliances.get(&ally_id).map(|a| a.executor_corp_id == corp_id).unwrap_or(false)
        });
        let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
        if !is_executor || !has_calendar_manager_role(is_ceo, roles) {
            return Err(BridgeRpcError {
                code: "CalendarAccessDenied".into(),
                message: "Creating alliance calendar events requires being a Director or Communications Officer of the executor corporation".into(),
            });
        }
    }

    let Some(date_time) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Calendar event requires a valid dateTime".into(),
        });
    };
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    if date_time < now_ft {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Cannot create a calendar event in the past".into(),
        });
    }
    let duration = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if !(0..=10_000).contains(&duration) {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Calendar event duration must be between 0 and 10000 minutes".into(),
        });
    }
    let Some(title) = args
        .get(2)
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "CalendarEventMustSpecifyTitle: Event title cannot be empty".into(),
        });
    };
    let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("");
    let importance = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
    let invitees = extract_id_list(args.get(5));

    let (scope, owner_id) = match method {
        "CreateCorporationEvent" => ("Corporation".to_string(), corp_id),
        "CreateAllianceEvent" => ("Alliance".to_string(), ally_id),
        _ => ("Personal".to_string(), char_id),
    };
    let eid = {
        let mut s = ctx.social_extras.lock().unwrap();
        s.next_id = s.next_id.max(max_db_id) + 1;
        let id = s.next_id;
        let mut responses = HashMap::new();
        responses.insert(char_id, 4);
        for &inv in &invitees {
            if inv != char_id {
                responses.insert(inv, 3); // initial invited status is 3 (not responded)
            }
        }
        s.calendar_events.insert(
            id,
            CalendarEventState {
                event_id: id,
                owner_id,
                creator_id: char_id,
                event_date_time: date_time,
                duration,
                importance,
                title: title.to_string(),
                description: desc.to_string(),
                _scope: scope,
                responses,
                is_deleted: false,
            },
        );
        id
    };
    persist_social_state(ctx);
    match method {
        "CreateCorporationEvent" => emit_new_calendar_event(
            ctx,
            NotificationTarget::Corporation(corp_id),
            eid,
            owner_id,
            date_time,
            duration,
            title,
            importance,
            true,
        ),
        "CreateAllianceEvent" => emit_new_calendar_event(
            ctx,
            NotificationTarget::Alliance(ally_id),
            eid,
            owner_id,
            date_time,
            duration,
            title,
            importance,
            true,
        ),
        _ => {
            for &inv in &invitees {
                if inv != char_id {
                    emit_new_calendar_event(
                        ctx,
                        NotificationTarget::Character(inv),
                        eid,
                        owner_id,
                        date_time,
                        duration,
                        title,
                        importance,
                        true,
                    );
                }
            }
        }
    }
    Ok(PyRep::keyval([
        ("eventID", PyRep::Integer(eid)),
        ("ownerID", PyRep::Integer(owner_id)),
        ("title", PyRep::String(title.to_string())),
    ]))
}

pub(crate) fn handle_calendar_mutate(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (char_corp, char_ally, is_ally_executor) = ctx.db.with_snapshot(|snap| {
        let (cc, ca) = snap
            .characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id));
        let eff_corp = session.corporation_id.unwrap_or(cc);
        let eff_ally = session.alliance_id.or(ca);
        let exec = eff_ally
            .and_then(|aid| snap.alliances.get(&aid))
            .is_some_and(|a| a.executor_corp_id == eff_corp);
        (cc, ca, exec)
    });
    let corp_id = session.corporation_id.unwrap_or(char_corp);
    let ally_id = session.alliance_id.or(char_ally);
    let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
    let has_comm_or_director = has_calendar_manager_role(is_ceo, roles);
    let check_auth = |ev: &CalendarEventState| -> bool {
        match ev._scope.as_str() {
            "Alliance" => {
                ally_id == Some(ev.owner_id)
                    && (ev.creator_id == char_id || (is_ally_executor && has_comm_or_director))
            }
            "Corporation" => {
                corp_id == ev.owner_id && (ev.creator_id == char_id || has_comm_or_director)
            }
            _ => ev.owner_id == char_id || ev.creator_id == char_id,
        }
    };

    let eid = match args.first().and_then(|a| a.as_i64()) {
        Some(id) if id > 0 => id,
        _ => {
            return Err(BridgeRpcError {
                code: "CalendarEventNotFound".into(),
                message: "Calendar mutation requires a valid positive eventID".into(),
            })
        }
    };
    if method == "DeleteEvent" {
        let (scope, owner_id, dt, participants) = {
            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ev) = s.calendar_events.get_mut(&eid) else {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: format!("Calendar event {eid} not found"),
                });
            };
            if ev.is_deleted {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: format!("Calendar event {eid} not found"),
                });
            }
            if !check_auth(ev) {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Only the event creator or authorized director may delete it".into(),
                });
            }
            ev.is_deleted = true;
            (
                ev._scope.clone(),
                ev.owner_id,
                ev.event_date_time,
                ev.responses.keys().copied().collect::<Vec<_>>(),
            )
        };
        persist_social_state(ctx);
        match scope.as_str() {
            "Corporation" => emit_remove_calendar_event(
                ctx,
                NotificationTarget::Corporation(owner_id),
                eid,
                dt,
                true,
            ),
            "Alliance" => emit_remove_calendar_event(
                ctx,
                NotificationTarget::Alliance(owner_id),
                eid,
                dt,
                true,
            ),
            _ => {
                for cid in participants {
                    emit_remove_calendar_event(
                        ctx,
                        NotificationTarget::Character(cid),
                        eid,
                        dt,
                        true,
                    );
                }
            }
        }
        return Ok(PyRep::keyval([
            ("eventID", PyRep::Integer(eid)),
            ("deleted", PyRep::Bool(true)),
        ]));
    }

    if method == "UpdateEventParticipants" {
        let chars_to_add = extract_id_list(args.get(1));
        let chars_to_remove = extract_id_list(args.get(2));
        let (owner_id, dt, dur, title, imp, added, removed) = {
            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ev) = s.calendar_events.get_mut(&eid) else {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: format!("Calendar event {eid} not found"),
                });
            };
            if ev.is_deleted {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: format!("Calendar event {eid} not found"),
                });
            }
            if !check_auth(ev) {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Only the event owner or authorized director may update participants"
                        .into(),
                });
            }
            let (mut added, mut removed) = (Vec::new(), Vec::new());
            for cid in chars_to_add {
                if cid != ev.owner_id && !ev.responses.contains_key(&cid) {
                    ev.responses.insert(cid, 3);
                    added.push(cid);
                }
            }
            for cid in chars_to_remove {
                if cid != ev.owner_id && ev.responses.remove(&cid).is_some() {
                    removed.push(cid);
                }
            }
            (
                ev.owner_id,
                ev.event_date_time,
                ev.duration,
                ev.title.clone(),
                ev.importance,
                added,
                removed,
            )
        };
        persist_social_state(ctx);
        for cid in added {
            emit_new_calendar_event(
                ctx,
                NotificationTarget::Character(cid),
                eid,
                owner_id,
                dt,
                dur,
                &title,
                imp,
                true,
            );
        }
        for cid in removed {
            emit_remove_calendar_event(ctx, NotificationTarget::Character(cid), eid, dt, false);
        }
        return Ok(PyRep::keyval([
            ("eventID", PyRep::Integer(eid)),
            ("status", PyRep::Integer(1)),
        ]));
    }

    let mut resp_val = 1i64;
    let mut edit_info = None;
    {
        let mut s = ctx.social_extras.lock().unwrap();
        let Some(ev) = s.calendar_events.get_mut(&eid) else {
            return Err(BridgeRpcError {
                code: "CalendarEventNotFound".into(),
                message: format!("Calendar event {eid} not found"),
            });
        };
        if ev.is_deleted {
            return Err(BridgeRpcError {
                code: "CalendarEventNotFound".into(),
                message: format!("Calendar event {eid} not found"),
            });
        }
        if method == "SendEventResponse" {
            let is_allowed = ev.owner_id == char_id
                || ev.creator_id == char_id
                || ev.responses.contains_key(&char_id)
                || (ev._scope == "Corporation" && corp_id == ev.owner_id)
                || (ev._scope == "Alliance" && ally_id == Some(ev.owner_id));
            if !is_allowed {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "You are not invited to this event".into(),
                });
            }
            let Some(arg_owner) = args.get(1).and_then(|a| a.as_i64()) else {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SendEventResponse requires (eventID, ownerID, response)".into(),
                });
            };
            if arg_owner != ev.owner_id {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: "Event ownerID mismatch".into(),
                });
            }
            let raw_resp = args.get(2).and_then(|a| a.as_i64());
            // Valid client RSVP codes in `eve/common/script/mgt/calendarConst.pyj`:
            // 2 = responseDeclined, 4 = responseAccepted, 5 = responseMaybe (1 = Deleted, 3 = Uninvited/NotResponded).
            let Some(r) = raw_resp.filter(|v| matches!(v, 2 | 4 | 5)) else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Invalid calendar event response status (must be 2=Declined, 4=Accepted, or 5=Tentative)".into(),
                });
            };
            resp_val = r;
            ev.responses.insert(char_id, resp_val);
        } else {
            let expected_scope = match method {
                "EditCorporationEvent" => "Corporation",
                "EditAllianceEvent" => "Alliance",
                _ => "Personal",
            };
            if ev._scope != expected_scope || !check_auth(ev) {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Only the event creator or authorized director may edit it".into(),
                });
            }
            let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
            let mut old_dt = ev.event_date_time;
            if args.len() >= 7 {
                let new_dt = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                if new_dt < now_ft {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "Cannot move a calendar event into the past".into(),
                    });
                }
                let dur_opt = args.get(3).and_then(|a| a.as_i64());
                if let Some(dur) = dur_opt {
                    if !(0..=10_000).contains(&dur) {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "Calendar event duration must be between 0 and 10000 minutes".into(),
                        });
                    }
                }
                if let Some(o) = args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 10_000) {
                    old_dt = o;
                }
                let Some(t) = args
                    .get(4)
                    .and_then(|a| a.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "CalendarEventMustSpecifyTitle: Event title cannot be empty".into(),
                    });
                };
                ev.event_date_time = new_dt;
                if let Some(dur) = dur_opt {
                    ev.duration = dur;
                } else if matches!(args.get(3), Some(PyRep::None)) {
                    ev.duration = 0;
                }
                ev.title = t.to_string();
                if let Some(d) = args.get(5).and_then(|a| a.as_str()) {
                    ev.description = d.to_string();
                }
                if let Some(imp) = args.get(6).and_then(|a| a.as_i64()) {
                    ev.importance = imp;
                }
            } else {
                let dt_opt = args.get(1).and_then(|a| a.as_i64());
                if let Some(dt) = dt_opt {
                    if dt < now_ft {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "Cannot move a calendar event into the past".into(),
                        });
                    }
                }
                let dur_opt = args.get(2).and_then(|a| a.as_i64());
                if let Some(dur) = dur_opt {
                    if !(0..=10_000).contains(&dur) {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "Calendar event duration must be between 0 and 10000 minutes".into(),
                        });
                    }
                }
                let Some(t) = args
                    .get(3)
                    .and_then(|a| a.as_str())
                    .or_else(|| args.get(1).and_then(|a| a.as_str()))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "CalendarEventMustSpecifyTitle: Event title cannot be empty".into(),
                    });
                };
                if let Some(dt) = dt_opt {
                    ev.event_date_time = dt;
                }
                if let Some(dur) = dur_opt {
                    ev.duration = dur;
                } else if matches!(args.get(2), Some(PyRep::None)) {
                    ev.duration = 0;
                }
                ev.title = t.to_string();
                if let Some(d) = args
                    .get(4)
                    .and_then(|a| a.as_str())
                    .or_else(|| args.get(2).and_then(|a| a.as_str()))
                {
                    ev.description = d.to_string();
                }
                if let Some(imp) = args.get(5).and_then(|a| a.as_i64()) {
                    ev.importance = imp;
                }
            }
            let participants: Vec<i64> = ev.responses.keys().copied().collect();
            edit_info = Some((
                ev._scope.clone(),
                ev.owner_id,
                old_dt,
                ev.event_date_time,
                ev.duration,
                ev.title.clone(),
                ev.importance,
                participants,
            ));
        }
    }
    persist_social_state(ctx);
    if let Some((scope, owner_id, old_dt, dt, dur, title, imp, participants)) = edit_info {
        match scope.as_str() {
            "Corporation" => emit_edit_calendar_event(
                ctx,
                NotificationTarget::Corporation(owner_id),
                eid,
                owner_id,
                old_dt,
                dt,
                dur,
                &title,
                imp,
            ),
            "Alliance" => emit_edit_calendar_event(
                ctx,
                NotificationTarget::Alliance(owner_id),
                eid,
                owner_id,
                old_dt,
                dt,
                dur,
                &title,
                imp,
            ),
            _ => {
                for cid in participants {
                    emit_edit_calendar_event(
                        ctx,
                        NotificationTarget::Character(cid),
                        eid,
                        owner_id,
                        old_dt,
                        dt,
                        dur,
                        &title,
                        imp,
                    );
                }
            }
        }
    }
    Ok(PyRep::keyval([
        ("eventID", PyRep::Integer(eid)),
        ("status", PyRep::Integer(resp_val)),
    ]))
}
