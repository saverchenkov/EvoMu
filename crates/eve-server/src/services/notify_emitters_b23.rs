//! `notify_emitters_b23` — Real Gameplay & Service Notification Emitters (Batch 23).
//!
//! Covers 12 V24 client notifications:
//! - Operations Manager (`OnOperationDeactivated`, `OnOperationSkipped`,
//!   `OnOperationsProgressReset`, `OnOperationsServerFSDReload`)
//! - Fleet Escalations (`OnFleetEscalatingPathMessage`)
//! - Cloaking (`OnCloakModuleForceDeactivated`)
//! - Dungeon Room Triggers & Camera (`OnDungeonCameraInterest`,
//!   `OnDungeonTriggerAmbientSound`, `OnDungeonTriggerAudio`,
//!   `OnDungeonTriggerMessage`, `OnDungeonTriggerMusic`)
//! - Stargate Jump Queue (`OnJumpQueueUpdate`)

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use serde::{Deserialize, Serialize};
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

/// Emits `OnOperationsProgressReset(completedCategories, completedOperations, activeTuple)` (`3` args, `id_type = "charid"`).
pub fn emit_operations_progress_reset(
    ctx: &ServerContext,
    char_id: i64,
    first_cat_id: i64,
    first_op_id: i64,
    first_task_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationsProgressReset",
        "charid",
        vec![
            PyRep::List(Vec::new()),
            PyRep::List(Vec::new()),
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
// 2. Fleet Escalation Notification
// ---------------------------------------------------------------------------

/// Emits `OnFleetEscalatingPathMessage(charID, dungeonTemplateID)` (`2` args, `id_type = "fleetid"`).
pub fn emit_fleet_escalating_path_message(
    ctx: &ServerContext,
    fleet_id: i64,
    killer_char_id: i64,
    expedition_dungeon_id: u32,
) {
    ctx.notify(
        NotificationTarget::Fleet(fleet_id),
        "OnFleetEscalatingPathMessage",
        "fleetid",
        vec![
            PyRep::Integer(killer_char_id),
            PyRep::Integer(i64::from(expedition_dungeon_id)),
        ],
    );
}

// ---------------------------------------------------------------------------
// 3. Forced Decloak Notification
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

// ---------------------------------------------------------------------------
// 5. Stargate Jump Queue Notification
// ---------------------------------------------------------------------------

pub const JUMP_QUEUE_LIMITS_BLOB_KEY: &str = "jump_queue_limits";
pub const JUMP_QUEUE_STATE_BLOB_KEY: &str = "jump_queue_state";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JumpQueueSystemState {
    pub queued_chars: Vec<i64>,
    pub granted_keys: HashMap<i64, i64>,
}

/// Emits `OnJumpQueueUpdate(solarSystemID, space, queue, jumpKeys, keyLife)` (`5` args, `id_type = "charid"`).
pub fn emit_jump_queue_update(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    space: i64,
    queue: Vec<i64>,
    jump_keys: Vec<i64>,
    key_life: i64,
) {
    let queue_rep = queue.into_iter().map(PyRep::Integer).collect();
    let keys = jump_keys.into_iter().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpQueueUpdate",
        "charid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(space),
            PyRep::List(queue_rep),
            PyRep::List(keys),
            PyRep::Integer(key_life),
        ],
    );
}

/// Checks if `dest_sys` has an active jump-queue capacity limit; if full, enqueues `char_id`,
/// emits `OnJumpQueueUpdate`, and returns `Err(UserError("JumpQueueActive"))`.
pub fn try_enqueue_jump_queue(
    ctx: &ServerContext,
    char_id: i64,
    dest_sys: SolarSystemId,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let outcome = ctx.db.with_mut_snapshot(|snap| {
        let limits: HashMap<SolarSystemId, usize> = snap
            .service_state_blobs
            .get(JUMP_QUEUE_LIMITS_BLOB_KEY)
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or_default();
        let Some(&cap) = limits.get(&dest_sys) else {
            return None;
        };
        let active_in_dest = snap
            .characters
            .values()
            .filter(|c| c.solar_system_id == dest_sys && c.active_ship_id > 0)
            .count();
        if active_in_dest < cap {
            return None;
        }
        let mut all_queues: HashMap<SolarSystemId, JumpQueueSystemState> = snap
            .service_state_blobs
            .get(JUMP_QUEUE_STATE_BLOB_KEY)
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or_default();
        let entry = all_queues.entry(dest_sys).or_default();
        if !entry.queued_chars.contains(&char_id) {
            entry.queued_chars.push(char_id);
        }
        let queued_chars = entry.queued_chars.clone();
        let key = entry
            .granted_keys
            .get(&char_id)
            .copied()
            .unwrap_or_else(|| (now_unix as i64).saturating_add(60));
        if let Ok(json) = serde_json::to_string(&all_queues) {
            snap.service_state_blobs
                .insert(JUMP_QUEUE_STATE_BLOB_KEY.to_string(), json);
        }
        Some((0_i64, queued_chars, vec![key], 60_i64))
    });

    if let Some((space, queued_chars, keys, key_life)) = outcome {
        emit_jump_queue_update(ctx, char_id, dest_sys, space, queued_chars, keys, key_life);
        return Err(BridgeRpcError::user_error("JumpQueueActive"));
    }
    Ok(())
}
