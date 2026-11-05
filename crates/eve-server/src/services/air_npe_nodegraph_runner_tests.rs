use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[tokio::test]
async fn test_air_npe_nodegraph_runner_lifecycle_checkpoints_and_skip() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    assert!(is_air_npe_graph_id(AIR_NPE_NODE_GRAPH_ID));
    assert!(is_air_npe_graph_id(49));
    assert!(!is_air_npe_graph_id(999_999));

    // Start tutorial
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(2));

    // Query blackboard values
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "act")
                .and_then(|v| v.as_i64()),
            Some(1)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "checkpoint")
                .and_then(|v| v.as_i64()),
            Some(12)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "completed")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(query_blackboard_value(snap, char_id, 999_999, "act").is_none());
    });

    // Transition into NPE pocket on intro cinematic completion
    let ev_res = on_client_npe_event(
        &ctx,
        &mut session,
        char_id,
        "new_eden_cinematic_ended",
        None,
    );
    assert_eq!(ev_res, Some(PyRep::Bool(true)));
    assert!(session.station_id.is_none());

    // Advance checkpoints via client graph messages and server gameplay events
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        13,
        "approach_station",
        &PyRep::None,
    ));
    on_server_gameplay_event(&ctx, char_id, "lock_target", &[]);
    on_server_gameplay_event(&ctx, char_id, "activate_weapons", &[]);
    on_server_gameplay_event(&ctx, char_id, "destroy_enemies", &[]);

    let cp_after = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id).map(|r| r.checkpoint_id)
    });
    assert_eq!(cp_after, Some(24));

    // Test complete_air_npe via client event
    let comp_res = on_client_npe_event(&ctx, &mut session, char_id, "complete_air_npe", None);
    assert_eq!(comp_res, Some(PyRep::Bool(true)));
    let is_comp = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id).map(|r| r.completed)
    });
    assert_eq!(is_comp, Some(true));

    // Restart and test on_skip_air_npe
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(1));
    let (_, node_id, skipped_comp) =
        on_skip_air_npe(&ctx, Some(&mut session), char_id, ctx.now_unix()).unwrap();
    assert_eq!(node_id, 4);
    assert!(skipped_comp);
}
