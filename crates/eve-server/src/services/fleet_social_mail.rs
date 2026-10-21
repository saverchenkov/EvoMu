//! Mail & Mailing Lists (`SO-1`: `mailMgr`, `mailingListsMgr`) and Calendar (`P-5`: `calendarMgr`).

use super::fleet_social_chat::{extract_id_list, CalendarEventState, MailingListState, SOCIAL_EXTRAS};
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{IskCents, MailLabelRecord, MailRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(super) fn handle_mail_and_calendar(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(service, "mailMgr" | "mailingListsMgr" | "calendarMgr") {
        return None;
    }

    match (service, method) {
        ("mailMgr", "SendMail") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mut recipient_ids: Vec<i64> = extract_id_list(args.first());
            let mut target_label = 1i64; // 1 = Inbox, 4 = Corp, 8 = Alliance
            let (subject, body) = if args.len() >= 5 {
                if let Some(list_id) = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0) {
                    let s = SOCIAL_EXTRAS.lock().unwrap();
                    if let Some(ml) = s.mailing_lists.get(&list_id) {
                        if ml.members.get(&char_id).copied() == Some(1) {
                            return Some(Err(BridgeRpcError {
                                code: "MutedInMailingList".into(),
                                message: "You are muted in this mailing list".into(),
                            }));
                        }
                        recipient_ids.extend(ml.members.keys().copied());
                    }
                }
                if recipient_ids.is_empty() {
                    if let Some(corp_or_alliance_id) = args.get(2).and_then(|a| a.as_i64()) {
                        let restrict = super::fleet_social_corps::CORP_GOV
                            .lock()
                            .unwrap()
                            .get(&corp_or_alliance_id)
                            .map(|g| g.restrict_corp_mails)
                            .unwrap_or(false);
                        let members = ctx.db.with_snapshot(|snap| -> Result<Vec<i64>, BridgeRpcError> {
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
                            let restrict = super::fleet_social_corps::CORP_GOV
                                .lock()
                                .unwrap()
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
                let is_blocked = ctx
                    .esi_state
                    .char_contacts
                    .read()
                    .ok()
                    .and_then(|g| {
                        g.get(&rec_id).and_then(|list| {
                            list.iter()
                                .find(|e| e.contact_id == char_id)
                                .map(|e| e.is_blocked)
                        })
                    })
                    .unwrap_or(false);
                if is_blocked {
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

            let mut total_cspa = IskCents(0);
            {
                let s = SOCIAL_EXTRAS.lock().unwrap();
                for &rec_id in &recipient_ids {
                    if rec_id == char_id {
                        continue;
                    }
                    let friendly_contact = s
                        .contacts
                        .get(&rec_id)
                        .and_then(|m| m.get(&char_id))
                        .is_some_and(|&(st, _, _)| st > 0.0);
                    if s.blocked
                        .get(&rec_id)
                        .is_some_and(|b| b.contains(&char_id))
                        && !friendly_contact
                    {
                        return Some(Err(BridgeRpcError {
                            code: "SenderBlockedByRecipient".into(),
                            message: format!("Recipient {rec_id} has blocked you"),
                        }));
                    }
                    if let Some(&charge) = s.cspa_charges.get(&rec_id) {
                        if charge.0 > 0 && !friendly_contact && target_label == 1 {
                            total_cspa = total_cspa.checked_add(charge).unwrap_or(total_cspa);
                        }
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
                if total_cspa.0 > 0 {
                    let Some(sender_ch) = snap.characters.get_mut(&char_id) else {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {char_id} not found"),
                        });
                    };
                    sender_ch.balance = sender_ch
                        .balance
                        .checked_sub(total_cspa)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientFundsForCSPA".into(),
                            message: "Insufficient ISK to pay recipient CSPA charge".into(),
                        })?;
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
                Ok(id)
            });

            match res {
                Ok(mid) => {
                    for &rec_id in &recipient_ids {
                        ctx.notify(
                            NotificationTarget::Character(rec_id),
                            "OnNewMailReceived",
                            "charid",
                            vec![PyRep::Integer(mid), PyRep::Integer(char_id)],
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnMailSent",
                        "charid",
                        vec![PyRep::Integer(mid), PyRep::Integer(primary_recipient)],
                    );
                    Some(Ok(PyRep::Integer(mid)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "PrimeOwners") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if let Some(PyRep::Real(isk)) = args.first() {
                SOCIAL_EXTRAS
                    .lock()
                    .unwrap()
                    .cspa_charges
                    .insert(char_id, IskCents::from_f64_isk(isk.clamp(0.0, 1_000_000.0)));
            }
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
            handle_mail_headers(ctx, session, method)
        }
        ("mailMgr", "SyncMail") => {
            handle_mail_headers(ctx, session, method)
        }
        ("mailMgr", m) if m.eq_ignore_ascii_case("getheaders") => {
            handle_mail_headers(ctx, session, method)
        }

        ("mailMgr", "GetBody") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(mail_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetBody requires messageID".into(),
                }));
            };
            let mail_opt = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let m = snap.mails.iter().find(|m| m.message_id == mail_id).cloned();
                    if let Some(ref mail) = m {
                        if mail.recipient_id == char_id {
                            snap.read_mails.insert((char_id, mail_id));
                        }
                    }
                    Ok::<_, std::convert::Infallible>(m)
                })
                .unwrap();
            let Some(mail) = mail_opt else {
                return Some(Err(BridgeRpcError {
                    code: "MailNotFound".into(),
                    message: format!("Mail message {mail_id} not found"),
                }));
            };
            if mail.sender_id != char_id && mail.recipient_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot read mail belonging to another character".into(),
                }));
            }
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(mail.message_id)),
                ("body", PyRep::String(mail.body)),
                ("read", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "MarkAsRead")
        | ("mailMgr", "MarkAsReadByLabel")
        | ("mailMgr", "MarkAsReadByList") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsRead requires messageID or messageIDs list".into(),
                }));
            }
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        snap.read_mails.insert((char_id, mid));
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
        | ("mailMgr", "MarkAsUnreadByLabel")
        | ("mailMgr", "MarkAsUnreadByList") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            if mail_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MarkAsUnread requires messageID or messageIDs list".into(),
                }));
            }
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        snap.read_mails.remove(&(char_id, mid));
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
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
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
                        if changed {
                            count += 1;
                        }
                    }
                    Ok::<_, std::convert::Infallible>(count)
                })
                .unwrap();
            Some(Ok(PyRep::keyval([("markedReadCount", PyRep::Integer(count))])))
        }

        ("mailMgr", "MoveToTrash")
        | ("mailMgr", "MoveAllToTrash")
        | ("mailMgr", "MoveToTrashByLabel")
        | ("mailMgr", "MoveToTrashByList") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            {
                let mut s = SOCIAL_EXTRAS.lock().unwrap();
                for &mid in &mail_ids {
                    s.trashed_mails.insert((char_id, mid));
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailTrashed",
                "charid",
                vec![PyRep::List(mail_ids.iter().copied().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::keyval([
                ("trashed", PyRep::Bool(true)),
                ("inTrash", PyRep::Bool(true)),
            ])))
        }

        ("mailMgr", "MoveFromTrash") | ("mailMgr", "MoveAllFromTrash") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mail_ids = extract_id_list(args.first());
            {
                let mut s = SOCIAL_EXTRAS.lock().unwrap();
                if mail_ids.is_empty() {
                    s.trashed_mails.retain(|&(cid, _)| cid != char_id);
                } else {
                    for &mid in &mail_ids {
                        s.trashed_mails.remove(&(char_id, mid));
                    }
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailRestored",
                "charid",
                vec![PyRep::List(mail_ids.iter().copied().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::keyval([
                ("restored", PyRep::Bool(true)),
                ("inTrash", PyRep::Bool(false)),
            ])))
        }

        ("mailMgr", "EmptyTrash") | ("mailMgr", "DeleteMail") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let mut ids = extract_id_list(args.first());
            if method == "EmptyTrash" {
                let mut s = SOCIAL_EXTRAS.lock().unwrap();
                let trashed_for_char: Vec<i64> = s
                    .trashed_mails
                    .iter()
                    .filter(|&&(cid, _)| cid == char_id)
                    .map(|&(_, mid)| mid)
                    .collect();
                s.trashed_mails.retain(|&(cid, _)| cid != char_id);
                ids.extend(trashed_for_char);
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
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &ids {
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
            let (custom_labels, mails, read_mails, assignments) = ctx.db.with_snapshot(|snap| {
                (
                    snap.mail_labels.get(&char_id).cloned().unwrap_or_default(),
                    snap.mails.clone(),
                    snap.read_mails.clone(),
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
            let rows: Vec<Vec<PyRep>> = all_labels
                .into_iter()
                .map(|lbl| {
                    let unread_count = mails
                        .iter()
                        .filter(|m| {
                            if m.recipient_id != char_id {
                                return false;
                            }
                            if read_mails.contains(&(char_id, m.message_id)) {
                                return false;
                            }
                            let assigned = assignments
                                .get(&(char_id, m.message_id))
                                .cloned()
                                .unwrap_or_else(|| vec![1]);
                            assigned.contains(&lbl.label_id)
                        })
                        .count();
                    vec![
                        PyRep::Integer(lbl.label_id),
                        PyRep::String(lbl.name),
                        PyRep::String(lbl.color),
                        PyRep::Integer(unread_count as i64),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["labelID", "name", "color", "unreadCount"],
                rows,
            )))
        }

        ("mailMgr", "CreateLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(1).and_then(|a| a.as_str()).unwrap_or("#ffffff");
            let lid = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let list = snap.mail_labels.entry(char_id).or_default();
                    let next_id = 16i64 << (list.len().min(20));
                    list.push(MailLabelRecord {
                        label_id: next_id,
                        name: name.to_string(),
                        color: color.to_string(),
                    });
                    Ok::<_, std::convert::Infallible>(next_id)
                })
                .unwrap();
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("name", PyRep::String(name.to_string())),
                ("color", PyRep::String(color.to_string())),
            ])))
        }

        ("mailMgr", "EditLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "EditLabel requires labelID".into(),
                }));
            };
            let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(2).and_then(|a| a.as_str()).unwrap_or("#ffffff");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                let Some(lbl) = list.iter_mut().find(|l| l.label_id == label_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                lbl.name = name.to_string();
                lbl.color = color.to_string();
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("labelID", PyRep::Integer(label_id)),
                    ("name", PyRep::String(name.to_string())),
                    ("color", PyRep::String(color.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "DeleteLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteLabel requires labelID".into(),
                }));
            };
            if label_id <= 8 {
                return Some(Err(BridgeRpcError {
                    code: "CannotDeleteSystemLabel".into(),
                    message: "Default system mail labels cannot be deleted".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                };
                let orig_len = list.len();
                list.retain(|l| l.label_id != label_id);
                if list.len() == orig_len {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Label {label_id} not found"),
                    });
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("labelID", PyRep::Integer(label_id)),
                    ("deleted", PyRep::Bool(true)),
                ]))),
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
            let labels_to_remove = extract_id_list(args.get(1));
            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mail_id in &mail_ids {
                        if let Some(current) = snap.mail_assigned_labels.get_mut(&(char_id, mail_id)) {
                            current.retain(|lid| !labels_to_remove.contains(lid));
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

        // Mailing Lists (`mailingListsMgr`)
        ("mailingListsMgr", "Create") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("EvoMu Mailing List");
            let default_access = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let default_role = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let cost = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0).max(0);
            let list_id = {
                let mut s = SOCIAL_EXTRAS.lock().unwrap();
                s.next_id += 1;
                let lid = s.next_id;
                let mut members = HashMap::new();
                members.insert(char_id, 3);
                s.mailing_lists.insert(
                    lid,
                    MailingListState {
                        list_id: lid,
                        name: name.to_string(),
                        owner_id: char_id,
                        default_access,
                        default_role,
                        cost,
                        members,
                        entity_access: HashMap::new(),
                        welcome_mail: None,
                    },
                );
                lid
            };
            Some(Ok(PyRep::keyval([
                ("listID", PyRep::Integer(list_id)),
                ("name", PyRep::String(name.to_string())),
                ("ownerID", PyRep::Integer(char_id)),
            ])))
        }

        ("mailingListsMgr", "GetJoinedLists") | ("mailingListsMgr", "GetInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let s = SOCIAL_EXTRAS.lock().unwrap();
            let rows: Vec<Vec<PyRep>> = s
                .mailing_lists
                .values()
                .filter(|ml| ml.members.contains_key(&char_id))
                .map(|ml| {
                    vec![
                        PyRep::Integer(ml.list_id),
                        PyRep::String(ml.name.clone()),
                        PyRep::Integer(ml.owner_id),
                        PyRep::Integer(*ml.members.get(&char_id).unwrap_or(&0)),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["listID", "displayName", "ownerID", "role"],
                rows,
            )))
        }

        ("mailingListsMgr", "Join") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id_or_name = args.first();
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            let ml_opt = match list_id_or_name {
                Some(PyRep::Integer(id)) => s.mailing_lists.get_mut(id),
                Some(PyRep::String(name)) => s.mailing_lists.values_mut().find(|m| m.name.eq_ignore_ascii_case(name)),
                _ => None,
            };
            let Some(ml) = ml_opt else {
                return Some(Err(BridgeRpcError {
                    code: "MailingListNotFound".into(),
                    message: "Mailing list not found".into(),
                }));
            };
            let entity_acc = ml.entity_access.get(&char_id).copied().unwrap_or(ml.default_access);
            if entity_acc <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "MailingListAccessDenied".into(),
                    message: "Access to this mailing list is blocked".into(),
                }));
            }
            ml.members.entry(char_id).or_insert(ml.default_role);
            let lid = ml.list_id;
            let welcome = ml.welcome_mail.clone();
            let owner_id = ml.owner_id;
            drop(s);

            if let Some((subj, body)) = welcome {
                let mid = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord {
                        message_id: mid,
                        sender_id: owner_id,
                        recipient_id: char_id,
                        subject: subj,
                        body,
                    });
                    snap.mail_assigned_labels.insert((char_id, mid), vec![1]);
                    mid
                });
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnNewMailReceived",
                    "charid",
                    vec![PyRep::Integer(mid)],
                );
            }
            Some(Ok(PyRep::keyval([
                ("listID", PyRep::Integer(lid)),
                ("joined", PyRep::Bool(true)),
            ])))
        }

        ("mailingListsMgr", "Leave") | ("mailingListsMgr", "Delete") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            if method == "Delete" {
                s.mailing_lists.remove(&list_id);
            } else if let Some(ml) = s.mailing_lists.get_mut(&list_id) {
                ml.members.remove(&char_id);
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("mailingListsMgr", "GetMembers") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let s = SOCIAL_EXTRAS.lock().unwrap();
            let rows: Vec<Vec<PyRep>> = s
                .mailing_lists
                .get(&list_id)
                .map(|ml| {
                    ml.members
                        .iter()
                        .map(|(&cid, &role)| vec![PyRep::Integer(cid), PyRep::Integer(role)])
                        .collect()
                })
                .unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["characterID", "role"], rows)))
        }

        ("mailingListsMgr", "SetMembersMuted")
        | ("mailingListsMgr", "SetMembersOperator")
        | ("mailingListsMgr", "SetMembersClear")
        | ("mailingListsMgr", "KickMembers") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let targets = extract_id_list(args.get(1));
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            if let Some(ml) = s.mailing_lists.get_mut(&list_id) {
                let caller_role = ml.members.get(&char_id).copied().unwrap_or(0);
                if ml.owner_id != char_id && caller_role < 2 {
                    return Some(Err(BridgeRpcError {
                        code: "MailingListAccessDenied".into(),
                        message: "Mailing list operator or owner role required".into(),
                    }));
                }
                for tid in targets {
                    match method {
                        "SetMembersMuted" => { ml.members.insert(tid, 1); }
                        "SetMembersOperator" => { ml.members.insert(tid, 2); }
                        "SetMembersClear" => { ml.members.insert(tid, 0); }
                        "KickMembers" => { ml.members.remove(&tid); }
                        _ => {}
                    }
                }
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("mailingListsMgr", "SetEntityAccess")
        | ("mailingListsMgr", "SetEntitiesAccess")
        | ("mailingListsMgr", "ClearEntityAccess")
        | ("mailingListsMgr", "SetDefaultAccess")
        | ("mailingListsMgr", "SaveSettings")
        | ("mailingListsMgr", "GetSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MailingListNotFound".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if method != "GetSettings"
                && ml.owner_id != char_id
                && ml.members.get(&char_id).copied().unwrap_or(0) < 2
            {
                return Some(Err(BridgeRpcError {
                    code: "MailingListPermissionDenied".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            match method {
                "SetEntityAccess" | "SetEntitiesAccess" => {
                    let entity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    let access = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
                    ml.entity_access.insert(entity_id, access);
                }
                "ClearEntityAccess" => {
                    let entity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    ml.entity_access.remove(&entity_id);
                }
                "SetDefaultAccess" | "SaveSettings" => {
                    ml.default_access = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                    ml.default_role = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    ml.cost = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0);
                }
                _ => {}
            }
            Some(Ok(PyRep::keyval([
                ("listID", PyRep::Integer(ml.list_id)),
                ("defaultAccess", PyRep::Integer(ml.default_access)),
                ("defaultRole", PyRep::Integer(ml.default_role)),
                ("cost", PyRep::Integer(ml.cost)),
            ])))
        }

        ("mailingListsMgr", "SaveWelcomeMail")
        | ("mailingListsMgr", "SendWelcomeMail")
        | ("mailingListsMgr", "GetWelcomeMail")
        | ("mailingListsMgr", "ClearWelcomeMail") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MailingListNotFound".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if method != "GetWelcomeMail"
                && ml.owner_id != char_id
                && ml.members.get(&char_id).copied().unwrap_or(0) < 2
            {
                return Some(Err(BridgeRpcError {
                    code: "MailingListPermissionDenied".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            match method {
                "SaveWelcomeMail" | "SendWelcomeMail" => {
                    let title = args.get(1).and_then(|a| a.as_str()).unwrap_or("Welcome!");
                    let body = args.get(2).and_then(|a| a.as_str()).unwrap_or("o7");
                    ml.welcome_mail = Some((title.to_string(), body.to_string()));
                }
                "ClearWelcomeMail" => {
                    ml.welcome_mail = None;
                }
                _ => {}
            }
            let (title, body) = ml.welcome_mail.clone().unwrap_or_else(|| (String::new(), String::new()));
            let owner_id = ml.owner_id;
            drop(s);
            if method == "SendWelcomeMail" && !title.is_empty() {
                let _ = ctx.db.try_with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord {
                        message_id: mid,
                        sender_id: owner_id,
                        recipient_id: char_id,
                        subject: title.clone(),
                        body: body.clone(),
                    });
                    snap.mail_assigned_labels.insert((char_id, mid), vec![1]);
                    Ok::<(), std::convert::Infallible>(())
                });
            }
            Some(Ok(PyRep::keyval([
                ("listID", PyRep::Integer(list_id)),
                ("title", PyRep::String(title)),
                ("body", PyRep::String(body)),
            ])))
        }

        // Calendar (`calendarMgr`)
        ("calendarMgr", "CreatePersonalEvent")
        | ("calendarMgr", "CreateCorporationEvent")
        | ("calendarMgr", "CreateAllianceEvent")
        | ("calendarMgr", "CreateNewEvent") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let date_time = args.first().and_then(|a| a.as_i64()).unwrap_or(133_000_000_000_000_000);
            let duration = args.get(1).and_then(|a| a.as_i64()).unwrap_or(60);
            let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Fleet Op");
            let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Formup in Jita");
            let importance = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
            let (char_corp, char_ally) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id))
                    .unwrap_or((session.corporation_id.unwrap_or(char_id), session.alliance_id))
            });
            let corp_id = session.corporation_id.unwrap_or(char_corp);
            let ally_id = session.alliance_id.or(char_ally).unwrap_or(corp_id);
            if matches!(method, "CreateCorporationEvent" | "CreateAllianceEvent") {
                let (is_ceo, roles) =
                    crate::services::fleet_social::get_effective_corp_roles(ctx, corp_id, char_id);
                if !is_ceo && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 40)) {
                    return Some(Err(BridgeRpcError {
                        code: "CalendarAccessDenied".into(),
                        message: "Creating corporation or alliance calendar events requires Communications Officer or Director role".into(),
                    }));
                }
            }
            let (scope, owner_id) = match method {
                "CreateCorporationEvent" => ("Corporation".to_string(), corp_id),
                "CreateAllianceEvent" => ("Alliance".to_string(), ally_id),
                _ => ("Personal".to_string(), char_id),
            };
            let eid = {
                let mut s = SOCIAL_EXTRAS.lock().unwrap();
                s.next_id += 1;
                let id = s.next_id;
                let mut responses = HashMap::new();
                responses.insert(char_id, 1);
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
            let target = match method {
                "CreateCorporationEvent" => NotificationTarget::Corporation(corp_id),
                "CreateAllianceEvent" => NotificationTarget::Alliance(ally_id),
                _ => NotificationTarget::Character(char_id),
            };
            ctx.notify(
                target,
                "OnNewCalendarEvent",
                "charid",
                vec![PyRep::Integer(eid), PyRep::String(title.to_string())],
            );
            Some(Ok(PyRep::keyval([
                ("eventID", PyRep::Integer(eid)),
                ("ownerID", PyRep::Integer(owner_id)),
                ("title", PyRep::String(title.to_string())),
            ])))
        }

        ("calendarMgr", "GetEventsByMonthYear")
        | ("calendarMgr", "GetEventList")
        | ("calendarMgr", "GetEventDetails")
        | ("calendarMgr", "GetResponsesToEvent")
        | ("calendarMgr", "GetResponsesForCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let (char_corp, char_ally) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (Some(c.corporation_id), c.alliance_id))
                    .unwrap_or((session.corporation_id, session.alliance_id))
            });
            let corp_opt = session.corporation_id.or(char_corp);
            let ally_opt = session.alliance_id.or(char_ally);
            let s = SOCIAL_EXTRAS.lock().unwrap();
            if method == "GetEventDetails" {
                let eid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                let Some(ev) = s.calendar_events.get(&eid) else {
                    return Some(Err(BridgeRpcError {
                        code: "CalendarEventNotFound".into(),
                        message: format!("Calendar event {eid} not found"),
                    }));
                };
                return Some(Ok(PyRep::keyval([
                    ("eventID", PyRep::Integer(ev.event_id)),
                    ("ownerID", PyRep::Integer(ev.owner_id)),
                    ("creatorID", PyRep::Integer(ev.creator_id)),
                    ("eventDateTime", PyRep::Integer(ev.event_date_time)),
                    ("duration", PyRep::Integer(ev.duration)),
                    ("importance", PyRep::Integer(ev.importance)),
                    ("title", PyRep::String(ev.title.clone())),
                    ("eventText", PyRep::String(ev.description.clone())),
                ])));
            }
            if method == "GetResponsesToEvent" {
                let eid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                let rows: Vec<Vec<PyRep>> = s
                    .calendar_events
                    .get(&eid)
                    .map(|ev| {
                        ev.responses
                            .iter()
                            .map(|(&cid, &status)| vec![PyRep::Integer(cid), PyRep::Integer(status)])
                            .collect()
                    })
                    .unwrap_or_default();
                return Some(Ok(PyRep::rowset(vec!["characterID", "status"], rows)));
            }
            let mut rows: Vec<Vec<PyRep>> = s
                .calendar_events
                .values()
                .filter(|ev| {
                    ev.owner_id == char_id
                        || Some(ev.owner_id) == corp_opt
                        || Some(ev.owner_id) == ally_opt
                })
                .map(|ev| {
                    let owner_type = match ev._scope.as_str() {
                        "Corporation" => 2,
                        "Alliance" => 16159,
                        _ => 1373,
                    };
                    vec![
                        PyRep::Integer(ev.event_id),
                        PyRep::Integer(ev.owner_id),
                        PyRep::Integer(ev.event_date_time),
                        PyRep::Integer(ev.duration),
                        PyRep::Integer(ev.importance),
                        PyRep::String(ev.title.clone()),
                        PyRep::Integer(owner_type),
                        PyRep::Integer(ev.responses.get(&char_id).copied().unwrap_or(0)),
                        PyRep::Bool(false),
                    ]
                })
                .collect();
            if method == "GetEventsByMonthYear" {
                rows.push(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(133_500_000_000_000_000),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::String("CCP Crimson Harvest".into()),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Bool(false),
                ]);
                rows.push(vec![
                    PyRep::Integer(2),
                    PyRep::Integer(corp_opt.unwrap_or(char_id)),
                    PyRep::Integer(133_500_000_000_000_000),
                    PyRep::Integer(180),
                    PyRep::Integer(1),
                    PyRep::String("Automated Moon Extraction Ready".into()),
                    PyRep::Integer(2),
                    PyRep::Integer(0),
                    PyRep::Bool(true),
                ]);
            }
            Some(Ok(PyRep::rowset(
                vec![
                    "eventID",
                    "ownerID",
                    "eventDateTime",
                    "duration",
                    "importance",
                    "title",
                    "ownerType",
                    "response",
                    "isAutomated",
                ],
                rows,
            )))
        }

        ("calendarMgr", "SendEventResponse")
        | ("calendarMgr", "RespondToEvent")
        | ("calendarMgr", "UpdateEventParticipants")
        | ("calendarMgr", "EditPersonalEvent")
        | ("calendarMgr", "EditCorporationEvent")
        | ("calendarMgr", "EditAllianceEvent")
        | ("calendarMgr", "DeleteEvent") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let eid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut s = SOCIAL_EXTRAS.lock().unwrap();
            if method == "DeleteEvent" {
                s.calendar_events.remove(&eid);
                return Some(Ok(PyRep::keyval([("eventID", PyRep::Integer(eid)), ("deleted", PyRep::Bool(true))])));
            }
            let mut resp_val = 1i64;
            if let Some(ev) = s.calendar_events.get_mut(&eid) {
                if method == "SendEventResponse" || method == "RespondToEvent" {
                    resp_val = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                    ev.responses.insert(char_id, resp_val);
                } else {
                    if let Some(dt) = args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 10_000) {
                        ev.event_date_time = dt;
                    }
                    if let Some(dur) = args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 0 && v <= 10_000) {
                        ev.duration = dur;
                    }
                    if let Some(t) = args.get(3).and_then(|a| a.as_str()).or_else(|| args.get(1).and_then(|a| a.as_str())) {
                        ev.title = t.to_string();
                    }
                    if let Some(d) = args.get(4).and_then(|a| a.as_str()).or_else(|| args.get(2).and_then(|a| a.as_str())) {
                        ev.description = d.to_string();
                    }
                    if let Some(imp) = args.get(5).and_then(|a| a.as_i64()) {
                        ev.importance = imp;
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("eventID", PyRep::Integer(eid)),
                ("status", PyRep::Integer(resp_val)),
            ])))
        }

        _ => None,
    }
}

fn handle_mail_headers(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let trashed = SOCIAL_EXTRAS.lock().unwrap().trashed_mails.clone();
    let (rows, status_rows) = ctx.db.with_snapshot(|snap| {
        let mut hdrs = Vec::new();
        let mut statuses = Vec::new();
        for m in snap.mails.iter().filter(|m| {
            (m.recipient_id == char_id || m.sender_id == char_id)
                && !snap.deleted_mails.contains(&(char_id, m.message_id))
                && !trashed.contains(&(char_id, m.message_id))
        }) {
            let read = snap.read_mails.contains(&(char_id, m.message_id));
            let labels = snap
                .mail_assigned_labels
                .get(&(char_id, m.message_id))
                .cloned()
                .unwrap_or_else(|| vec![if m.sender_id == char_id { 2 } else { 1 }]);
            let label_mask: i64 = labels.iter().copied().fold(0i64, |acc, l| acc | l);
            hdrs.push(vec![
                PyRep::Integer(m.message_id),
                PyRep::Integer(m.sender_id),
                PyRep::List(vec![PyRep::Integer(m.recipient_id)]),
                PyRep::String(m.subject.clone()),
                PyRep::Integer(133_000_000_000_000_000),
                PyRep::Bool(read),
                PyRep::List(labels.into_iter().map(PyRep::Integer).collect()),
            ]);
            statuses.push(vec![
                PyRep::Integer(m.message_id),
                PyRep::Integer(if read { 1 } else { 0 }),
                PyRep::Integer(label_mask),
                PyRep::Bool(read),
            ]);
        }
        (hdrs, statuses)
    });

    let headers_rowset = PyRep::rowset(
        vec![
            "messageID",
            "senderID",
            "toCharacterIDs",
            "title",
            "sentDate",
            "read",
            "labels",
        ],
        rows,
    );
    if method == "SyncMail" {
        let old_mail_rowset = PyRep::rowset(
            vec![
                "messageID",
                "senderID",
                "toCharacterIDs",
                "title",
                "sentDate",
                "read",
                "labels",
            ],
            Vec::new(),
        );
        let status_rowset = PyRep::rowset(
            vec!["messageID", "statusMask", "labelMask", "read"],
            status_rows,
        );
        Some(Ok(PyRep::keyval([
            ("oldMail", old_mail_rowset),
            ("newMail", headers_rowset),
            ("mailStatus", status_rowset),
        ])))
    } else {
        Some(Ok(headers_rowset))
    }
}
