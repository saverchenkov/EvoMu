//! Mail & Mailing Lists (`SO-1`: `mailMgr`, `mailingListsMgr`) and Calendar (`P-5`: `calendarMgr`).

#[path = "mailing_lists_ops.rs"]
mod mailing_lists_ops;

use super::fleet_social_chat::extract_id_list;
use super::fleet_social_persistence::ensure_social_extras_mailing_lists_seeded;
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{IskCents, MailLabelRecord, MailRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub(super) fn handle_mail_and_calendar(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(service, "mailMgr" | "mailingListsMgr" | "calendarMgr" | "calendarProxy") {
        return None;
    }

    ensure_social_extras_mailing_lists_seeded(ctx);

    if let Some(res) = mailing_lists_ops::handle_mail_and_mailing_lists_ops(ctx, session, service, method, args, _kwargs) {
        return Some(res);
    }

    match (service, method) {
        ("mailMgr", "SendMail") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mut recipient_ids: Vec<i64> = extract_id_list(args.first());
            // Client layout (mailSvc.SendMail): (toCharacterIDs, toListID, toCorpOrAllianceID, title,
            // body, isReplyTo, isForwardedFrom); kept as sent for OnMailSent.
            let to_character_ids = recipient_ids.clone();
            let (to_list_id, to_corp_or_alliance_id) = if args.len() >= 5 {
                (args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0), args.get(2).and_then(|a| a.as_i64()))
            } else {
                (None, None)
            };
            let mut target_label = 1i64; // 1 = Inbox, 4 = Corp, 8 = Alliance
            let mut ml_cost_transfer: Option<(i64, eve_db::IskCents)> = None;
            let (subject, body) = if args.len() >= 5 {
                if let Some(list_id) = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0) {
                    let (ml_cost, ml_owner) = {
                        let s = ctx.social_extras.lock().unwrap();
                        let ml = match s.mailing_lists.get(&list_id) {
                            Some(m) => m,
                            None => {
                                return Some(Err(BridgeRpcError {
                                    code: "UserError".into(),
                                    message: "MailingListNotFound".into(),
                                }));
                            }
                        };
                        let role = ml.members.get(&char_id).copied();
                        if char_id != ml.owner_id {
                            if role.is_none() {
                                return Some(Err(BridgeRpcError {
                                    code: "UserError".into(),
                                    message: "MailingListAccessDenied".into(),
                                }));
                            }
                            if role == Some(0) {
                                return Some(Err(BridgeRpcError {
                                    code: "UserError".into(),
                                    message: "MutedInMailingList".into(),
                                }));
                            }
                        }
                        for &rid in ml.members.keys() {
                            recipient_ids.push(rid);
                        }
                        (ml.cost, ml.owner_id)
                    };
                    let ml_cost_isk = ml_cost.clamp(0, 1_000_000_000);
                    if ml_cost_isk > 0 && char_id != ml_owner {
                        ml_cost_transfer = Some((ml_owner, eve_db::IskCents::from_isk(ml_cost_isk)));
                    }
                    recipient_ids.sort_unstable();
                    recipient_ids.dedup();
                }
                if recipient_ids.is_empty() {
                    if let Some(corp_or_alliance_id) = args.get(2).and_then(|a| a.as_i64()) {
                        let members = ctx.db.with_snapshot(|snap| -> Result<Vec<i64>, BridgeRpcError> {
                            let restrict = snap
                                .corp_governance
                                .get(&corp_or_alliance_id)
                                .map(|g| g.restrict_corp_mails)
                                .unwrap_or(false);
                            if let Some(c) = snap.corporations.get(&corp_or_alliance_id) {
                                let role = c.member_roles.get(&char_id).copied().unwrap_or(0);
                                if restrict && char_id != c.ceo_id && role == 0 {
                                    return Err(BridgeRpcError {
                                        code: "CorpMailRestricted".into(),
                                        message: "Corporation mail is restricted to leadership".into(),
                                    });
                                }
                                target_label = 4;
                                return Ok(c.members.iter().copied().collect());
                            }
                            if let Some(a) = snap.alliances.get(&corp_or_alliance_id) {
                                target_label = 8;
                                let mut m = Vec::new();
                                for cid in &a.member_corp_ids {
                                    if let Some(c) = snap.corporations.get(cid) {
                                        m.extend(c.members.iter().copied());
                                    }
                                }
                                return Ok(m);
                            }
                            Ok(Vec::new())
                        });
                        match members {
                            Ok(m) => recipient_ids.extend(m),
                            Err(e) => return Some(Err(e)),
                        }
                    }
                }
                (
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(4).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            } else {
                (
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            };

            if !recipient_ids.is_empty() {
                let expanded = ctx.db.with_snapshot(|snap| -> Result<Option<(i64, Vec<i64>)>, BridgeRpcError> {
                    let mut extra = Vec::new();
                    let mut lbl = target_label;
                    for &id in &recipient_ids {
                        if let Some(c) = snap.corporations.get(&id) {
                            let restrict = snap
                                .corp_governance
                                .get(&id)
                                .map(|g| g.restrict_corp_mails)
                                .unwrap_or(false);
                            let role = c.member_roles.get(&char_id).copied().unwrap_or(0);
                            if restrict && char_id != c.ceo_id && role == 0 {
                                return Err(BridgeRpcError {
                                    code: "CorpMailRestricted".into(),
                                    message: "Corporation mail is restricted to leadership".into(),
                                });
                            }
                            lbl = 4;
                            extra.extend(c.members.iter().copied());
                        } else if let Some(a) = snap.alliances.get(&id) {
                            lbl = 8;
                            for cid in &a.member_corp_ids {
                                if let Some(c) = snap.corporations.get(cid) {
                                    extra.extend(c.members.iter().copied());
                                }
                            }
                        }
                    }
                    if extra.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some((lbl, extra)))
                    }
                });
                match expanded {
                    Ok(Some((lbl, members))) => {
                        target_label = lbl;
                        recipient_ids = members;
                    }
                    Ok(None) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            if recipient_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRecipient".into(),
                    message: "SendMail requires at least one recipient".into(),
                }));
            }
            for &rec_id in &recipient_ids {
                if crate::services::character_contacts::is_blocked_by(ctx, rec_id, char_id) {
                    return Some(Err(BridgeRpcError {
                        code: "ContactBlocked".into(),
                        message: format!("Recipient {rec_id} has blocked sender {char_id}"),
                    }));
                }
            }
            if subject.is_empty() || subject.chars().count() > 150 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMailSubject".into(),
                    message: "Mail subject must be 1..=150 characters".into(),
                }));
            }
            if body.chars().count() > 8_000 {
                return Some(Err(BridgeRpcError {
                    code: "MailBodyTooLong".into(),
                    message: "Mail body exceeds 8,000 character limit".into(),
                }));
            }

            let (extra_contacts, extra_blocked, extra_cspa) = {
                let s = ctx.social_extras.lock().unwrap();
                let mut contacts = HashMap::new();
                let mut blocked = HashSet::new();
                let mut cspa = HashMap::new();
                for &rec_id in &recipient_ids {
                    if rec_id == char_id {
                        continue;
                    }
                    if let Some(m) = s.contacts.get(&rec_id) {
                        if let Some(&(st, _, _)) = m.get(&char_id) {
                            contacts.insert(rec_id, st);
                        }
                    }
                    if s.blocked.get(&rec_id).is_some_and(|b| b.contains(&char_id)) {
                        blocked.insert(rec_id);
                    }
                    if let Some(&ch) = s.cspa_charges.get(&rec_id) {
                        cspa.insert(rec_id, ch);
                    }
                }
                (contacts, blocked, cspa)
            };

            let mut total_cspa = IskCents(0);
            for &rec_id in &recipient_ids {
                if rec_id == char_id {
                    continue;
                }
                let friendly_contact = extra_contacts.get(&rec_id).is_some_and(|&st| st > 0.0)
                    || ctx.db.with_snapshot(|snap| {
                        snap.char_contacts.get(&rec_id).is_some_and(|list| {
                            list.iter()
                                .any(|c| c.contact_id == char_id && !c.is_blocked && c.standing > 0.0)
                        })
                    });
                if extra_blocked.contains(&rec_id) && !friendly_contact {
                    return Some(Err(BridgeRpcError {
                        code: "SenderBlockedByRecipient".into(),
                        message: format!("Recipient {rec_id} has blocked you"),
                    }));
                }
                if let Some(charge) = extra_cspa
                    .get(&rec_id)
                    .copied()
                    .or_else(|| ctx.db.with_snapshot(|snap| snap.character_cspa_charges.get(&rec_id).copied()))
                {
                    if charge.0 > 0 && !friendly_contact && target_label == 1 {
                        total_cspa = match total_cspa.checked_add(charge) {
                            Some(val) => val,
                            None => {
                                return Some(Err(BridgeRpcError {
                                    code: "InsufficientFundsForCSPA".into(),
                                    message: "Insufficient ISK to cover CSPA / mailing list cost".into(),
                                }));
                            }
                        };
                    }
                }
            }

            let now_unix = ctx.now_unix();
            let primary_recipient = recipient_ids[0];
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let char_mail_count = snap
                    .mails
                    .iter()
                    .filter(|m| {
                        (m.recipient_id == primary_recipient || m.sender_id == char_id)
                            && !snap.deleted_mails.contains(&(primary_recipient, m.message_id))
                    })
                    .count();
                if char_mail_count >= 1_000 || snap.mails.len() >= 3_000 {
                    return Err(BridgeRpcError {
                        code: "MailQuotaExceeded".into(),
                        message: "Mailbox limit reached".into(),
                    });
                }
                    let sender_ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;
                    let ml_cost_cents = ml_cost_transfer.map(|(_, c)| c).unwrap_or(eve_db::IskCents(0));
                    let total_cost = total_cspa.checked_add(ml_cost_cents).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFundsForCSPA".into(),
                        message: "Insufficient ISK to cover CSPA / mailing list cost".into(),
                    })?;
                    if sender_ch.balance < total_cost {
                        return Err(BridgeRpcError {
                            code: "InsufficientFundsForCSPA".into(),
                            message: "Insufficient ISK to cover CSPA / mailing list cost".into(),
                        });
                    }
                    if total_cspa.0 > 0 {
                        sender_ch.balance = sender_ch
                            .balance
                            .checked_sub(total_cspa)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFundsForCSPA".into(),
                                message: "Insufficient ISK to cover CSPA / mailing list cost".into(),
                            })?;
                    }
                    if let Some((owner_id, cost)) = ml_cost_transfer {
                        sender_ch.balance = sender_ch
                            .balance
                            .checked_sub(cost)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFundsForCSPA".into(),
                                message: "Insufficient ISK to cover CSPA / mailing list cost".into(),
                            })?;
                        if let Some(owner_ch) = snap.characters.get_mut(&owner_id) {
                            owner_ch.balance = owner_ch.balance.checked_add(cost).ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFundsForCSPA".into(),
                                message: "Balance overflow on mailing list owner".into(),
                            })?;
                        }
                    }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                for &rec_id in &recipient_ids {
                    snap.mails.push(MailRecord {
                        message_id: id,
                        sender_id: char_id,
                        recipient_id: rec_id,
                        subject: subject.to_string(),
                        body: body.to_string(),
                    });
                    snap.mail_assigned_labels
                        .insert((rec_id, id), vec![target_label]);
                }
                snap.mail_timestamps.insert(id, now_unix);
                snap.mail_assigned_labels.insert((char_id, id), vec![2]);
                snap.read_mails.insert((char_id, id));
                if let Some(lid) = to_list_id {
                    if let Some(ml) = snap.mailing_lists.get_mut(&lid) {
                        ml.mail_ids.insert(id);
                    }
                }
                Ok(id)
            });

            match res {
                Ok(mid) => {
                    crate::services::mail_notify::record_mail_meta(mid, &to_character_ids, to_list_id, to_corp_or_alliance_id);
                    if let Some(lid) = to_list_id {
                        let mut s = ctx.social_extras.lock().unwrap();
                        if let Some(ml) = s.mailing_lists.get_mut(&lid) {
                            ml.mail_ids.insert(mid);
                        }
                    }
                    // The sender's own mailSvc.SendMail already calls OnMailSent locally.
                    let others: Vec<i64> = recipient_ids.iter().copied().filter(|&r| r != char_id).collect();
                    let sent = crate::services::mail_notify::SentMail {
                        message_id: mid,
                        sender_id: char_id,
                        to_character_ids: &to_character_ids,
                        to_list_id,
                        to_corp_or_alliance_id,
                        title: subject,
                        status_mask: 0,
                    };
                    crate::services::mail_notify::emit_mail_sent(ctx, &sent, &others);
                    Some(Ok(PyRep::Integer(mid)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "PrimeOwners") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = extract_id_list(args.first());
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                ids.into_iter()
                    .map(|id| {
                        let name = snap
                            .characters
                            .get(&id)
                            .map(|c| c.name.clone())
                            .or_else(|| snap.corporations.get(&id).map(|c| c.name.clone()))
                            .unwrap_or_else(|| format!("Entity {id}"));
                        vec![PyRep::Integer(id), PyRep::String(name), PyRep::Integer(1373)]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["ownerID", "ownerName", "typeID"],
                rows,
            )))
        }

        ("mailMgr", "GetMailHeaders") => {
            crate::services::mail_notify::handle_mail_headers(ctx, session, method, args)
        }
        ("mailMgr", "SyncMail") => {
            crate::services::mail_notify::handle_mail_headers(ctx, session, method, args)
        }
        ("mailMgr", m) if m.eq_ignore_ascii_case("getheaders") => {
            crate::services::mail_notify::handle_mail_headers(ctx, session, method, args)
        }
        ("mailMgr", "keys") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let trashed = ctx.social_extras.lock().unwrap().trashed_mails.clone();
            let keys: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                snap.mails.iter()
                    .filter(|m| (m.recipient_id == char_id || m.sender_id == char_id)
                        && !snap.deleted_mails.contains(&(char_id, m.message_id))
                        && !trashed.contains(&(char_id, m.message_id)))
                    .map(|m| PyRep::Integer(m.message_id))
                    .collect()
            });
            Some(Ok(PyRep::List(keys)))
        }
        ("mailMgr", "iteritems") | ("mailMgr", "itervalues") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let trashed = ctx.social_extras.lock().unwrap().trashed_mails.clone();
            let items: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                snap.mails.iter()
                    .filter(|m| (m.recipient_id == char_id || m.sender_id == char_id)
                        && !snap.deleted_mails.contains(&(char_id, m.message_id))
                        && !trashed.contains(&(char_id, m.message_id)))
                    .map(|m| {
                        let read = snap.read_mails.contains(&(char_id, m.message_id));
                        let val = PyRep::keyval([
                            ("messageID", PyRep::Integer(m.message_id)),
                            ("senderID", PyRep::Integer(m.sender_id)),
                            ("title", PyRep::String(m.subject.clone())),
                            ("read", PyRep::Bool(read)),
                        ]);
                        if method == "iteritems" {
                            PyRep::Tuple(vec![PyRep::Integer(m.message_id), val])
                        } else {
                            val
                        }
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(items)))
        }
        ("mailMgr", "PokePlayerAboutChatMsgGm") => {
            crate::services::mail_notify::handle_poke_player_about_chat_msg_gm(ctx, session, args)
        }

        ("mailMgr", "GetBody") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(mail_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetBody requires messageID".into(),
                }));
            };
            let res_opt = ctx.db.try_with_mut_snapshot(|snap| {
                let matching: Vec<_> = snap.mails.iter().filter(|m| m.message_id == mail_id).cloned().collect();
                if matching.is_empty() {
                    return Ok::<_, std::convert::Infallible>(None);
                }
                let body = matching[0].body.clone();
                let authorized = crate::services::mail_notify::is_mail_authorized(snap, mail_id, char_id);
                if authorized {
                    snap.read_mails.insert((char_id, mail_id));
                    let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, false);
                    meta.is_read = true;
                    ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
                }
                Ok(Some((body, authorized)))
            }).unwrap();

            let Some((body, authorized)) = res_opt else {
                return Some(Err(BridgeRpcError {
                    code: "MailNotFound".into(),
                    message: format!("Mail message {mail_id} not found"),
                }));
            };
            if !authorized {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot read mail belonging to another character".into(),
                }));
            }
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(body.as_bytes(), 6);
            Some(Ok(PyRep::Buffer(compressed)))
        }

        ("mailMgr", "MarkAsRead")
        | ("mailMgr", "MarkAsReadByLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mail_ids = crate::services::mail_notify::resolve_mail_targets(ctx, char_id, method, args.first());
            if mail_ids.is_empty() {
                if method.ends_with("ByLabel") {
                    return Some(Ok(PyRep::keyval([
                        ("messageID", PyRep::Integer(0)),
                        ("messageIDs", PyRep::List(Vec::new())),
                        ("read", PyRep::Bool(true)),
                    ])));
                }
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsRead requires messageID or messageIDs list".into(),
                }));
            }
            if !method.ends_with("ByLabel") {
                let check_ownership = ctx.db.with_snapshot(|snap| {
                    crate::services::mail_notify::check_mails_authorized(snap, &mail_ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot modify mail belonging to another character".into(),
                    }));
                }
            }
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        snap.read_mails.insert((char_id, mid));
                        let mut meta = ctx.esi_state.get_mail_meta(char_id, mid, false);
                        meta.is_read = true;
                        ctx.esi_state.set_mail_meta(char_id, mid, meta);
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailCountersUpdate",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("messageIDs", PyRep::List(mail_ids.into_iter().map(PyRep::Integer).collect())),
                ("read", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "MarkAsUnread")
        | ("mailMgr", "MarkAsUnreadByLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mail_ids = crate::services::mail_notify::resolve_mail_targets(ctx, char_id, method, args.first());
            if mail_ids.is_empty() {
                if method.ends_with("ByLabel") {
                    return Some(Ok(PyRep::keyval([
                        ("messageID", PyRep::Integer(0)),
                        ("messageIDs", PyRep::List(Vec::new())),
                        ("read", PyRep::Bool(false)),
                    ])));
                }
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsUnread requires messageID or messageIDs list".into(),
                }));
            }
            if !method.ends_with("ByLabel") {
                let check_ownership = ctx.db.with_snapshot(|snap| {
                    crate::services::mail_notify::check_mails_authorized(snap, &mail_ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot modify mail belonging to another character".into(),
                    }));
                }
            }
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        snap.read_mails.remove(&(char_id, mid));
                        let mut meta = ctx.esi_state.get_mail_meta(char_id, mid, false);
                        meta.is_read = false;
                        ctx.esi_state.set_mail_meta(char_id, mid, meta);
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailCountersUpdate",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("messageIDs", PyRep::List(mail_ids.into_iter().map(PyRep::Integer).collect())),
                ("read", PyRep::Bool(false)),
            ])))
        }

        ("mailMgr", "MarkAllAsRead") | ("mailMgr", "MarkAllAsUnread") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mark_read = method == "MarkAllAsRead";
            let count = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let mut count = 0;
                    let mids: Vec<i64> = snap
                        .mails
                        .iter()
                        .filter(|m| m.recipient_id == char_id)
                        .map(|m| m.message_id)
                        .collect();
                    for mid in mids {
                        let changed = if mark_read {
                            snap.read_mails.insert((char_id, mid))
                        } else {
                            snap.read_mails.remove(&(char_id, mid))
                        };
                        let mut meta = ctx.esi_state.get_mail_meta(char_id, mid, false);
                        meta.is_read = mark_read;
                        ctx.esi_state.set_mail_meta(char_id, mid, meta);
                        if changed {
                            count += 1;
                        }
                    }
                    Ok::<_, std::convert::Infallible>(count)
                })
                .unwrap();
            Some(Ok(PyRep::keyval([("markedReadCount", PyRep::Integer(count))])))
        }

        ("mailMgr", "MoveFromTrash") | ("mailMgr", "MoveAllFromTrash") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mail_ids = extract_id_list(args.first());
            if !mail_ids.is_empty() && method == "MoveFromTrash" {
                let check_ownership = ctx.db.with_snapshot(|snap| {
                    crate::services::mail_notify::check_mails_authorized(snap, &mail_ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot modify mail belonging to another character".into(),
                    }));
                }
            }
            let mut restored_ids = Vec::new();
            {
                let mut s = ctx.social_extras.lock().unwrap();
                if mail_ids.is_empty() || method == "MoveAllFromTrash" {
                    for &(cid, mid) in &s.trashed_mails {
                        if cid == char_id {
                            restored_ids.push(mid);
                        }
                    }
                    s.trashed_mails.retain(|&(cid, _)| cid != char_id);
                } else {
                    for &mid in &mail_ids {
                        if s.trashed_mails.remove(&(char_id, mid)) {
                            restored_ids.push(mid);
                        }
                    }
                }
            }
            ctx.db.with_mut_snapshot(|snap| {
                if mail_ids.is_empty() || method == "MoveAllFromTrash" {
                    for &(cid, mid) in &snap.trashed_mails {
                        if cid == char_id {
                            restored_ids.push(mid);
                        }
                    }
                    snap.trashed_mails.retain(|&(cid, _)| cid != char_id);
                } else {
                    for &mid in &mail_ids {
                        if snap.trashed_mails.remove(&(char_id, mid)) {
                            restored_ids.push(mid);
                        }
                    }
                }
            });
            restored_ids.sort_unstable();
            restored_ids.dedup();
            if !restored_ids.is_empty() {
                let mail_id_reps = PyRep::List(restored_ids.iter().copied().map(PyRep::Integer).collect());
                ctx.notify(NotificationTarget::Character(char_id), "OnMailRestored", "charid", vec![mail_id_reps]);
            }
            Some(Ok(PyRep::keyval([
                ("restored", PyRep::Bool(!restored_ids.is_empty())),
                ("inTrash", PyRep::Bool(false)),
            ])))
        }

        ("mailMgr", "EmptyTrash") | ("mailMgr", "DeleteMail") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mut ids = extract_id_list(args.first());
            if method == "EmptyTrash" {
                let mut trashed_for_char: Vec<i64> = {
                    let s = ctx.social_extras.lock().unwrap();
                    s.trashed_mails
                        .iter()
                        .filter(|&&(cid, _)| cid == char_id)
                        .map(|&(_, mid)| mid)
                        .collect()
                };
                let db_trashed: Vec<i64> = ctx.db.with_snapshot(|snap| {
                    snap.trashed_mails
                        .iter()
                        .filter(|&&(cid, _)| cid == char_id)
                        .map(|&(_, mid)| mid)
                        .collect()
                });
                trashed_for_char.extend(db_trashed);
                trashed_for_char.sort_unstable();
                trashed_for_char.dedup();

                {
                    let mut s = ctx.social_extras.lock().unwrap();
                    s.trashed_mails.retain(|&(cid, _)| cid != char_id);
                }
                ctx.db.with_mut_snapshot(|snap| {
                    snap.trashed_mails.retain(|&(cid, _)| cid != char_id);
                });
                ids.extend(trashed_for_char);
            } else {
                if ids.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "DeleteMail requires messageID".into(),
                    }));
                }
                let check_ownership = ctx.db.with_snapshot(|snap| {
                    crate::services::mail_notify::check_mails_authorized(snap, &ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete mail belonging to another character".into(),
                    }));
                }
            }
            if ids.is_empty() {
                if method == "EmptyTrash" {
                    return Some(Ok(PyRep::keyval([
                        ("emptied", PyRep::Bool(true)),
                        ("deleted", PyRep::Bool(true)),
                    ])));
                }
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteMail requires messageID".into(),
                }));
            }
            {
                let mut s = ctx.social_extras.lock().unwrap();
                for &mid in &ids {
                    s.trashed_mails.remove(&(char_id, mid));
                }
            }
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &ids {
                        snap.trashed_mails.remove(&(char_id, mid));
                        snap.deleted_mails.insert((char_id, mid));
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailDeleted",
                "charid",
                vec![PyRep::List(ids.iter().copied().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(ids[0])),
                ("deleted", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "GetLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let trashed_mails = super::get_trashed_mails(ctx);
            let (custom_labels, mails, read_mails, deleted_mails, assignments) = ctx.db.with_snapshot(|snap| {
                (
                    snap.mail_labels.get(&char_id).cloned().unwrap_or_default(),
                    snap.mails.clone(),
                    snap.read_mails.clone(),
                    snap.deleted_mails.clone(),
                    snap.mail_assigned_labels.clone(),
                )
            });
            let mut all_labels = vec![
                MailLabelRecord { label_id: 1, name: "Inbox".into(), color: "#ffffff".into() },
                MailLabelRecord { label_id: 2, name: "Sent".into(), color: "#ffffff".into() },
                MailLabelRecord { label_id: 4, name: "Corp".into(), color: "#0099ff".into() },
                MailLabelRecord { label_id: 8, name: "Alliance".into(), color: "#00ff33".into() },
            ];
            all_labels.extend(custom_labels);
            let dict: Vec<(PyRep, PyRep)> = all_labels
                .into_iter()
                .map(|lbl| {
                    let unread_count = mails
                        .iter()
                        .filter(|m| {
                            if m.recipient_id != char_id {
                                return false;
                            }
                            if read_mails.contains(&(char_id, m.message_id))
                                || deleted_mails.contains(&(char_id, m.message_id))
                                || trashed_mails.contains(&(char_id, m.message_id))
                            {
                                return false;
                            }
                            let assigned = assignments
                                .get(&(char_id, m.message_id))
                                .cloned()
                                .unwrap_or_else(|| vec![1]);
                            assigned.contains(&lbl.label_id)
                        })
                        .count();
                    let kv = PyRep::keyval([
                        ("labelID", PyRep::Integer(lbl.label_id)),
                        ("name", PyRep::String(lbl.name)),
                        ("color", PyRep::String(lbl.color)),
                        ("unreadCount", PyRep::Integer(unread_count as i64)),
                    ]);
                    (PyRep::Integer(lbl.label_id), kv)
                })
                .collect();
            Some(Ok(PyRep::Dict(dict)))
        }

        ("mailMgr", "CreateLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(1).and_then(|a| a.as_str()).unwrap_or("#ffffff");
            let lid = ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.mail_labels.entry(char_id).or_default();
                let next_id = (0..24).map(|k| 16i64 << k).find(|&id| !list.iter().any(|l| l.label_id == id)).unwrap_or(16i64 << 24);
                list.push(MailLabelRecord { label_id: next_id, name: name.to_string(), color: color.to_string() });
                if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                    let esi_list = map.entry(char_id).or_insert_with(crate::esi::runtime_state::EsiRuntimeState::default_mail_labels);
                    esi_list.push(crate::esi::runtime_state::EsiMailLabel { label_id: next_id, name: name.to_string(), color: color.to_string() });
                }
                Ok::<_, std::convert::Infallible>(next_id)
            }).unwrap();
            Some(Ok(PyRep::Integer(lid)))
        }

        ("mailMgr", "EditLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "EditLabel requires labelID".into() }));
            };
            let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(2).and_then(|a| a.as_str()).unwrap_or("#ffffff");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found") });
                };
                let Some(lbl) = list.iter_mut().find(|l| l.label_id == label_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found") });
                };
                lbl.name = name.to_string();
                lbl.color = color.to_string();
                if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                    if let Some(el) = map.get_mut(&char_id).and_then(|v| v.iter_mut().find(|l| l.label_id == label_id)) {
                        el.name = name.to_string();
                        el.color = color.to_string();
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(label_id)), ("name", PyRep::String(name.to_string())), ("color", PyRep::String(color.to_string()))]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "DeleteLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DeleteLabel requires labelID".into() }));
            };
            if label_id <= 8 {
                return Some(Err(BridgeRpcError { code: "CannotDeleteSystemLabel".into(), message: "Default system mail labels cannot be deleted".into() }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found") });
                };
                let orig_len = list.len();
                list.retain(|l| l.label_id != label_id);
                if list.len() == orig_len {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found") });
                }
                for ((cid, _), assigned) in snap.mail_assigned_labels.iter_mut() {
                    if *cid == char_id {
                        assigned.retain(|&l| l != label_id);
                    }
                }
                if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                    if let Some(esi_list) = map.get_mut(&char_id) {
                        esi_list.retain(|l| l.label_id != label_id);
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(label_id)), ("deleted", PyRep::Bool(true))]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "AssignLabels") | ("mailMgr", "ReplaceLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AssignLabels requires messageID or messageIDs list".into(),
                }));
            }
            let check_ownership = ctx.db.with_snapshot(|snap| {
                crate::services::mail_notify::check_mails_authorized(snap, &mail_ids, char_id)
            });
            if !check_ownership {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot modify mail belonging to another character".into(),
                }));
            }
            let labels_to_add = extract_id_list(args.get(1));
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mail_id in &mail_ids {
                        let current = snap.mail_assigned_labels.entry((char_id, mail_id)).or_default();
                        if method == "ReplaceLabels" {
                            current.clear();
                        }
                        for &lid in &labels_to_add {
                            if !current.contains(&lid) {
                                current.push(lid);
                            }
                        }
                        let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, false);
                        meta.labels = current.clone();
                        ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("assigned", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "RemoveLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveLabels requires messageID or messageIDs list".into(),
                }));
            }
            let check_ownership = ctx.db.with_snapshot(|snap| {
                crate::services::mail_notify::check_mails_authorized(snap, &mail_ids, char_id)
            });
            if !check_ownership {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot modify mail belonging to another character".into(),
                }));
            }
            let labels_to_remove = extract_id_list(args.get(1));
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mail_id in &mail_ids {
                        if let Some(current) = snap.mail_assigned_labels.get_mut(&(char_id, mail_id)) {
                            current.retain(|lid| !labels_to_remove.contains(lid));
                            let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, false);
                            meta.labels = current.clone();
                            ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
                        }
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail_ids[0])),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        // Calendar (`calendarMgr`)
        ("calendarMgr", "CreatePersonalEvent")
        | ("calendarMgr", "CreateCorporationEvent")
        | ("calendarMgr", "CreateAllianceEvent") => {
            Some(super::handle_calendar_create(ctx, session, method, args))
        }

        ("calendarMgr" | "calendarProxy", "GetEventList")
        | ("calendarMgr" | "calendarProxy", "GetEventDetails")
        | ("calendarMgr", "GetResponsesToEvent")
        | ("calendarMgr", "GetResponsesForCharacter") => {
            super::fleet_social_calendar_ops::handle_calendar_get(ctx, session, service, method, args)
        }

        ("calendarMgr", "SendEventResponse")
        | ("calendarMgr", "UpdateEventParticipants")
        | ("calendarMgr", "EditPersonalEvent")
        | ("calendarMgr", "EditCorporationEvent")
        | ("calendarMgr", "EditAllianceEvent")
        | ("calendarMgr", "DeleteEvent") => {
            Some(super::handle_calendar_mutate(ctx, session, method, args))
        }

        _ => None,
    }
}
