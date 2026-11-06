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
