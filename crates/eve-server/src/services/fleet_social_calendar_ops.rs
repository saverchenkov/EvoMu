//! Calendar event creation, mutation, and response handlers.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use crate::services::fleet_social::fleet_social_chat::{extract_id_list, CalendarEventState};
use crate::services::fleet_social::fleet_social_persistence::sync_calendar_events_to_db;
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
    let date_time = args.first().and_then(|a| a.as_i64()).unwrap_or(133_000_000_000_000_000);
    let duration = args.get(1).and_then(|a| a.as_i64()).unwrap_or(60);
    let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Fleet Op");
    let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Formup in Jita");
    let importance = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
    let invitees = extract_id_list(args.get(5));
    let (char_corp, char_ally) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id))
    });
    let corp_id = session.corporation_id.unwrap_or(char_corp);
    let ally_id = session.alliance_id.or(char_ally).unwrap_or(corp_id);
    if matches!(method, "CreateCorporationEvent" | "CreateAllianceEvent") {
        let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
        if !is_ceo && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40)) {
            return Err(BridgeRpcError {
                code: "CalendarAccessDenied".into(),
                message: "Creating corporation or alliance calendar events requires Communications Officer or Director role".into(),
            });
        }
    }
    let (scope, owner_id) = match method {
        "CreateCorporationEvent" => ("Corporation".to_string(), corp_id),
        "CreateAllianceEvent" => {
            if session.alliance_id.or(char_ally).is_none() {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Creating alliance calendar events requires membership in an alliance".into(),
                });
            }
            ("Alliance".to_string(), ally_id)
        }
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
                responses.insert(inv, 3);
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
            },
        );
        id
    };
    sync_calendar_events_to_db(ctx);
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
    let (char_corp, char_ally) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id))
    });
    let corp_id = session.corporation_id.unwrap_or(char_corp);
    let ally_id = session.alliance_id.or(char_ally);
    let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
    let has_comm_or_director = is_ceo
        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40));
    let check_auth = |ev: &CalendarEventState| -> bool {
        match ev._scope.as_str() {
            "Alliance" => {
                ally_id == Some(ev.owner_id) && (ev.creator_id == char_id || has_comm_or_director)
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
        let removed = {
            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ev) = s.calendar_events.get(&eid) else {
                return Err(BridgeRpcError {
                    code: "CalendarEventNotFound".into(),
                    message: format!("Calendar event {eid} not found"),
                });
            };
            if !check_auth(ev) {
                return Err(BridgeRpcError {
                    code: "CalendarAccessDenied".into(),
                    message: "Only the event creator or authorized director may delete it".into(),
                });
            }
            s.calendar_events.remove(&eid).unwrap()
        };
        sync_calendar_events_to_db(ctx);
        match removed._scope.as_str() {
            "Corporation" => emit_remove_calendar_event(
                ctx,
                NotificationTarget::Corporation(removed.owner_id),
                eid,
                removed.event_date_time,
                true,
            ),
            "Alliance" => emit_remove_calendar_event(
                ctx,
                NotificationTarget::Alliance(removed.owner_id),
                eid,
                removed.event_date_time,
                true,
            ),
            _ => {
                for &cid in removed.responses.keys() {
                    emit_remove_calendar_event(
                        ctx,
                        NotificationTarget::Character(cid),
                        eid,
                        removed.event_date_time,
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
        sync_calendar_events_to_db(ctx);
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
        if method == "SendEventResponse" || method == "RespondToEvent" {
            resp_val = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
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
                ev.event_date_time = new_dt;
                if let Some(dur) = args
                    .get(3)
                    .and_then(|a| a.as_i64())
                    .filter(|&v| v > 0 && v <= 10_000)
                {
                    ev.duration = dur;
                }
                if let Some(t) = args.get(4).and_then(|a| a.as_str()) {
                    ev.title = t.to_string();
                }
                if let Some(d) = args.get(5).and_then(|a| a.as_str()) {
                    ev.description = d.to_string();
                }
                if let Some(imp) = args.get(6).and_then(|a| a.as_i64()) {
                    ev.importance = imp;
                }
            } else {
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
                if let Some(t) = args
                    .get(3)
                    .and_then(|a| a.as_str())
                    .or_else(|| args.get(1).and_then(|a| a.as_str()))
                {
                    ev.title = t.to_string();
                }
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
    sync_calendar_events_to_db(ctx);
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
