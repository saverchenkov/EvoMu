//! `notify_emitters_ess_bounties` — Server-side V24 notification emitters for Batch 36
//! (`OnProgressionAdvanced`, `OnProgressionTaskCompleted`, and `OnCombatMessage`).

#![forbid(unsafe_code)]

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

pub(crate) fn push_char_notification_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    event: &str,
    args: Vec<PyRep>,
) {
    if char_id <= 0 {
        return;
    }
    let Some(pend) = pending_notifications else {
        return;
    };
    let uid_opt = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
    let (packet, marshaled_packet) =
        ServerContext::build_macho_notification_packet(char_id, uid_opt, event, "charid", args.clone());
    let notif = PendingMachoNotification {
        recipient_char_id: char_id,
        event: event.to_string(),
        id_type: "charid".to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime: db.clock().now_blueos_filetime_ticks(),
    };
    let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
    let q = queues.entry(char_id).or_default();
    if q.len() >= 256 {
        q.remove(0);
    }
    q.push(notif.clone());
    if let Some(tx) = notify_tx {
        let _ = tx.send((char_id, notif));
    }
}

/// Emits `OnProgressionAdvanced(objective_id, last_completed_task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionAdvanced`.
pub(crate) fn emit_progression_advanced(
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

/// DB-backed variant of [`emit_progression_advanced`] for `SystemActor` / dungeon death hooks.
pub(crate) fn emit_progression_advanced_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    push_char_notification_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnProgressionAdvanced",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(last_completed_task_id),
        ],
    );
}

/// Emits `OnProgressionTaskCompleted(task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionTaskCompleted`.
#[allow(dead_code)]
pub(crate) fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskCompleted",
        "charid",
        vec![PyRep::Integer(task_id)],
    );
}

/// DB-backed variant of [`emit_progression_task_completed`] for `SystemActor` / dungeon death hooks.
pub(crate) fn emit_progression_task_completed_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    task_id: i64,
) {
    push_char_notification_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnProgressionTaskCompleted",
        vec![PyRep::Integer(task_id)],
    );
}

/// Emits `OnCombatMessage(messageName, messageArgs)` (`id_type = "charid"`).
/// Matches `eve/client/script/parklife/tactical.pyj:TacticalSvc.OnCombatMessage(self, messageName, messageArgs)`
/// where `messageArgs` contains `"specialObject"`, `"damage"`, `"type"`, `"weapon"`, `"hitQualityText"`, and `"isWrecking"`.
pub(crate) fn emit_combat_message(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
    weapon_type_id: u32,
    is_wrecking: bool,
) {
    let hit_quality_text = if damage <= 0.0 {
        "misses"
    } else if is_wrecking {
        "wrecks"
    } else {
        "hits"
    };
    let message_args = PyRep::Dict(vec![
        (
            PyRep::String("specialObject".into()),
            PyRep::Integer(special_object_id),
        ),
        (PyRep::String("damage".into()), PyRep::Real(damage)),
        (
            PyRep::String("type".into()),
            PyRep::Integer(i64::from(weapon_type_id)),
        ),
        (
            PyRep::String("weapon".into()),
            PyRep::Integer(i64::from(weapon_type_id)),
        ),
        (
            PyRep::String("hitQualityText".into()),
            PyRep::String(hit_quality_text.into()),
        ),
        (PyRep::String("isWrecking".into()), PyRep::Bool(is_wrecking)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCombatMessage",
        "charid",
        vec![PyRep::String(message_name.into()), message_args],
    );
}
