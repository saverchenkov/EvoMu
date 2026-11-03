//! Server-side V24 notification emitters wired to production RPCs and ticks.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

/// Emits `OnEffectMessage(msgkey, args)` (`2` args, `id_type = "charid"`).
/// Matches `eve/client/script/environment/godma.pyj:Godma.OnEffectMessage`.
pub fn emit_effect_message(
    ctx: &ServerContext,
    char_id: i64,
    msg_key: &str,
    msg_args: &[(&str, PyRep)],
) {
    let args_dict = msg_args
        .iter()
        .map(|(k, v)| (PyRep::String((*k).to_string()), v.clone()))
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEffectMessage",
        "charid",
        vec![PyRep::String(msg_key.to_string()), PyRep::Dict(args_dict)],
    );
}

/// Emits `OnPlexDepositFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexDepositListener.OnPlexDepositFailed`.
pub fn emit_plex_deposit_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDepositFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}

/// Emits `OnPlexWithdrawFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexWithdrawalListener.OnPlexWithdrawFailed`.
pub fn emit_plex_withdraw_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexWithdrawFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}

/// Emits `OnBeanPrime(beans)` (`1` arg, `id_type = "charid"` / broadcast).
/// Matches `carbon/common/script/sys/alert.pyj:Alert.OnBeanPrime`.
pub fn emit_bean_prime(
    ctx: &ServerContext,
    target: NotificationTarget,
    beans: Vec<(i64, i64, String)>,
) {
    let dict_entries = beans
        .into_iter()
        .map(|(hash, err_id, mode)| {
            (
                PyRep::Integer(hash),
                PyRep::Tuple(vec![PyRep::Integer(err_id), PyRep::String(mode)]),
            )
        })
        .collect();
    ctx.notify(
        target,
        "OnBeanPrime",
        "charid",
        vec![PyRep::Dict(dict_entries)],
    );
}

/// Emits `OnClearFetchedData()` (`0` args, cluster broadcast).
/// Matches `carbon/common/script/net/machoNet.pyj:MachoNetService.OnClearFetchedData`.
pub fn emit_clear_fetched_data(ctx: &ServerContext) {
    ctx.notify(
        NotificationTarget::All,
        "OnClearFetchedData",
        "clientID",
        vec![],
    );
}

/// Emits `OnFlaggedListsChange()` (`0` args, broadcast).
/// Matches `carbonui/control/browser/sites.pyj:SitesSvc.OnFlaggedListsChange`.
pub fn emit_flagged_lists_change(ctx: &ServerContext) {
    ctx.notify(
        NotificationTarget::All,
        "OnFlaggedListsChange",
        "clientID",
        vec![],
    );
}

/// Emits `OnNotificationUndeleted(notificationIDs)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/ui/services/mail/notificationSvc.pyj:notificationSvc.OnNotificationUndeleted`.
pub fn emit_notification_undeleted(
    ctx: &ServerContext,
    char_id: i64,
    notification_ids: &[i64],
) {
    let ids = notification_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNotificationUndeleted",
        "charid",
        vec![PyRep::List(ids)],
    );
}

/// Restores (undeletes) deleted notifications for `char_id` and emits `OnNotificationUndeleted`.
pub fn restore_character_notifications(
    ctx: &ServerContext,
    char_id: i64,
    notification_ids: &[i64],
) -> usize {
    let id_set: std::collections::HashSet<i64> = notification_ids.iter().copied().collect();
    let mut restored = Vec::new();
    {
        let mut records = ctx
            .notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for n in records.iter_mut() {
            if n.receiver_id == char_id && id_set.contains(&n.notification_id) && n.deleted {
                n.deleted = false;
                restored.push(n.notification_id);
            }
        }
    }
    if !restored.is_empty() {
        ctx.sync_character_notifications_to_db(char_id);
        emit_notification_undeleted(ctx, char_id, &restored);
    }
    restored.len()
}

