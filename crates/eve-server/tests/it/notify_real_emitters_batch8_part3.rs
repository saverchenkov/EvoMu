//! Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 3: Operations & AIR NPE).
//!
//! Covers the V24-triggered Group 3 Operations / NPE parity rows:
//! - `notify:OnOperationTaskSkipped`
//! - `notify:OnAirNpeStateChanged`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::operations_manager_helpers::get_operation_defs;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

// parity: notify:OnOperationTaskSkipped, notify:OnAirNpeStateChanged
#[tokio::test]
async fn notify_batch8_operations_npe_and_client_node_graphs() {
    eve_testkit::attest(&[
        "notify:OnOperationTaskSkipped",
        "notify:OnAirNpeStateChanged",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_300);
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess = SessionState::new(9_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: non-SDE graph 999_999_888 is rejected with NodeGraphNotFound
    let err_ng = rpc(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(999_999_888),
            PyRep::Integer(1),
            PyRep::String("noop_msg".into()),
            PyRep::Dict(Vec::new()),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_ng.code, "NodeGraphNotFound");

    // 1. start_tutorial via process_client_event -> emits OnAirNpeStateChanged([PILOT_A, 1])
    let start_tut = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("start_tutorial".into()),
            PyRep::None,
            PyRep::Dict(vec![(PyRep::String("state_id".into()), PyRep::Integer(1))]),
        ],
    )
    .await
    .expect("start_tutorial");
    assert_eq!(start_tut, PyRep::Bool(true));

    let tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&tut_notifs, "OnAirNpeStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
    );

    // 2. process_client_graph_message on real SDE graph (3) does NOT echo client input back
    let _ = rpc(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(3),
            PyRep::Integer(5),
            PyRep::String("node_completed".into()),
            PyRep::Dict(vec![(PyRep::String("stage".into()), PyRep::Integer(2))]),
        ],
    )
    .await
    .expect("process_client_graph_message on SDE graph 3");
    let ng_msg_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(
        !ng_msg_notifs.iter().any(|(ev, _)| ev == "OnClientNodeGraphMessage" || ev == "OnClientNodeGraphUpdate"),
        "must not echo client node graph input back: {ng_msg_notifs:?}"
    );

    // 3. skip_tutorial -> emits OnAirNpeStateChanged([PILOT_A, 3])
    let skip_tut = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .expect("skip_tutorial");
    assert_eq!(skip_tut, PyRep::Bool(true));

    let skip_tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&skip_tut_notifs, "OnAirNpeStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(3)]
    );

    // 4. Discover a real operation with tasks from SDE -> activate & skip_task -> OnOperationTaskSkipped
    let op_defs = get_operation_defs(&ctx.sde);
    let (&op_id, tasks) = op_defs
        .op_tasks
        .iter()
        .find(|(_, ts)| ts.len() >= 2)
        .or_else(|| op_defs.op_tasks.iter().find(|(_, ts)| !ts.is_empty()))
        .expect("at least one operation with tasks in SDE");
    let cat_id = 1_i64;
    let first_task_id = tasks[0];
    let expected_next_task = tasks.get(1).copied().map(PyRep::Integer).unwrap_or(PyRep::None);

    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let act_op = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(op_id),
            PyRep::Integer(SYS_JITA as i64),
            PyRep::Integer(cat_id),
        ],
    )
    .await
    .expect("activate_operation_in_solar_system");
    assert_eq!(act_op, PyRep::Bool(true));
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: skip_task with invalid task ID fails with CustomInfo
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "CustomInfo"
    );

    let skip_task_rep = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "skip_task",
        &[
            PyRep::Integer(cat_id),
            PyRep::Integer(op_id),
            PyRep::Integer(first_task_id),
        ],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip_task_rep, PyRep::Bool(true));
    let skip_task_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let task_skipped_args = find_notif(&skip_task_notifs, "OnOperationTaskSkipped");
    assert_eq!(
        task_skipped_args,
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(cat_id),
                PyRep::Integer(op_id),
                PyRep::Integer(first_task_id),
                expected_next_task,
            ])]),
            PyRep::Bool(true),
        ]
    );
}
