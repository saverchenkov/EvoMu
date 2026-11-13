//! Server-side V24 notification emitters wired to production RPCs and ticks.

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    OnBeanPrime, OnClearFetchedData, OnClientEventMarketItemReceived, OnDungeonCameraInterest,
    OnDungeonTriggerAmbientSound, OnDungeonTriggerAudio, OnDungeonTriggerMessage,
    OnDungeonTriggerMusic, OnEffectMessage, OnFlaggedListsChange, OnNotificationUndeleted,
    OnPlexDepositFailed, OnPlexWithdrawFailed,
};
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
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnEffectMessage {
            msgkey: msg_key.to_string(),
            args: PyRep::Dict(args_dict),
        },
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
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnPlexDepositFailed {
            error_key: error_key.to_string(),
            reference,
        },
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
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnPlexWithdrawFailed {
            error_key: error_key.to_string(),
            reference,
        },
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
    ctx.emit_typed(
        target,
        OnBeanPrime {
            beans: PyRep::Dict(dict_entries),
        },
    );
}

/// Emits `OnClearFetchedData()` (`0` args, cluster broadcast).
/// Matches `carbon/common/script/net/machoNet.pyj:MachoNetService.OnClearFetchedData`.
pub fn emit_clear_fetched_data(ctx: &ServerContext) {
    ctx.emit_typed(NotificationTarget::All, OnClearFetchedData);
}

/// Emits `OnFlaggedListsChange()` (`0` args, broadcast).
/// Matches `carbonui/control/browser/sites.pyj:SitesSvc.OnFlaggedListsChange`.
pub fn emit_flagged_lists_change(ctx: &ServerContext) {
    ctx.emit_typed(NotificationTarget::All, OnFlaggedListsChange);
}

/// Emits `OnNotificationUndeleted(notificationIDs)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/ui/services/mail/notificationSvc.pyj:notificationSvc.OnNotificationUndeleted`.
pub fn emit_notification_undeleted(
    ctx: &ServerContext,
    char_id: i64,
    notification_ids: &[i64],
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnNotificationUndeleted {
            notification_ids: notification_ids.to_vec(),
        },
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

/// Emits `OnClientEvent_MarketItemReceived(itemID, typeID, flag, locationID)`.
/// Matches `eve/client/script/ui/shared/market/marketQuote.pyj`.
pub fn emit_client_event_market_item_received(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    type_id: i64,
    flag: i64,
    location_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnClientEventMarketItemReceived {
            item_id,
            type_id,
            flag_id: flag,
            location_id,
        },
    );
}

/// Emits `OnDungeonCameraInterest(itemID, speed, duration)` (`3` args, `id_type = "charid"`).
pub fn emit_dungeon_camera_interest(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    speed: f64,
    duration: f64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonCameraInterest {
            item_id,
            speed,
            duration,
        },
    );
}

/// Emits `OnDungeonTriggerAmbientSound(dungeonID, soundID, entityItemID)`.
pub fn emit_dungeon_trigger_ambient_sound(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    sound_id: i64,
    entity_item_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerAmbientSound {
            dungeon_id,
            sound_id,
            entity_item_id,
        },
    );
}

/// Emits `OnDungeonTriggerAudio(dungeonID, audioUrl)`.
pub fn emit_dungeon_trigger_audio(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    audio_url: &str,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerAudio {
            dungeon_id,
            audio_url: audio_url.to_string(),
        },
    );
}

/// Emits `OnDungeonTriggerMessage(messageType, messageID)`.
pub fn emit_dungeon_trigger_message(
    ctx: &ServerContext,
    char_id: i64,
    message_type: i64,
    message_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerMessage {
            message_type,
            message_id,
        },
    );
}

/// Emits `OnDungeonTriggerMusic(dungeonID, musicTriggerID)`.
pub fn emit_dungeon_trigger_music(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: i64,
    music_trigger_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDungeonTriggerMusic {
            dungeon_id,
            music_trigger_id,
        },
    );
}

#[cfg(test)]
#[path = "notify_emitters_missing_tests.rs"]
mod notify_emitters_missing_tests;


