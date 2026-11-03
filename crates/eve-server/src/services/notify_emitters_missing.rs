//! Server-side V24 notification emitters for remaining open `notify:*` parity rows.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{BridgeRpcError, PyRep};

/// Emits `OnBeanPrime(beans)` (`1` arg, `id_type = "charid"`).
/// Matches `carbon/common/script/sys/alert.pyj:Alert.OnBeanPrime`.
pub fn emit_bean_prime(ctx: &ServerContext, char_id: i64, beans: &[(i64, i64, i64)]) {
    let entries = beans
        .iter()
        .map(|&(error_id, count, last_ft)| {
            (
                PyRep::Integer(error_id),
                PyRep::Tuple(vec![PyRep::Integer(count), PyRep::Integer(last_ft)]),
            )
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnBeanPrime",
        "charid",
        vec![PyRep::Dict(entries)],
    );
}

/// Emits `OnClearFetchedData()` (`0` args, `id_type = "charid"`).
/// Matches `carbon/common/script/net/machoNet.pyj:MachoNetService.OnClearFetchedData`.
pub fn emit_clear_fetched_data(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnClearFetchedData",
        "charid",
        vec![],
    );
}

/// Emits `OnClientEvent_MarketItemReceived(item_id, type_id, flag_id, location_id)` (`4` args, `id_type = "charid"`).
/// Matches `eve/client/script/ui/shared/inventory/neocomInventoryBadging.pyj:NeocomInventoryBadging.OnClientEvent_MarketItemReceived`.
pub fn emit_client_market_item_received(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    type_id: i64,
    flag_id: i64,
    location_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnClientEvent_MarketItemReceived",
        "charid",
        vec![
            PyRep::Integer(item_id),
            PyRep::Integer(type_id),
            PyRep::Integer(flag_id),
            PyRep::Integer(location_id),
        ],
    );
}

/// Emits `OnEffectMessage(msgKey, args)` (`2` args, `id_type = "charid"`).
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

/// Emits `OnFlaggedListsChange()` (`0` args, `id_type = "charid"`).
/// Matches `carbonui/control/browser/sites.pyj:SitesSvc.OnFlaggedListsChange`.
pub fn emit_flagged_lists_change(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFlaggedListsChange",
        "charid",
        vec![],
    );
}

/// Emits `OnNotificationUndeleted(notificationIDs)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/ui/services/mail/notificationSvc.pyj:notificationSvc.OnNotificationUndeleted`.
pub fn emit_notification_undeleted(ctx: &ServerContext, char_id: i64, notification_ids: &[i64]) {
    let list = notification_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNotificationUndeleted",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Handles `notificationMgr.UndeleteNotifications(notificationIDs)` and emits `OnNotificationUndeleted`.
pub fn handle_undelete_notifications(
    ctx: &ServerContext,
    char_id: i64,
    ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    if !ids.is_empty() {
        emit_notification_undeleted(ctx, char_id, ids);
    }
    Ok(PyRep::Integer(ids.len() as i64))
}

/// Emits `OnOfferReceived(offerID, offerData)` (`2` args, `id_type = "charid"`).
/// Matches `contextualOffers/client/contextualOffersSvc.pyj:ContextualOffersSvc.OnOfferReceived`.
pub fn emit_contextual_offer_received(
    ctx: &ServerContext,
    char_id: i64,
    offer_id: i64,
    offer_type_id: i64,
    price_plex: i64,
) {
    let offer_data = PyRep::keyval([
        ("offerID", PyRep::Integer(offer_id)),
        ("offerTypeID", PyRep::Integer(offer_type_id)),
        ("pricePlex", PyRep::Integer(price_plex)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOfferReceived",
        "charid",
        vec![PyRep::Integer(offer_id), offer_data],
    );
}

/// Emits `OnPlexDepositFailed(vaultID, amount, reason)` (`3` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:OnPlexDepositFailed`.
pub fn emit_plex_deposit_failed(
    ctx: &ServerContext,
    char_id: i64,
    vault_id: i64,
    amount: i64,
    reason: &str,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDepositFailed",
        "charid",
        vec![
            PyRep::Integer(vault_id),
            PyRep::Integer(amount),
            PyRep::String(reason.to_string()),
        ],
    );
}

/// Emits `OnPlexWithdrawFailed(vaultID, amount, reason)` (`3` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:OnPlexWithdrawFailed`.
pub fn emit_plex_withdraw_failed(
    ctx: &ServerContext,
    char_id: i64,
    vault_id: i64,
    amount: i64,
    reason: &str,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexWithdrawFailed",
        "charid",
        vec![
            PyRep::Integer(vault_id),
            PyRep::Integer(amount),
            PyRep::String(reason.to_string()),
        ],
    );
}

/// Emits `OnProgressionTaskWidgetUpdate(objective_id, task_id, widget_state)` (`3` args, `id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionTaskWidgetUpdate`.
pub fn emit_progression_task_widget_update(
    ctx: &ServerContext,
    char_id: i64,
    objective_id: i64,
    task_id: i64,
    current: i64,
    target: i64,
) {
    let widget_state = PyRep::keyval([
        ("current", PyRep::Integer(current)),
        ("target", PyRep::Integer(target)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskWidgetUpdate",
        "charid",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(task_id),
            widget_state,
        ],
    );
}

/// Emits `OnRecommendationsChanged()` (`0` args, `id_type = "charid"`).
/// Matches `eve/client/script/ui/shared/infoPanels/infoPanelRecommendations.pyj:InfoPanelRecommendations.OnRecommendationsChanged`.
pub fn emit_recommendations_changed(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRecommendationsChanged",
        "charid",
        vec![],
    );
}

/// Emits `OnServerBrainUpdated(version, grayMatter)` (`2` args, `id_type = "charid"`).
/// Matches `eve/client/script/dogma/clientDogmaLocation.pyj:OnServerBrainUpdated`.
pub fn emit_server_brain_updated(
    ctx: &ServerContext,
    char_id: i64,
    version: i64,
    gray_matter: Vec<u8>,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerBrainUpdated",
        "charid",
        vec![PyRep::Integer(version), PyRep::Buffer(gray_matter)],
    );
}

/// Emits `OnServerMapDataPush(mapData)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/ui/shared/maps/starMapSvc.pyj:StarMapSvc.OnServerMapDataPush`.
pub fn emit_server_map_data_push(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: i64,
    jumps: i64,
    kills: i64,
) {
    let map_data = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(solar_system_id)),
        ("jumps", PyRep::Integer(jumps)),
        ("kills", PyRep::Integer(kills)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerMapDataPush",
        "charid",
        vec![map_data],
    );
}

/// Emits `OnStartConversation(conversationID, speakerID, lines)` (`3` args, `id_type = "charid"`).
/// Matches `conversations/conversationswithhighlights/conversationservice.pyj:ConversationService.OnStartConversation`.
pub fn emit_start_conversation(
    ctx: &ServerContext,
    char_id: i64,
    conversation_id: i64,
    speaker_id: i64,
    lines: &[i64],
) {
    let line_list = lines.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStartConversation",
        "charid",
        vec![
            PyRep::Integer(conversation_id),
            PyRep::Integer(speaker_id),
            PyRep::List(line_list),
        ],
    );
}

/// Emits `OnUpdateLocalizationTextCache(cacheUpdates)` (`1` arg, `id_type = "charid"`).
/// Matches `carbon/client/script/localization/localizationClient.pyj:LocalizationClient.OnUpdateLocalizationTextCache`.
pub fn emit_update_localization_text_cache(
    ctx: &ServerContext,
    char_id: i64,
    updates: &[(i64, &str, &str)],
) {
    let dict_entries = updates
        .iter()
        .map(|&(msg_id, lang, text)| {
            (
                PyRep::Integer(msg_id),
                PyRep::Dict(vec![(
                    PyRep::String(lang.to_string()),
                    PyRep::String(text.to_string()),
                )]),
            )
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnUpdateLocalizationTextCache",
        "charid",
        vec![PyRep::Dict(dict_entries)],
    );
}

/// Emits `OnUpdateServiceMapping(serviceMapping)` (`1` arg, `id_type = "charid"`).
/// Matches `carbon/common/script/net/ServiceCallGPCS.pyj:ServiceCallGPCS.OnUpdateServiceMapping`.
pub fn emit_update_service_mapping(ctx: &ServerContext, char_id: i64, mappings: &[(&str, i64)]) {
    let dict_entries = mappings
        .iter()
        .map(|&(svc, node_id)| (PyRep::String(svc.to_string()), PyRep::Integer(node_id)))
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnUpdateServiceMapping",
        "charid",
        vec![PyRep::Dict(dict_entries)],
    );
}
