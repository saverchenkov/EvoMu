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

pub(crate) fn handle_calendar_create(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (char_corp, char_ally) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id))
    });
    let corp_id = session.corporation_id.unwrap_or(char_corp);
    let ally_id = session.alliance_id.or(char_ally).unwrap_or(corp_id);

    if method == "CreateCorporationEvent" {
        let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
        if !is_ceo && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40)) {
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
        let has_director = is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40));
        if !is_executor || !has_director {
            return Err(BridgeRpcError {
                code: "CalendarAccessDenied".into(),
                message: "Creating alliance calendar events requires being a Director of the executor corporation".into(),
            });
        }
    }

    let Some(raw_dt) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Calendar event requires a valid dateTime".into(),
        });
    };
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    let date_time = if method == "CreateNewEvent" && raw_dt > 0 && raw_dt < 10_000_000_000 {
        (raw_dt + 11_644_473_600) * 10_000_000
    } else {
        raw_dt
    };
    if date_time < now_ft {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Cannot create a calendar event in the past".into(),
        });
    }
    let duration = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if duration <= 0 {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Calendar event requires a positive duration".into(),
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
        s.next_id += 1;
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
    let has_comm_or_director = is_ceo
        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40));
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
        if method == "SendEventResponse" || method == "RespondToEvent" {
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
            let raw_resp = if args.len() >= 3 {
                args.get(2).and_then(|a| a.as_i64())
            } else {
                args.get(1).and_then(|a| a.as_i64())
            };
            let Some(r) = raw_resp.filter(|v| (1..=5).contains(v)) else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Invalid calendar event response status".into(),
                });
            };
            resp_val = r;
            ev.responses.insert(char_id, resp_val);
        } else {
            if !check_auth(ev) {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Only the event creator or authorized director may edit it".into(),
                });
            }
            let mut old_dt = ev.event_date_time;
            if let Some(new_dt) = args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 10_000) {
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
                if let Some(dur) = args
                    .get(3)
                    .and_then(|a| a.as_i64())
                    .filter(|&v| v > 0 && v <= 10_000)
                {
                    ev.duration = dur;
                }
                ev.title = t.to_string();
                if let Some(d) = args.get(5).and_then(|a| a.as_str()) {
                    ev.description = d.to_string();
                }
                if let Some(imp) = args.get(6).and_then(|a| a.as_i64()) {
                    ev.importance = imp;
                }
            } else {
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
                if let Some(dt) = args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 10_000) {
                    ev.event_date_time = dt;
                }
                if let Some(dur) = args
                    .get(2)
                    .and_then(|a| a.as_i64())
                    .filter(|&v| v > 0 && v <= 10_000)
                {
                    ev.duration = dur;
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
