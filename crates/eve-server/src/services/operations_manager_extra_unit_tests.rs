//! Extra unit and mutation-killing tests for `operations_manager`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::operations_manager::*;
use crate::services::operations_manager_helpers::load_operation_definitions;
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn operations_manager_helpers_npe_skip_spawnpoint_and_dungeon_lookup() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // push_operation_event eviction above MAX_OPERATION_EVENTS (128)
    let mut rec = CharacterOperationsRecord::default();
    for i in 1..=135 {
        push_operation_event(&mut rec, format!("ev_{i}"), i);
    }
    assert_eq!(rec.operation_events.len(), MAX_OPERATION_EVENTS);
    assert_eq!(rec.operation_events[0].1, 8);

    // mark_new_character_tutorial_pending & sync_air_npe_skip & can_character_play_tutorial
    ctx.db.with_mut_snapshot(|snap| {
        mark_new_character_tutorial_pending(snap, char_id, 1_700_000_000);
        assert!(can_character_play_tutorial(snap, char_id));
        sync_air_npe_skip(snap, char_id, 1_700_000_010);
        assert!(!can_character_play_tutorial(snap, char_id));
    });

    // resolve_operation_spawnpoint_solar_system fallbacks: school_id, race_id school, and solar_system_id
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.school_id = 0;
        ch.race_id = 4; // Amarr school fallback
    });
    assert!(resolve_operation_spawnpoint_solar_system(&ctx, char_id, None).is_some());

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.school_id = 0;
        ch.race_id = 999; // invalid race -> falls back to ch.solar_system_id
    });
    assert_eq!(
        resolve_operation_spawnpoint_solar_system(&ctx, char_id, None),
        Some(sys_id)
    );

    // active_operation_dungeon_for_character & is_operation_site_or_system & completed_operations_for_character
    let sample_dungeon = *ctx.sde.dungeons.keys().next().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        let r = store.by_character.entry(char_id).or_default();
        r.active_operation_id = Some(501);
        r.active_operation_system_id = Some(sys_id);
        r.active_site_id = Some(901);
        r.active_dungeon_id = Some(sample_dungeon);
        r.completed_operations.insert(777);
        save_store_pub(snap, &store, 1_700_000_020);
    });
    assert!(is_operation_site_or_system(&ctx, char_id, 901));
    assert!(is_operation_site_or_system(&ctx, char_id, sys_id as i64));
    assert!(!is_operation_site_or_system(&ctx, char_id, 999_999));

    let (d_id, _room_id, inst_id, _d_name) =
        active_operation_dungeon_for_character(&ctx, char_id).expect("active dungeon");
    assert_eq!(d_id, sample_dungeon);
    assert_eq!(inst_id, 901);

    // Wrong system hides active operation dungeon
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        store
            .by_character
            .get_mut(&char_id)
            .unwrap()
            .active_operation_system_id = Some(sys_id + 1);
        save_store_pub(snap, &store, 1_700_000_030);
        assert!(completed_operations_for_character(snap, char_id).contains(&777));
    });
    assert!(active_operation_dungeon_for_character(&ctx, char_id).is_none());
}

#[tokio::test]
async fn operations_manager_non_gm_role_checks_prereqs_replays_and_progress() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    session.role = 0; // Non-GM caller

    let op_defs = load_operation_definitions(&ctx.sde);
    let sample_cat = *op_defs.sde_categories.iter().next().unwrap();
    let sample_op = *op_defs
        .op_prerequisites
        .iter()
        .find(|(_, p)| p.is_empty())
        .map(|(k, _)| k)
        .unwrap_or_else(|| op_defs.sde_operations.iter().next().unwrap());

    // Non-GM querying another character's tutorial finished state fails UserError
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_main_tutorial_finished",
            &[PyRep::Integer(char_id + 999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserError"
    );

    // Out-of-range category ID returns Bool(false) for is_category_complete and CustomInfo for get_category_state
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_category_complete",
            &[PyRep::Integer(-1)],
            &kw,
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
            "get_category_state",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CustomInfo"
    );

    // Non-GM complete_category_for_character: cross-char fails InsufficientRole; incomplete fails OperationCategoryIncomplete
    let err_role = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(char_id + 1), PyRep::Integer(sample_cat)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_role.code.to_ascii_lowercase(), "insufficientrole");
    let err_incomp = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(char_id), PyRep::Integer(sample_cat)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_incomp.code.to_ascii_lowercase(), "operationcategoryincomplete");

    // Find an operation with prerequisites and verify non-GM activation fails OperationPrerequisitesNotMet
    if let Some((&gated_op, prereqs)) = op_defs
        .op_prerequisites
        .iter()
        .find(|(_, p)| !p.is_empty())
    {
        assert!(!prereqs.is_empty());
        let err_pre = handle(
            &ctx,
            &mut session,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[
                PyRep::Integer(gated_op),
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(sample_cat),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err_pre.code, "OperationPrerequisitesNotMet");
    }

    // Activate sample_op, then replay it (already completed) and switch to another op
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        let r = store.by_character.entry(char_id).or_default();
        for &oid in &op_defs.sde_operations {
            r.completed_operations.insert(oid);
        }
        if let Some(tasks) = op_defs.op_tasks.get(&sample_op) {
            for &t in tasks {
                r.completed_tasks.insert(t);
                r.task_progress.insert(t, 1);
            }
        }
        save_store_pub(snap, &store, 1_700_000_050);
    });
    let replayed = handle(
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
    assert_eq!(replayed, PyRep::Bool(true));

    // get_character_progress & get_operations_completed_at_least_once
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
    let completed_once = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_operations_completed_at_least_once",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(!completed_once.as_list().unwrap().is_empty());

    // Non-GM skip_task on non-skippable task fails TaskNotSkippable
    if let Some(&non_skip_tid) = op_defs
        .op_tasks
        .get(&sample_op)
        .and_then(|ts| ts.iter().find(|t| !op_defs.task_is_client_skippable.contains(t)))
    {
        let err_skip = handle(
            &ctx,
            &mut session,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(non_skip_tid)],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err_skip.code.to_ascii_lowercase(), "tasknotskippable");
    }

    // start_site while docked fails CustomInfo; in space with expected site/dungeon succeeds
    let (site_op, site_arg) = op_defs
        .sde_operations
        .iter()
        .find_map(|&oid| {
            if let Some(&s) = op_defs.op_site_ids.get(&oid).filter(|&&s| s > 0) {
                op_defs.resolve_site_dungeon_id(&ctx.sde, s, 0).map(|_| (oid, s))
            } else {
                ctx.sde.dungeons.keys().next().map(|&d| (oid, d as i64))
            }
        })
        .expect("operation for start_site");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut store = load_store_pub(snap);
        let r = store.by_character.entry(char_id).or_default();
        r.active_operation_id = Some(site_op);
        r.active_operation_system_id = Some(sys_id);
        save_store_pub(snap, &store, 1_700_000_060);
    });
    session.station_id = Some(60_003_760);
    let err_dock = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(site_arg)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_dock.code, "CustomInfo");
    session.station_id = None;
    let started_site = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(site_arg)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(started_site, PyRep::Bool(true));

    // process_client_event Dict variants + start_tutorial <= 0 error + survey service delegation
    let err_tut = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("start_tutorial".into()),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_tut.code.to_ascii_lowercase(), "invalidtutorialstate");
    let _ = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("set_career_path".into()),
            PyRep::Dict(vec![(
                PyRep::String("careerPathID".into()),
                PyRep::Integer(2),
            )]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("start_tutorial".into()),
            PyRep::Dict(vec![(
                PyRep::String("operationID".into()),
                PyRep::Integer(sample_op),
            )]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Survey service via operations_manager::handle("survey", ...)
    let active_survey = handle(&ctx, &mut session, "survey", "GetActiveSurveyID", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(active_survey, PyRep::Integer(_) | PyRep::None));
    let _ = handle(
        &ctx,
        &mut session,
        "survey",
        "PerformSurveyChecks",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
}

#[test]
fn unauth_validation_switch_complete_via_skip_site_and_client_events() {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let mut anon = eve_machonet::SessionState::default();

    for (svc, method) in [
        ("operationsManager", "get_tutorial_state"),
        ("operationsManager", "has_skipped_tutorial"),
        ("operationsManager", "is_main_tutorial_finished"),
        ("operationsManager", "get_active_category_id"),
        ("operationsManager", "is_category_complete"),
        ("operationsManager", "get_category_state"),
        ("operationsManager", "complete_category_for_character"),
        ("operationsManager", "get_character_progress"),
        ("operationsManager", "get_mission_avoidance_systems"),
        ("operationsManager", "get_operations_completed_at_least_once"),
        ("operationsManager", "activate_operation_in_solar_system"),
        ("operationsManager", "block_task"),
        ("operationsManager", "unblock_task"),
        ("operationsManager", "skip_task"),
        ("operationsManager", "start_site"),
        ("operationsManager", "process_client_event"),
    ] {
        assert!(handle(&ctx, &mut anon, svc, method, &[], &kw).unwrap().is_err());
    }

    // can_character_play_tutorial false when skipped (`L123`, `L132`), resolve spawnpoint valid school & missing char (`L189-205`)
    assert!(resolve_operation_spawnpoint_solar_system(&ctx, 999_999_999, None).is_none());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.school_id = *ctx.sde.schools.keys().next().unwrap_or(&11);
        }
        mark_new_character_tutorial_pending(snap, char_id, 100);
        sync_air_npe_skip(snap, char_id, 101);
        assert!(!can_character_play_tutorial(snap, char_id));
    });
    assert!(resolve_operation_spawnpoint_solar_system(&ctx, char_id, None).is_some());

    let op_defs = load_operation_definitions(&ctx.sde);
    let mut op_ids: Vec<i64> = op_defs.sde_operations.iter().copied().collect();
    op_ids.sort_unstable();
    let op1 = op_ids[0];
    let op2 = *op_ids.get(1).unwrap_or(&op1);

    // activate_operation_in_solar_system validation errors (`L631-658`) & task errors (`L711-785`)
    assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(999_999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op1), PyRep::Integer(999_999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op1), PyRep::Integer(i64::from(sys_id)), PyRep::Integer(999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(999_999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(999_999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(999_999_999)], &kw).unwrap().is_err());

    // Non-GM skip_task fails without active op (`L785`), then activate op1 as GM and switch to op2 to trigger OnOperationDeactivated (`L696`)
    let some_task = *op_defs.op_tasks.get(&op1).and_then(|ts| ts.first()).unwrap_or(&1);
    let err = handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(some_task)], &kw).unwrap().unwrap_err();
    // rpc-lint: allow MachoError code assertion, not an RPC method name
    assert_eq!(err.code, "NoActiveOperation");

    sess.role |= eve_machonet::service_roles::ROLE_GMH;
    handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op1)], &kw).unwrap().unwrap();
    if op2 != op1 {
        handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op2)], &kw).unwrap().unwrap();
    }

    // Complete all tasks in op2 via GM skip_task -> grant_task_and_operation_rewards & COMPLETE state (`L429`, `L821-838`)
    if let Some(tasks) = op_defs.op_tasks.get(&op2) {
        for &tid in tasks {
            let _ = handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(tid)], &kw).unwrap().unwrap();
        }
    }
    assert!(handle(&ctx, &mut sess, "operationsManager", "get_operations_completed_at_least_once", &[], &kw).unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "operationsManager", "get_character_progress", &[], &kw).unwrap().is_ok());

    // start_site errors & success (`L875-960`)
    assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(op2), PyRep::Integer(999_999_999)], &kw).unwrap().is_err());
    let mut kw_site = HashMap::new();
    kw_site.insert("operation_id".to_string(), PyRep::Integer(op2));
    kw_site.insert("site_id".to_string(), PyRep::Integer(999_999_999));
    assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[], &kw_site).unwrap().is_err());

    if let Some((&site_op, &site_id)) = op_defs.op_site_ids.iter().find(|(_, &sid)| sid > 0) {
        sess.station_id = None;
        sess.structure_id = None;
        handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(site_op)], &kw).unwrap().unwrap();
        let _ = handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(site_op), PyRep::Integer(site_id)], &kw);
    }

    // process_client_event branches (`L1101-1253`)
    let valid_cat = *op_defs.sde_categories.iter().next().unwrap_or(&31);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("set_active_category_id".into()),
                PyRep::Dict(vec![(PyRep::String("category_id".into()), PyRep::Integer(valid_cat))]),
            ],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("activate_category".into()), PyRep::Integer(999_999_999)],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("add_mission_avoidance_system".into()),
                PyRep::Dict(vec![(PyRep::String("system_id".into()), PyRep::Integer(i64::from(sys_id)))]),
            ],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("add_mission_avoidance_system".into()), PyRep::Integer(999_999_999)],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("set_mission_avoidance_systems".into()),
                PyRep::List(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(999_999_999)]),
            ],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("set_mission_avoidance_systems".into()),
                PyRep::Dict(vec![(PyRep::String("systems".into()), PyRep::List(vec![PyRep::Integer(i64::from(sys_id))]))]),
            ],
            &kw,
        ).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    sess.role = 0;
    let role_err = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("complete_air_npe".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    // rpc-lint: allow MachoError code assertion, not an RPC method name
    assert_eq!(role_err.code, "InsufficientRole");
    let _ = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("task_ui_clicked".into()), PyRep::Dict(vec![])],
        &kw,
    );
}

#[test]
fn operations_manager_skip_complete_category_states_and_event_variants() {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let op_defs = load_operation_definitions(&ctx.sde);
    let kw = HashMap::new();

    // Find a multi-task operation with at least one skippable/optional task so skip_task finishes it
    let (skip_op_id, pre_done, skip_tid) = op_defs
        .op_tasks
        .iter()
        .find_map(|(&oid, ts)| {
            if ts.len() < 2 {
                return None;
            }
            let stid = ts.iter().copied().find(|t| {
                op_defs.task_is_client_skippable.contains(t) || op_defs.task_is_optional.contains(t)
            })?;
            let others: Vec<i64> = ts.iter().copied().filter(|&t| t != stid).collect();
            Some((oid, others, stid))
        })
        .expect("SDE has at least one multi-task op with skippable/optional task");
    let first_cat = *op_defs.sde_categories.iter().next().unwrap();
    let other_cat = op_defs
        .sde_categories
        .iter()
        .copied()
        .find(|&c| c != first_cat)
        .unwrap_or(first_cat);

    // Pre-populate blocked task -> TaskNotSkippable (L800-801), then unblock & skip -> completes op (L821-838)
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_operation_id = Some(skip_op_id);
        rec.active_category_id = Some(first_cat);
        rec.completed_categories.insert(other_cat);
        for t in &pre_done {
            rec.completed_tasks.insert(*t);
        }
        rec.blocked_tasks.insert(skip_tid);
        save_store(snap, &store, 1_700_000_000);
    });
    let skip_err = handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(skip_tid)], &kw)
        .unwrap()
        .unwrap_err();
    // rpc-lint: allow MachoError code assertion, not an RPC method name
    assert_eq!(skip_err.code, "TaskNotSkippable");

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.blocked_tasks.remove(&skip_tid);
        save_store(snap, &store, 1_700_000_001);
    });
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(skip_tid)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // get_category_state: completed (3), active (2), available (1) (L428-435)
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(first_cat)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(2)
    );
    if other_cat != first_cat {
        assert_eq!(
            handle(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(other_cat)], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Integer(3)
        );
    }

    // activate_operation_in_solar_system with session.solar_system_id == None falls back to snap (L633-636)
    let act_op_id = op_defs
        .sde_operations
        .iter()
        .copied()
        .find(|oid| op_defs.op_prerequisites.get(oid).is_none_or(|p| p.is_empty()))
        .unwrap_or(skip_op_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.completed_operations
            .extend(op_defs.sde_operations.iter().copied().filter(|&id| id != act_op_id));
        save_store(snap, &store, 1_700_000_001);
    });
    sess.solar_system_id = None;
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(act_op_id)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    sess.solar_system_id = Some(sys_id);

    // start_site expected_site mismatch (L936-939)
    if let Some((&site_op, &expected_site)) = op_defs.op_site_ids.iter().find(|(_, &s)| s > 0) {
        if let Some(bad_site) = op_defs.op_site_ids.values().copied().find(|&sid| sid > 0 && sid != expected_site) {
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.active_operation_id = Some(site_op);
                rec.active_operation_system_id = Some(sys_id);
                save_store(snap, &store, 1_700_000_002);
            });
            assert!(
                handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(bad_site), PyRep::Integer(site_op)], &kw)
                    .unwrap()
                    .is_err()
            );
        }
    }

    // process_client_event extra parameter variants (L1106-1115, L1144-1153, L1196-1198)
    for cat_arg in [
        PyRep::Dict(vec![(PyRep::String("cat_id".into()), PyRep::Integer(first_cat))]),
        PyRep::Integer(first_cat),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("set_active_category_id".into()), cat_arg], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
    }
    for sys_arg in [
        PyRep::Dict(vec![(PyRep::String("systemID".into()), PyRep::Integer(i64::from(sys_id)))]),
        PyRep::Integer(i64::from(sys_id)),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("add_mission_avoidance_system".into()), sys_arg], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("set_mission_avoidance_systems".into()), PyRep::None, PyRep::Integer(i64::from(sys_id))],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}
