//! EVE mail notifications in the argument layout of the V24 client's
//! `eve/client/script/ui/services/mail/mailSvc.pyj` (`mailSvc.On…` handlers).
//!
//! `OnNewMailReceived` and `OnMailCountersUpdate` are *not* sent from here: `mailSvc.OnMailSent`
//! calls `self.OnNewMailReceived(header)` and `mailSvc` scatters `OnMailCountersUpdate` itself.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_proto::PyRep;

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
