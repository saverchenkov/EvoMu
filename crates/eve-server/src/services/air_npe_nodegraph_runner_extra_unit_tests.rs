//! Additional unit and mutation-killing tests for `air_npe_nodegraph_runner`.

#![forbid(unsafe_code)]

use crate::services::air_npe_nodegraph_runner::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_proto::PyRep;

#[tokio::test]
async fn graph_id_boundaries_and_blackboard_all_keys_and_custom_types() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Exact boundary checks on is_air_npe_graph_id:
    // 49 | 102 | 103 | 685 | 686 | 695 | 707 | 708 | 709..=726 | 733 | 58_745
    for valid_id in [
        49,
        102,
        103,
        685,
        686,
        695,
        AIR_NPE_ACT_1_NODE_GRAPH_ID,
        AIR_NPE_ACT_2_NODE_GRAPH_ID,
        709,
        AIR_NPE_NODE_GRAPH_ID,
        726,
        733,
        AIR_NPE_CLIENT_NODE_GRAPH_ID,
    ] {
        assert!(is_air_npe_graph_id(valid_id), "expected valid id {valid_id}");
    }
    for invalid_id in [0, 48, 50, 101, 104, 684, 687, 694, 696, 706, 727, 732, 734, 58_744, 58_746] {
        assert!(!is_air_npe_graph_id(invalid_id), "expected invalid id {invalid_id}");
    }

    // Before tutorial starts, query_blackboard_value returns None
    ctx.db.with_snapshot(|snap| {
        assert!(query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "act").is_none());
    });

    // Start tutorial while already in space (station_id = None, structure_id = None -> moved_into_pocket = true)
    session.station_id = None;
    session.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(3));

    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).expect("runner state");
        assert!(r.moved_into_pocket);
        assert!(r.intro_cinematic_completed);
        assert_eq!(r.active_server_graph_id, 685);
        assert!(r.executed_server_graphs.contains(&SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID));
        assert!(r.executed_server_graphs.contains(&685));
        r.pocket_instance_id = Some(42_001);
        r.blackboard.insert("custom_int".into(), "123".into());
        r.blackboard.insert("custom_bool_t".into(), "true".into());
        r.blackboard.insert("custom_bool_f".into(), "false".into());
        r.blackboard.insert("custom_str".into(), "hello_npe".into());
        save_runner_state(snap, &r);
    });

    ctx.db.with_snapshot(|snap| {
        // Test all 4 valid graph_id query slots (master_client_graph_id=716, act_client_graph_id=707, scene_client_graph_id=725, active_server_graph_id=685)
        for gid in [AIR_NPE_NODE_GRAPH_ID, AIR_NPE_ACT_1_NODE_GRAPH_ID, 725, 685] {
            assert_eq!(
                query_blackboard_value(snap, char_id, gid, "act").and_then(|v| v.as_i64()),
                Some(1)
            );
        }
        // Unrelated graph_id returns None
        assert!(query_blackboard_value(snap, char_id, 708, "act").is_none());

        // Test every built-in blackboard key
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "scene")
                .and_then(|v| v.as_i64()),
            Some(1)
        );
        for cp_key in ["checkpoint", "checkpoint_id", "current_node_id"] {
            assert_eq!(
                query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, cp_key)
                    .and_then(|v| v.as_i64()),
                Some(12)
            );
        }
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "state_id")
                .and_then(|v| v.as_i64()),
            Some(AIR_NPE_STATE_ACTIVE)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "completed")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "skipped")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "moved_into_pocket")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "pocket_instance_id")
                .and_then(|v| v.as_i64()),
            Some(42_001)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "dungeon_id")
                .and_then(|v| v.as_i64()),
            Some(i64::from(NPE_DUNGEON_SCENE_1_ID))
        );

        // Custom blackboard entries (parsed int, bool true/false, string) and unknown key
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "custom_int")
                .and_then(|v| v.as_i64()),
            Some(123)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "custom_bool_t")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "custom_bool_f")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "custom_str")
                .and_then(|v| v.as_str().map(String::from))
                .as_deref(),
            Some("hello_npe")
        );
        assert!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "nonexistent").is_none()
        );
    });
}

#[tokio::test]
async fn pocket_transition_structure_undock_no_ship_error_and_default_runner_init() {
    let (ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    // 1. When character has active_ship_id <= 0 -> Err("NoActiveShip")
    session.ship_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 0;
    });
    let err = transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id).unwrap_err();
    assert_eq!(err.code, "NoActiveShip");

    // 2. Restore ship, dock character in an Upwell structure (>= 1_000_000_000_000) with no prior runner state
    let struct_id = 1_040_000_000_123_i64;
    session.structure_id = Some(struct_id);
    session.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = ship_id;
        ch.structure_id = Some(struct_id);
        ch.station_id = None;
        snap.items.get_mut(&ship_id).unwrap().location_id = struct_id;
    });

    transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id)
        .expect("transition from structure");
    assert!(session.structure_id.is_none());
    assert_eq!(session.ship_id, Some(ship_id));

    ctx.db.with_snapshot(|snap| {
        let r = load_runner_state(snap, char_id).expect("initialized runner state");
        assert!(r.moved_into_pocket);
        assert!(r.intro_cinematic_completed);
        assert_eq!(r.origin_station_id, Some(struct_id));
        assert_eq!(r.checkpoint_id, 13);
        assert!(r.completed_checkpoints.contains(&12));
    });
}

#[tokio::test]
async fn full_23_checkpoint_progression_act2_transition_and_client_events() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    // Before tutorial starts, on_client_graph_message returns false even on valid NPE graph
    assert!(!on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        10,
        "cinematic_finished",
        &PyRep::None,
    ));

    // Trigger on_client_npe_event("new_eden_cinematic_ended") without prior runner -> auto-starts tutorial & transitions into pocket (cp 12 -> 13)
    assert_eq!(
        on_client_npe_event(
            &ctx,
            &mut session,
            char_id,
            "new_eden_cinematic_ended",
            None
        ),
        Some(PyRep::Bool(true))
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        13
    );

    // Unrelated event does not advance checkpoint 13
    on_server_gameplay_event(&ctx, char_id, "completely_unrelated_event", &[]);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        13
    );

    // Non-NPE graph ID in on_client_graph_message returns false
    assert!(!on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        999_999,
        10,
        "approach_complete",
        &PyRep::None,
    ));

    // Custom non-checkpoint message on NPE graph emits update and returns true without advancing
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        77,
        "ui_highlight_ack",
        &PyRep::None,
    ));
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id),
        13
    );

    // Step through remaining Act 1 checkpoints (13..=81) using alias events
    let act1_events = [
        (13, "approach_complete"),
        (18, "target_locked"),
        (20, "activatemodule"),
        (22, "npcdestroyed"),
        (24, "activate_afterburner"),
        (25, "activate_webifier"),
        (26, "destroynpc"),
        (27, "warpto"),
        (28, "ship_destroyed"),
        (29, "dockatstation"),
        (30, "clone_rebirth_complete"),
        (81, "assembleship"),
    ];

    for (expected_cp, ev_name) in act1_events {
        let cur = ctx
            .db
            .with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id);
        assert_eq!(cur, expected_cp);
        on_server_gameplay_event(
            &ctx,
            char_id,
            ev_name,
            &[("step", expected_cp.to_string())],
        );
    }

    // After checkpoint 81 ("assembleship"), runner must be in Act 2, scene_index 6 ("2.1"), checkpoint 83, act_client_graph_id = 708
    ctx.db.with_snapshot(|s| {
        let r = load_runner_state(s, char_id).unwrap();
        assert_eq!(r.active_act, 2);
        assert_eq!(r.scene_index, 6);
        assert_eq!(r.checkpoint_id, 83);
        assert_eq!(r.act_client_graph_id, Some(AIR_NPE_ACT_2_NODE_GRAPH_ID));
        assert!(!r.completed);
    });

    // Step through Act 2 checkpoints (83..=92) including force-advance aliases and client graph messages
    let act2_events = [
        (83, "undockfromstation"),
        (84, "trainskill"),
        (85, "warp_to_gate"),
        (86, "gateactivated"),
        (87, "orbit_started"),
        (88, "destroy_enemies_boss"),
        (89, "checkpoint_reached"), // force-advance alias
        (90, "stargatejump"),
        (91, "warp_to_station"),
        (92, "dock_at_career_station"),
    ];
    for (expected_cp, ev_name) in act2_events {
        let cur = ctx
            .db
            .with_snapshot(|s| load_runner_state(s, char_id).unwrap().checkpoint_id);
        assert_eq!(cur, expected_cp);
        on_server_gameplay_event(&ctx, char_id, ev_name, &[]);
    }

    // After final checkpoint 92, tutorial is automatically marked completed!
    ctx.db.with_snapshot(|s| {
        let r = load_runner_state(s, char_id).unwrap();
        assert!(r.completed);
        assert!(!r.skipped);
        assert_eq!(r.state_id, AIR_NPE_STATE_COMPLETED);
        assert_eq!(r.checkpoint_id, 92);
    });

    // Restart and test on_client_npe_event("complete_air_npe") and on_skip_air_npe
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(2));
    assert_eq!(
        on_client_npe_event(&ctx, &mut session, char_id, "complete_air_npe", None),
        Some(PyRep::Bool(true))
    );
    assert!(ctx
        .db
        .with_snapshot(|s| load_runner_state(s, char_id).unwrap().completed));

    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(2));
    let (_gid, cnode, comp) =
        on_skip_air_npe(&ctx, Some(&mut session), char_id, ctx.now_unix()).expect("skip");
    assert_eq!(cnode, 4);
    assert!(comp);
    ctx.db.with_snapshot(|s| {
        let r = load_runner_state(s, char_id).unwrap();
        assert!(r.completed);
        assert!(r.skipped);
        assert_eq!(r.state_id, AIR_NPE_STATE_SKIPPED);
    });

    assert_eq!(
        on_client_npe_event(&ctx, &mut session, char_id, "unknown_npe_event_xyz", None),
        None
    );
}
