//! `notify_emitters_ess_bounties` — Server-side V24 notification emitters for Batch 36
//! (`OnProgressionAdvanced`, `OnProgressionTaskCompleted`, and `OnCombatMessage`).

#![forbid(unsafe_code)]

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::notify_registry::{
    build_pending_registered_notification, emit_typed_char_with_db,
    push_pending_char_notification_256, OnCombatMessage, OnProgressionAdvanced,
    OnProgressionTaskCompleted,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::lock_rank::AnyRwLock;
use eve_proto::PyRep;
use std::collections::HashMap;
use tokio::sync::broadcast;

pub(crate) fn push_char_notification_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
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
    let Some(notif) = build_pending_registered_notification(
        char_id,
        uid_opt,
        event,
        "charid",
        args,
        db.clock().now_blueos_filetime_ticks(),
    ) else {
        return;
    };
    push_pending_char_notification_256(pend, notify_tx, char_id, notif);
}

/// Emits `OnProgressionAdvanced(objective_id, last_completed_task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionAdvanced`.
pub(crate) fn emit_progression_advanced(
    ctx: &ServerContext,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnProgressionAdvanced {
            objective_id,
            last_completed_task_id,
        },
    );
}

/// DB-backed variant of [`emit_progression_advanced`] for `SystemActor` / dungeon death hooks.
pub(crate) fn emit_progression_advanced_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    emit_typed_char_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        OnProgressionAdvanced {
            objective_id,
            last_completed_task_id,
        },
    );
}

/// Emits `OnProgressionTaskCompleted(task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionTaskCompleted`.
#[allow(dead_code)]
pub(crate) fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnProgressionTaskCompleted { task_id },
    );
}

/// DB-backed variant of [`emit_progression_task_completed`] for `SystemActor` / dungeon death hooks.
pub(crate) fn emit_progression_task_completed_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    task_id: i64,
) {
    emit_typed_char_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        OnProgressionTaskCompleted { task_id },
    );
}

/// Emits `OnCombatMessage(messageName, messageArgs)` (`id_type = "charid"`).
/// Matches `eve/client/script/parklife/tactical.pyj:TacticalSvc.OnCombatMessage(self, messageName, messageArgs)`
/// where `messageArgs` contains `"specialObject"`, `"damage"`, `"type"`, `"weapon"`, `"hitQualityText"`, and `"isWrecking"`.
#[cfg(test)]
pub(crate) fn emit_combat_message(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
    weapon_type_id: u32,
    is_wrecking: bool,
) {
    emit_combat_message_in_scene(
        ctx,
        char_id,
        message_name,
        special_object_id,
        damage,
        weapon_type_id,
        is_wrecking,
        crate::router::notify_registry::BallparkScene::try_solar_system(30_000_142)
            .expect("non-zero test scene"),
    );
}

/// Scene-scoped variant of [`emit_combat_message`] attaching [`crate::router::notify_registry::BallparkScene`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_combat_message_in_scene(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
    weapon_type_id: u32,
    is_wrecking: bool,
    scene: crate::router::notify_registry::BallparkScene,
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
    ctx.emit_typed_in_ballpark(
        NotificationTarget::Character(char_id),
        scene,
        OnCombatMessage {
            message_name: message_name.into(),
            message_args,
        },
    );
}

#[cfg(test)]
#[path = "notify_emitters_ess_bounties_tests.rs"]
mod notify_emitters_ess_bounties_tests;

