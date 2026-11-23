//! GM and QA harness time-control and session-timer RPC handlers (`timeControl.*`, `harness.*`).

#![forbid(unsafe_code)]

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};
use crate::router::{RpcArgs, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn handle_advance_clock(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError::with_client_error(
            "InsufficientRole",
            format!(
                "UserError: GM_InsufficientRole: {service}.{method} requires a V24.01 GM/Admin service role bitmask"
            ),
            eve_proto::ClientError::user(eve_proto::UserErrorKey::InsufficientRoles),
        ));
    }
    let rpc = RpcArgs::new(args, kwargs);
    let secs = rpc.opt_u64(0, "seconds")?.unwrap_or(12);
    let explicit_target_cid = rpc.opt_positive_i64(1, "targetCharID")?;
    let target_space = if let Some(target_cid) = explicit_target_cid {
        let resolved = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&target_cid).map(|ch| {
                if ch.station_id.is_none()
                    && ch.structure_id.is_none()
                    && ch.solar_system_id > 0
                    && ch.active_ship_id > 0
                {
                    Some((ch.solar_system_id, ch.active_ship_id))
                } else {
                    None
                }
            })
        });
        resolved.ok_or_else(|| {
            let msg = format!("Character {target_cid} not found");
            BridgeRpcError::with_client_error(
                "CharacterNotFound",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            )
        })?
    } else {
        match (session.solar_system_id, session.ship_id) {
            (Some(sys_id), Some(ship_id)) => Some((sys_id, ship_id)),
            _ => None,
        }
    };
    ctx.advance_secs_without_sim_clock_rebase(secs);
    if secs > 0 {
        let target_cid = explicit_target_cid.or(session.character_id).unwrap_or(0);
        crate::services::notify_emitters_character_services::fast_forward_and_sync_space_clock(
            ctx,
            target_cid,
            target_space,
            secs,
        )
        .await;
    }
    Ok(PyRep::Bool(true))
}

fn handle_clear_session_timer(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError::with_client_error(
            "InsufficientRole",
            format!(
                "UserError: GM_InsufficientRole: {service}.{method} requires a V24.01 GM/Admin service role bitmask"
            ),
            eve_proto::ClientError::user(eve_proto::UserErrorKey::InsufficientRoles),
        ));
    }
    let target_cid = match RpcArgs::new(args, kwargs).opt_positive_i64(0, "targetCharID")? {
        Some(cid) => {
            if !ctx.db.with_snapshot(|snap| snap.characters.contains_key(&cid)) {
                let msg = format!("Character {cid} not found");
                return Err(BridgeRpcError::with_client_error(
                    "CharacterNotFound",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            Some(cid)
        }
        None => session.character_id,
    };
    if let Some(cid) = target_cid {
        crate::services::space_destiny::clear_session_change_timer(ctx, cid);
    }
    Ok(PyRep::Bool(true))
}

fn dispatch_advance_clock<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        Some(handle_advance_clock(ctx, session, service, method, args, kwargs).await)
    })
}

fn dispatch_clear_session_timer(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(handle_clear_session_timer(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("timeControl", "AdvanceClock", RpcDispatchHandler::Async(dispatch_advance_clock)),
    rpc_entry(
        "timeControl",
        "FastForwardSessionTimer",
        RpcDispatchHandler::Sync(dispatch_clear_session_timer),
    ),
    rpc_entry(
        "timeControl",
        "ClearSessionTimer",
        RpcDispatchHandler::Sync(dispatch_clear_session_timer),
    ),
    rpc_entry(
        "harness",
        "FastForwardSessionTimer",
        RpcDispatchHandler::Sync(dispatch_clear_session_timer),
    ),
    rpc_entry(
        "harness",
        "ClearSessionTimer",
        RpcDispatchHandler::Sync(dispatch_clear_session_timer),
    ),
];
