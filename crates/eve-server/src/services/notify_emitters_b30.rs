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

use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use crate::services::inventory_fitting::fitting_containers_ships;
use eve_proto::{BridgeRpcError, PyRep};
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
    if !events.is_empty() {
        let _ = ctx.notify_target_events(char_id, events);
    }
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
    if !new_idles.is_empty() || !new_suspects.is_empty() || !new_criminals.is_empty() {
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, new_idles, new_suspects, new_criminals);
    }
}

/// Emits `OnShipDeath()` (`0` args, `id_type = "charid"`).
// parity: notify:OnShipDeath
pub fn emit_ship_death(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnShipDeath",
        "charid",
        vec![],
    );
}

fn parse_optional_password(rep: Option<&PyRep>, field: &str) -> Result<Option<String>, BridgeRpcError> {
    match rep {
        None | Some(PyRep::None) => Ok(None),
        Some(PyRep::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(BridgeRpcError {
            code: "InvalidPassword".into(),
            message: format!("Container {field} must be a string or None"),
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
    let (container_id, which, pw_arg, old_arg) = if let Some(b) = bound_entry {
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
            let cloaked = session.ship_id.is_some_and(|sid| {
                sys_handle.ballpark.read().ok().is_some_and(|bp| {
                    bp.combat_states.get(&sid).is_some_and(|cs| cs.is_cloaked)
                        || bp.ball_slim_meta.get(&sid).is_some_and(|m| m.is_cloaked)
                })
            });
            let cb = sys_handle.get_ball_snapshot(container_id).await.ok().flatten();
            Some((sys_id, sb, cloaked, cb))
        } else {
            None
        }
    } else {
        None
    };

    let corp_id = session.corporation_id.unwrap_or(0);
    let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
    let now_unix = ctx.now_unix_secs();
    let pw_type_str = if which == 2 { "config" } else { "general" };

    let tid = ctx.db.try_with_mut_snapshot(|snap| -> Result<u32, BridgeRpcError> {
        let item = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Secure container {container_id} not found or not owned"),
        })?;
        if item.owner_id != char_id {
            let is_ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id == char_id).unwrap_or(false);
            if corp_id == 0 || item.owner_id != corp_id {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Secure container {container_id} not found or not owned"),
                });
            }
            let eff_role = session.corp_role | snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&char_id).copied()).unwrap_or(0);
            if !is_ceo && (eff_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG)) == 0 {
                return Err(BridgeRpcError {
                    code: "CorpRoleRequired".into(),
                    message: "Corporation role required to configure corp container password".into(),
                });
            }
        }
        let item_tid = item.type_id;
        let grp_id = ctx.sde.types.get(&item_tid).map(|t| t.group_id).unwrap_or(0);
        if !matches!(grp_id, 340 | 448) {
            return Err(BridgeRpcError {
                code: "ContainerDoesNotSupportPassword".into(),
                message: format!("Container {container_id} (group {grp_id}) does not support passwords"),
            });
        }
        if let Some((sys_id, ref sb_opt, cloaked, ref cb_opt)) = in_space_balls {
            if item.location_id == i64::from(sys_id) {
                let (_, sb) = sb_opt.as_ref().ok_or_else(|| BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in space".into(),
                })?;
                if cloaked || matches!(sb.mode, eve_destiny::MovementMode::Warp { .. }) {
                    return Err(BridgeRpcError {
                        code: "CannotDoWhileWarpingOrCloaked".into(),
                        message: "Cannot configure container password while warping or cloaked".into(),
                    });
                }
                let (_, cb) = cb_opt.as_ref().ok_or_else(|| BridgeRpcError {
                    code: "ContainerNotInSpace".into(),
                    message: format!("Container {container_id} is not spawned in space"),
                })?;
                let dist = (sb.position.distance(cb.position) - sb.radius - cb.radius).max(0.0);
                if dist > 2_500.0 {
                    return Err(BridgeRpcError {
                        code: "OutsideOperationalRange".into(),
                        message: format!("Container {container_id} is {dist:.1} m away (must be <= 2500 m)"),
                    });
                }
            } else if session.ship_id != Some(item.location_id) {
                return Err(BridgeRpcError {
                    code: "ColocationRequired".into(),
                    message: "Character must be colocated with container to configure password".into(),
                });
            }
        } else if Some(item.location_id) != caller_loc && session.ship_id != Some(item.location_id) {
            return Err(BridgeRpcError {
                code: "ColocationRequired".into(),
                message: "Character must be colocated with container to configure password".into(),
            });
        }

        let mut st = fitting_containers_ships::read_container_ship_state_snap(snap);
        if let Some(existing_pw) = st.get_container_password(container_id, which).filter(|p| !p.is_empty()) {
            if old_password.as_deref() != Some(existing_pw) {
                return Err(BridgeRpcError {
                    code: "InvalidPassword".into(),
                    message: "Incorrect old container password".into(),
                });
            }
        }
        st.set_container_password(container_id, which, password.clone());
        if which != 2 {
            st.unlocked_containers.retain(|&(a, b)| a != container_id && b != container_id);
            if !password.is_empty() {
                st.unlocked_containers.insert((container_id, char_id));
            }
        }
        st.alsc_logs.entry(container_id).or_default().push(
            fitting_containers_ships::AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionSetPassword".into(),
                item_id: container_id,
                item_type_id: item_tid,
                quantity: 1,
            },
        );
        fitting_containers_ships::save_container_ship_state_snap(snap, now_unix, &st);
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
        Ok(item_tid)
    })?;

    if method == "SetPassword" {
        emit_password_changed(ctx, char_id, which, container_id, tid);
    }
    emit_invalidate_cache_for_locations(ctx, NotificationTarget::Character(char_id), &[container_id]);
    Ok(PyRep::keyval([
        ("containerID", PyRep::Integer(container_id)),
        ("passwordSet", PyRep::Bool(!password.is_empty())),
    ]))
}
