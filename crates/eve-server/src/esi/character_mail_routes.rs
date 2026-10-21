//! `esi::character_mail_routes` — `WorldRepository`-backed ESI route handlers for Character
//! and Corporation Wallet balances, Loyalty points, and Mail headers/send/body/delete (`ESI-4`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, parse_path_i64, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::{IskCents, MailRecord, WalletJournalEntry};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Dispatches Wallet balance/divisions, Loyalty points, and Mail OpenAPI operations (`ESI-4`).
#[allow(clippy::too_many_lines)]
pub fn dispatch_character_mail_routes(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    match (method, path_pattern) {
        // 7. Wallet tag
        ("GET", "/characters/{character_id}/wallet") => {
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
                RouteExecutionResult::ok_json(json!(ch.balance.to_wire_f64()), 120)
            }))
        }
        ("GET", "/corporations/{corporation_id}/wallets") => {
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
                let mut divisions = Vec::with_capacity(7);
                for div in 1..=7u8 {
                    let bal = corp.division_balance(div).to_wire_f64();
                    divisions.push(json!({
                        "division": div as i64,
                        "balance": bal,
                    }));
                }
                RouteExecutionResult::ok_json(json!(divisions), 300)
            }))
        }

        // 8. Loyalty tag
        ("GET", "/characters/{character_id}/loyalty/points") => {
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
                let mut lps: Vec<_> = ch.loyalty_points.iter().collect();
                lps.sort_by_key(|(&cid, _)| cid);
                let out: Vec<Value> = lps
                    .into_iter()
                    .map(|(&corp_id, &pts)| {
                        json!({
                            "corporation_id": corp_id,
                            "loyalty_points": pts,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 9. Mail tag
        ("GET", "/characters/{character_id}/mail") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let last_mail_id: Option<i64> = match query_params.get("last_mail_id") {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(v) if v > 0 => Some(v),
                    _ => {
                        return Some(RouteExecutionResult::error(
                            400,
                            format!("Invalid last_mail_id '{raw}'"),
                        ));
                    }
                },
                None => None,
            };
            let labels_filter: Option<Vec<i64>> = match query_params.get("labels") {
                Some(raw) => {
                    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
                    let mut parsed = Vec::new();
                    if !trimmed.is_empty() {
                        for part in trimmed.split(',') {
                            match part.trim().parse::<i64>() {
                                Ok(v) if v > 0 => parsed.push(v),
                                _ => {
                                    return Some(RouteExecutionResult::error(
                                        400,
                                        format!("Invalid labels query parameter '{raw}'"),
                                    ));
                                }
                            }
                        }
                    }
                    Some(parsed)
                }
                None => None,
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut msg_ids: Vec<i64> = snap
                    .mails
                    .iter()
                    .filter(|m| {
                        (m.recipient_id == char_id || m.sender_id == char_id)
                            && !snap.deleted_mails.contains(&(char_id, m.message_id))
                            && !ctx.esi_state.is_mail_deleted(char_id, m.message_id)
                            && last_mail_id.is_none_or(|lid| m.message_id < lid)
                    })
                    .map(|m| m.message_id)
                    .collect();
                msg_ids.sort_by(|a, b| b.cmp(a));
                msg_ids.dedup();
                let out: Vec<Value> = msg_ids
                    .into_iter()
                    .filter_map(|mid| {
                        let rows: Vec<_> =
                            snap.mails.iter().filter(|m| m.message_id == mid).collect();
                        let first = rows.first()?;
                        let is_sender_only = first.sender_id == char_id
                            && !rows.iter().any(|r| r.recipient_id == char_id);
                        let meta = ctx.esi_state.get_mail_meta(char_id, mid, is_sender_only);
                        let is_read = meta.is_read || snap.read_mails.contains(&(char_id, mid));
                        let labels = snap
                            .mail_assigned_labels
                            .get(&(char_id, mid))
                            .cloned()
                            .unwrap_or(meta.labels);
                        if let Some(ref req_labels) = labels_filter {
                            if !req_labels.iter().any(|l| labels.contains(l)) {
                                return None;
                            }
                        }
                        let ts = snap.mail_timestamps.get(&mid).copied().unwrap_or(0);
                        let recipients: Vec<Value> = rows
                            .iter()
                            .map(|m| {
                                let recipient_type = if snap.alliances.contains_key(&m.recipient_id)
                                {
                                    "alliance"
                                } else if snap.corporations.contains_key(&m.recipient_id)
                                    || ctx.sde.npc_corporations.contains_key(&m.recipient_id)
                                {
                                    "corporation"
                                } else {
                                    "character"
                                };
                                json!({
                                    "recipient_id": m.recipient_id,
                                    "recipient_type": recipient_type,
                                })
                            })
                            .collect();
                        Some(json!({
                            "mail_id": first.message_id,
                            "from": first.sender_id,
                            "subject": first.subject,
                            "timestamp": format_iso8601_utc(ts),
                            "is_read": is_read,
                            "labels": labels,
                            "recipients": recipients,
                        }))
                    })
                    .take(50)
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 30)
            }))
        }
        ("POST", "/characters/{character_id}/mail") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid mail JSON body: {e}"),
                    ));
                }
            };
            let approved_cost = match payload.get("approved_cost") {
                Some(v) => match v.as_i64() {
                    Some(c) if c >= 0 => c,
                    _ => {
                        return Some(RouteExecutionResult::error(
                            400,
                            "Mail 'approved_cost' must be a non-negative integer",
                        ));
                    }
                },
                None => 0,
            };
            let Some(subject) = payload.get("subject").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing mail 'subject'"));
            };
            if subject.is_empty() || subject.len() > 1000 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'subject' must be 1..=1000 characters",
                ));
            }
            let Some(body) = payload.get("body").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing mail 'body'"));
            };
            if body.len() > 10_000 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'body' exceeds 10000 characters",
                ));
            }
            let Some(recipients) = payload.get("recipients").and_then(Value::as_array) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing mail 'recipients'",
                ));
            };
            if recipients.is_empty() || recipients.len() > 50 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'recipients' must contain 1..=50 entries",
                ));
            }
            let mut recipient_specs = Vec::with_capacity(recipients.len());
            for r in recipients {
                let Some(rid) = r.get("recipient_id").and_then(Value::as_i64) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Mail recipient missing 'recipient_id'",
                    ));
                };
                let Some(rtype) = r.get("recipient_type").and_then(Value::as_str) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Mail recipient missing 'recipient_type'",
                    ));
                };
                if !matches!(
                    rtype,
                    "alliance" | "character" | "corporation" | "mailing_list"
                ) {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid recipient_type '{rtype}'"),
                    ));
                }
                if !recipient_specs.iter().any(|&(id, _)| id == rid) {
                    recipient_specs.push((rid, rtype.to_string()));
                }
            }
            let now = ctx.now_unix();
            let res: Result<i64, RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(sender) = snap.characters.get(&char_id).cloned() else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                };
                let mut total_cspa_cents = 0i64;
                for &(rid, ref rtype) in &recipient_specs {
                    if rtype == "character" && rid != char_id {
                        let same_corp = snap
                            .characters
                            .get(&rid)
                            .is_some_and(|rc| rc.corporation_id == sender.corporation_id);
                        let is_exempt_contact = ctx
                            .esi_state
                            .char_contacts
                            .read()
                            .ok()
                            .and_then(|map| map.get(&rid).cloned())
                            .and_then(|list| list.into_iter().find(|c| c.contact_id == char_id))
                            .is_some_and(|c| !c.is_blocked && c.standing > -10.0);
                        if !same_corp && !is_exempt_contact {
                            let cspa = snap
                                .character_cspa_charges
                                .get(&rid)
                                .copied()
                                .unwrap_or(IskCents(0));
                            total_cspa_cents = total_cspa_cents.saturating_add(cspa.0.max(0));
                        }
                    }
                }
                let approved_cents = IskCents::from_f64_isk(approved_cost as f64).0;
                if total_cspa_cents > approved_cents {
                    return Err(RouteExecutionResult::error(
                        400,
                        format!(
                            "CSPA charge ({:.2} ISK) exceeds approved_cost ({approved_cost} ISK)",
                            IskCents(total_cspa_cents).to_wire_f64()
                        ),
                    ));
                }
                if total_cspa_cents > 0 {
                    if sender.balance.0 < total_cspa_cents {
                        return Err(RouteExecutionResult::error(
                            400,
                            "Insufficient wallet balance to cover CSPA charge",
                        ));
                    }
                    let new_balance = IskCents(sender.balance.0 - total_cspa_cents);
                    if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.balance = new_balance;
                    }
                    snap.next_entity_id += 1;
                    let tx_id = snap.next_entity_id;
                    snap.wallet_journal.push(WalletJournalEntry {
                        entry_id: tx_id,
                        character_id: char_id,
                        entry_type: "cspa".to_string(),
                        amount: IskCents(-total_cspa_cents),
                        balance_after: new_balance,
                        reason: "CONCORD Spam Protection Act (CSPA) mail charge".to_string(),
                    });
                }
                snap.next_entity_id += 1;
                let mail_id = snap.next_entity_id;
                snap.mail_timestamps.insert(mail_id, now);
                snap.read_mails.insert((char_id, mail_id));
                snap.mail_assigned_labels.insert((char_id, mail_id), vec![2]);
                ctx.esi_state.set_mail_meta(
                    char_id,
                    mail_id,
                    crate::esi::runtime_state::EsiMailMeta {
                        is_read: true,
                        labels: vec![2],
                    },
                );
                for (rid, rtype) in recipient_specs {
                    snap.mails.push(MailRecord {
                        message_id: mail_id,
                        sender_id: char_id,
                        recipient_id: rid,
                        subject: subject.to_string(),
                        body: body.to_string(),
                    });
                    if rid != char_id {
                        let label_id = match rtype.as_str() {
                            "corporation" => 4,
                            "alliance" => 8,
                            _ => 1,
                        };
                        snap.mail_assigned_labels
                            .insert((rid, mail_id), vec![label_id]);
                        ctx.esi_state.set_mail_meta(
                            rid,
                            mail_id,
                            crate::esi::runtime_state::EsiMailMeta {
                                is_read: false,
                                labels: vec![label_id],
                            },
                        );
                    }
                }
                Ok(mail_id)
            });
            Some(match res {
                Ok(mail_id) => RouteExecutionResult::created_json(json!(mail_id)),
                Err(e) => e,
            })
        }
        ("GET", "/characters/{character_id}/mail/{mail_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let mail_id = match parse_path_i64(path_params, "mail_id") {
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
                if snap.deleted_mails.contains(&(char_id, mail_id))
                    || ctx.esi_state.is_mail_deleted(char_id, mail_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    );
                }
                let rows: Vec<_> = snap
                    .mails
                    .iter()
                    .filter(|m| m.message_id == mail_id)
                    .collect();
                if rows.is_empty()
                    || !rows
                        .iter()
                        .any(|m| m.recipient_id == char_id || m.sender_id == char_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    );
                }
                let first = rows[0];
                let is_sender_only = first.sender_id == char_id
                    && !rows.iter().any(|r| r.recipient_id == char_id);
                let meta = ctx.esi_state.get_mail_meta(char_id, mail_id, is_sender_only);
                let is_read = meta.is_read || snap.read_mails.contains(&(char_id, mail_id));
                let labels = snap
                    .mail_assigned_labels
                    .get(&(char_id, mail_id))
                    .cloned()
                    .unwrap_or(meta.labels);
                let ts = snap.mail_timestamps.get(&mail_id).copied().unwrap_or(0);
                let recipients: Vec<Value> = rows
                    .iter()
                    .map(|m| {
                        let recipient_type = if snap.alliances.contains_key(&m.recipient_id) {
                            "alliance"
                        } else if snap.corporations.contains_key(&m.recipient_id)
                            || ctx.sde.npc_corporations.contains_key(&m.recipient_id)
                        {
                            "corporation"
                        } else {
                            "character"
                        };
                        json!({
                            "recipient_id": m.recipient_id,
                            "recipient_type": recipient_type,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "from": first.sender_id,
                        "subject": first.subject,
                        "body": first.body,
                        "timestamp": format_iso8601_utc(ts),
                        "read": is_read,
                        "labels": labels,
                        "recipients": recipients,
                    }),
                    30,
                )
            }))
        }
        ("DELETE", "/characters/{character_id}/mail/{mail_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let mail_id = match parse_path_i64(path_params, "mail_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
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
                let authorized = snap.mails.iter().any(|m| {
                    m.message_id == mail_id && (m.recipient_id == char_id || m.sender_id == char_id)
                });
                if !authorized {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    ));
                }
                snap.deleted_mails.insert((char_id, mail_id));
                ctx.esi_state.mark_mail_deleted(char_id, mail_id);
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
