//! `notify_emitters_channel_events` — Real Gameplay & Service Notification Emitters (Batch 23).
//!
//! Covers 11 V24 client notifications:
//! - Operations Manager (`OnOperationDeactivated`, `OnOperationSkipped`,
//!   `OnOperationsProgressReset`, `OnOperationsServerFSDReload`)
//! - Fleet Escalations (`OnFleetEscalatingPathMessage`)
//! - Cloaking (`OnCloakModuleForceDeactivated`)
//! - Dungeon Room Triggers & Camera (`OnDungeonCameraInterest`,
//!   `OnDungeonTriggerAmbientSound`, `OnDungeonTriggerAudio`,
//!   `OnDungeonTriggerMessage`, `OnDungeonTriggerMusic`)

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// 1. Operations Manager Notifications
// ---------------------------------------------------------------------------

/// Emits `OnOperationDeactivated()` (`0` args, `id_type = "charid"`).
pub fn emit_operation_deactivated(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationDeactivated",
        "charid",
        vec![],
    );
}

/// Emits `OnOperationSkipped(categoryID, operationID)` (`2` args, `id_type = "charid"`).
pub fn emit_operation_skipped(ctx: &ServerContext, char_id: i64, cat_id: i64, op_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationSkipped",
        "charid",
        vec![PyRep::Integer(cat_id), PyRep::Integer(op_id)],
    );
}

/// Emits `OnOperationsProgressReset(itemsToLock, itemsToSkip, activeTuple)` (`3` args, `id_type = "charid"`).
pub fn emit_operations_progress_reset(
    ctx: &ServerContext,
    char_id: i64,
    items_to_lock: &[i64],
    items_to_skip: &[i64],
    first_cat_id: i64,
    first_op_id: i64,
    first_task_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationsProgressReset",
        "charid",
        vec![
            PyRep::List(items_to_lock.iter().copied().map(PyRep::Integer).collect()),
            PyRep::List(items_to_skip.iter().copied().map(PyRep::Integer).collect()),
            PyRep::Tuple(vec![
                PyRep::Integer(first_cat_id),
                PyRep::Integer(first_op_id),
                PyRep::Integer(first_task_id),
            ]),
        ],
    );
}

/// Emits `OnOperationsServerFSDReload()` (`0` args, `id_type = "charid"`).
pub fn emit_operations_server_fsd_reload(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationsServerFSDReload",
        "charid",
        vec![],
    );
}

// ---------------------------------------------------------------------------
// 2. Forced Decloak Notification
// ---------------------------------------------------------------------------

/// Emits `OnCloakModuleForceDeactivated(notifyMsg, notifyArgs)` (`2` args, `id_type = "charid"`).
pub fn emit_cloak_module_force_deactivated(
    ctx: &ServerContext,
    char_id: i64,
    reason: &str,
    dist_m: f64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCloakModuleForceDeactivated",
        "charid",
        vec![
            PyRep::String(reason.to_string()),
            PyRep::Dict(vec![(
                PyRep::String("distance".into()),
                PyRep::Real(dist_m),
            )]),
        ],
    );
}

// ---------------------------------------------------------------------------
// 4. Dungeon Room Trigger & Camera Interest Notifications
// ---------------------------------------------------------------------------

fn push_char_notification_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    event: &str,
    args: Vec<PyRep>,
) {
    if char_id <= 0 {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let (pkt, mpkt) = ServerContext::build_macho_notification_packet(
        char_id,
        None,
        event,
        "charid",
        args.clone(),
    );
    let notif = PendingMachoNotification {
        seq_id: crate::router::cache_notifications::next_notification_seq_id(),
        recipient_char_id: char_id,
        event: event.to_string(),
        id_type: "charid".to_string(),
        args,
        packet: pkt,
        marshaled_packet: mpkt,
        created_filetime: now_ft,
    };
    if let Some(pn) = pending_notifications {
        if let Ok(mut queues) = pn.write() {
            let q = queues.entry(char_id).or_default();
            if q.len() >= 256 {
                q.remove(0);
            }
            q.push(notif.clone());
        }
    }
    if let Some(tx) = notify_tx {
        let _ = tx.send((char_id, notif));
    }
}

/// Emits `OnDungeonCameraInterest(itemID, speed, duration)` (`3` args, `id_type = "charid"`).
pub fn emit_dungeon_camera_interest_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    item_id: i64,
    speed: f64,
    duration: f64,
) {
    push_char_notification_core(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonCameraInterest",
        vec![
            PyRep::Integer(item_id),
            PyRep::Real(speed),
            PyRep::Real(duration),
        ],
    );
}

/// Emits `OnDungeonTriggerAmbientSound(dungeonID, soundID, entityItemID)` (`3` args, `id_type = "charid"`).
pub fn emit_dungeon_trigger_ambient_sound_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    sound_id: i64,
    entity_item_id: i64,
    _emitter_id: i64,
) {
    push_char_notification_core(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerAmbientSound",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(sound_id),
            PyRep::Integer(entity_item_id),
        ],
    );
}

/// Emits `OnDungeonTriggerAudio(dungeonID, audioUrl)` (`2` args, `id_type = "charid"`).
pub fn emit_dungeon_trigger_audio_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    audio_url: &str,
) {
    push_char_notification_core(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerAudio",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::String(audio_url.to_string()),
        ],
    );
}

/// Emits `OnDungeonTriggerMessage(messageType, messageID)` (`2` args, `id_type = "charid"`).
pub fn emit_dungeon_trigger_message_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    message_type: i64,
    message_id: i64,
) {
    push_char_notification_core(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerMessage",
        vec![PyRep::Integer(message_type), PyRep::Integer(message_id)],
    );
}

/// Emits `OnDungeonTriggerMusic(dungeonID, musicTriggerID)` (`2` args, `id_type = "charid"`).
pub fn emit_dungeon_trigger_music_core(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    music_trigger_id: i64,
) {
    push_char_notification_core(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnDungeonTriggerMusic",
        vec![
            PyRep::Integer(dungeon_id),
            PyRep::Integer(music_trigger_id),
        ],
    );
}

pub fn dispatch_dungeon_trigger_notify_actions(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<
        &std::sync::RwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    >,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    dungeon_id: i64,
    obj_ball_ids: &[i64],
    actions: Vec<crate::services::pve_dungeon_runtime::DungeonTriggerAction>,
) {
    use crate::services::pve_dungeon_runtime::DungeonTriggerAction;
    for act in actions {
        match act {
            DungeonTriggerAction::CameraInterest { item_id, speed_bits, duration_bits } => {
                let target_item = if item_id > 0 { item_id } else { obj_ball_ids.first().copied().unwrap_or(0) };
                emit_dungeon_camera_interest_core(db, pending_notifications, notify_tx, char_id, target_item, f64::from_bits(speed_bits), f64::from_bits(duration_bits));
            }
            DungeonTriggerAction::TriggerAmbientSound { sound_id, entity_item_id, emitter_id } => {
                emit_dungeon_trigger_ambient_sound_core(db, pending_notifications, notify_tx, char_id, dungeon_id, sound_id, entity_item_id, emitter_id);
            }
            DungeonTriggerAction::TriggerAudio { audio_url } => {
                emit_dungeon_trigger_audio_core(db, pending_notifications, notify_tx, char_id, dungeon_id, &audio_url);
            }
            DungeonTriggerAction::TriggerMessage { message_type, message_id } => {
                emit_dungeon_trigger_message_core(db, pending_notifications, notify_tx, char_id, message_type, message_id);
            }
            DungeonTriggerAction::TriggerMusic { music_trigger_id } => {
                emit_dungeon_trigger_music_core(db, pending_notifications, notify_tx, char_id, dungeon_id, music_trigger_id);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "notify_emitters_channel_events_tests.rs"]
mod notify_emitters_channel_events_tests;

