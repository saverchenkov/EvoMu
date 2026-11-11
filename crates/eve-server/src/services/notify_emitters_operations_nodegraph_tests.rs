//! Unit tests for `notify_emitters_operations_nodegraph`.

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
fn client_node_graph_lifecycle_blackboard_conversion_and_limits() {
    let (ctx, char_id, graph_id_u32) = make_online_ctx();
    let graph_id = i64::from(graph_id_u32);

    // Invalid graph_id <= 0
    assert_eq!(
        start_client_node_graph_instance(&ctx, char_id, 0, 1, 1, BTreeMap::new())
            .expect_err("graph_id=0")
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        start_client_node_graph_instance(&ctx, char_id, -5, 1, 1, BTreeMap::new())
            .expect_err("graph_id=-5")
            .code,
        "InvalidArgument"
    );
    // Non-existent graph_id in SDE
    assert_eq!(
        start_client_node_graph_instance(&ctx, char_id, 999_999_999, 1, 1, BTreeMap::new())
            .expect_err("missing in sde")
            .code,
        "NodeGraphNotFound"
    );

    // maybe_start_operation_node_graph with None is a no-op
    maybe_start_operation_node_graph(&ctx, char_id, 10, 2, None).expect("noop");
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Start with typed blackboard values (integer, true, false, string)
    let mut bb = BTreeMap::new();
    bb.insert("count".into(), "42".into());
    bb.insert("flag_true".into(), "true".into());
    bb.insert("flag_false".into(), "false".into());
    bb.insert("label".into(), "alpha".into());
    let inst1 = start_client_node_graph_instance(&ctx, char_id, graph_id, 2, 10, bb)
        .expect("start node graph");
    assert_eq!(inst1, 100_001);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].event, "OnStartClientNodeGraph");
    assert_eq!(drained[0].args[0], PyRep::Integer(graph_id));
    assert_eq!(drained[0].args[1], PyRep::Integer(inst1));

    // Record client graph messages of various PyRep types
    on_client_graph_message(&ctx, char_id, graph_id, 7, "str_k", &PyRep::String("v".into()));
    on_client_graph_message(&ctx, char_id, graph_id, 8, "int_k", &PyRep::Integer(99));
    on_client_graph_message(&ctx, char_id, graph_id, 9, "bool_k", &PyRep::Bool(true));
    on_client_graph_message(&ctx, char_id, graph_id, 10, "real_k", &PyRep::Real(3.5));
    on_client_graph_message(&ctx, char_id, graph_id, 11, "none_k", &PyRep::None);
    on_client_graph_message(&ctx, char_id, graph_id, 12, "list_k", &PyRep::List(vec![]));

    // Stop by category/operation id
    stop_active_operation_node_graph_for_category(&ctx, char_id, 2);
    let stopped_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(stopped_notifs.len(), 1);
    assert_eq!(stopped_notifs[0].event, "OnStopClientNodeGraph");
    assert_eq!(stopped_notifs[0].args, vec![PyRep::Integer(inst1)]);

    // Start via maybe_start_operation_node_graph(Some) and stop by graph_id
    maybe_start_operation_node_graph(&ctx, char_id, 10, 2, Some(graph_id)).expect("starts");
    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(!stop_client_node_graph_instances_by_id(&ctx, char_id, 0));
    assert!(!stop_client_node_graph_instances_by_id(&ctx, char_id, 999_999_999));
    assert!(stop_client_node_graph_instances_by_id(&ctx, char_id, graph_id));
    let by_id_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(by_id_notifs.len(), 1);
    assert_eq!(by_id_notifs[0].event, "OnStopClientNodeGraphById");
    assert_eq!(by_id_notifs[0].args, vec![PyRep::Integer(graph_id)]);

    // Fill up to MAX_ACTIVE_CLIENT_GRAPHS (16) and verify 17th fails with TooManyNodeGraphs
    for _ in 0..16 {
        start_client_node_graph_instance(&ctx, char_id, graph_id, 2, 10, BTreeMap::new())
            .expect("within limit");
    }
    assert_eq!(
        start_client_node_graph_instance(&ctx, char_id, graph_id, 2, 10, BTreeMap::new())
            .expect_err("17th fails")
            .code,
        "TooManyNodeGraphs"
    );

    let _ = ctx.drain_notifications_for_char(char_id);
    stop_all_client_node_graphs_for_char(&ctx, char_id);
    let stop_all = ctx.drain_notifications_for_char(char_id);
    assert_eq!(stop_all.len(), 1);
    assert_eq!(stop_all[0].event, "OnStopAllClientNodeGraphs");
}

#[test]
fn tutorial_task_skip_and_operation_slash_commands() {
    let (ctx, char_id, _) = make_online_ctx();

    on_tutorial_started(&ctx, char_id, 1);
    on_tutorial_skipped(&ctx, char_id);

    let mut op_defs = crate::services::operations_manager_helpers::OperationDefinitions::default();
    op_defs.op_tasks.insert(1, vec![10, 20]);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 1, 1, 10);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 4);
    assert_eq!(drained[0].event, "OnAirNpeStateChanged");
    assert_eq!(
        drained[0].args,
        vec![PyRep::Integer(char_id), PyRep::Integer(1)]
    );
    assert_eq!(drained[1].event, "OnAirNpeStateChanged");
    assert_eq!(
        drained[1].args,
        vec![
            PyRep::Integer(char_id),
            PyRep::Integer(AIR_NPE_STATE_SKIPPED)
        ]
    );
    assert_eq!(drained[2].event, "OnTutorialSkipped");
    assert_eq!(drained[3].event, "OnOperationTaskSkipped");

    // Non-/operation command returns None
    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    assert!(handle_operation_slash(&ctx, &session, char_id, "/other").is_none());

    // Unknown subcommand returns InvalidSlashArgument
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation unknown")
            .unwrap()
            .expect_err("unknown subcommand")
            .code,
        "InvalidSlashArgument"
    );

    // deactivate when no active operation -> UserError
    assert!(handle_operation_slash(&ctx, &session, char_id, "/operation deactivate")
        .unwrap()
        .is_err());

    // Seed an active operation in the store so deactivate succeeds
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = crate::services::operations_manager::load_store(snap);
        store.by_character.entry(char_id).or_default().active_operation_id = Some(1);
        crate::services::operations_manager::save_store(snap, &store, 1_775_400_000);
    });
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation deactivate")
            .unwrap()
            .expect("deactivate"),
        PyRep::Bool(true)
    );

    // replay/skip invalid operation returns UserError
    assert!(
        handle_operation_slash(&ctx, &session, char_id, "/operation replay 1 999999")
            .unwrap()
            .is_err()
    );
    assert!(
        handle_operation_slash(&ctx, &session, char_id, "/operation skip 1 999999")
            .unwrap()
            .is_err()
    );
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation reset_progress")
            .unwrap()
            .expect("reset"),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation reveal_ui")
            .unwrap()
            .expect("reveal_ui"),
        PyRep::Bool(true)
    );

    // reload_fsd requires GM/Content/Programmer role
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation reload_fsd")
            .unwrap()
            .expect_err("no role")
            .code,
        "InsufficientRole"
    );
    session.role = service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        handle_operation_slash(&ctx, &session, char_id, "/operation reload_fsd")
            .unwrap()
            .expect("with gm role"),
        PyRep::Bool(true)
    );
}

#[test]
fn blackboard_limits_task_skip_next_replay_skip_reset_and_role_variants() {
    let (ctx, char_id, graph_id_u32) = make_online_ctx();
    let graph_id = i64::from(graph_id_u32);

    // 1. Blackboard capped at MAX_BLACKBOARD_KEYS (64): start with 70 keys, then verify existing-key update works while new-key insert is ignored
    let mut big_bb = BTreeMap::new();
    for i in 0..70 {
        big_bb.insert(format!("k_{i:02}"), i.to_string());
    }
    let inst = start_client_node_graph_instance(&ctx, char_id, graph_id, 5, 77, big_bb)
        .expect("start with capped bb");
    on_client_graph_message(&ctx, char_id, 999_999_999, 1, "ignored", &PyRep::Integer(1)); // missing graph_id noop
    on_client_graph_message(&ctx, char_id, graph_id, 3, "k_00", &PyRep::String("updated".into()));
    on_client_graph_message(&ctx, char_id, graph_id, 3, "brand_new_key", &PyRep::Integer(1));
    ctx.db.with_snapshot(|snap| {
        let graphs = load_active_client_graphs(snap, char_id);
        let g = &graphs[&inst];
        assert_eq!(g.blackboard.len(), MAX_BLACKBOARD_KEYS);
        assert_eq!(g.blackboard.get("k_00").map(String::as_str), Some("updated"));
        assert!(!g.blackboard.contains_key("brand_new_key"));
    });

    // stop_active_operation_node_graph_for_category matches operation_id too; unmatched is noop
    let _ = ctx.drain_notifications_for_char(char_id);
    stop_active_operation_node_graph_for_category(&ctx, char_id, 999_999);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    stop_active_operation_node_graph_for_category(&ctx, char_id, 77);
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);

    // 2. emit_operation_task_skipped: middle task -> Integer(next), last task -> None, missing op -> None
    let mut op_defs = OperationDefinitions::default();
    op_defs.op_tasks.insert(10, vec![100, 200]);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 1, 10, 100);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 1, 10, 200);
    emit_operation_task_skipped(&ctx, char_id, &op_defs, 1, 999, 100);
    let task_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(task_notifs.len(), 3);
    assert!(format!("{:?}", task_notifs[0].args).contains("Integer(200)"));
    assert!(format!("{:?}", task_notifs[1].args).contains("None"));
    assert!(format!("{:?}", task_notifs[2].args).contains("None"));

    // 3. Valid /operation replay & skip against SDE operation definitions
    let sde_defs = get_operation_defs(&ctx.sde);
    if let Some((&valid_op_id, tasks)) = sde_defs.op_tasks.iter().next() {
        let mut session = SessionState::new(501);
        session.user_id = Some(1);
        session.character_id = Some(char_id);

        // Boundary rejections for cat_id/op_id out of 1..=100_000
        assert!(replay_operation_for_char(&ctx, char_id, 0, valid_op_id).is_err());
        assert!(replay_operation_for_char(&ctx, char_id, 100_001, valid_op_id).is_err());
        assert!(skip_operation_for_char(&ctx, char_id, 0, valid_op_id).is_err());
        assert!(skip_operation_for_char(&ctx, char_id, 1, 100_001).is_err());

        // Pre-seed completed/skipped tasks, then replay clears them and emits OnOperationReplayed
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_store(snap);
            let rec = st.by_character.entry(char_id).or_default();
            for &t in tasks {
                rec.completed_tasks.insert(t);
                rec.skipped_tasks.insert(t);
            }
            rec.completed_categories.insert(9);
            rec.completed_operations.insert(99);
            rec.blocked_tasks.insert(888);
            save_store(snap, &st, 1_775_400_000);
        });
        assert_eq!(
            handle_operation_slash(&ctx, &session, char_id, &format!("/operation replay 2 {valid_op_id}"))
                .unwrap()
                .expect("replay ok"),
            PyRep::Bool(true)
        );
        ctx.db.with_snapshot(|snap| {
            let st = load_store(snap);
            let rec = &st.by_character[&char_id];
            assert_eq!(rec.active_category_id, Some(2));
            assert_eq!(rec.active_operation_id, Some(valid_op_id));
            for t in tasks {
                assert!(!rec.completed_tasks.contains(t));
                assert!(!rec.skipped_tasks.contains(t));
            }
        });
        let r_notifs = ctx.drain_notifications_for_char(char_id);
        assert!(r_notifs.iter().any(|n| n.event == "OnOperationReplayed"));

        // Start a node graph tied to valid_op_id, then /operation skip stops it and clears active_operation_id
        let _ = start_client_node_graph_instance(&ctx, char_id, graph_id, 2, valid_op_id, BTreeMap::new())
            .expect("graph for skip");
        let _ = ctx.drain_notifications_for_char(char_id);
        assert_eq!(
            handle_operation_slash(&ctx, &session, char_id, &format!("/operation skip 2 {valid_op_id}"))
                .unwrap()
                .expect("skip ok"),
            PyRep::Bool(true)
        );
        ctx.db.with_snapshot(|snap| {
            let st = load_store(snap);
            let rec = &st.by_character[&char_id];
            assert!(rec.completed_operations.contains(&valid_op_id));
            assert_eq!(rec.active_operation_id, None);
            for t in tasks {
                assert!(rec.skipped_tasks.contains(t));
            }
        });
        let s_notifs = ctx.drain_notifications_for_char(char_id);
        assert!(s_notifs.iter().any(|n| n.event == "OnStopClientNodeGraph"));
        assert!(s_notifs.iter().any(|n| n.event == "OnOperationSkipped"));

        // /operation reset alias clears all sets and emits OnOperationsProgressReset
        assert_eq!(
            handle_operation_slash(&ctx, &session, char_id, "/operation reset")
                .unwrap()
                .expect("reset alias"),
            PyRep::Bool(true)
        );
        ctx.db.with_snapshot(|snap| {
            let st = load_store(snap);
            let rec = &st.by_character[&char_id];
            assert!(rec.completed_categories.is_empty());
            assert!(rec.completed_operations.is_empty());
            assert!(rec.completed_tasks.is_empty());
            assert!(rec.skipped_tasks.is_empty());
            assert!(rec.blocked_tasks.is_empty());
            assert!(rec.active_operation_id.is_some());
        });
        let reset_notifs = ctx.drain_notifications_for_char(char_id);
        assert!(reset_notifs.iter().any(|n| n.event == "OnStopAllClientNodeGraphs"));
        assert!(reset_notifs.iter().any(|n| n.event == "OnOperationsProgressReset"));

        // ROLE_CONTENT & ROLE_PROGRAMMER also permitted for reload_fsd
        for role in [service_roles::ROLE_CONTENT, service_roles::ROLE_PROGRAMMER] {
            session.role = role;
            assert_eq!(
                reload_operations_fsd_data(&ctx, &session, char_id).expect("role allowed"),
                PyRep::Bool(true)
            );
        }
    }
}
