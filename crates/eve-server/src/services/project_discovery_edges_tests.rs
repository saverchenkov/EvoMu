//! Edge-case tests for `project_discovery`: bounded task caches, classification payload
//! validation across all PyRep/JSON shapes, tutorial rank-up crate rewards, URL resolution,
//! GM `fix_task_id` validation, and `/pd` slash command errors.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[test]
fn project_discovery_payload_validation_bounded_tasks_and_config_edges() {
    // 1. json_has_non_empty_classification_content & has_valid_classification_payload across all variants
    assert!(!has_valid_classification_payload(&PyRep::None));
    assert!(!has_valid_classification_payload(&PyRep::Bool(false)));
    assert!(!has_valid_classification_payload(&PyRep::Bool(true)));
    assert!(!has_valid_classification_payload(&PyRep::Integer(1)));
    assert!(!has_valid_classification_payload(&PyRep::Real(1.5)));
    assert!(!has_valid_classification_payload(&PyRep::String("  ".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("[]".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("{}".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("NULL".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("none".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("{\"polygons\":[]}".into())));
    assert!(!has_valid_classification_payload(&PyRep::String("{\"gates\":[null,false]}".into())));
    assert!(has_valid_classification_payload(&PyRep::String("{\"polygons\":[[1,2]]}".into())));
    assert!(has_valid_classification_payload(&PyRep::String("[1, 2]".into())));
    assert!(has_valid_classification_payload(&PyRep::String("{\"other\":true}".into())));
    assert!(has_valid_classification_payload(&PyRep::String("{\"other\":42}".into())));
    assert!(has_valid_classification_payload(&PyRep::String("plain_gate".into())));
    assert!(!has_valid_classification_payload(&PyRep::Bytes(vec![])));
    assert!(has_valid_classification_payload(&PyRep::Bytes(vec![1])));
    assert!(!has_valid_classification_payload(&PyRep::Buffer(vec![])));
    assert!(has_valid_classification_payload(&PyRep::Buffer(vec![2])));
    assert!(!has_valid_classification_payload(&PyRep::Dict(vec![])));
    assert!(!has_valid_classification_payload(&PyRep::Dict(vec![(
        PyRep::String("polygons".into()),
        PyRep::List(vec![]),
    )])));
    assert!(has_valid_classification_payload(&PyRep::Dict(vec![(
        PyRep::String("gates".into()),
        PyRep::Tuple(vec![PyRep::Integer(1)]),
    )])));
    assert!(!has_valid_classification_payload(&PyRep::KeyVal(HashMap::new())));
    assert!(!has_valid_classification_payload(&PyRep::KeyVal(HashMap::from([(
        "gates".into(),
        PyRep::Integer(1),
    )]))));
    assert!(has_valid_classification_payload(&PyRep::KeyVal(HashMap::from([(
        "score".into(),
        PyRep::Real(0.9),
    )]))));
    assert!(has_valid_classification_payload(&PyRep::Object {
        type_name: "cls".into(),
        args: Box::new(PyRep::List(vec![PyRep::Integer(1)])),
    }));
    assert!(!has_valid_classification_payload(&PyRep::Rowset {
        columns: vec!["id".into()],
        rows: vec![],
    }));
    assert!(has_valid_classification_payload(&PyRep::Rowset {
        columns: vec!["id".into()],
        rows: vec![vec![PyRep::Integer(1)]],
    }));

    // 2. insert_bounded_char_task evicts oldest beyond 64 per-character and global entries
    let mut store = ProjectDiscoveryStore::default();
    for i in 1..=70 {
        let task = generate_authentic_task_data(i, ACTIVE_PROJECT_ID, 5);
        insert_bounded_char_task(&mut store, 90_000_001, i, task);
    }
    assert_eq!(store.char_tasks.len(), 64);
    assert_eq!(store.tasks.len(), 64);
    assert!(!store.tasks.contains_key(&1));
    assert!(store.tasks.contains_key(&70));

    // 3. is_project_discovery_enabled across Integer and unknown fallback
    let ctx = test_ctx();
    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), PyRep::Integer(0));
    }
    assert!(!is_project_discovery_enabled(&ctx));
    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), PyRep::Integer(1));
    }
    assert!(is_project_discovery_enabled(&ctx));
    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), PyRep::None);
    }
    assert_eq!(
        is_project_discovery_enabled(&ctx),
        ctx.feature_flags_snapshot().enable_project_discovery
    );
}

#[test]
fn project_discovery_tutorial_crates_url_classification_and_slash_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    let mut gm = sess.clone();
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    let kw = HashMap::new();

    // Unauthenticated session fails
    let mut unauth = SessionState::default();
    assert!(handle_project_discovery(&ctx, &mut unauth, "ProjectDiscovery", "get_player_state", &[], &kw)
        .unwrap()
        .is_err());

    // Configure huge tutorial XP so give_tutorial_rewards crosses NEEDED_LEVEL_FOR_SUPERIOR_CRATES (25)
    assert!(handle_project_discovery_slash(&ctx, "/pd config 10 5000 10 5 10").unwrap().is_ok());
    assert!(handle_project_discovery_slash(&ctx, "/pd consensus 0 true").unwrap().is_err());
    assert!(handle_project_discovery_slash(&ctx, "/pd bonus 0").unwrap().is_err());

    for _ in 0..4 {
        let _ = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[], &kw)
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_complete_experience", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    // Verify superior crate was spawned in DB items
    let has_superior = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.owner_id == char_id && it.type_id == SUPERIOR_LOOT_CRATE_TYPE_ID)
    });
    assert!(has_superior);

    // new_training_task without arg falls back to tutorial_level (5)
    let tr_default = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(tr_default.dict_get("task_number").and_then(|v| v.as_i64()), Some(5));

    // get_tutorial_xp_reward returns configured 5000.0
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_xp_reward", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Real(5000.0)
    );

    // fix_task_id permission & validation checks
    assert!(handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "fix_task_id", &[PyRep::Integer(5)], &kw)
        .unwrap()
        .is_err());
    for bad in [PyRep::Integer(0), PyRep::Integer(-2), PyRep::String("not_num".into()), PyRep::None] {
        let err = handle_project_discovery(&ctx, &mut gm, "ProjectDiscovery", "fix_task_id", &[bad], &kw)
            .unwrap()
            .unwrap_err();
        assert!(err.code.eq_ignore_ascii_case("invalidtaskid"), "got {}", err.code);
    }
    assert!(handle_project_discovery(&ctx, &mut gm, "ProjectDiscovery", "fix_task_id", &[PyRep::Integer(7)], &kw)
        .unwrap()
        .is_ok());

    // get_data_from_url edge cases: invalid segment, TaskNotFound, auto-generated for current_task_index,
    // lookup by sample_uuid, and payload_json: None vs invalid JSON
    assert!(handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/bad_seg".into())],
        &kw,
    )
    .unwrap()
    .is_err());

    assert!(handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/task_999".into())],
        &kw,
    )
    .unwrap()
    .is_err());

    // Remove char_task for task 7 and let get_data_from_url auto-create for current_task_index == 7
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_discovery_store(snap);
        store.char_tasks.remove(&char_task_key(char_id, 7));
        store.tasks.insert(
            8,
            ProjectDiscoveryTaskData {
                task_id: "task_8".into(),
                task_number: 8,
                project_id: ACTIVE_PROJECT_ID.into(),
                url: "https://projectdiscovery.eveonline.com/tasks/task_8".into(),
                samples: 3,
                fetch_count: 0,
                task_type: None,
                payload_json: None,
            },
        );
        save_discovery_store(snap, &store);
    });
    let fetched_7 = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/task_7".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(fetched_7.as_str().unwrap().contains("\"task_number\":7"));

    // get_new_task reuses pre-seeded store.tasks[8] with payload_json: None
    let t8 = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(t8.dict_get("task_number").and_then(|v| v.as_i64()), Some(8));
    let uuid8 = ops::sample_uuid_for_index(8);
    let fetched_8 = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(format!("https://projectdiscovery.eveonline.com/samples/{uuid8}"))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(fetched_8.as_str().unwrap().contains("\"task_id\":\"task_8\""));

    // Corrupt payload_json still returns valid JSON with metadata injected
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_discovery_store(snap);
        if let Some(e) = store.char_tasks.get_mut(&char_task_key(char_id, 8)) {
            e.payload_json = Some("{corrupt".into());
        }
        save_discovery_store(snap, &store);
    });
    let fetched_8_corrupt = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/task_8".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(fetched_8_corrupt.as_str().unwrap().contains("\"fetch_count\":2"));

    // post_classification invalid task_id format & level-up notification branch
    let sub_err = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "submit",
        &[PyRep::String("not_a_number".into()), PyRep::String("{\"gates\":[[1,2]]}".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert!(sub_err.code.eq_ignore_ascii_case("invalidtaskid"), "got {}", sub_err.code);
    let sub8 = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "submit_classification",
        &[PyRep::Integer(8), PyRep::String("{\"gates\":[[1,2]]}".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(sub8.dict_get("XP_Reward").is_some());

    // Direct helper assertions for spend_analysis_kredits_in_snap, with_player_state, try_with_player_state
    let ak_char = 90_777_001_i64;
    super::with_player_state(&ctx, ak_char, |st, _snap| {
        st.analysis_kredits = 50;
    });
    assert_eq!(
        super::try_with_player_state(&ctx, ak_char, |st, snap| {
            assert_eq!(
                super::spend_analysis_kredits_in_snap(snap, ak_char, 0)
                    .unwrap_err()
                    .code,
                "InvalidAmount"
            );
            assert_eq!(
                super::spend_analysis_kredits_in_snap(snap, ak_char, -5)
                    .unwrap_err()
                    .code,
                "InvalidAmount"
            );
            assert_eq!(
                super::spend_analysis_kredits_in_snap(snap, ak_char, 100)
                    .unwrap_err()
                    .code,
                "InsufficientAnalysisKredits"
            );
            let rem = super::spend_analysis_kredits_in_snap(snap, ak_char, 30)?;
            st.analysis_kredits = rem;
            st.rank = 9;
            Ok(rem)
        })
        .unwrap(),
        20
    );
    ctx.db.with_snapshot(|snap| {
        let persisted = snap
            .seasons_and_campaigns
            .project_discovery_players
            .get(&ak_char)
            .unwrap();
        assert_eq!(persisted.analysis_kredits, 20);
        assert_eq!(persisted.rank, 9);
    });
}
