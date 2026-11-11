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

    // Unprivileged client cannot complete AIR NPE directly; GM role is required
    let unpriv_comp = on_client_npe_event(&ctx, &mut session, char_id, "complete_air_npe", None);
    assert_eq!(unpriv_comp, None);
    assert_eq!(
        ctx.db.with_snapshot(|snap| load_runner_state(snap, char_id).map(|r| r.completed)),
        Some(false)
    );

    session.role |= eve_machonet::service_roles::ROLE_GML;
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

#[cfg(test)]
#[path = "air_npe_nodegraph_runner_extra_unit_tests.rs"]
mod air_npe_nodegraph_runner_extra_unit_tests;

#[tokio::test]
async fn test_air_npe_runner_auth_errors_trusted_checkpoints_and_none_session() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    // 1. on_skip_air_npe when no npe_progress record exists creates default (node_graph_id=0, current_node_id=4, completed=true)
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.remove(&char_id);
    });
    let (gid, cnode, comp) =
        on_skip_air_npe(&ctx, None, char_id, ctx.now_unix()).expect("skip without prior npe_progress");
    assert_eq!(gid, 0);
    assert_eq!(cnode, 4);
    assert!(comp);

    // After skipping, try_client_npe_event("new_eden_cinematic_ended") fails with TutorialNotActive
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "new_eden_cinematic_ended", None)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );
    assert_eq!(
        on_client_npe_event(&ctx, &mut session, char_id, "new_eden_cinematic_ended", None),
        None
    );

    // GM sending complete_air_npe when tutorial is NOT active -> Err("TutorialNotActive")
    session.role |= eve_machonet::service_roles::ROLE_GML;
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "complete_air_npe", None)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );
    assert_eq!(
        complete_air_npe_checked(&ctx, None, char_id, ctx.now_unix())
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );

    // 2. Start tutorial and test query_blackboard_value with None pocket_instance_id / dungeon_id
    session.role = 0;
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, None);
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).unwrap();
        r.pocket_instance_id = None;
        r.pocket_dungeon_id = None;
        save_runner_state(snap, &r);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "pocket_instance_id"),
            Some(PyRep::None)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "dungeon_id"),
            Some(PyRep::None)
        );
    });

    // 3. on_server_gameplay_event does nothing when state_id != AIR_NPE_STATE_ACTIVE
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).unwrap();
        r.state_id = AIR_NPE_STATE_DISABLED;
        save_runner_state(snap, &r);
    });
    on_server_gameplay_event(&ctx, char_id, "new_eden_intro", &[]);
    assert_eq!(
        ctx.db.with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        12
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).unwrap();
        r.state_id = AIR_NPE_STATE_ACTIVE;
        save_runner_state(snap, &r);
    });

    // 4. Client cannot advance trusted checkpoints (18+) without GM role
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "new_eden_cinematic_ended", None).unwrap(),
        Some(PyRep::Bool(true))
    );
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        13,
        "approach_station",
        &PyRep::None,
    ));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        18
    );

    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "lock_target", None).unwrap(),
        None
    );
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "checkpoint_reached", None).unwrap(),
        None
    );
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        18,
        "lock_target",
        &PyRep::None,
    ));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        18
    );
    assert!(!on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        18,
        "complete_air_npe",
        &PyRep::None,
    ));

    // GM CAN advance trusted checkpoints
    session.role |= eve_machonet::service_roles::ROLE_GML;
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "advance_checkpoint", None).unwrap(),
        Some(PyRep::Bool(true))
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        20
    );
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        20,
        "objective_completed",
        &PyRep::None,
    ));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        22
    );

    // 5. transition_character_into_npe_pocket error guards: CharacterNotFound, WarpScrambled
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, None, 99_999_999)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.set_warp_scramble_strength(ship_id, 2);
    }
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, None, char_id)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert!(!on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        22,
        "enter_tutorial_pocket",
        &PyRep::None,
    ));
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.set_warp_scramble_strength(ship_id, 0);
    }

    // 6. Complete via complete_air_npe (session = None)
    complete_air_npe(&ctx, None, char_id, ctx.now_unix());
    ctx.db.with_snapshot(|s| {
        let r = load_runner_state(s, char_id).unwrap();
        assert!(r.completed);
        assert_eq!(r.state_id, AIR_NPE_STATE_COMPLETED);
    });
}
