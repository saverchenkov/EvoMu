//! Batch 30 state-backed notification emitters matching V24 client payload shapes.
//!
//! Covers:
//! - `notify:OnOperationReplayed`
//! - `notify:OnUiRevealForced`
//! - `notify:OnPasswordChanged`
//! - `notify:OnInvalidateCacheForLocations`
//! - `notify:OnMultiEvent`
//! - `notify:OnSolarSystemLPChange`
//! - `notify:OnVictoryPointStateUpdated`
//! - `notify:OnWeaponsTimerUpdate`
//! - `notify:OnPvpTimerUpdate`
//! - `notify:OnCriminalTimerUpdate`
//! - `notify:OnSystemCriminalFlagUpdates`
//! - `notify:OnShipDeath`

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use eve_sde::TypeId;

/// Emits `OnOperationReplayed(categoryID, operationID)` (`2` args, `id_type = "charid"`).
// parity: notify:OnOperationReplayed
pub fn emit_operation_replayed(ctx: &ServerContext, char_id: i64, cat_id: i64, op_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationReplayed",
        "charid",
        vec![PyRep::Integer(cat_id), PyRep::Integer(op_id)],
    );
}

/// Emits `OnUiRevealForced()` (`0` args, `id_type = "charid"`).
// parity: notify:OnUiRevealForced
pub fn emit_ui_reveal_forced(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnUiRevealForced",
        "charid",
        vec![],
    );
}

/// Emits `OnPasswordChanged(which, containerItem)` (`2` args, `id_type = "charid"`).
// parity: notify:OnPasswordChanged
pub fn emit_password_changed(
    ctx: &ServerContext,
    char_id: i64,
    which: i64,
    container_id: i64,
    type_id: TypeId,
) {
    let container_item = PyRep::keyval([
        ("itemID", PyRep::Integer(container_id)),
        ("typeID", PyRep::Integer(i64::from(type_id))),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPasswordChanged",
        "charid",
        vec![PyRep::Integer(which), container_item],
    );
}

/// Emits `OnInvalidateCacheForLocations(locationIDs)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnInvalidateCacheForLocations
pub fn emit_invalidate_cache_for_locations(
    ctx: &ServerContext,
    target: NotificationTarget,
    location_ids: &[i64],
) {
    if location_ids.is_empty() {
        return;
    }
    let list = location_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        target,
        "OnInvalidateCacheForLocations",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Emits `OnMultiEvent(events)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnMultiEvent
pub fn emit_multi_event(ctx: &ServerContext, char_id: i64, events: &[(&str, i64, &str)]) {
    if events.is_empty() {
        return;
    }
    let list = events
        .iter()
        .map(|(action, target_id, reason)| {
            PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String((*action).into()),
                    PyRep::Integer(*target_id),
                    PyRep::String((*reason).into()),
                ]),
                PyRep::Integer(0),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMultiEvent",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Emits `OnSolarSystemLPChange(oldPoints, newPoints)` (`2` args, `id_type = "solarsystemid"`).
// parity: notify:OnSolarSystemLPChange
pub fn emit_solar_system_lp_change(
    ctx: &ServerContext,
    sys_id: u32,
    old_points: i64,
    new_points: i64,
) {
    if old_points == new_points {
        return;
    }
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarSystemLPChange",
        "solarsystemid",
        vec![PyRep::Integer(old_points), PyRep::Integer(new_points)],
    );
}

/// Emits `OnVictoryPointStateUpdated(solarsystemID, newVictoryPointState)` (`2` args, `id_type = "solarsystemid"`).
// parity: notify:OnVictoryPointStateUpdated
pub fn emit_victory_point_state_updated(
    ctx: &ServerContext,
    sys_id: u32,
    new_vp: i64,
    contested: f64,
    threshold_i: i64,
) {
    let vp_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("victoryPoints", PyRep::Integer(new_vp)),
        ("contestedPercentage", PyRep::Real(contested)),
        ("threshold", PyRep::Integer(threshold_i)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnVictoryPointStateUpdated",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), vp_state],
    );
}

/// Emits `OnWeaponsTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
// parity: notify:OnWeaponsTimerUpdate
pub fn emit_weapons_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_val: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnWeaponsTimerUpdate",
        "charid",
        vec![PyRep::Integer(state_code), expiry_val],
    );
}

/// Emits `OnPvpTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
// parity: notify:OnPvpTimerUpdate
pub fn emit_pvp_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_val: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPvpTimerUpdate",
        "charid",
        vec![PyRep::Integer(state_code), expiry_val],
    );
}

/// Emits `OnCriminalTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
// parity: notify:OnCriminalTimerUpdate
pub fn emit_criminal_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_val: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCriminalTimerUpdate",
        "charid",
        vec![PyRep::Integer(state_code), expiry_val],
    );
}

/// Emits `OnSystemCriminalFlagUpdates(newIdles, newSuspects, newCriminals)` (`3` args, `id_type = "solarsystemid"`).
// parity: notify:OnSystemCriminalFlagUpdates
pub fn emit_system_criminal_flag_updates(
    ctx: &ServerContext,
    sys_id: u32,
    new_idles: &[i64],
    new_suspects: &[i64],
    new_criminals: &[i64],
) {
    if new_idles.is_empty() && new_suspects.is_empty() && new_criminals.is_empty() {
        return;
    }
    let ids = |v: &[i64]| PyRep::List(v.iter().map(|c| PyRep::Integer(*c)).collect());
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSystemCriminalFlagUpdates",
        "solarsystemid",
        vec![ids(new_idles), ids(new_suspects), ids(new_criminals)],
    );
}

/// Emits `OnShipDeath()` (`0` args, `id_type = "charid"`).
// parity: notify:OnShipDeath
pub fn emit_ship_death(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnShipDeath",
        "charid",
        vec![],
    );
}
