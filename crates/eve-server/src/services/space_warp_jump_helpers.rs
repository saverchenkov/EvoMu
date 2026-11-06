//! Bound-system resolution, dock notifications, and undock timer helpers for `space_warp_jump`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use super::super::{check_session_change_timer_active, record_session_change_timer};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(crate) fn resolve_bound_or_session_sys(
    bound_entry: Option<&BoundObjectEntry>,
    session: &SessionState,
    expected_service: &str,
) -> Result<u32, BridgeRpcError> {
    match bound_entry {
        Some(b) => {
            if b.service != expected_service {
                return Err(BridgeRpcError {
                    code: "InvalidBoundService".into(),
                    message: format!("Expected bound service {expected_service}, got {}", b.service),
                });
            }
            Ok(b.object_id as u32)
        }
        None => session.solar_system_id.ok_or_else(|| BridgeRpcError {
            code: "BoundObjectRequired".into(),
            message: format!(
                "Calls to {expected_service} require a bound object handle or active solar system"
            ),
        }),
    }
}

pub(crate) fn bookmark_expired_err(bm_id: i64) -> BridgeRpcError {
    BridgeRpcError {
        code: "BookmarkExpired".into(),
        message: format!("Bookmark {bm_id} has expired"),
    }
}

pub(crate) fn emit_structure_dock_notifications(
    ctx: &ServerContext,
    dock_id: i64,
    char_id: i64,
    corp_id: i64,
    ally_id: Option<i64>,
    fac_id: Option<i64>,
    st_type_id: u32,
    ship_id: i64,
) {
    ctx.notify(
        NotificationTarget::Station(dock_id),
        "OnCharacterEnteredStructure",
        "stationid",
        vec![
            PyRep::Integer(char_id),
            PyRep::Integer(corp_id),
            ally_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            fac_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ],
    );
    let hv_state = PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("structureID", PyRep::Integer(dock_id)),
        ("structureTypeID", PyRep::Integer(i64::from(st_type_id))),
        ("shipID", PyRep::Integer(ship_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnHangarViewStateUpdated",
        "charid",
        vec![PyRep::Integer(dock_id), hv_state],
    );
}

pub(crate) async fn handle_undock(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now_unix = ctx.now_unix();
    if let Some(active_timer) = check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot undock while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        }));
    }
    let res = crate::services::station_structure::handle(
        ctx,
        session,
        "stationSvc",
        "Undock",
        args,
        kwargs,
    )
    .await?;
    if let Ok(ref rep) = res {
        let sys_id = session
            .solar_system_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            })
            .unwrap_or(0);
        let spawn_tick = rep
            .dict_get("spawnTick")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64;
        record_session_change_timer(ctx, char_id, now_unix, sys_id, spawn_tick, "undock");
    }
    Some(res)
}

#[cfg(test)]
#[path = "space_warp_jump_helpers_cov_unit_tests.rs"]
mod space_warp_jump_helpers_cov_unit_tests;

