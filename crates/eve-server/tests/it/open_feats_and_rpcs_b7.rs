//! Shared helpers for open `feat:` and `rpc:` parity tests (Batch 7).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const STATION_ID: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

pub(crate) async fn setup_pilot_for_user(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    let uid = sess.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.station_id = Some(STATION_ID);
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYS;
            ch.home_station_id = STATION_ID;
        }
    });
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    sess.solar_system_id = Some(JITA_SYS);
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

pub(crate) async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    setup_pilot_for_user(ctx, char_id as u64, "demo", "demo-secret", char_id).await
}
