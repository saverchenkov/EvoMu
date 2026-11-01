//! Integration tests for `track/p7-sp1-sp2` notification parity closure.
//! Exercises real server-side RPCs and verifies emitted V24 MachoNet notifications.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const JITA_SYSTEM: u32 = 30000142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_notify_operations_and_progression_transitions() {
    // parity: notify:OnOperationDeactivated
    let ctx = new_ctx();
    let mut s = pilot(&ctx, 2, "demo", "demo-secret", P1).await;
    s.solar_system_id = Some(JITA_SYSTEM);
    let kw = HashMap::new();
    let mut op_ids: Vec<i64> = eve_server::services::operations_manager::get_operation_defs(&ctx.sde)
        .op_tasks
        .keys()
        .copied()
        .collect();
    op_ids.sort_unstable();
    let (op1, op2) = (op_ids[0], op_ids[1]);

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op1), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 1");
    assert!(
        events(&ctx, P1, "OnOperationDeactivated").is_empty(),
        "First operation activation must not emit OnOperationDeactivated"
    );

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op2), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 2");
    let deact_ev = events(&ctx, P1, "OnOperationDeactivated");
    assert_eq!(deact_ev.len(), 1);
    assert!(deact_ev[0].is_empty());
}
