//! `notify_emitters_b36` — Server-side V24 notification emitters for Batch 36
//! (Notifications undelete, PLEX vault failures, NPE/progression tasks & conversations,
//! dogma brain updates, combat messages, contextual offers, flagged lists, and service mappings).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

/// Emits `OnNotificationUndeleted(notificationIDs)` (`id_type = "charid"`).
/// Matches `notificationSvc.OnNotificationUndeleted(self, notificationIDs)`.
pub fn emit_notification_undeleted(
    ctx: &ServerContext,
    char_id: i64,
    notification_ids: &[i64],
) {
    if notification_ids.is_empty() {
        return;
    }
    let id_list = notification_ids
        .iter()
        .copied()
        .map(PyRep::Integer)
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNotificationUndeleted",
        "charid",
        vec![PyRep::List(id_list)],
    );
}

/// Emits `OnPlexDepositFailed(error_key, reference)` (`id_type = "charid"`).
/// Matches `PlexDepositListener.OnPlexDepositFailed(self, error_key, reference)`.
pub fn emit_plex_deposit_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDepositFailed",
        "charid",
        vec![PyRep::String(error_key.into()), PyRep::Integer(reference)],
    );
}

/// Emits `OnPlexWithdrawFailed(error_key, reference)` (`id_type = "charid"`).
/// Matches `PlexWithdrawalListener.OnPlexWithdrawFailed(self, error_key, reference)`.
pub fn emit_plex_withdraw_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexWithdrawFailed",
        "charid",
        vec![PyRep::String(error_key.into()), PyRep::Integer(reference)],
    );
}

/// Emits `OnProgressionAdvanced(objective_id, last_completed_task_id)` (`id_type = "charid"`).
/// Matches `ProgressionService.OnProgressionAdvanced(self, objective_id, last_completed_task_id)`.
pub fn emit_progression_advanced(
    ctx: &ServerContext,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionAdvanced",
        "charid",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(last_completed_task_id),
        ],
    );
}

/// Emits `OnProgressionTaskCompleted(task_id)` (`id_type = "charid"`).
/// Matches `ProgressionService.OnProgressionTaskCompleted(self, task_id)`.
pub fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskCompleted",
        "charid",
        vec![PyRep::Integer(task_id)],
    );
}

/// Emits `OnProgressionTaskWidgetUpdate(task_id, task_widget_id, widget_state)` (`id_type = "charid"`).
/// Matches `ProgressionService.OnProgressionTaskWidgetUpdate(self, task_id, task_widget_id, widget_state)`.
pub fn emit_progression_task_widget_update(
    ctx: &ServerContext,
    char_id: i64,
    task_id: i64,
    task_widget_id: i64,
    widget_state: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskWidgetUpdate",
        "charid",
        vec![
            PyRep::Integer(task_id),
            PyRep::Integer(task_widget_id),
            widget_state,
        ],
    );
}

/// Emits `OnStartConversation(conversation_id, show_continue_button)` (`id_type = "charid"`).
/// Matches `ConversationService.OnStartConversation(self, conversation_id, show_continue_button)`.
pub fn emit_start_conversation(
    ctx: &ServerContext,
    char_id: i64,
    conversation_id: i64,
    show_continue_button: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStartConversation",
        "charid",
        vec![
            PyRep::Integer(conversation_id),
            PyRep::Bool(show_continue_button),
        ],
    );
}

/// Emits `OnServerBrainUpdated(brainData)` (`id_type = "charid"`).
/// Matches `DogmaLocation.OnServerBrainUpdated(self, brainData)` where `brainData[0]` is the
/// integer brain version counter followed by character, ship, and structure modifier lists.
pub fn emit_server_brain_updated(
    ctx: &ServerContext,
    char_id: i64,
    brain_version: i64,
    char_modifiers: Vec<PyRep>,
    ship_modifiers: Vec<PyRep>,
    structure_modifiers: Vec<PyRep>,
) {
    let brain_data = PyRep::Tuple(vec![
        PyRep::Integer(brain_version),
        PyRep::List(char_modifiers),
        PyRep::List(ship_modifiers),
        PyRep::List(structure_modifiers),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerBrainUpdated",
        "charid",
        vec![brain_data],
    );
}

/// Emits `OnCombatMessage(messageName, messageArgs)` (`id_type = "charid"`).
/// Matches `TacticalSvc.OnCombatMessage(self, messageName, messageArgs)` where `messageArgs`
/// is a dict containing `"specialObject"` and combat telemetry fields.
pub fn emit_combat_message(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
) {
    let message_args = PyRep::Dict(vec![
        (
            PyRep::String("specialObject".into()),
            PyRep::Integer(special_object_id),
        ),
        (PyRep::String("damage".into()), PyRep::Real(damage)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCombatMessage",
        "charid",
        vec![PyRep::String(message_name.into()), message_args],
    );
}

/// Emits `OnOfferReceived(offerID, offer)` (`id_type = "charid"`).
/// Matches `ContextualOfferService.OnOfferReceived(self, offerID, offer)`.
pub fn emit_offer_received(ctx: &ServerContext, char_id: i64, offer_id: i64, offer: PyRep) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOfferReceived",
        "charid",
        vec![PyRep::Integer(offer_id), offer],
    );
}

/// Emits `OnFlaggedListsChange()` (`id_type = "charid"`).
/// Matches `SitesSvc.OnFlaggedListsChange(self)`.
pub fn emit_flagged_lists_change(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFlaggedListsChange",
        "charid",
        vec![],
    );
}

/// Emits `OnUpdateServiceMapping(svc, nodeID)` (`id_type = "charid"`).
/// Matches `CoreServiceCall.OnUpdateServiceMapping(self, svc, nodeID)`.
pub fn emit_update_service_mapping(
    ctx: &ServerContext,
    target: NotificationTarget,
    svc: &str,
    node_id: i64,
) {
    ctx.notify(
        target,
        "OnUpdateServiceMapping",
        "charid",
        vec![PyRep::String(svc.into()), PyRep::Integer(node_id)],
    );
}

/// Handles `start_conversation` / `open_conversation` and `update_task_widget`
/// client events, persisting state in `service_state_blobs` and emitting
/// `OnStartConversation` or `OnProgressionTaskWidgetUpdate`.
pub fn handle_conversation_and_widget_events(
    ctx: &ServerContext,
    char_id: i64,
    event_name: &str,
    args: &[PyRep],
    amount: Option<i64>,
    extra_data: Option<&PyRep>,
) -> bool {
    if event_name == "start_conversation" || event_name == "open_conversation" {
        let conversation_id = extra_data
            .and_then(|ed| match ed {
                PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                    if k.as_str() == Some("conversation_id")
                        || k.as_str() == Some("conversationID")
                    {
                        v.as_i64()
                    } else {
                        None
                    }
                }),
                PyRep::Integer(i) => Some(*i),
                _ => None,
            })
            .or(amount)
            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
            .unwrap_or(1);
        let show_continue = extra_data
            .and_then(|ed| match ed {
                PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                    if k.as_str() == Some("show_continue_button") {
                        match v {
                            PyRep::Bool(b) => Some(*b),
                            PyRep::Integer(i) => Some(*i != 0),
                            _ => None,
                        }
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .unwrap_or(true);
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                format!("char_conversation_{char_id}"),
                conversation_id.to_string(),
            );
        });
        emit_start_conversation(ctx, char_id, conversation_id, show_continue);
        return true;
    }

    if event_name == "update_task_widget" {
        let task_id = amount
            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
            .unwrap_or(1);
        let task_widget_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
        let widget_state = extra_data.cloned().unwrap_or_else(|| {
            PyRep::Dict(vec![
                (PyRep::String("completed".into()), PyRep::Bool(false)),
                (PyRep::String("progress".into()), PyRep::Integer(1)),
            ])
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                format!("char_task_widget_{char_id}_{task_id}"),
                task_widget_id.to_string(),
            );
        });
        emit_progression_task_widget_update(ctx, char_id, task_id, task_widget_id, widget_state);
        return true;
    }

    false
}
