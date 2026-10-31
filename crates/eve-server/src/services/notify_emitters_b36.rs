//! `notify_emitters_b36` — Server-side V24 notification emitters for Batch 36
//! (`OnProgressionAdvanced`, `OnProgressionTaskCompleted`, and `OnCombatMessage`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

/// Emits `OnProgressionAdvanced(objective_id, last_completed_task_id)` (`id_type = "charid"`).
/// Matches `ProgressionService.OnProgressionAdvanced(self, objective_id, last_completed_task_id)`.
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

/// Emits `OnProgressionTaskCompleted(task_id)` (`id_type = "charid"`).
/// Matches `ProgressionService.OnProgressionTaskCompleted(self, task_id)`.
pub(crate) fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskCompleted",
        "charid",
        vec![PyRep::Integer(task_id)],
    );
}

/// Emits `OnCombatMessage(messageName, messageArgs)` (`id_type = "charid"`).
/// Matches `TacticalSvc.OnCombatMessage(self, messageName, messageArgs)` where `messageArgs`
/// contains `"specialObject"` and weapon/damage telemetry fields.
pub(crate) fn emit_combat_message(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
    weapon_type_id: u32,
    is_wrecking: bool,
) {
    let message_args = PyRep::Dict(vec![
        (
            PyRep::String("specialObject".into()),
            PyRep::Integer(special_object_id),
        ),
        (PyRep::String("damage".into()), PyRep::Real(damage)),
        (
            PyRep::String("weapon".into()),
            PyRep::Integer(i64::from(weapon_type_id)),
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
