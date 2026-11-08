//! `esi::character_mail_routes` — `WorldRepository`-backed ESI route handlers for Character
//! and Corporation Wallet balances, Loyalty points, and Mail headers/send/body/delete (`ESI-4`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{
    format_iso8601_utc, paginate_slice, parse_path_i64, RouteExecutionResult,
};
use crate::router::ServerContext;
use eve_db::{MailRecord, SsoTokenClaims};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(crate) fn mail_record_accessible_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    m: &MailRecord,
) -> bool {
    if m.recipient_id == char_id || m.sender_id == char_id {
        return true;
    }
    if snap.mail_assigned_labels.contains_key(&(char_id, m.message_id)) {
        return true;
    }
    if snap
        .corporations
        .get(&m.recipient_id)
        .is_some_and(|c| c.members.contains(&char_id) || c.ceo_id == char_id)
    {
        return true;
    }
    if let Some(ch) = snap.characters.get(&char_id) {
        if ch.corporation_id == m.recipient_id {
            return true;
        }
        if snap.alliances.get(&m.recipient_id).is_some_and(|al| {
            ch.alliance_id == Some(m.recipient_id)
                || (ch.corporation_id > 0 && al.member_corp_ids.contains(&ch.corporation_id))
        }) {
            return true;
        }
    }
    snap.mailing_lists.values().any(|ml| {
        (ml.list_id == m.recipient_id || ml.mail_ids.contains(&m.message_id))
            && ml.members.contains_key(&char_id)
    })
}

fn parse_optional_from_id(
    query_params: &HashMap<String, String>,
) -> Result<Option<i64>, RouteExecutionResult> {
    match query_params.get("from_id") {
        Some(raw) => match raw.parse::<i64>() {
            Ok(v) if v > 0 => Ok(Some(v)),
            _ => Err(RouteExecutionResult::error(
                400,
                format!("Invalid from_id '{raw}'"),
            )),
        },
        None => Ok(None),
    }
}

#[cfg(test)]
fn check_corp_wallet_access(
    corp: &eve_db::CorporationRecord,
    claims: Option<&SsoTokenClaims>,
) -> Result<(), RouteExecutionResult> {
    if let Some(c) = claims {
        let role_mask = corp.member_roles.get(&c.character_id).copied().unwrap_or(0);
        let has_role = corp.ceo_id == c.character_id
            || (role_mask
                & (eve_db::corp_roles::ROLE_DIRECTOR
                    | eve_db::corp_roles::ROLE_ACCOUNTANT
                    | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT))
                != 0;
        if (!corp.members.contains(&c.character_id) && corp.ceo_id != c.character_id) || !has_role {
            return Err(RouteExecutionResult::error(
                403,
                format!(
                    "Character {} lacks Accountant/Director/CEO role for corporation {}",
                    c.character_id, corp.corporation_id
                ),
            ));
        }
    }
    Ok(())
}

fn check_corp_wallet_access_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    corp: &eve_db::CorporationRecord,
    claims: Option<&SsoTokenClaims>,
    now_unix: u64,
) -> Result<(), RouteExecutionResult> {
    if let Some(c) = claims {
        let eff = super::router::effective_esi_corp_role_breakdown(
            snap,
            corp.corporation_id,
            c.character_id,
            now_unix,
        );
        let allowed_wallet_roles = eve_db::corp_roles::ROLE_DIRECTOR
            | eve_db::corp_roles::ROLE_ACCOUNTANT
            | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT;
        let has_role = eff.is_ceo || (eff.any_active_roles() & allowed_wallet_roles) != 0;
        if (!corp.members.contains(&c.character_id) && !eff.is_ceo) || !has_role {
            return Err(RouteExecutionResult::error(
                403,
                format!(
                    "Character {} lacks Accountant/Director/CEO role for corporation {}",
                    c.character_id, corp.corporation_id
                ),
            ));
        }
    }
    Ok(())
}

/// Dispatches Wallet balance/divisions/journal/transactions, Loyalty points/offers, and Mail OpenAPI operations (`ESI-4`).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn dispatch_character_mail_routes(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
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
        ("GET", "/characters/{character_id}/wallet/journal") => {
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
                let mut entries: Vec<_> = snap
                    .wallet_journal
                    .iter()
                    .filter(|e| e.character_id == char_id)
                    .collect();
                entries.sort_by_key(|e| std::cmp::Reverse(e.entry_id));
                let out: Vec<Value> = entries
                    .into_iter()
                    .map(|e| {
                        let ref_type = if e.entry_type.is_empty() {
                            "player_donation"
                        } else {
                            e.entry_type.as_str()
                        };
                        let desc = if e.reason.is_empty() {
                            "Wallet transaction"
                        } else {
                            e.reason.as_str()
                        };
                        let ts = snap
                            .wallet_journal_timestamps
                            .get(&e.entry_id)
                            .copied()
                            .unwrap_or(0);
                        json!({
                            "id": e.entry_id,
                            "date": format_iso8601_utc(ts),
                            "ref_type": ref_type,
                            "description": desc,
                            "amount": e.amount.to_wire_f64(),
                            "balance": e.balance_after.to_wire_f64(),
                            "reason": e.reason,
                        })
                    })
                    .collect();
                let (slice, total_pages) = match paginate_slice(&out, page, 1000) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                RouteExecutionResult::ok_paginated_json(json!(slice), 3600, total_pages)
            }))
        }
        ("GET", "/characters/{character_id}/wallet/transactions") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let from_id = match parse_optional_from_id(query_params) {
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
                let mut rows: Vec<_> = snap
                    .char_wallet_transactions
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| from_id.is_none_or(|fid| t.transaction_id < fid))
                    .collect();
                rows.sort_by_key(|t| std::cmp::Reverse(t.transaction_id));
                let out: Vec<Value> = rows
                    .into_iter()
                    .map(|t| {
                        json!({
                            "transaction_id": t.transaction_id,
                            "date": format_iso8601_utc(t.date_unix),
                            "location_id": t.location_id,
                            "type_id": t.type_id as i64,
                            "unit_price": t.unit_price,
                            "quantity": t.quantity,
                            "client_id": t.client_id,
                            "is_buy": t.is_buy,
                            "is_personal": t.is_personal,
                            "journal_ref_id": t.journal_ref_id,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }
        ("GET", "/corporations/{corporation_id}/wallets") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let now = ctx.now_unix();
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                if let Err(e) = check_corp_wallet_access_in_snap(snap, corp, claims, now) {
                    return e;
                }
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
        ("GET", "/corporations/{corporation_id}/wallets/{division}/journal") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let division = match parse_path_i64(path_params, "division") {
                Ok(v) if (1..=7).contains(&v) => v,
                Ok(v) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Wallet division {v} must be between 1 and 7"),
                    ));
                }
                Err(e) => return Some(e),
            };
            let now = ctx.now_unix();
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                if let Err(e) = check_corp_wallet_access_in_snap(snap, corp, claims, now) {
                    return e;
                }
                let mut rows = snap
                    .corp_wallet_journals
                    .get(&(corp_id, division))
                    .cloned()
                    .unwrap_or_default();
                rows.sort_by_key(|e| std::cmp::Reverse(e.id));
                let out: Vec<Value> = rows
                    .into_iter()
                    .map(|e| {
                        let mut obj = json!({
                            "id": e.id,
                            "date": format_iso8601_utc(e.date_unix),
                            "ref_type": e.ref_type,
                            "description": e.description,
                            "amount": e.amount,
                            "balance": e.balance,
                        });
                        if let Some(fp) = e.first_party_id {
                            obj["first_party_id"] = json!(fp);
                        }
                        if let Some(sp) = e.second_party_id {
                            obj["second_party_id"] = json!(sp);
                        }
                        if let Some(ref r) = e.reason {
                            obj["reason"] = json!(r);
                        }
                        obj
                    })
                    .collect();
                let (slice, total_pages) = match paginate_slice(&out, page, 1000) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                RouteExecutionResult::ok_paginated_json(json!(slice), 300, total_pages)
            }))
        }
        ("GET", "/corporations/{corporation_id}/wallets/{division}/transactions") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let division = match parse_path_i64(path_params, "division") {
                Ok(v) if (1..=7).contains(&v) => v,
                Ok(v) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Wallet division {v} must be between 1 and 7"),
                    ));
                }
                Err(e) => return Some(e),
            };
            let from_id = match parse_optional_from_id(query_params) {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let now = ctx.now_unix();
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                if let Err(e) = check_corp_wallet_access_in_snap(snap, corp, claims, now) {
                    return e;
                }
                let mut rows: Vec<_> = snap
                    .corp_wallet_transactions
                    .get(&(corp_id, division))
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| from_id.is_none_or(|fid| t.transaction_id < fid))
                    .collect();
                rows.sort_by_key(|t| std::cmp::Reverse(t.transaction_id));
                let out: Vec<Value> = rows
                    .into_iter()
                    .map(|t| {
                        json!({
                            "transaction_id": t.transaction_id,
                            "date": format_iso8601_utc(t.date_unix),
                            "location_id": t.location_id,
                            "type_id": t.type_id as i64,
                            "unit_price": t.unit_price,
                            "quantity": t.quantity,
                            "client_id": t.client_id,
                            "is_buy": t.is_buy,
                            "journal_ref_id": t.journal_ref_id,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
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
        // Public corporation loyalty store offers resolved via `get_corp_lp_offers_with_sde`
        // (dynamic runtime overrides -> Paragon V24 SDE emblems -> Tranquility ESI LP catalog).
        // parity: esi:GET /loyalty/stores/{corporation_id}/offers, feat:esi.loyalty.offers
        ("GET", "/loyalty/stores/{corporation_id}/offers") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !ctx.sde.npc_corporations.contains_key(&corp_id)
                    && !snap.corporations.contains_key(&corp_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let mut offers = crate::services::lp_store::get_corp_lp_offers_with_sde(
                    &ctx.sde, snap, corp_id,
                );
                offers.sort_by_key(|o| o.offer_id);
                let out: Vec<Value> = offers
                    .into_iter()
                    .map(|o| {
                        let req_items: Vec<Value> = o
                            .req_items
                            .into_iter()
                            .map(|(tid, qty)| {
                                json!({
                                    "type_id": tid as i64,
                                    "quantity": qty as i64,
                                })
                            })
                            .collect();
                        let mut obj = json!({
                            "offer_id": o.offer_id,
                            "type_id": o.type_id as i64,
                            "quantity": o.quantity as i64,
                            "lp_cost": o.lp_cost,
                            "isk_cost": (o.isk_cost.0 / 100) as i64,
                            "required_items": req_items,
                        });
                        if o.ak_cost > 0 {
                            obj["ak_cost"] = json!(o.ak_cost);
                        }
                        obj
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
                        mail_record_accessible_in_snap(snap, char_id, m)
                            && !snap.deleted_mails.contains(&(char_id, m.message_id))
                            && !snap.trashed_mails.contains(&(char_id, m.message_id))
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
                        let is_read = is_sender_only || snap.read_mails.contains(&(char_id, mid));
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
                                } else if snap.mailing_lists.contains_key(&m.recipient_id) {
                                    "mailing_list"
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
                let kind = match rtype {
                    "alliance" => crate::domain::mail::MailRecipientKind::Alliance,
                    "character" => crate::domain::mail::MailRecipientKind::Character,
                    "corporation" => crate::domain::mail::MailRecipientKind::Corporation,
                    "mailing_list" => crate::domain::mail::MailRecipientKind::MailingList,
                    _ => {
                        return Some(RouteExecutionResult::error(
                            400,
                            format!("Invalid recipient_type '{rtype}'"),
                        ));
                    }
                };
                if !recipient_specs
                    .iter()
                    .any(|s: &crate::domain::mail::MailRecipientSpec| {
                        s.recipient_id == rid && s.kind == kind
                    })
                {
                    recipient_specs.push(crate::domain::mail::MailRecipientSpec {
                        recipient_id: rid,
                        kind,
                    });
                }
            }
            if recipient_specs
                .iter()
                .any(|s| s.kind == crate::domain::mail::MailRecipientKind::MailingList)
            {
                crate::services::fleet_social::fleet_social_persistence::ensure_social_extras_mailing_lists_seeded(ctx);
            }
            let res = crate::domain::mail::send_mail(
                ctx,
                crate::domain::mail::SendMailInput {
                    sender_id: char_id,
                    recipients: recipient_specs,
                    to_list_id: None,
                    to_corp_or_alliance_id: None,
                    subject: subject.to_string(),
                    body: body.to_string(),
                    approved_cost_isk: Some(approved_cost),
                    max_subject_chars: 1_000,
                    max_body_chars: 10_000,
                    expand_org_recipients_in_db: false,
                    record_cspa_journal: true,
                    cspa_exempt_min_standing: -10.0,
                    cspa_exempt_same_corp: true,
                    now_unix: ctx.now_unix(),
                },
            );
            Some(match res {
                Ok(out) => {
                    ctx.esi_state.set_mail_meta(
                        char_id,
                        out.mail_id,
                        crate::esi::runtime_state::EsiMailMeta {
                            is_read: true,
                            labels: vec![2],
                        },
                    );
                    for &(rid, label_id) in &out.stored_recipient_labels {
                        ctx.esi_state.set_mail_meta(
                            rid,
                            out.mail_id,
                            crate::esi::runtime_state::EsiMailMeta {
                                is_read: false,
                                labels: vec![label_id],
                            },
                        );
                    }
                    crate::services::mail_notify::record_mail_meta(
                        ctx,
                        out.mail_id,
                        &out.to_character_ids,
                        out.to_list_id,
                        out.to_corp_or_alliance_id,
                    );
                    let mut notify_targets = vec![char_id];
                    notify_targets.extend(out.notify_recipient_ids);
                    let sent = crate::services::mail_notify::SentMail {
                        message_id: out.mail_id,
                        sender_id: char_id,
                        to_character_ids: &out.to_character_ids,
                        to_list_id: out.to_list_id,
                        to_corp_or_alliance_id: out.to_corp_or_alliance_id,
                        title: subject,
                        status_mask: 0,
                    };
                    crate::services::mail_notify::emit_mail_sent(ctx, &sent, &notify_targets);
                    RouteExecutionResult::created_json(json!(out.mail_id))
                }
                Err(e) => super::domain_err_to_esi(e),
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
                        .any(|m| mail_record_accessible_in_snap(snap, char_id, m))
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
                let is_read = is_sender_only || snap.read_mails.contains(&(char_id, mail_id));
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
                        } else if snap.mailing_lists.contains_key(&m.recipient_id) {
                            "mailing_list"
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
                    m.message_id == mail_id && mail_record_accessible_in_snap(snap, char_id, m)
                });
                if !authorized {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    ));
                }
                snap.deleted_mails.insert((char_id, mail_id));
                Ok(())
            });
            if res.is_ok() {
                ctx.esi_state.mark_mail_deleted(char_id, mail_id);
                // Deleted outside the game client: mailSvc.OnMailDeleted(messageIDs).
                crate::services::mail_notify::emit_mail_deleted(ctx, char_id, &[mail_id]);
            }
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "character_mail_routes_unit_tests.rs"]
mod character_mail_routes_unit_tests;

