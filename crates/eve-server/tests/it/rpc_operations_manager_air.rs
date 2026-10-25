//! Integration test suite for AIR NPE & Operations Manager (`operationsManager`).
//!
//! Validates full live-server parity for all 16 AIR operationsManager endpoints:
//! - `operationsManager.get_tutorial_state`
//! - `operationsManager.has_skipped_tutorial`
//! - `operationsManager.is_main_tutorial_finished`
//! - `operationsManager.get_active_category_id`
//! - `operationsManager.is_category_complete`
//! - `operationsManager.get_category_state`
//! - `operationsManager.complete_category_for_character`
//! - `operationsManager.get_character_progress`
//! - `operationsManager.get_mission_avoidance_systems`
//! - `operationsManager.get_operations_completed_at_least_once`
//! - `operationsManager.activate_operation_in_solar_system`
//! - `operationsManager.block_task`
//! - `operationsManager.unblock_task`
//! - `operationsManager.skip_task`
//! - `operationsManager.start_site`
//! - `operationsManager.process_client_event`

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_AIR: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess
}

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_operations_tutorial_and_categories() {
    // parity: rpc:operationsManager.complete_category_for_character, rpc:operationsManager.get_active_category_id
    // parity: rpc:operationsManager.get_category_state, rpc:operationsManager.get_character_progress
    // parity: rpc:operationsManager.get_tutorial_state, rpc:operationsManager.has_skipped_tutorial
    // parity: rpc:operationsManager.is_category_complete, rpc:operationsManager.is_main_tutorial_finished
    eve_testkit::attest(&[
        "rpc:operationsManager.complete_category_for_character",
        "rpc:operationsManager.get_active_category_id",
        "rpc:operationsManager.get_category_state",
        "rpc:operationsManager.get_character_progress",
        "rpc:operationsManager.get_tutorial_state",
        "rpc:operationsManager.has_skipped_tutorial",
        "rpc:operationsManager.is_category_complete",
        "rpc:operationsManager.is_main_tutorial_finished",
    ]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 80_001, PILOT_AIR).await;

    // 1. Initial tutorial queries: get_tutorial_state, has_skipped_tutorial, is_main_tutorial_finished
    let tut_state = call_rpc(&ctx, &mut sess, "operationsManager", "get_tutorial_state", &[])
        .await
        .expect("get_tutorial_state call");
    let PyRep::Dict(tut_items) = tut_state else {
        panic!("get_tutorial_state must return Dict");
    };
    let is_tut_active = tut_items
        .iter()
        .find(|(k, _)| k.as_str() == Some("is_tutorial_active"))
        .map(|(_, v)| v);
    assert_eq!(is_tut_active, Some(&PyRep::Bool(false)));
    let tut_state_val = tut_items
        .iter()
        .find(|(k, _)| k.as_str() == Some("tutorial_state"))
        .map(|(_, v)| v);
    assert_eq!(tut_state_val, Some(&PyRep::Integer(0)));
    let career_path = tut_items
        .iter()
        .find(|(k, _)| k.as_str() == Some("career_path"))
        .map(|(_, v)| v);
    assert_eq!(career_path, Some(&PyRep::Integer(0)));

    let has_skipped = call_rpc(&ctx, &mut sess, "operationsManager", "has_skipped_tutorial", &[])
        .await
        .expect("has_skipped_tutorial call");
    assert_eq!(has_skipped, PyRep::Bool(false));

    let main_finished = call_rpc(&ctx, &mut sess, "operationsManager", "is_main_tutorial_finished", &[])
        .await
        .expect("is_main_tutorial_finished call");
    assert_eq!(main_finished, PyRep::Bool(true));

    // 2. Category state and progress before completion
    let active_cat_initial = call_rpc(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[])
        .await
        .expect("get_active_category_id initial");
    assert_eq!(active_cat_initial, PyRep::None);

    let cat_id = 42;
    let is_cat_comp_before = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("is_category_complete before");
    assert_eq!(is_cat_comp_before, PyRep::Bool(false));

    let is_cat_comp_out_of_bounds = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("is_category_complete 0");
    assert_eq!(is_cat_comp_out_of_bounds, PyRep::Bool(false));

    let cat_state_before = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("get_category_state before");
    assert_eq!(cat_state_before, PyRep::Integer(1)); // AVAILABLE = 1

    let cat_state_invalid = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("get_category_state invalid cat_id 0");
    assert_eq!(cat_state_invalid.code, "CustomInfo");

    // 3. Complete category: complete_category_for_character
    let complete_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("complete_category_for_character");
    assert_eq!(complete_rep, PyRep::Bool(true));

    let notifs = ctx.drain_notifications_for_char(PILOT_AIR);
    assert!(
        notifs.iter().any(|n| n.event == "OnOperationCategoryCompleted"),
        "expected OnOperationCategoryCompleted notification"
    );

    // 4. Verify post-completion state: is_category_complete and get_category_state
    let is_cat_comp_after = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("is_category_complete after");
    assert_eq!(is_cat_comp_after, PyRep::Bool(true));

    let cat_state_after = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("get_category_state after");
    assert_eq!(cat_state_after, PyRep::Integer(3)); // COMPLETE = 3

    // 5. Verify character progress: get_character_progress
    let progress_rep = call_rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress");
    let PyRep::Dict(prog_dict) = progress_rep else {
        panic!("expected Dict from get_character_progress");
    };
    let cat_entry = prog_dict
        .iter()
        .find(|(k, _)| k.as_i64() == Some(cat_id))
        .map(|(_, v)| v);
    assert_eq!(cat_entry, Some(&PyRep::Integer(100)));
}

#[tokio::test]
async fn test_operations_tasks_sites_and_events() {
    // parity: rpc:operationsManager.activate_operation_in_solar_system, rpc:operationsManager.block_task
    // parity: rpc:operationsManager.get_mission_avoidance_systems, rpc:operationsManager.get_operations_completed_at_least_once
    // parity: rpc:operationsManager.process_client_event, rpc:operationsManager.skip_task
    // parity: rpc:operationsManager.start_site, rpc:operationsManager.unblock_task
    eve_testkit::attest(&[
        "rpc:operationsManager.activate_operation_in_solar_system",
        "rpc:operationsManager.block_task",
        "rpc:operationsManager.get_mission_avoidance_systems",
        "rpc:operationsManager.get_operations_completed_at_least_once",
        "rpc:operationsManager.process_client_event",
        "rpc:operationsManager.skip_task",
        "rpc:operationsManager.start_site",
        "rpc:operationsManager.unblock_task",
    ]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 80_002, PILOT_AIR).await;

    // 1. Mission avoidance systems and completed operations initially empty
    let avoidance = call_rpc(&ctx, &mut sess, "operationsManager", "get_mission_avoidance_systems", &[])
        .await
        .expect("get_mission_avoidance_systems");
    assert_eq!(avoidance, PyRep::List(Vec::new()));

    let completed_ops_init = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "get_operations_completed_at_least_once",
        &[],
    )
    .await
    .expect("get_operations_completed_at_least_once init");
    assert_eq!(completed_ops_init, PyRep::List(Vec::new()));

    // 2. Activate operation: activate_operation_in_solar_system
    let op_id = 501;
    let act_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(SYS_JITA as i64)],
    )
    .await
    .expect("activate_operation_in_solar_system");
    assert_eq!(act_rep, PyRep::Bool(true));

    let act_err = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("activate_operation_in_solar_system invalid op_id");
    assert_eq!(act_err.code, "CustomInfo");

    let notifs = ctx.drain_notifications_for_char(PILOT_AIR);
    assert!(
        notifs.iter().any(|n| n.event == "OnOperationCategoryChanged"),
        "expected OnOperationCategoryChanged notification"
    );

    // 3. Task blocking and unblocking: block_task, unblock_task
    let task_id = 1_001;
    let block_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "block_task",
        &[PyRep::Integer(1), PyRep::Integer(op_id), PyRep::Integer(task_id)],
    )
    .await
    .expect("block_task");
    assert_eq!(block_rep, PyRep::Bool(true));

    let prog_blocked = call_rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress blocked");
    let PyRep::Dict(dict_blocked) = prog_blocked else {
        panic!("expected Dict from get_character_progress");
    };
    let task_blocked_entry = dict_blocked
        .iter()
        .find(|(k, _)| k.as_i64() == Some(task_id))
        .map(|(_, v)| v);
    assert_eq!(task_blocked_entry, Some(&PyRep::Integer(0)));

    let unblock_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "unblock_task",
        &[PyRep::Integer(1), PyRep::Integer(op_id), PyRep::Integer(task_id)],
    )
    .await
    .expect("unblock_task");
    assert_eq!(unblock_rep, PyRep::Bool(true));

    // 4. Skip task: skip_task
    let skip_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "skip_task",
        &[PyRep::Integer(1), PyRep::Integer(op_id), PyRep::Integer(task_id)],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip_rep, PyRep::Bool(true));

    let skip_notifs = ctx.drain_notifications_for_char(PILOT_AIR);
    assert!(
        skip_notifs.iter().any(|n| n.event == "OnOperationTaskCompleted"),
        "expected OnOperationTaskCompleted after skip_task"
    );

    let prog_skipped = call_rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress skipped");
    let PyRep::Dict(dict_skipped) = prog_skipped else {
        panic!("expected Dict from get_character_progress");
    };
    let task_skipped_entry = dict_skipped
        .iter()
        .find(|(k, _)| k.as_i64() == Some(task_id))
        .map(|(_, v)| v);
    assert_eq!(task_skipped_entry, Some(&PyRep::Integer(100)));

    // 5. Start site: start_site
    let start_err = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(999_999)],
    )
    .await
    .expect_err("start_site invalid dungeon");
    assert_eq!(start_err.code, "CustomInfo");

    // Dungeon 43 exists in SDE ("Pith Merchant Depot")
    let start_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(43)],
    )
    .await
    .expect("start_site valid dungeon 43");
    assert_eq!(start_rep, PyRep::Bool(true));

    // 6. Process client event: process_client_event
    let task_id_2 = 1_002;
    let event_rep = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("air_operation_completed_event".into()), PyRep::Integer(task_id_2)],
    )
    .await
    .expect("process_client_event");
    assert_eq!(event_rep, PyRep::Bool(true));

    let event_notifs = ctx.drain_notifications_for_char(PILOT_AIR);
    assert!(
        event_notifs.iter().any(|n| n.event == "OnOperationTaskCompleted"),
        "expected OnOperationTaskCompleted after client event"
    );

    // Verify completed operations now includes active op_id
    let completed_ops_after = call_rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "get_operations_completed_at_least_once",
        &[],
    )
    .await
    .expect("get_operations_completed_at_least_once after");
    let PyRep::List(ops_list) = completed_ops_after else {
        panic!("expected List from get_operations_completed_at_least_once");
    };
    assert!(
        ops_list.iter().any(|item| item.as_i64() == Some(op_id)),
        "completed operations must include op_id {op_id}"
    );
}
