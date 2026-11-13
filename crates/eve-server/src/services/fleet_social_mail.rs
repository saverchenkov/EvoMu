//! Mail & Mailing Lists (`SO-1`: `mailMgr`, `mailingListsMgr`) and Calendar (`P-5`: `calendarMgr`).

#[path = "mailing_lists_ops.rs"]
mod mailing_lists_ops;

use super::fleet_social_chat::extract_id_list;
use super::fleet_social_persistence::ensure_social_extras_mailing_lists_seeded;
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::MailLabelRecord;
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
            let raw_recipient_ids: Vec<i64> = extract_id_list(args.first());
            // Client layout (mailSvc.SendMail): (toCharacterIDs, toListID, toCorpOrAllianceID, title,
            // body, isReplyTo, isForwardedFrom); kept as sent for OnMailSent.
            let to_character_ids = raw_recipient_ids.clone();
            let (to_list_id, to_corp_or_alliance_id, subject, body) = if args.len() >= 5 {
                (
                    args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0),
                    args.get(2).and_then(|a| a.as_i64()),
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(4).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            } else {
                (
                    None,
                    None,
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("o7"),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!"),
                )
            };
            let recipients = raw_recipient_ids
                .into_iter()
                .map(|recipient_id| crate::domain::mail::MailRecipientSpec {
                    recipient_id,
                    kind: crate::domain::mail::MailRecipientKind::Character,
                })
                .collect();
            let res = crate::domain::mail::send_mail(
                ctx,
                crate::domain::mail::SendMailInput {
                    sender_id: char_id,
                    recipients,
                    to_list_id,
                    to_corp_or_alliance_id,
                    subject: subject.to_string(),
                    body: body.to_string(),
                    approved_cost_isk: None,
                    max_subject_chars: 150,
                    max_body_chars: 8_000,
                    expand_org_recipients_in_db: true,
                    record_cspa_journal: false,
                    cspa_exempt_min_standing: 0.0,
                    cspa_exempt_same_corp: false,
                    now_unix: ctx.now_unix(),
                },
            );
            match res {
                Ok(out) => {
                    crate::services::mail_notify::record_mail_meta(
                        ctx,
                        out.mail_id,
                        &to_character_ids,
                        to_list_id,
                        to_corp_or_alliance_id,
                    );
                    // The sender's own mailSvc.SendMail already calls OnMailSent locally.
                    let others: Vec<i64> = out
                        .notify_recipient_ids
                        .into_iter()
                        .filter(|&r| r != char_id)
                        .collect();
                    let sent = crate::services::mail_notify::SentMail {
                        message_id: out.mail_id,
                        sender_id: char_id,
                        to_character_ids: &to_character_ids,
                        to_list_id,
                        to_corp_or_alliance_id,
                        title: subject,
                        status_mask: 0,
                    };
                    crate::services::mail_notify::emit_mail_sent(ctx, &sent, &others);
                    Some(Ok(PyRep::Integer(out.mail_id)))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
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
                let allowed_ids = crate::services::mail_notify::active_mailbox_window_ids(snap, char_id);
                snap.mails.iter()
                    .filter(|m| (m.recipient_id == char_id || m.sender_id == char_id)
                        && allowed_ids.contains(&m.message_id)
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
                let allowed_ids = crate::services::mail_notify::active_mailbox_window_ids(snap, char_id);
                snap.mails.iter()
                    .filter(|m| (m.recipient_id == char_id || m.sender_id == char_id)
                        && allowed_ids.contains(&m.message_id)
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
                    client_error: None,
                }));
            };
            let res_opt = ctx.db.try_with_mut_snapshot(|snap| {
                let matching: Vec<_> = snap.mails.iter().filter(|m| m.message_id == mail_id).cloned().collect();
                if matching.is_empty() {
                    return Ok::<_, std::convert::Infallible>(None);
                }
                let body = matching[0].body.clone();
                let authorized = crate::services::mail_notify::is_mail_authorized(ctx, snap, mail_id, char_id);
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
                    client_error: None,
                }));
            };
            if !authorized {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot read mail belonging to another character".into(),
                    client_error: None,
                }));
            }
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(body.as_bytes(), 6);
            Some(Ok(PyRep::Buffer(compressed)))
        }

        ("mailMgr", "MarkAsRead") => {
            handle_mark_as_read(ctx, session, args)
        }
        ("mailMgr", "MarkAsReadByLabel") => {
            handle_mark_as_read_by_label(ctx, session, args)
        }
        ("mailMgr", "MarkAsUnread") => {
            handle_mark_as_unread(ctx, session, args)
        }
        ("mailMgr", "MarkAsUnreadByLabel") => {
            handle_mark_as_unread_by_label(ctx, session, args)
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
                    crate::services::mail_notify::check_mails_authorized(ctx, snap, &mail_ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot modify mail belonging to another character".into(),
                        client_error: None,
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
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnMailRestored {
                        message_ids: restored_ids.clone(),
                    },
                );
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnMailUndeleted {
                        message_ids: restored_ids.clone(),
                    },
                );
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
                        client_error: None,
                    }));
                }
                let check_ownership = ctx.db.with_snapshot(|snap| {
                    crate::services::mail_notify::check_mails_authorized(ctx, snap, &ids, char_id)
                });
                if !check_ownership {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete mail belonging to another character".into(),
                        client_error: None,
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
                    client_error: None,
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
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnMailDeleted {
                    message_ids: ids.clone(),
                },
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
            let lid = match ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.mail_labels.entry(char_id).or_default();
                let next_id = (0..24).map(|k| 64i64 << k).find(|&id| !list.iter().any(|l| l.label_id == id)).unwrap_or(64i64 << 24);
                list.push(MailLabelRecord { label_id: next_id, name: name.to_string(), color: color.to_string() });
                Ok::<_, BridgeRpcError>(next_id)
            }) {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                let esi_list = map.entry(char_id).or_insert_with(crate::esi::runtime_state::EsiRuntimeState::default_mail_labels);
                esi_list.push(crate::esi::runtime_state::EsiMailLabel { label_id: lid, name: name.to_string(), color: color.to_string() });
            }
            Some(Ok(PyRep::Integer(lid)))
        }

        ("mailMgr", "EditLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "EditLabel requires labelID".into(), client_error: None }));
            };
            let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("CustomLabel");
            let color = args.get(2).and_then(|a| a.as_str()).unwrap_or("#ffffff");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found"), client_error: None });
                };
                let Some(lbl) = list.iter_mut().find(|l| l.label_id == label_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found"), client_error: None });
                };
                lbl.name = name.to_string();
                lbl.color = color.to_string();
                Ok(())
            });
            match res {
                Ok(()) => {
                    if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                        if let Some(el) = map.get_mut(&char_id).and_then(|v| v.iter_mut().find(|l| l.label_id == label_id)) {
                            el.name = name.to_string();
                            el.color = color.to_string();
                        }
                    }
                    Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(label_id)), ("name", PyRep::String(name.to_string())), ("color", PyRep::String(color.to_string()))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("mailMgr", "DeleteLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DeleteLabel requires labelID".into(), client_error: None }));
            };
            if label_id < 64 {
                return Some(Err(BridgeRpcError { code: "CannotDeleteSystemLabel".into(), message: "Default system mail labels cannot be deleted".into(), client_error: None }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(list) = snap.mail_labels.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found"), client_error: None });
                };
                let orig_len = list.len();
                list.retain(|l| l.label_id != label_id);
                if list.len() == orig_len {
                    return Err(BridgeRpcError { code: "LabelNotFound".into(), message: format!("Label {label_id} not found"), client_error: None });
                }
                for ((cid, _), assigned) in snap.mail_assigned_labels.iter_mut() {
                    if *cid == char_id {
                        assigned.retain(|&l| l != label_id);
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    if let Ok(mut map) = ctx.esi_state.mail_labels.write() {
                        if let Some(esi_list) = map.get_mut(&char_id) {
                            esi_list.retain(|l| l.label_id != label_id);
                        }
                    }
                    Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(label_id)), ("deleted", PyRep::Bool(true))])))
                }
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
                    client_error: None,
                }));
            }
            let check_ownership = ctx.db.with_snapshot(|snap| {
                crate::services::mail_notify::check_mails_authorized(ctx, snap, &mail_ids, char_id)
            });
            if !check_ownership {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot modify mail belonging to another character".into(),
                    client_error: None,
                }));
            }
            let labels_to_add = extract_label_bits(args.get(1));
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
                    client_error: None,
                }));
            }
            let check_ownership = ctx.db.with_snapshot(|snap| {
                crate::services::mail_notify::check_mails_authorized(ctx, snap, &mail_ids, char_id)
            });
            if !check_ownership {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot modify mail belonging to another character".into(),
                    client_error: None,
                }));
            }
            let labels_to_remove = extract_label_bits(args.get(1));
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

        ("calendarMgr", "SendEventResponse") => {
            Some(super::handle_calendar_mutate(ctx, session, "SendEventResponse", args))
        }
        ("calendarMgr", "UpdateEventParticipants") => {
            Some(super::handle_calendar_mutate(ctx, session, "UpdateEventParticipants", args))
        }
        ("calendarMgr", "EditPersonalEvent")
        | ("calendarMgr", "EditCorporationEvent")
        | ("calendarMgr", "EditAllianceEvent") => {
            Some(super::handle_calendar_mutate(ctx, session, method, args))
        }
        ("calendarMgr", "DeleteEvent") => {
            Some(super::handle_calendar_mutate(ctx, session, "DeleteEvent", args))
        }

        _ => None,
    }
}

fn apply_mail_read_status(
    ctx: &ServerContext,
    char_id: i64,
    mail_ids: &[i64],
    is_read: bool,
) -> PyRep {
    ctx.db
        .try_with_mut_snapshot(|snap| {
            for &mid in mail_ids {
                if is_read {
                    snap.read_mails.insert((char_id, mid));
                } else {
                    snap.read_mails.remove(&(char_id, mid));
                }
                let mut meta = ctx.esi_state.get_mail_meta(char_id, mid, false);
                meta.is_read = is_read;
                ctx.esi_state.set_mail_meta(char_id, mid, meta);
            }
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnMailCountersUpdate {
            char_id: Some(char_id),
        },
    );
    PyRep::keyval([
        ("messageID", PyRep::Integer(mail_ids[0])),
        (
            "messageIDs",
            PyRep::List(mail_ids.iter().copied().map(PyRep::Integer).collect()),
        ),
        ("read", PyRep::Bool(is_read)),
    ])
}

fn handle_mark_as_read(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mail_ids = extract_id_list(args.first());
    if mail_ids.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "MarkAsRead requires messageID or messageIDs list".into(),
            client_error: None,
        }));
    }
    let authorized = ctx.db.with_snapshot(|snap| {
        crate::services::mail_notify::check_mails_authorized(ctx, snap, &mail_ids, char_id)
    });
    if !authorized {
        return Some(Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Cannot modify mail belonging to another character".into(),
            client_error: None,
        }));
    }
    Some(Ok(apply_mail_read_status(ctx, char_id, &mail_ids, true)))
}

fn handle_mark_as_read_by_label(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mail_ids = crate::services::mail_notify::resolve_mail_targets(
        ctx,
        char_id,
        "MarkAsReadByLabel",
        args.first(),
    );
    if mail_ids.is_empty() {
        return Some(Ok(PyRep::keyval([
            ("messageID", PyRep::Integer(0)),
            ("messageIDs", PyRep::List(Vec::new())),
            ("read", PyRep::Bool(true)),
        ])));
    }
    Some(Ok(apply_mail_read_status(ctx, char_id, &mail_ids, true)))
}

fn handle_mark_as_unread(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mail_ids = extract_id_list(args.first());
    if mail_ids.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "MarkAsUnread requires messageID or messageIDs list".into(),
            client_error: None,
        }));
    }
    let authorized = ctx.db.with_snapshot(|snap| {
        crate::services::mail_notify::check_mails_authorized(ctx, snap, &mail_ids, char_id)
    });
    if !authorized {
        return Some(Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Cannot modify mail belonging to another character".into(),
            client_error: None,
        }));
    }
    Some(Ok(apply_mail_read_status(ctx, char_id, &mail_ids, false)))
}

fn handle_mark_as_unread_by_label(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mail_ids = crate::services::mail_notify::resolve_mail_targets(
        ctx,
        char_id,
        "MarkAsUnreadByLabel",
        args.first(),
    );
    if mail_ids.is_empty() {
        return Some(Ok(PyRep::keyval([
            ("messageID", PyRep::Integer(0)),
            ("messageIDs", PyRep::List(Vec::new())),
            ("read", PyRep::Bool(false)),
        ])));
    }
    Some(Ok(apply_mail_read_status(ctx, char_id, &mail_ids, false)))
}

fn extract_label_bits(arg: Option<&PyRep>) -> Vec<i64> {
    let mut bits = Vec::new();
    match arg {
        Some(PyRep::Integer(mask)) => {
            let m = *mask;
            if m > 0 {
                for k in 0..62 {
                    let bit = 1i64 << k;
                    if (m & bit) != 0 {
                        bits.push(bit);
                    }
                }
            }
        }
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            for item in items {
                if let Some(mask) = item.as_i64() {
                    if mask > 0 {
                        if (mask & (mask - 1)) == 0 {
                            bits.push(mask);
                        } else {
                            for k in 0..62 {
                                let bit = 1i64 << k;
                                if (mask & bit) != 0 {
                                    bits.push(bit);
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    bits.sort_unstable();
    bits.dedup();
    bits
}

#[cfg(test)] #[path = "fleet_social_mail_tests.rs"]
mod fleet_social_mail_tests;

