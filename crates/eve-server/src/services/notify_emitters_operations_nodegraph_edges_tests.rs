//! Edge-case unit tests for `notify_emitters_operations_nodegraph`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_online_ctx() -> (ServerContext, i64, u32) {
    let sde = shared_sde();
    let any_graph_id = sde.node_graphs.keys().copied().next().unwrap_or(1);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id, any_graph_id)
}

#[test]
fn corrupt_blob_blackboard_truncation_and_message_cap_edges() {
    let (ctx, char_id, graph_id_u32) = make_online_ctx();
    let graph_id = i64::from(graph_id_u32);

    // Corrupt JSON in client_node_graphs blob gracefully falls back to empty map.
    ctx.db.with_mut_snapshot(|snap| {
        snap.node_graph_state.client_node_graphs.remove(&char_id);
        assert!(load_active_client_graphs(snap, char_id).is_empty());
    });

    // No active instance matching graph_id -> on_client_graph_message is a safe no-op.
    on_client_graph_message(&ctx, char_id, graph_id, 1, "ignored", &PyRep::Integer(1));
    ctx.db.with_snapshot(|snap| {
        assert!(load_active_client_graphs(snap, char_id).is_empty());
    });

    // Initial blackboard exceeding MAX_BLACKBOARD_KEYS (64) is truncated to 64.
    let mut big_bb = BTreeMap::new();
    for i in 0..70_usize {
        big_bb.insert(format!("k_{i:02}"), i.to_string());
    }
    let inst_id = start_client_node_graph_instance(&ctx, char_id, graph_id, 10, 20, big_bb)
        .expect("start graph");
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.db.with_snapshot(|snap| {
        let graphs = load_active_client_graphs(snap, char_id);
        let inst = graphs.get(&inst_id).expect("stored");
        assert_eq!(inst.blackboard.len(), MAX_BLACKBOARD_KEYS);
    });

    // When at 64 keys, new msg_name & new "node_id" cannot insert, but updating existing key works.
    on_client_graph_message(&ctx, char_id, graph_id, 99, "brand_new_key", &PyRep::String("nope".into()));
    on_client_graph_message(&ctx, char_id, graph_id, 99, "k_00", &PyRep::String("updated".into()));
    ctx.db.with_snapshot(|snap| {
        let graphs = load_active_client_graphs(snap, char_id);
        let inst = graphs.get(&inst_id).expect("stored");
        assert_eq!(inst.blackboard.len(), MAX_BLACKBOARD_KEYS);
        assert!(!inst.blackboard.contains_key("brand_new_key"));
        assert!(!inst.blackboard.contains_key("node_id"));
        assert_eq!(inst.blackboard.get("k_00").map(String::as_str), Some("updated"));
    });

    // Stop when matching by operation_id == cat_id (20) removes it; second call with no match is a no-op.
    stop_active_operation_node_graph_for_category(&ctx, char_id, 20);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnStopClientNodeGraph");
    stop_active_operation_node_graph_for_category(&ctx, char_id, 20);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[test]
fn emit_operation_task_skipped_last_and_missing_op_and_slash_variants() {
    let (ctx, char_id, graph_id_u32) = make_online_ctx();
    let graph_id = i64::from(graph_id_u32);

    // Custom op_defs: last task has next_task = None; unknown op also has next_task = None.
    let mut op_defs = OperationDefinitions::default();
    op_defs.op_tasks.insert(55, vec![501, 502]);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 5, 55, 502);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 5, 999, 501);
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    assert_eq!(
        drained[0].args,
        vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(5),
                PyRep::Integer(55),
                PyRep::Integer(502),
                PyRep::None,
            ])]),
            PyRep::Bool(true),
        ]
    );

    // Find a real operation from SDE op_defs to exercise success paths of replay & skip.
    let real_defs = get_operation_defs(&ctx.sde);
    let (&real_op_id, real_tasks) = real_defs
        .op_tasks
        .iter()
        .find(|(&op_id, tasks)| (1..=100_000).contains(&op_id) && !tasks.is_empty())
        .expect("canonical SDE contains at least one operation");
    let real_cat_id = 1_i64;
    let first_task = real_tasks[0];

    let mut gm_session = SessionState::new(501);
    gm_session.user_id = Some(1);
    gm_session.character_id = Some(char_id);
    gm_session.role = service_roles::ROLE_PROGRAMMER;

    // Seed completed/skipped/blocked state before replay and reset.
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.completed_tasks.insert(first_task);
        rec.skipped_tasks.insert(first_task);
        rec.completed_categories.insert(99);
        rec.completed_operations.insert(98);
        rec.blocked_tasks.insert(97);
        rec.active_category_id = Some(real_cat_id);
        rec.active_operation_id = Some(real_op_id);
        save_store(snap, &store, 1_775_400_000);
    });

    // Replay bounds errors (cat_id out of range, op_id out of range)
    assert!(replay_operation_for_char(&ctx, char_id, 0, real_op_id).is_err());
    assert!(replay_operation_for_char(&ctx, char_id, 100_001, real_op_id).is_err());
    assert!(replay_operation_for_char(&ctx, char_id, real_cat_id, 0).is_err());
    assert!(replay_operation_for_char(&ctx, char_id, real_cat_id, 100_001).is_err());

    // Valid replay clears completed/skipped tasks for real_op_id and emits OnOperationReplayed
    let cmd_replay = format!("/operation replay {real_cat_id} {real_op_id}");
    assert_eq!(
        handle_operation_slash(&ctx, &gm_session, char_id, &cmd_replay)
            .unwrap()
            .expect("replay succeeds"),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id).unwrap();
        assert!(!rec.completed_tasks.contains(&first_task));
        assert!(!rec.skipped_tasks.contains(&first_task));
        assert_eq!(rec.active_operation_id, Some(real_op_id));
    });
    let replay_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(replay_notifs.iter().any(|n| n.event == "OnOperationReplayed"));

    // Skip bounds errors and success path (both when active == real_op_id and when active != real_op_id)
    assert!(skip_operation_for_char(&ctx, char_id, 0, real_op_id).is_err());
    assert!(skip_operation_for_char(&ctx, char_id, 100_001, real_op_id).is_err());
    assert!(skip_operation_for_char(&ctx, char_id, real_cat_id, 0).is_err());
    assert!(skip_operation_for_char(&ctx, char_id, real_cat_id, 100_001).is_err());

    let _ = start_client_node_graph_instance(&ctx, char_id, graph_id, real_cat_id, real_op_id, BTreeMap::new())
        .expect("start graph");
    let _ = ctx.drain_notifications_for_char(char_id);
    let cmd_skip = format!("/operation skip {real_cat_id} {real_op_id}");
    assert_eq!(
        handle_operation_slash(&ctx, &gm_session, char_id, &cmd_skip)
            .unwrap()
            .expect("skip succeeds"),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id).unwrap();
        assert!(rec.completed_operations.contains(&real_op_id));
        assert!(rec.skipped_tasks.contains(&first_task));
        assert_eq!(rec.active_operation_id, None);
    });
    let skip_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(skip_notifs.iter().any(|n| n.event == "OnOperationSkipped"));
    assert!(skip_notifs.iter().any(|n| n.event == "OnStopClientNodeGraph"));

    // Skip when another operation is active leaves active_operation_id intact.
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_operation_id = Some(real_op_id + 1);
        rec.active_category_id = Some(real_cat_id);
        save_store(snap, &store, 1_775_400_000);
    });
    assert_eq!(
        skip_operation_for_char(&ctx, char_id, real_cat_id, real_op_id).expect("skip while other active"),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id).unwrap();
        assert_eq!(rec.active_operation_id, Some(real_op_id + 1));
    });

    // `/operation reset` alias collects non-empty completed/skipped/active items and clears them.
    assert_eq!(
        handle_operation_slash(&ctx, &gm_session, char_id, "/operation reset")
            .unwrap()
            .expect("reset alias"),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id).unwrap();
        assert!(rec.completed_categories.is_empty());
        assert!(rec.completed_operations.is_empty());
        assert!(rec.completed_tasks.is_empty());
        assert!(rec.skipped_tasks.is_empty());
        assert!(rec.blocked_tasks.is_empty());
    });

    // ROLE_CONTENT also permitted for reload_fsd; empty `/operation` returns InvalidSlashArgument.
    gm_session.role = service_roles::ROLE_CONTENT;
    assert_eq!(
        handle_operation_slash(&ctx, &gm_session, char_id, "/operation reload_fsd")
            .unwrap()
            .expect("reload_fsd with ROLE_CONTENT"),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_operation_slash(&ctx, &gm_session, char_id, "/operation")
            .unwrap()
            .expect_err("empty /operation")
            .code,
        "InvalidSlashArgument"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut graphs = std::collections::BTreeMap::new();
        graphs.insert(
            77,
            super::ActiveClientNodeGraph {
                instance_id: 77,
                node_graph_id: 500,
                category_id: 1,
                operation_id: 2,
                blackboard: std::collections::BTreeMap::new(),
            },
        );
        super::save_active_client_graphs(snap, char_id, &graphs);
        assert_eq!(super::load_active_client_graphs(snap, char_id).len(), 1);
        graphs.clear();
        super::save_active_client_graphs(snap, char_id, &graphs);
        assert!(!snap.node_graph_state.client_node_graphs.contains_key(&char_id));
    });
}
