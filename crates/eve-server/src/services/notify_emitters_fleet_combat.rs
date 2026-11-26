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

use crate::router::notify_registry::{
    OnCriminalTimerUpdate, OnInvalidateCacheForLocations, OnOperationReplayed, OnPasswordChanged,
    OnPvpTimerUpdate, OnShipDeath, OnSolarSystemLPChange, OnUiRevealForced,
    OnVictoryPointStateUpdated, OnWeaponsTimerUpdate,
};
use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use crate::services::inventory_fitting::fitting_containers_ships;
use eve_db::clock::SimFileTime;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;

/// Emits `OnOperationReplayed(categoryID, operationID)` (`2` args, `id_type = "charid"`).
/// Emitter for notify:OnOperationReplayed
pub fn emit_operation_replayed(ctx: &ServerContext, char_id: i64, cat_id: i64, op_id: i64) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnOperationReplayed {
            category_id: cat_id,
            operation_id: op_id,
        },
    );
}

/// Emits `OnUiRevealForced()` (`0` args, `id_type = "charid"`).
/// Emitter for notify:OnUiRevealForced
pub fn emit_ui_reveal_forced(ctx: &ServerContext, char_id: i64) {
    ctx.emit_typed(NotificationTarget::Character(char_id), OnUiRevealForced {});
}

/// Emits `OnPasswordChanged(which, containerItem)` (`2` args, `id_type = "charid"`).
/// Emitter for notify:OnPasswordChanged
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
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnPasswordChanged {
            which,
            container_item,
        },
    );
}

/// Emits `OnInvalidateCacheForLocations(locationIDs)` (`1` arg, `id_type = "charid"`).
/// Emitter for notify:OnInvalidateCacheForLocations
pub fn emit_invalidate_cache_for_locations(
    ctx: &ServerContext,
    target: NotificationTarget,
    location_ids: &[i64],
) {
    if location_ids.is_empty() {
        return;
    }
    ctx.emit_typed(
        target,
        OnInvalidateCacheForLocations {
            location_ids: location_ids.to_vec(),
        },
    );
}

/// Emits `OnMultiEvent(events)` (`1` arg, `id_type = "charid"`).
/// Emitter for notify:OnMultiEvent
pub fn emit_multi_event(ctx: &ServerContext, char_id: i64, events: &[(&str, i64, &str)]) {
    if !events.is_empty() {
        let _ = ctx.notify_target_events(char_id, events);
    }
}

/// Emits `OnSolarSystemLPChange(oldPoints, newPoints)` (`2` args, `id_type = "solarsystemid"`).
/// Emitter for notify:OnSolarSystemLPChange
pub fn emit_solar_system_lp_change(
    ctx: &ServerContext,
    sys_id: u32,
    old_points: i64,
    new_points: i64,
) {
    if old_points == new_points {
        return;
    }
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnSolarSystemLPChange {
            old_points,
            new_points,
        },
    );
}

/// Emits `OnVictoryPointStateUpdated(solarsystemID, newVictoryPointState)` (`2` args, `id_type = "solarsystemid"`).
/// Emitter for notify:OnVictoryPointStateUpdated
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
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        OnVictoryPointStateUpdated {
            solar_system_id: i64::from(sys_id),
            new_victory_point_state: vp_state,
        },
    );
}

/// Emits `OnWeaponsTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
/// Emitter for notify:OnWeaponsTimerUpdate
pub fn emit_weapons_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_time: Option<SimFileTime>,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnWeaponsTimerUpdate {
            state: state_code,
            expiry_time,
            do_alert: None,
        },
    );
}

/// Emits `OnPvpTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
/// Emitter for notify:OnPvpTimerUpdate
pub fn emit_pvp_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_time: Option<SimFileTime>,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnPvpTimerUpdate {
            state: state_code,
            expiry_time,
            do_alert: None,
        },
    );
}

/// Emits `OnCriminalTimerUpdate(state, expiryTime)` (`2` args, `id_type = "charid"`).
/// Emitter for notify:OnCriminalTimerUpdate
pub fn emit_criminal_timer_update(
    ctx: &ServerContext,
    char_id: i64,
    state_code: i64,
    expiry_time: Option<SimFileTime>,
) {
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnCriminalTimerUpdate {
            state: state_code,
            expiry_time,
            do_alert: None,
        },
    );
}

/// Emits `OnSystemCriminalFlagUpdates(newIdles, newSuspects, newCriminals)` (`3` args, `id_type = "solarsystemid"`).
/// Emitter for notify:OnSystemCriminalFlagUpdates
pub fn emit_system_criminal_flag_updates(
    ctx: &ServerContext,
    sys_id: u32,
    new_idles: &[i64],
    new_suspects: &[i64],
    new_criminals: &[i64],
) {
    if !new_idles.is_empty() || !new_suspects.is_empty() || !new_criminals.is_empty() {
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, new_idles, new_suspects, new_criminals);
    }
}

/// Emits `OnShipDeath()` (`0` args, `id_type = "charid"`) and records AIR `destruction`
/// when the destroyed hull is a non-capsule, non-corvette capsuleer ship (`group_id` not `29 | 237`).
pub fn emit_ship_death_with_type(
    ctx: &ServerContext,
    char_id: i64,
    destroyed_ship_type_id: Option<u32>,
) {
    if char_id <= 0 {
        return;
    }
    let resolved_ship_type_opt = destroyed_ship_type_id.or_else(|| {
        ctx.db.with_snapshot(|snap| {
            snap.combat_crimewatch_state
                .detailed_killmails
                .values()
                .filter(|dkm| dkm.victim_character_id == char_id)
                .max_by_key(|dkm| dkm.kill_id)
                .map(|dkm| dkm.victim_ship_type_id)
        })
    });
    if let Some(ship_type_id) = resolved_ship_type_opt {
        let is_pod_or_corvette = ctx
            .sde
            .types
            .get(&ship_type_id)
            .is_some_and(|t| matches!(t.group_id, 29 | 237));
        if ship_type_id != 670 && ship_type_id != 33_328 && !is_pod_or_corvette {
            crate::services::quasar_career_goals::record_career_sub_kind(
                ctx,
                char_id,
                "destruction",
                None,
                1,
            );
        }
    }
    ctx.emit_typed(NotificationTarget::Character(char_id), OnShipDeath {});
}

/// Emits `OnShipDeath()` (`0` args, `id_type = "charid"`).
/// Emitter for notify:OnShipDeath
pub fn emit_ship_death(ctx: &ServerContext, char_id: i64) {
    emit_ship_death_with_type(ctx, char_id, None);
}

fn parse_optional_password(rep: Option<&PyRep>, field: &str) -> Result<Option<String>, BridgeRpcError> {
    match rep {
        None | Some(PyRep::None) => Ok(None),
        Some(PyRep::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(BridgeRpcError {
            code: "InvalidPassword".into(),
            message: format!("Container {field} must be a string or None"),
            client_error: None,
        }),
    }
}

/// Validates and executes `invbroker.SetPassword` / `ALSCConfigSet` / `ConfigureALSC` (`BF-04`).
pub(crate) async fn handle_set_password(
    ctx: &ServerContext,
    session: &eve_machonet::SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
    kwargs: &std::collections::HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
    let item_bound = bound_entry.filter(|b| {
        !(Some(b.object_id) == caller_loc && args.first().and_then(|a| a.as_i64()).is_some_and(|v| v > 2))
    });
    let (container_id, which, pw_arg, old_arg) = if let Some(b) = item_bound {
        if args.first().and_then(|a| a.as_i64()).is_some() || kwargs.contains_key("which") {
            let w = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("which").and_then(|v| v.as_i64())).unwrap_or(0);
            (b.object_id, w, args.get(1).or_else(|| kwargs.get("password")).or_else(|| kwargs.get("newPassword")), args.get(2).or_else(|| kwargs.get("oldPassword")))
        } else if matches!(args.first(), Some(PyRep::String(_) | PyRep::None)) {
            let w = kwargs.get("which").and_then(|v| v.as_i64()).unwrap_or(1);
            (b.object_id, w, args.first(), args.get(1).or_else(|| kwargs.get("oldPassword")))
        } else {
            (b.object_id, 0, None, None)
        }
    } else {
        let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        if args.get(1).and_then(|a| a.as_i64()).is_some() || kwargs.contains_key("which") {
            let w = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("which").and_then(|v| v.as_i64())).unwrap_or(0);
            (cid, w, args.get(2).or_else(|| kwargs.get("password")).or_else(|| kwargs.get("newPassword")), args.get(3).or_else(|| kwargs.get("oldPassword")))
        } else if matches!(args.get(1), Some(PyRep::String(_) | PyRep::None)) {
            let w = kwargs.get("which").and_then(|v| v.as_i64()).unwrap_or(1);
            (cid, w, args.get(1), args.get(2).or_else(|| kwargs.get("oldPassword")))
        } else {
            (cid, 0, None, None)
        }
    };
    if !matches!(which, 1 | 2) {
        return Err(BridgeRpcError {
            code: "InvalidPasswordType".into(),
            message: format!("Invalid container password slot {which}; expected 1 or 2"),
            client_error: None,
        });
    }
    let password = parse_optional_password(pw_arg, "password")?.unwrap_or_default();
    let old_password = parse_optional_password(old_arg, "oldPassword")?.filter(|s| !s.is_empty());

    let in_space_balls = if session.station_id.is_none() && session.structure_id.is_none() {
        if let Some(sys_id) = session.solar_system_id {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let sb = match session.ship_id {
                Some(sid) => sys_handle.get_ball_snapshot(sid).await.ok().flatten(),
                None => None,
            };
            let cloaked = session
                .ship_id
                .is_some_and(|sid| sys_handle.view().is_ball_cloaked(sid));
            let cb = sys_handle.get_ball_snapshot(container_id).await.ok().flatten();
            Some((sys_id, sb, cloaked, cb))
        } else {
            None
        }
    } else {
        None
    };

    let corp_id = session.corporation_id.unwrap_or(0);
    let now_unix = ctx.now_unix_secs();
    let pw_type_str = if which == 2 { "config" } else { "general" };

    let tid = ctx.db.try_with_mut_snapshot(|snap| -> Result<Result<u32, BridgeRpcError>, BridgeRpcError> {
        let item = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Secure container {container_id} not found or not owned"),
            client_error: None,
        })?;
        if item.owner_id != char_id {
            if corp_id == 0 || item.owner_id != corp_id {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Secure container {container_id} not found or not owned"),
                    client_error: None,
                });
            }
            let (is_ceo, eff_role) = snap.effective_corp_roles_at_db(
                corp_id,
                char_id,
                Some(item.location_id),
                ctx.now_unix(),
            );
            if !is_ceo && (eff_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG)) == 0 {
                return Err(BridgeRpcError {
                    code: "CorpRoleRequired".into(),
                    message: "Corporation role required to configure corp container password".into(),
                    client_error: None,
                });
            }
        }
        let item_tid = item.type_id;
        let grp_id = ctx.sde.types.get(&item_tid).map(|t| t.group_id).unwrap_or(0);
        // Only Secure Cargo Containers (340) and Audit Log Secure Containers (448) take passwords;
        // freight (649) and plain cargo/station containers share the same rejection.
        if !matches!(grp_id, 340 | 448) {
            return Err(BridgeRpcError {
                code: "ContainerDoesNotSupportPassword".into(),
                message: format!("Container {container_id} (group {grp_id}) does not support passwords"),
                client_error: None,
            });
        }
        if let Some((sys_id, ref sb_opt, cloaked, ref cb_opt)) = in_space_balls {
            if item.location_id == i64::from(sys_id) {
                let (_, sb) = sb_opt.as_ref().ok_or_else(|| {
                    BridgeRpcError::ship_not_in_space(
                        session.ship_id.unwrap_or(0),
                        Some(char_id),
                        Some(sys_id),
                        "configure_container_password",
                        "Active ship is not spawned in space",
                    )
                })?;
                if cloaked || matches!(sb.mode, eve_destiny::MovementMode::Warp { .. }) {
                    return Err(BridgeRpcError {
                        code: "CannotDoWhileWarpingOrCloaked".into(),
                        message: "Cannot configure container password while warping or cloaked".into(),
                        client_error: None,
                    });
                }
                let (_, cb) = cb_opt.as_ref().ok_or_else(|| BridgeRpcError {
                    code: "ContainerNotInSpace".into(),
                    message: format!("Container {container_id} is not spawned in space"),
                    client_error: None,
                })?;
                let dist = (sb.position.distance(cb.position) - sb.radius - cb.radius).max(0.0);
                if dist > 2_500.0 {
                    return Err(BridgeRpcError {
                        code: "OutsideOperationalRange".into(),
                        message: format!("Container {container_id} is {dist:.1} m away (must be <= 2500 m)"),
                        client_error: None,
                    });
                }
            } else if session.ship_id != Some(item.location_id) {
                return Err(BridgeRpcError {
                    code: "ColocationRequired".into(),
                    message: "Character must be colocated with container to configure password".into(),
                    client_error: None,
                });
            }
        } else if Some(item.location_id) != caller_loc
            && session.ship_id != Some(item.location_id)
            && !snap.items.get(&item.location_id).is_some_and(|p| Some(p.location_id) == caller_loc)
        {
            return Err(BridgeRpcError {
                code: "ColocationRequired".into(),
                message: "Character must be colocated with container to configure password".into(),
                client_error: None,
            });
        }

        if let Some(existing_pw) = fitting_containers_ships::container_password_snap(snap, container_id, which) {
            if old_password.as_deref() != Some(existing_pw.as_str()) {
                fitting_containers_ships::append_alsc_log_snap(
                    snap,
                    container_id,
                    fitting_containers_ships::AlscLogEntry {
                        timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordFailure".into(), item_id: container_id, item_type_id: item_tid, quantity: 1,
                    },
                    now_unix,
                )?;
                return Ok(Err(BridgeRpcError {
                    code: "InvalidPassword".into(),
                    message: "Incorrect old container password".into(),
                    client_error: None,
                }));
            }
            fitting_containers_ships::append_alsc_log_snap(
                snap,
                container_id,
                fitting_containers_ships::AlscLogEntry {
                    timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordSuccess".into(), item_id: container_id, item_type_id: item_tid, quantity: 1,
                },
                now_unix,
            )?;
        }
        fitting_containers_ships::set_container_password_snap(snap, container_id, which, &password, now_unix)?;
        if which != 2 {
            fitting_containers_ships::clear_container_unlocks_snap(snap, container_id);
            if !password.is_empty() {
                fitting_containers_ships::set_container_unlocked_snap(snap, container_id, char_id, true, now_unix)?;
            }
        }
        let is_cfg_method = method == "ALSCConfigSet" || method == "ConfigureALSC";
        if is_cfg_method {
            let mut cfg = fitting_containers_ships::container_config_snap(snap, container_id);
            if let Some(v) = kwargs.get("lockByDefault").or_else(|| kwargs.get("lock_by_default")).and_then(|v| v.as_bool()) { cfg.lock_by_default = v; }
            if let Some(v) = kwargs.get("passwordPerAction").or_else(|| kwargs.get("password_per_action")).and_then(|v| v.as_bool()) { cfg.password_per_action = v; }
            if let Some(bm) = kwargs.get("configBitmask").or_else(|| kwargs.get("bitmask")).and_then(|v| v.as_i64()) {
                cfg.config_bitmask = bm as u32;
                if (bm & 1) != 0 { cfg.lock_by_default = true; }
                if (bm & 2) != 0 { cfg.password_per_action = true; }
            }
            fitting_containers_ships::set_container_config_snap(snap, container_id, &cfg, now_unix)?;
            fitting_containers_ships::append_alsc_log_snap(
                snap,
                container_id,
                fitting_containers_ships::AlscLogEntry {
                    timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionConfigChange".into(), item_id: container_id, item_type_id: item_tid, quantity: 1,
                },
                now_unix,
            )?;
        } else {
            fitting_containers_ships::append_alsc_log_snap(
                snap,
                container_id,
                fitting_containers_ships::AlscLogEntry {
                    timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionSetPassword".into(), item_id: container_id, item_type_id: item_tid, quantity: 1,
                },
                now_unix,
            )?;
        }
        if let Some((log_corp_id, _, loc_id, loc_flag)) =
            fitting_containers_ships::resolve_corp_container_log_target(snap, container_id)
        {
            snap.corp_container_logs.entry(log_corp_id).or_default().push(eve_db::CorpContainerLogEntry {
                logged_at_unix: now_unix,
                container_id,
                container_type_id: item_tid as i32,
                character_id: char_id,
                location_id: loc_id,
                location_flag: loc_flag,
                action: "set_password".into(),
                password_type: Some(pw_type_str.into()),
                type_id: None,
                quantity: None,
                old_config_bitmask: None,
                new_config_bitmask: None,
            });
        }
        Ok(Ok(item_tid))
    })??;

    if method == "SetPassword" {
        emit_password_changed(ctx, char_id, which, container_id, tid);
    }
    emit_invalidate_cache_for_locations(ctx, NotificationTarget::Character(char_id), &[container_id]);
    Ok(PyRep::keyval([
        ("containerID", PyRep::Integer(container_id)),
        ("passwordSet", PyRep::Bool(!password.is_empty())),
    ]))
}

#[cfg(test)]
#[path = "notify_emitters_fleet_combat_tests.rs"]
mod notify_emitters_fleet_combat_tests;

