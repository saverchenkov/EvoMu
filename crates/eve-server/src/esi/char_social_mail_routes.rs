//! `esi::char_social_mail_routes` — `ESI-4` Contacts (`GET/POST/PUT/DELETE /characters/{character_id}/contacts`,
//! `/contacts/labels`, `/corporations/{corporation_id}/contacts*`, `/alliances/{alliance_id}/contacts*`),
//! Mail update (`PUT /characters/{character_id}/mail/{mail_id}`), Mail labels (`GET/POST/DELETE`),
//! and Mailing lists (`GET /characters/{character_id}/mail/lists`).

#![forbid(unsafe_code)]

use crate::esi::char_social_routes::infer_contact_type;
use crate::esi::runtime_state::{
    EsiContactEntry, EsiContactNotification, EsiMailLabel, EsiMailMeta,
};
use crate::esi::sample_routes::RouteExecutionResult;
use crate::router::ServerContext;
use serde_json::{json, Value};
use std::collections::HashMap;

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

#[allow(clippy::too_many_lines)]
pub fn dispatch_char_social_mail_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    match (method, path) {
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
                            obj["label_ids"] = json!(c.label_ids);
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
                .is_some_and(|v| v == "true" || v == "1");
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
            if contact_ids.is_empty()
                || contact_ids.len() > 100
                || contact_ids.iter().any(|&id| id <= 0)
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
                let mut notif_map = ctx
                    .esi_state
                    .contact_notifications
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                for &cid in &contact_ids {
                    let ctype = infer_contact_type(ctx, snap, cid).to_string();
                    if let Some(existing) = list.iter_mut().find(|e| e.contact_id == cid) {
                        existing.standing = standing;
                        existing.is_watched = watched;
                        existing.is_blocked = standing < -5.0;
                        existing.label_ids = label_ids.clone();
                    } else {
                        list.push(EsiContactEntry {
                            contact_id: cid,
                            contact_type: ctype.clone(),
                            standing,
                            is_watched: watched,
                            is_blocked: standing < -5.0,
                            label_ids: label_ids.clone(),
                        });
                    }
                    if method == "POST" && ctype == "character" && cid != char_id {
                        let nid = ctx.esi_state.alloc_id();
                        let send_date_unix = ctx.now_unix();
                        notif_map
                            .entry(cid)
                            .or_default()
                            .push(EsiContactNotification {
                                notification_id: nid,
                                sender_character_id: char_id,
                                send_date_unix,
                                standing_level: standing,
                                message: format!("Character {char_id} added you to contacts"),
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
                let labels = ctx
                    .esi_state
                    .char_contact_labels
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = labels
                    .into_iter()
                    .map(|(lid, lname)| json!({ "label_id": lid, "label_name": lname }))
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/corporations/{corporation_id}/contacts") => {
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
                let list = ctx
                    .esi_state
                    .corp_contacts
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&corp_id)
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
                        });
                        if !c.label_ids.is_empty() {
                            obj["label_ids"] = json!(c.label_ids);
                        }
                        obj
                    })
                    .collect();
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
                let labels = ctx
                    .esi_state
                    .corp_contact_labels
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = labels
                    .into_iter()
                    .map(|(lid, lname)| json!({ "label_id": lid, "label_name": lname }))
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/alliances/{alliance_id}/contacts") => {
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
                }
                let list = ctx
                    .esi_state
                    .alliance_contacts
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&alliance_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = list
                    .into_iter()
                    .map(|c| {
                        let mut obj = json!({
                            "contact_id": c.contact_id,
                            "contact_type": c.contact_type,
                            "standing": c.standing,
                        });
                        if !c.label_ids.is_empty() {
                            obj["label_ids"] = json!(c.label_ids);
                        }
                        obj
                    })
                    .collect();
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
                }
                let labels = ctx
                    .esi_state
                    .alliance_contact_labels
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&alliance_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = labels
                    .into_iter()
                    .map(|(lid, lname)| json!({ "label_id": lid, "label_name": lname }))
                    .collect();
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
            let label_mask_for_notify = new_labels.clone();
            if new_read.is_none() && new_labels.is_none() {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail update body must specify 'read' and/or 'labels'",
                ));
            }
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                }
                if snap.deleted_mails.contains(&(char_id, mail_id))
                    || ctx.esi_state.is_mail_deleted(char_id, mail_id)
                {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    ));
                }
                let Some(first) = snap.mails.iter().find(|m| {
                    m.message_id == mail_id && (m.recipient_id == char_id || m.sender_id == char_id)
                }) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    ));
                };
                let is_sender_only = first.sender_id == char_id
                    && !snap
                        .mails
                        .iter()
                        .any(|m| m.message_id == mail_id && m.recipient_id == char_id);
                let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, is_sender_only);
                if let Some(r) = new_read {
                    meta.is_read = r;
                    if r {
                        snap.read_mails.insert((char_id, mail_id));
                    } else {
                        snap.read_mails.remove(&(char_id, mail_id));
                    }
                }
                if let Some(lbls) = new_labels {
                    meta.labels = lbls.clone();
                    snap.mail_assigned_labels.insert((char_id, mail_id), lbls);
                }
                ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
                Ok(())
            });
            if res.is_ok() {
                // Changed outside the game client: mailSvc.OnMailUpdatedByExternal(messageIDs,
                // is_read, new_label); mail label IDs are bit values, so the mask is their OR.
                let label_mask = label_mask_for_notify.map(|l| l.iter().fold(0i64, |m, id| m | id));
                crate::services::mail_notify::emit_mail_updated_by_external(ctx, char_id, &[mail_id], new_read, label_mask);
            }
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
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
                    let is_sender_only = snap
                        .mails
                        .iter()
                        .filter(|m| m.message_id == mid)
                        .all(|m| m.sender_id == char_id && m.recipient_id != char_id);
                    let meta: EsiMailMeta =
                        ctx.esi_state.get_mail_meta(char_id, mid, is_sender_only);
                    let is_read = meta.is_read || snap.read_mails.contains(&(char_id, mid));
                    let assigned = snap
                        .mail_assigned_labels
                        .get(&(char_id, mid))
                        .cloned()
                        .unwrap_or(meta.labels);
                    if !is_read {
                        total_unread += 1;
                        for lid in assigned {
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
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let lists = ctx
                    .esi_state
                    .mail_lists
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                let out: Vec<Value> = lists
                    .into_iter()
                    .map(|(id, name)| {
                        json!({
                            "mailing_list_id": id,
                            "name": name,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 120)
            }))
        }

        _ => None,
    }
}
