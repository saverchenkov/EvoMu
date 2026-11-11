//! Edge-case tests for `operations_manager`: tutorial eligibility, operation activation/prerequisites,
//! task blocking/skipping/progress, start_site validation, client event routing, and surveys.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::NpeProgressRecord;

#[test]
fn operations_manager_tutorial_and_helper_edge_cases() {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // Unauthenticated session fails on RPCs; unknown service/method returns None
    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "operationsManager", "get_tutorial_state", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "otherService", "get_tutorial_state", &[], &kw).is_none());
    assert!(handle(&ctx, &mut sess, "operationsManager", "unknown_op_method", &[], &kw).is_none());

    // push_operation_event caps at MAX_OPERATION_EVENTS
    let mut rec = CharacterOperationsRecord::default();
    for i in 0..140 {
        push_operation_event(&mut rec, format!("ev_{i}"), i);
    }
    assert_eq!(rec.operation_events.len(), MAX_OPERATION_EVENTS);
    assert_eq!(rec.operation_events[0].1, 12);

    // can_character_play_tutorial across NPE progress & store states (isolated tut_cid)
    let tut_cid = char_id + 999;
    ctx.db.with_mut_snapshot(|snap| {
        assert!(!can_character_play_tutorial(snap, tut_cid));
        snap.npe_progress.insert(
            tut_cid,
            NpeProgressRecord {
                character_id: tut_cid,
                node_graph_id: 716,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
        assert!(can_character_play_tutorial(snap, tut_cid));

        // Node graph != 716 && current_node_id >= 4 is considered completed
        snap.npe_progress.get_mut(&tut_cid).unwrap().node_graph_id = 800;
        snap.npe_progress.get_mut(&tut_cid).unwrap().current_node_id = 4;
        assert!(!can_character_play_tutorial(snap, tut_cid));

        snap.npe_progress.remove(&tut_cid);
        mark_new_character_tutorial_pending(snap, tut_cid, 1_760_000_000);
        assert!(can_character_play_tutorial(snap, tut_cid));
        sync_air_npe_skip(snap, tut_cid, 1_760_000_010);
        assert!(!can_character_play_tutorial(snap, tut_cid));
        assert!(completed_operations_for_character(snap, tut_cid).is_empty());
    });

    // Non-GM querying another character's tutorial finished status -> UserError; GM -> Ok
    assert!(handle(&ctx, &mut sess, "operationsManager", "is_main_tutorial_finished", &[PyRep::Integer(char_id + 1)], &kw).unwrap().is_err());
    let mut gm = sess.clone();
    gm.role = u64::MAX;
    assert_eq!(
        handle(&ctx, &mut gm, "operationsManager", "is_main_tutorial_finished", &[PyRep::Integer(char_id + 1)], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );

    // resolve_operation_spawnpoint_solar_system fallbacks
    assert_eq!(resolve_operation_spawnpoint_solar_system(&ctx, 999_888_777, None), None);
    let sp = resolve_operation_spawnpoint_solar_system(&ctx, char_id, None);
    assert!(sp.is_some());

    // is_operation_site_or_system false for <= 0 and missing record
    assert!(!is_operation_site_or_system(&ctx, char_id, 0));
    assert!(!is_operation_site_or_system(&ctx, 999_888_777, 1));
    assert!(active_operation_dungeon_for_character(&ctx, char_id).is_none());

    // is_category_complete out of range -> false; get_category_state invalid -> Err, valid -> 1 (AVAILABLE)
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(0)], &kw).unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert!(handle(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(999_999)], &kw).unwrap().is_err());

    let op_defs = get_operation_defs(&ctx.sde);
    if let Some(&cat_id) = op_defs.sde_categories.iter().next() {
        assert_eq!(
            handle(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(cat_id)], &kw).unwrap().unwrap(),
            PyRep::Integer(1)
        );
    }

    // Seed active operation + dungeon + site + task progress to verify active_operation_dungeon_for_character & get_character_progress
    if let (Some(&op_id), Some(&dungeon_id)) = (op_defs.sde_operations.iter().next(), ctx.sde.dungeons.keys().next()) {
        let task_id = op_defs.op_tasks.get(&op_id).and_then(|ts| ts.first().copied()).unwrap_or(1);
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store(snap);
            let r = store.by_character.entry(char_id).or_default();
            r.active_operation_id = Some(op_id);
            r.active_category_id = op_defs.op_default_category.get(&op_id).copied();
            r.active_operation_system_id = Some(sys_id);
            r.active_site_id = Some(555_001);
            r.active_dungeon_id = Some(dungeon_id);
            r.blocked_tasks.insert(task_id);
            r.task_progress.insert(task_id, 1);
            save_store(snap, &store, 1_760_000_020);
        });
        assert!(active_operation_dungeon_for_character(&ctx, char_id).is_some());
        assert!(is_operation_site_or_system(&ctx, char_id, 555_001));
        assert!(is_operation_site_or_system(&ctx, char_id, i64::from(sys_id)));
        let prog = handle(&ctx, &mut sess, "operationsManager", "get_character_progress", &[], &kw).unwrap().unwrap();
        assert!(matches!(prog, PyRep::Dict(ref d) if !d.is_empty()));

        // Wrong system hides active dungeon and site
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id + 1;
        });
        assert!(active_operation_dungeon_for_character(&ctx, char_id).is_none());
        assert!(!is_operation_site_or_system(&ctx, char_id, 555_001));
    }
}

#[test]
fn operations_manager_rpcs_error_paths_client_events_and_surveys() {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let op_defs = get_operation_defs(&ctx.sde);

    // Ensure undocked in valid system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);

    // complete_category_for_character error cases
    assert!(handle(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(0)], &kw).unwrap().is_err());
    let role_err = handle(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(char_id + 1), PyRep::Integer(31)], &kw)
        .unwrap()
        .unwrap_err();
    assert!(role_err.code.eq_ignore_ascii_case("insufficientrole"), "got {}", role_err.code);
    if let Some((&cat_id, _)) = op_defs.cat_operation_trees.iter().find(|(_, ops)| !ops.is_empty()) {
        let inc_err = handle(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)], &kw)
            .unwrap()
            .unwrap_err();
        assert!(inc_err.code.eq_ignore_ascii_case("operationcategoryincomplete"), "got {}", inc_err.code);
    }
    let mut gm = sess.clone();
    gm.role = u64::MAX;
    if let Some(&cat_id) = op_defs.sde_categories.iter().next() {
        let missing_err = handle(&ctx, &mut gm, "operationsManager", "complete_category_for_character", &[PyRep::Integer(999_888_777), PyRep::Integer(cat_id)], &kw)
            .unwrap()
            .unwrap_err();
        assert!(missing_err.code.eq_ignore_ascii_case("characternotfound"), "got {}", missing_err.code);
    }

    // activate_operation_in_solar_system error cases & prerequisites check
    assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(999_999)], &kw).unwrap().is_err());
    if let Some(&op_id) = op_defs.sde_operations.iter().next() {
        // Mismatched system arg
        assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(sys_id) + 1)], &kw).unwrap().is_err());
        // Invalid explicit category arg
        assert!(handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(sys_id)), PyRep::Integer(999_999)], &kw).unwrap().is_err());
    }
    if let Some((&op_with_prereq, _)) = op_defs.op_prerequisites.iter().find(|(_, p)| !p.is_empty()) {
        let pre_err = handle(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_with_prereq)], &kw)
            .unwrap()
            .unwrap_err();
        assert!(pre_err.code.eq_ignore_ascii_case("operationprerequisitesnotmet"), "got {}", pre_err.code);
    }

    // block_task, unblock_task, skip_task error paths
    assert!(handle(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(999_999)], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(999_999)], &kw).unwrap().is_err());
    if let Some(&tid) = op_defs.task_to_op.keys().next() {
        let no_op_err = handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(tid)], &kw)
            .unwrap()
            .unwrap_err();
        assert!(no_op_err.code.eq_ignore_ascii_case("noactiveoperation"), "got {}", no_op_err.code);
    }

    // start_site error paths & valid activation -> start_site
    assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(1)], &kw).unwrap().is_err());

    if let Some((&op_id, &site_id)) = op_defs.op_site_ids.iter().find(|(_, &s)| s > 0) {
        assert_eq!(
            handle(&ctx, &mut gm, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id)], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        // Replay triggers OnOperationReplayed branch
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_store(snap);
            st.by_character.entry(char_id).or_default().completed_operations.insert(op_id);
            save_store(snap, &st, 1_760_000_050);
        });
        if let Some(&other_op) = op_defs.sde_operations.iter().find(|&&o| o != op_id) {
            let _ = handle(&ctx, &mut gm, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(other_op)], &kw);
            let _ = handle(&ctx, &mut gm, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id)], &kw);
        }
        // Docked rejects start_site; undocked with valid site_id succeeds
        sess.station_id = Some(60_003_760);
        assert!(handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(site_id)], &kw).unwrap().is_err());
        sess.station_id = None;
        if op_defs.resolve_site_dungeon_id(&ctx.sde, site_id, 1).is_some() {
            assert_eq!(
                handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(op_id), PyRep::Integer(site_id)], &kw)
                    .unwrap()
                    .unwrap(),
                PyRep::Bool(true)
            );
        }
    }

    // process_client_event branches
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[
                PyRep::String("set_career_path".into()),
                PyRep::Integer(0),
                PyRep::Dict(vec![(PyRep::String("career_path".into()), PyRep::Integer(4))]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let tut_err = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into()), PyRep::Integer(-1)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert!(tut_err.code.eq_ignore_ascii_case("invalidtutorialstate"), "got {}", tut_err.code);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("activate_category".into()), PyRep::Integer(999_999)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    if let Some(&cat_id) = op_defs.sde_categories.iter().next() {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "operationsManager",
                "process_client_event",
                &[
                    PyRep::String("set_active_category_id".into()),
                    PyRep::Integer(0),
                    PyRep::Dict(vec![(PyRep::String("category_id".into()), PyRep::Integer(cat_id))]),
                ],
                &kw,
            )
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
            &[PyRep::String("add_mission_avoidance_system".into()), PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap(),
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
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let npe_err = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("complete_air_npe".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert!(npe_err.code.eq_ignore_ascii_case("insufficientrole"), "got {}", npe_err.code);

    // survey RPCs
    let active_survey = handle(&ctx, &mut sess, "survey", "GetActiveSurveyID", &[], &kw).unwrap().unwrap();
    assert_eq!(active_survey, PyRep::None);
    let _ = handle(&ctx, &mut sess, "survey", "PerformSurveyChecks", &[], &kw).unwrap();
    let _ = handle(&ctx, &mut sess, "survey", "ClaimSurveyRewards", &[], &kw).unwrap();
}
