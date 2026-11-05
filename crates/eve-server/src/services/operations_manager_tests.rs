use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[test]
fn test_operations_manager_tutorial_categories_tasks_and_telemetry() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    session.role = u64::MAX;

    // 1. Initialize new character tutorial pending and verify eligibility
    ctx.db.with_mut_snapshot(|snap| {
        mark_new_character_tutorial_pending(snap, char_id, 1_700_000_000);
        assert!(can_character_play_tutorial(snap, char_id));
    });

    let tut_state = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_tutorial_state",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        tut_state
            .dict_get("is_tutorial_active")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "has_skipped_tutorial",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_main_tutorial_finished",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    // 2. Pick a real SDE category, operation, and task
    let op_defs = get_operation_defs(&ctx.sde);
    let sample_cat = *op_defs.sde_categories.iter().next().unwrap();
    let sample_op = *op_defs.sde_operations.iter().next().unwrap();
    let sample_task = *op_defs.task_to_op.keys().next().unwrap();

    // Activate operation in solar system
    let act = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(sys_id as i64),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(act, PyRep::Bool(true));

    assert!(is_operation_site_or_system(&ctx, char_id, sample_op));
    assert_eq!(
        resolve_operation_spawnpoint_solar_system(&ctx, char_id, Some(sample_op)),
        Some(sys_id)
    );

    // get_active_category_id, get_category_state, is_category_complete
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "get_active_category_id",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(sample_cat)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "get_category_state",
            &[PyRep::Integer(sample_cat)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(2) // ACTIVE
    );

    // block_task, unblock_task, skip_task, complete_category_for_character
    for method in ["block_task", "unblock_task", "skip_task"] {
        let r = handle(
            &ctx,
            &mut session,
            "operationsManager",
            method,
            &[PyRep::Integer(sample_task)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }

    let comp_cat = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(sample_cat)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(comp_cat, PyRep::Bool(true));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_category_complete",
            &[PyRep::Integer(sample_cat)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // process_client_event for set_career_path, add_mission_avoidance_system, skip_tutorial
    for (ev, arg) in [
        ("set_career_path", PyRep::Integer(3)),
        ("add_mission_avoidance_system", PyRep::Integer(sys_id as i64)),
        ("skip_tutorial", PyRep::Integer(0)),
    ] {
        let r = handle(
            &ctx,
            &mut session,
            "operationsManager",
            "process_client_event",
            &[PyRep::String(ev.into()), arg],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }

    let avoid = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_mission_avoidance_systems",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(avoid, PyRep::List(vec![PyRep::Integer(sys_id as i64)]));

    let prog = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_character_progress",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(prog, PyRep::Dict(_)));

    // eventLog RPCs
    for method in [
        "LogClientEvent",
        "LogClientStats",
        "LogPlayerRequestedDisconnect",
    ] {
        let r = handle(&ctx, &mut session, "eventLog", method, &[], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(r, PyRep::None);
    }
}
