//! EVE mail, mailing-list, and calendar notifications in the argument layout of the V24 client's
//! `eve/client/script/ui/services/mail/mailSvc.pyj`,
//! `eve/client/script/ui/services/mail/mailingListsSvc.pyj`, and
//! `eve/client/script/ui/services/eveCalendarsvc.pyj`.
//!
//! `OnNewMailReceived` and `OnMailCountersUpdate` are *not* sent from here: `mailSvc.OnMailSent`
//! calls `self.OnNewMailReceived(header)` and `mailSvc` scatters `OnMailCountersUpdate` itself.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub struct MailHeaderMeta {
    pub to_corp_or_alliance_id: Option<i64>,
    pub to_list_id: Option<i64>,
    pub to_character_ids: Vec<i64>,
}

pub(crate) fn record_mail_meta(
    ctx: &ServerContext,
    mid: i64,
    to_chars: &[i64],
    to_list: Option<i64>,
    to_corp_or_alliance: Option<i64>,
) {
    let mut s = ctx.social_extras.lock().unwrap();
    s.mail_headers_meta.insert(
        mid,
        MailHeaderMeta {
            to_corp_or_alliance_id: to_corp_or_alliance,
            to_list_id: to_list,
            to_character_ids: to_chars.to_vec(),
        },
    );
}

fn extract_id_list(arg: Option<&PyRep>) -> Vec<i64> {
    match arg {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    }
}

/// `eve/common/lib/appConst.pyj`: `mailStatusMaskAutomated = 32`.
pub(crate) const MAIL_STATUS_AUTOMATED: i64 = 32;
/// `const.MIN` in 100 ns FILETIME ticks; `mailSvc.SendMail` truncates `sentDate` to the minute.
const FILETIME_MINUTE: i64 = 600_000_000;

/// One sent mail as `mailSvc.OnMailSent(messageID, senderID, sentDate, toCharacterIDs, toListID,
/// toCorpOrAllianceID, title, statusMask)` unpacks it.
pub(crate) struct SentMail<'a> {
    pub message_id: i64,
    pub sender_id: i64,
    pub to_character_ids: &'a [i64],
    pub to_list_id: Option<i64>,
    pub to_corp_or_alliance_id: Option<i64>,
    pub title: &'a str,
    pub status_mask: i64,
}

/// Sends `OnMailSent` to every distinct character in `recipients` (each client files it into its
/// inbox / sent folder and raises its own `OnNewMailReceived`).
pub(crate) fn emit_mail_sent(ctx: &ServerContext, mail: &SentMail<'_>, recipients: &[i64]) {
    let sent_date = ctx.now_blueos_filetime_ticks() / FILETIME_MINUTE * FILETIME_MINUTE;
    let opt = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
    let args = vec![
        PyRep::Integer(mail.message_id),
        PyRep::Integer(mail.sender_id),
        PyRep::Integer(sent_date),
        PyRep::List(mail.to_character_ids.iter().map(|&c| PyRep::Integer(c)).collect()),
        opt(mail.to_list_id),
        opt(mail.to_corp_or_alliance_id),
        PyRep::String(mail.title.to_string()),
        PyRep::Integer(mail.status_mask),
    ];
    let mut seen = Vec::with_capacity(recipients.len());
    for &cid in recipients {
        if !seen.contains(&cid) {
            seen.push(cid);
            ctx.notify(NotificationTarget::Character(cid), "OnMailSent", "charid", args.clone());
        }
    }
}

/// `mailSvc.OnMailDeleted(messageIDs)`: the mail was deleted outside the client (ESI).
pub(crate) fn emit_mail_deleted(ctx: &ServerContext, char_id: i64, message_ids: &[i64]) {
    let ids = PyRep::List(message_ids.iter().map(|&m| PyRep::Integer(m)).collect());
    ctx.notify(NotificationTarget::Character(char_id), "OnMailDeleted", "charid", vec![ids]);
}

/// `mailSvc.OnMailUpdatedByExternal(messageIDs, is_read, new_label)`: read flag and/or label mask
/// changed outside the client; `None` leaves that field alone, `new_label` is a label bitmask
/// (the client applies it with `ReplaceLabels(messageIDs, new_label, notifyServer=False)`).
pub(crate) fn emit_mail_updated_by_external(
    ctx: &ServerContext,
    char_id: i64,
    message_ids: &[i64],
    is_read: Option<bool>,
    new_label: Option<i64>,
) {
    let args = vec![
        PyRep::List(message_ids.iter().map(|&m| PyRep::Integer(m)).collect()),
        is_read.map(PyRep::Bool).unwrap_or(PyRep::None),
        new_label.map(PyRep::Integer).unwrap_or(PyRep::None),
    ];
    ctx.notify(NotificationTarget::Character(char_id), "OnMailUpdatedByExternal", "charid", args);
}

/// `mailSvc.OnLabelsCreatedByExternal(label)`: unpacks `label.labelID`, `label.name`, `label.color`
/// when a custom mail label is created via ESI `POST /characters/{character_id}/mail/labels`.
pub(crate) fn emit_labels_created_by_external(
    ctx: &ServerContext,
    char_id: i64,
    label_id: i64,
    name: &str,
    color: &str,
) {
    let label = PyRep::keyval([
        ("labelID", PyRep::Integer(label_id)),
        ("name", PyRep::String(name.to_string())),
        ("color", PyRep::String(color.to_string())),
    ]);
    ctx.notify(NotificationTarget::Character(char_id), "OnLabelsCreatedByExternal", "charid", vec![label]);
}

/// `mailingListsSvc.OnMailingListSetOperator(listID)` / `OnMailingListSetMuted(listID)` /
/// `OnMailingListSetClear(listID)` / `OnMailingListDeleted(listID)`.
pub(crate) fn emit_mailing_list_list_id(ctx: &ServerContext, event: &str, list_id: i64, recipients: &[i64]) {
    let args = vec![PyRep::Integer(list_id)];
    for &cid in recipients {
        ctx.notify(NotificationTarget::Character(cid), event, "charid", args.clone());
    }
}

/// `mailingListsSvc.OnMailingListLeave(listID, characterID)`: sent to a member kicked from `listID`.
pub(crate) fn emit_mailing_list_leave(ctx: &ServerContext, list_id: i64, kicked_ids: &[i64]) {
    for &cid in kicked_ids {
        let args = vec![PyRep::Integer(list_id), PyRep::Integer(cid)];
        ctx.notify(NotificationTarget::Character(cid), "OnMailingListLeave", "charid", args);
    }
}

/// `eveCalendarsvc.OnEventResponseByExternal(eventID, eventKV, response)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_event_response_by_external(
    ctx: &ServerContext,
    char_id: i64,
    event_id: i64,
    owner_id: i64,
    event_date_time: i64,
    duration: i64,
    title: &str,
    importance: i64,
    response_code: i64,
) {
    let event_kv = PyRep::keyval([
        ("eventID", PyRep::Integer(event_id)),
        ("ownerID", PyRep::Integer(owner_id)),
        ("eventDateTime", PyRep::Integer(event_date_time)),
        ("eventDuration", PyRep::Integer(duration)),
        ("eventTitle", PyRep::String(title.to_string())),
        ("importance", PyRep::Integer(importance)),
        ("flag", PyRep::Integer(owner_id)),
        ("isDeleted", PyRep::Bool(false)),
    ]);
    let args = vec![
        PyRep::Integer(event_id),
        event_kv,
        PyRep::Integer(response_code),
    ];
    ctx.notify(NotificationTarget::Character(char_id), "OnEventResponseByExternal", "charid", args);
}

/// `xmppchatclient/xmppchatsvc.pyj:XmppChatService.OnCharacterPokedByGM(gmCharID, channelName)`:
/// notifies `target_char_id` that `gm_char_id` poked them about chat channel `channel_name`.
pub(crate) fn emit_character_poked_by_gm(
    ctx: &ServerContext,
    target_char_id: i64,
    gm_char_id: i64,
    channel_name: &str,
) {
    let args = vec![
        PyRep::Integer(gm_char_id),
        PyRep::String(channel_name.to_string()),
    ];
    ctx.notify(
        NotificationTarget::Character(target_char_id),
        "OnCharacterPokedByGM",
        "charid",
        args,
    );
}

pub(crate) fn resolve_mail_targets(
    ctx: &ServerContext,
    char_id: i64,
    method: &str,
    arg: Option<&PyRep>,
) -> Vec<i64> {
    if matches!(
        method,
        "MarkAsReadByLabel" | "MarkAsUnreadByLabel" | "MoveToTrashByLabel"
    ) {
        let lid = arg.and_then(|a| a.as_i64()).unwrap_or(1);
        let trashed = ctx.social_extras.lock().unwrap().trashed_mails.clone();
        ctx.db.with_snapshot(|snap| {
            snap.mails
                .iter()
                .filter_map(|m| {
                    if m.recipient_id != char_id && m.sender_id != char_id {
                        return None;
                    }
                    if snap.deleted_mails.contains(&(char_id, m.message_id))
                        || snap.trashed_mails.contains(&(char_id, m.message_id))
                        || trashed.contains(&(char_id, m.message_id))
                    {
                        return None;
                    }
                    let labels = snap
                        .mail_assigned_labels
                        .get(&(char_id, m.message_id))
                        .cloned()
                        .unwrap_or_else(|| vec![if m.sender_id == char_id { 2 } else { 1 }]);
                    if labels.contains(&lid) {
                        Some(m.message_id)
                    } else {
                        None
                    }
                })
                .collect()
        })
    } else if method == "MoveAllToTrash" {
        ctx.db.with_snapshot(|snap| {
            snap.mails
                .iter()
                .filter_map(|m| {
                    if (m.recipient_id == char_id || m.sender_id == char_id)
                        && !snap.deleted_mails.contains(&(char_id, m.message_id))
                    {
                        Some(m.message_id)
                    } else {
                        None
                    }
                })
                .collect()
        })
    } else {
        extract_id_list(arg)
    }
}

pub(crate) fn handle_mail_headers(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let to_fetch: Option<HashSet<i64>> = if method == "GetMailHeaders" {
        args.first().and_then(|a| match a {
            PyRep::List(items) | PyRep::Tuple(items) => {
                let s: HashSet<i64> = items.iter().filter_map(|x| x.as_i64()).collect();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            }
            PyRep::Integer(id) => {
                let mut s = HashSet::new();
                s.insert(*id);
                Some(s)
            }
            _ => None,
        })
    } else {
        None
    };

    let trashed = crate::services::fleet_social::get_trashed_mails(ctx);
    let (rows, status_rows) = ctx.db.with_snapshot(|snap| {
        let mut hdrs = Vec::new();
        let mut statuses = Vec::new();
        let mut seen = HashSet::new();
        let meta_map = ctx.social_extras.lock().unwrap().mail_headers_meta.clone();

        for m in snap.mails.iter() {
            if !seen.insert(m.message_id) {
                continue;
            }
            if let Some(ref fetch_set) = to_fetch {
                if !fetch_set.contains(&m.message_id) {
                    continue;
                }
            }
            if snap.deleted_mails.contains(&(char_id, m.message_id))
                || trashed.contains(&(char_id, m.message_id))
            {
                continue;
            }

            let meta_opt = meta_map.get(&m.message_id);
            let to_list_id = meta_opt
                .and_then(|meta| meta.to_list_id)
                .or_else(|| snap.mailing_lists.values().find(|ml| ml.mail_ids.contains(&m.message_id)).map(|ml| ml.list_id));
            let to_corp_or_alliance_id = meta_opt.and_then(|meta| meta.to_corp_or_alliance_id);
            let to_character_ids: Vec<i64> = meta_opt
                .map(|meta| meta.to_character_ids.clone())
                .unwrap_or_else(|| {
                    let mut recs: Vec<i64> = snap
                        .mails
                        .iter()
                        .filter(|other| other.message_id == m.message_id)
                        .map(|other| other.recipient_id)
                        .collect();
                    recs.sort_unstable();
                    recs.dedup();
                    recs
                });

            let is_sender = m.sender_id == char_id;
            let is_recipient = to_character_ids.contains(&char_id) || m.recipient_id == char_id;
            let is_list_member = to_list_id.is_some_and(|lid| {
                snap.mailing_lists.get(&lid).is_some_and(|ml| ml.members.contains_key(&char_id))
            });

            if !is_sender && !is_recipient && !is_list_member {
                continue;
            }

            let read = snap.read_mails.contains(&(char_id, m.message_id));
            let labels = snap
                .mail_assigned_labels
                .get(&(char_id, m.message_id))
                .cloned()
                .unwrap_or_else(|| vec![if m.sender_id == char_id { 2 } else { 1 }]);
            let label_mask: i64 = labels.iter().copied().fold(0i64, |acc, l| acc | l);
            let sent_filetime = if let Some(&ts) = snap.mail_timestamps.get(&m.message_id) {
                if ts > 100_000_000_000_000_000 {
                    ts as i64
                } else {
                    ((ts as i64) + 11_644_473_600) * 10_000_000
                }
            } else {
                ((ctx.now_unix() as i64) + 11_644_473_600) * 10_000_000
            };

            let to_chars_str = to_character_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");

            hdrs.push(vec![
                PyRep::Integer(m.message_id),
                PyRep::Integer(m.sender_id),
                PyRep::String(to_chars_str),
                to_corp_or_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                to_list_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::String(m.subject.clone()),
                PyRep::Integer(sent_filetime),
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

    let headers_columns = vec![
        "messageID",
        "senderID",
        "toCharacterIDs",
        "toCorpOrAllianceID",
        "toListID",
        "title",
        "sentDate",
        "read",
        "labels",
    ];

    let headers_rowset = PyRep::rowset(headers_columns.clone(), rows);
    if method == "SyncMail" {
        let old_mail_rowset = PyRep::rowset(headers_columns, Vec::new());
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

pub(crate) fn handle_poke_player_about_chat_msg_gm(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !session.has_gm_role() {
        return Some(Err(BridgeRpcError {
            code: "UserError".into(),
            message: "Requires GM role".into(),
        }));
    }
    let (_, gm_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(target_char_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Some(Err(BridgeRpcError::user_error("InvalidTargetCharacter")));
    };
    let Some(channel_name) = args
        .get(1)
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Some(Err(BridgeRpcError::user_error("PokeMessageRequired")));
    };
    if channel_name.chars().count() > 4000 {
        return Some(Err(BridgeRpcError::user_error("CustomInfo")));
    }
    // DERIVED: V24 client `mailMgr.PokePlayerAboutChatMsgGm(targetCharID, channelName)` does not pass
    // a separate mail subject; use a fixed subject line for the accompanying GM notification mail.
    const GM_POKE_MAIL_SUBJECT: &str = "GM Chat Notification";
    let now_unix = ctx.now_unix();
    let poke_res = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&target_char_id) {
            return Err(BridgeRpcError::user_error("TargetCharacterNotFound"));
        }
        snap.next_entity_id += 1;
        let mid = snap.next_entity_id;
        snap.mails.push(eve_db::MailRecord {
            message_id: mid,
            sender_id: gm_id,
            recipient_id: target_char_id,
            subject: GM_POKE_MAIL_SUBJECT.to_string(),
            body: channel_name.to_string(),
        });
        snap.mail_assigned_labels.insert((target_char_id, mid), vec![1]);
        snap.mail_timestamps.insert(mid, now_unix);
        Ok(mid)
    });
    let mid = match poke_res {
        Ok(id) => id,
        Err(e) => return Some(Err(e)),
    };
    emit_character_poked_by_gm(ctx, target_char_id, gm_id, channel_name);
    let recipients = [target_char_id];
    let sent = SentMail {
        message_id: mid,
        sender_id: gm_id,
        to_character_ids: &recipients,
        to_list_id: None,
        to_corp_or_alliance_id: None,
        title: GM_POKE_MAIL_SUBJECT,
        status_mask: 0,
    };
    emit_mail_sent(ctx, &sent, &recipients);
    Some(Ok(PyRep::Integer(mid)))
}

pub fn is_mail_authorized(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    mid: i64,
    char_id: i64,
) -> bool {
    let matching: Vec<_> = snap.mails.iter().filter(|m| m.message_id == mid).collect();
    if matching.is_empty() {
        return false;
    }
    let is_sender = matching.iter().any(|m| m.sender_id == char_id);
    let is_recipient = matching.iter().any(|m| m.recipient_id == char_id);
    let is_meta_recipient = {
        let s = ctx.social_extras.lock().unwrap();
        s.mail_headers_meta
            .get(&mid)
            .map(|meta| meta.to_character_ids.contains(&char_id))
            .unwrap_or(false)
    };
    let is_list_member = snap
        .mailing_lists
        .values()
        .any(|ml| ml.mail_ids.contains(&mid) && ml.members.contains_key(&char_id));
    is_sender || is_recipient || is_meta_recipient || is_list_member
}

pub fn check_mails_authorized(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    mids: &[i64],
    char_id: i64,
) -> bool {
    if mids.is_empty() {
        return true;
    }
    mids.iter().all(|&mid| is_mail_authorized(ctx, snap, mid, char_id))
}

