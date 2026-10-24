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

pub(crate) fn resolve_mail_targets(
    ctx: &ServerContext,
    char_id: i64,
    method: &str,
    arg: Option<&PyRep>,
) -> Vec<i64> {
    if method.ends_with("ByLabel") {
        let lid = arg.and_then(|a| a.as_i64()).unwrap_or(1);
        let ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.mails
                .iter()
                .filter_map(|m| {
                    if m.recipient_id != char_id && m.sender_id != char_id {
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
        });
        if ids.is_empty() {
            extract_id_list(arg)
        } else {
            ids
        }
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
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let trashed = crate::services::fleet_social::get_trashed_mails(ctx);
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
            let sent_filetime = if let Some(&ts) = snap.mail_timestamps.get(&m.message_id) {
                if ts > 100_000_000_000_000_000 {
                    ts as i64
                } else {
                    ((ts as i64) + 11_644_473_600) * 10_000_000
                }
            } else {
                ((ctx.now_unix() as i64) + 11_644_473_600) * 10_000_000
            };
            hdrs.push(vec![
                PyRep::Integer(m.message_id),
                PyRep::Integer(m.sender_id),
                PyRep::List(vec![PyRep::Integer(m.recipient_id)]),
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
