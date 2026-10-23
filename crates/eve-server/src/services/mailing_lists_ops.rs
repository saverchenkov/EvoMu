//! Mailing Lists Operations & Extended Mail RPC handlers (`SO-1`).
//!
//! Implements all 18 open EVE Mail & Mailing Lists RPC methods:
//! - 6 `mailMgr` methods:
//!   - `MarkAsReadByList`
//!   - `MarkAsUnreadByList`
//!   - `MoveAllToTrash`
//!   - `MoveToTrash`
//!   - `MoveToTrashByLabel`
//!   - `MoveToTrashByList`
//! - 12 `mailingListsMgr` methods:
//!   - `ClearEntityAccess`
//!   - `ClearWelcomeMail`
//!   - `Delete`
//!   - `GetSettings`
//!   - `KickMembers`
//!   - `SaveWelcomeMail`
//!   - `SendWelcomeMail`
//!   - `SetDefaultAccess`
//!   - `SetEntitiesAccess`
//!   - `SetMembersClear`
//!   - `SetMembersMuted`
//!   - `SetMembersOperator`

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use crate::services::fleet_social::fleet_social_persistence::{sync_mailing_lists_to_db, MailingListState};
use crate::services::mail_notify;
use eve_db::MailRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn extract_id_list(arg: Option<&PyRep>) -> Vec<i64> {
    let mut ids = Vec::new();
    match arg {
        Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
            for item in list {
                if let Some(id) = item.as_i64() {
                    ids.push(id);
                }
            }
        }
        Some(rep) => {
            if let Some(id) = rep.as_i64() {
                ids.push(id);
            }
        }
        None => {}
    }
    ids
}

/// Dispatches mailMgr and mailingListsMgr requests.
pub(crate) fn handle_mail_and_mailing_lists_ops(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match service {
        "mailMgr" => handle_mail_ops(ctx, session, method, args),
        "mailingListsMgr" => handle_mailing_list_ops(ctx, session, method, args),
        _ => None,
    }
}

fn resolve_target_message_ids(
    ctx: &ServerContext,
    char_id: i64,
    method: &str,
    arg: Option<&PyRep>,
) -> Vec<i64> {
    match method {
        "MoveAllToTrash" => ctx.db.with_snapshot(|snap| {
            snap.mails
                .iter()
                .filter(|m| {
                    (m.recipient_id == char_id || m.sender_id == char_id)
                        && !snap.deleted_mails.contains(&(char_id, m.message_id))
                        && !snap.trashed_mails.contains(&(char_id, m.message_id))
                })
                .map(|m| m.message_id)
                .collect()
        }),
        "MoveToTrashByLabel" => {
            let label_id = arg.and_then(|a| a.as_i64()).unwrap_or(1);
            ctx.db.with_snapshot(|snap| {
                snap.mails
                    .iter()
                    .filter(|m| {
                        if m.recipient_id != char_id && m.sender_id != char_id {
                            return false;
                        }
                        if snap.deleted_mails.contains(&(char_id, m.message_id))
                            || snap.trashed_mails.contains(&(char_id, m.message_id))
                        {
                            return false;
                        }
                        let labels = snap
                            .mail_assigned_labels
                            .get(&(char_id, m.message_id))
                            .cloned()
                            .unwrap_or_else(|| vec![if m.sender_id == char_id { 2 } else { 1 }]);
                        labels.contains(&label_id)
                    })
                    .map(|m| m.message_id)
                    .collect()
            })
        }
        "MoveToTrash" => {
            let mids = extract_id_list(arg);
            ctx.db.with_snapshot(|snap| {
                mids.into_iter()
                    .filter(|&mid| {
                        snap.mails.iter().any(|m| {
                            m.message_id == mid
                                && (m.recipient_id == char_id || m.sender_id == char_id)
                        }) && !snap.deleted_mails.contains(&(char_id, mid))
                    })
                    .collect()
            })
        }
        "MarkAsReadByList" | "MarkAsUnreadByList" | "MoveToTrashByList" => {
            if let Some(lid) = arg.and_then(|a| a.as_i64()) {
                ctx.db.with_snapshot(|snap| {
                    if let Some(ml) = snap.mailing_lists.get(&lid) {
                        let mut mids: Vec<i64> = snap.mails
                            .iter()
                            .filter(|m| {
                                (m.recipient_id == char_id || m.sender_id == char_id)
                                    && !snap.deleted_mails.contains(&(char_id, m.message_id))
                                    && ml.mail_ids.contains(&m.message_id)
                            })
                            .map(|m| m.message_id)
                            .collect();
                        mids.sort_unstable();
                        mids.dedup();
                        mids
                    } else {
                        Vec::new()
                    }
                })
            } else {
                let raw_ids = extract_id_list(arg);
                ctx.db.with_snapshot(|snap| {
                    raw_ids
                        .into_iter()
                        .filter(|&mid| {
                            snap.mails.iter().any(|m| {
                                m.message_id == mid
                                    && (m.recipient_id == char_id || m.sender_id == char_id)
                            }) && !snap.deleted_mails.contains(&(char_id, mid))
                        })
                        .collect()
                })
            }
        }
        _ => extract_id_list(arg),
    }
}

fn handle_mail_ops(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "MarkAsReadByList" | "MarkAsUnreadByList" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mark_read = method == "MarkAsReadByList";
            let mail_ids = resolve_target_message_ids(ctx, char_id, method, args.first());

            ctx.db
                .try_with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        if mark_read {
                            snap.read_mails.insert((char_id, mid));
                        } else {
                            snap.read_mails.remove(&(char_id, mid));
                        }
                    }
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();

            for &mid in &mail_ids {
                let mut meta = ctx.esi_state.get_mail_meta(char_id, mid, false);
                meta.is_read = mark_read;
                ctx.esi_state.set_mail_meta(char_id, mid, meta);
            }

            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMailCountersUpdate",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            if !mail_ids.is_empty() {
                mail_notify::emit_mail_updated_by_external(ctx, char_id, &mail_ids, Some(mark_read), None);
            }

            let first_mid = mail_ids.first().copied().unwrap_or(0);
            let rep_list = PyRep::List(mail_ids.into_iter().map(PyRep::Integer).collect());
            Some(Ok(PyRep::keyval([
                ("messageID", PyRep::Integer(first_mid)),
                ("messageIDs", rep_list),
                ("read", PyRep::Bool(mark_read)),
            ])))
        }

        "MoveAllToTrash" | "MoveToTrash" | "MoveToTrashByLabel" | "MoveToTrashByList" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mail_ids = resolve_target_message_ids(ctx, char_id, method, args.first());
            let mut trashed_ids = Vec::new();
            {
                let mut s = ctx.social_extras.lock().unwrap();
                ctx.db.with_mut_snapshot(|snap| {
                    for &mid in &mail_ids {
                        let is_mine = snap.mails.iter().any(|m| {
                            m.message_id == mid && (m.recipient_id == char_id || m.sender_id == char_id)
                        });
                        let is_deleted = snap.deleted_mails.contains(&(char_id, mid));
                        if is_mine && !is_deleted {
                            let added_s = s.trashed_mails.insert((char_id, mid));
                            let added_db = snap.trashed_mails.insert((char_id, mid));
                            if added_s || added_db {
                                trashed_ids.push(mid);
                            }
                        }
                    }
                });
            }
            if !trashed_ids.is_empty() {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnMailTrashed",
                    "charid",
                    vec![PyRep::List(trashed_ids.iter().copied().map(PyRep::Integer).collect())],
                );
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnMailCountersUpdate",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::keyval([
                ("trashed", PyRep::Bool(!trashed_ids.is_empty())),
                ("inTrash", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}

fn handle_mailing_list_ops(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "ClearEntityAccess" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let entity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            ml.entity_access.remove(&entity_id);
            drop(s);
            sync_mailing_lists_to_db(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        "GetWelcomeMail" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };

            let (corp_id, alliance_id) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id))
                    .unwrap_or((0, None))
            });
            let effective_acc = ml
                .entity_access
                .get(&char_id)
                .or_else(|| ml.entity_access.get(&corp_id))
                .or_else(|| alliance_id.and_then(|aid| ml.entity_access.get(&aid)))
                .copied()
                .unwrap_or(ml.default_access);

            let is_member = ml.members.contains_key(&char_id);
            let has_access = ml.owner_id == char_id || is_member || effective_acc > 0;
            if !has_access {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "MailingListAccessDenied".into(),
                }));
            }

            match &ml.welcome_mail {
                Some((title, body)) => {
                    let item = PyRep::keyval([
                        ("title", PyRep::String(title.clone())),
                        ("body", PyRep::String(body.clone())),
                    ]);
                    Some(Ok(PyRep::List(vec![item])))
                }
                None => Some(Ok(PyRep::List(Vec::new()))),
            }
        }

        "ClearWelcomeMail" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            ml.welcome_mail = None;
            drop(s);
            sync_mailing_lists_to_db(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        "Delete" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list not found".into(),
                }));
            };
            if ml.owner_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "MailingListAccessDenied".into(),
                }));
            }
            let removed = s.mailing_lists.remove(&list_id).unwrap();
            let other_members: Vec<i64> = removed
                .members
                .keys()
                .copied()
                .filter(|&cid| cid != char_id)
                .collect();
            drop(s);
            sync_mailing_lists_to_db(ctx);
            mail_notify::emit_mailing_list_list_id(
                ctx,
                "OnMailingListDeleted",
                list_id,
                &other_members,
            );
            Some(Ok(PyRep::Bool(true)))
        }

        "GetSettings" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };

            let (corp_id, alliance_id) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id))
                    .unwrap_or((0, None))
            });
            let effective_acc = ml
                .entity_access
                .get(&char_id)
                .or_else(|| ml.entity_access.get(&corp_id))
                .or_else(|| alliance_id.and_then(|aid| ml.entity_access.get(&aid)))
                .copied()
                .unwrap_or(ml.default_access);

            let is_member = ml.members.contains_key(&char_id);
            let has_access = ml.owner_id == char_id || is_member || effective_acc > 0;
            if !has_access {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "MailingListAccessDenied".into(),
                }));
            }

            let welcome_rep = match &ml.welcome_mail {
                Some((title, body)) => PyRep::keyval([
                    ("title", PyRep::String(title.clone())),
                    ("body", PyRep::String(body.clone())),
                ]),
                None => PyRep::None,
            };
            let access_dict: Vec<(PyRep, PyRep)> = ml
                .entity_access
                .iter()
                .map(|(&eid, &lvl)| (PyRep::Integer(eid), PyRep::Integer(lvl)))
                .collect();

            let resp = PyRep::keyval([
                ("listID", PyRep::Integer(ml.list_id)),
                ("defaultAccess", PyRep::Integer(ml.default_access)),
                ("defaultMemberAccess", PyRep::Integer(ml.default_role)),
                ("defaultRole", PyRep::Integer(ml.default_role)),
                ("cost", PyRep::Integer(ml.cost)),
                ("mailCost", PyRep::Integer(ml.cost)),
                ("access", PyRep::Dict(access_dict)),
                ("welcomeMail", welcome_rep),
            ]);
            Some(Ok(resp))
        }

        "KickMembers" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let targets = extract_id_list(args.get(1));

            let mut affected = Vec::new();
            {
                let mut s = ctx.social_extras.lock().unwrap();
                let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: format!("Mailing list {list_id} not found"),
                    }));
                };
                if !caller_has_operator_or_owner_role(ml, char_id) {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "Mailing list operator or owner role required".into(),
                    }));
                }
                let is_owner = char_id == ml.owner_id;
                for tid in targets {
                    if tid == ml.owner_id {
                        continue; // Cannot kick the owner
                    }
                    if let Some(&current_role) = ml.members.get(&tid) {
                        if !is_owner && current_role >= 2 {
                            continue; // Operators cannot kick other operators
                        }
                        ml.members.remove(&tid);
                        affected.push(tid);
                    }
                }
            }
            sync_mailing_lists_to_db(ctx);
            if !affected.is_empty() {
                mail_notify::emit_mailing_list_leave(ctx, list_id, &affected);
            }
            Some(Ok(PyRep::Bool(true)))
        }

        "SaveWelcomeMail" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let title = args.get(1).and_then(|a| a.as_str()).unwrap_or("Welcome!");
            let body = args.get(2).and_then(|a| a.as_str()).unwrap_or("o7");

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            ml.welcome_mail = Some((title.to_string(), body.to_string()));
            drop(s);
            sync_mailing_lists_to_db(ctx);
            Some(Ok(PyRep::keyval([
                ("title", PyRep::String(title.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        "SendWelcomeMail" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            
            let title = args.get(1).and_then(|a| a.as_str()).map(|s| s.to_string())
                .unwrap_or_else(|| ml.welcome_mail.as_ref().map(|w| w.0.clone()).unwrap_or_else(|| "Welcome!".to_string()));
            let body = args.get(2).and_then(|a| a.as_str()).map(|s| s.to_string())
                .unwrap_or_else(|| ml.welcome_mail.as_ref().map(|w| w.1.clone()).unwrap_or_else(|| "o7".to_string()));

            let members: Vec<i64> = ml.members.keys().copied().filter(|&c| c != char_id).collect();
            drop(s);

            let now_unix = ctx.now_unix();
            let mut sent_mids = Vec::new();
            ctx.db.with_mut_snapshot(|snap| {
                for &tid in &members {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord {
                        message_id: mid,
                        sender_id: char_id,
                        recipient_id: tid,
                        subject: title.clone(),
                        body: body.clone(),
                    });
                    snap.mail_assigned_labels.insert((tid, mid), vec![1]);
                    snap.mail_timestamps.insert(mid, now_unix);
                    if let Some(ml) = snap.mailing_lists.get_mut(&list_id) {
                        ml.mail_ids.insert(mid);
                    }
                    sent_mids.push((tid, mid));
                }
            });
            {
                let mut s = ctx.social_extras.lock().unwrap();
                if let Some(ml) = s.mailing_lists.get_mut(&list_id) {
                    for &(_, mid) in &sent_mids {
                        ml.mail_ids.insert(mid);
                    }
                }
            }
            for (tid, mid) in sent_mids {
                let sent = mail_notify::SentMail {
                    message_id: mid,
                    sender_id: char_id,
                    to_character_ids: &[tid],
                    to_list_id: Some(list_id),
                    to_corp_or_alliance_id: None,
                    title: &title,
                    status_mask: mail_notify::MAIL_STATUS_AUTOMATED,
                };
                mail_notify::emit_mail_sent(ctx, &sent, &[tid]);
            }
            Some(Ok(PyRep::Bool(true)))
        }

        "SetDefaultAccess" | "SaveSettings" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let default_access = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let default_role = args.get(2).and_then(|a| a.as_i64());
            let mail_cost = args.get(3).and_then(|a| a.as_i64());

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            ml.default_access = default_access.clamp(0, 2);
            if let Some(r) = default_role {
                ml.default_role = r.clamp(0, 1);
            }
            if let Some(c) = mail_cost {
                ml.cost = c.max(0);
            }
            let resp = PyRep::keyval([
                ("listID", PyRep::Integer(ml.list_id)),
                ("defaultAccess", PyRep::Integer(ml.default_access)),
                ("defaultMemberAccess", PyRep::Integer(ml.default_role)),
                ("defaultRole", PyRep::Integer(ml.default_role)),
                ("cost", PyRep::Integer(ml.cost)),
            ]);
            drop(s);
            sync_mailing_lists_to_db(ctx);
            Some(Ok(resp))
        }

        "SetEntitiesAccess" | "SetEntityAccess" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let mut s = ctx.social_extras.lock().unwrap();
            let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Mailing list {list_id} not found"),
                }));
            };
            if !caller_has_operator_or_owner_role(ml, char_id) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Mailing list operator or owner role required".into(),
                }));
            }
            if let Some(PyRep::Dict(map)) = args.get(1) {
                for (k, v) in map {
                    if let (Some(eid), Some(acc)) = (k.as_i64(), v.as_i64()) {
                        if eid > 0 {
                            ml.entity_access.insert(eid, acc.clamp(0, 2));
                        }
                    }
                }
            } else {
                let entity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                if entity_id <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "Invalid entity ID".into(),
                    }));
                }
                let access = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
                ml.entity_access.insert(entity_id, access.clamp(0, 2));
            }
            let resp = PyRep::keyval([
                ("listID", PyRep::Integer(ml.list_id)),
                ("entityAccessCount", PyRep::Integer(ml.entity_access.len() as i64)),
            ]);
            drop(s);
            sync_mailing_lists_to_db(ctx);
            Some(Ok(resp))
        }

        "SetMembersClear" | "SetMembersMuted" | "SetMembersOperator" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let targets = extract_id_list(args.get(1));

            let new_role = match method {
                "SetMembersOperator" => 2,
                "SetMembersMuted" => 0,
                _ => 1,
            };

            let mut affected = Vec::new();
            {
                let mut s = ctx.social_extras.lock().unwrap();
                let Some(ml) = s.mailing_lists.get_mut(&list_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: format!("Mailing list {list_id} not found"),
                    }));
                };
                if !caller_has_operator_or_owner_role(ml, char_id) {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "Mailing list operator or owner role required".into(),
                    }));
                }
                let is_owner = char_id == ml.owner_id;
                for tid in targets {
                    if tid == ml.owner_id {
                        continue; // Cannot mute, demote or alter list owner's role
                    }
                    if let Some(&current_role) = ml.members.get(&tid) {
                        if !is_owner && method != "SetMembersOperator" && current_role >= 2 {
                            continue; // Operators cannot demote/mute other operators
                        }
                        ml.members.insert(tid, new_role);
                        affected.push(tid);
                    }
                }
            }
            sync_mailing_lists_to_db(ctx);

            if !affected.is_empty() {
                let notif_name = match method {
                    "SetMembersOperator" => "OnMailingListSetOperator",
                    "SetMembersMuted" => "OnMailingListSetMuted",
                    _ => "OnMailingListSetClear",
                };
                mail_notify::emit_mailing_list_list_id(ctx, notif_name, list_id, &affected);
            }
            Some(Ok(PyRep::Bool(true)))
        }

        _ => None,
    }
}

fn caller_has_operator_or_owner_role(ml: &MailingListState, char_id: i64) -> bool {
    ml.owner_id == char_id || ml.members.get(&char_id).copied().unwrap_or(0) >= 2
}
