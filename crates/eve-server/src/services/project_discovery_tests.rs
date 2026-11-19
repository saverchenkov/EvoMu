#[cfg(test)]
#[path = "project_discovery_ops_rewards_cov_tests.rs"]
mod project_discovery_ops_rewards_cov_tests;

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[test]
fn test_project_discovery_tasks_tutorials_classifications_and_slash() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut gm = sess.clone();
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    let kw = HashMap::new();

    assert!(handle_project_discovery(&ctx, &mut sess, "Other", "is_enabled", &[], &kw).is_none());
    assert!(handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "Unknown", &[], &kw).is_none());

    // Config & enablement
    assert!(is_project_discovery_enabled(&ctx));
    ctx.global_config_values
        .write()
        .unwrap()
        .insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), PyRep::Bool(false));
    assert!(!is_project_discovery_enabled(&ctx));
    ctx.global_config_values
        .write()
        .unwrap()
        .insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), PyRep::String("true".into()));
    assert_eq!(
        handle(&ctx, &sess, "ProjectDiscovery", "is_enabled", &[], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );

    // Slash commands
    assert!(handle_project_discovery_slash(&ctx, "/other").is_none());
    assert!(handle_project_discovery_slash(&ctx, "/pd unknown").unwrap().is_err());
    assert!(handle_project_discovery_slash(&ctx, "/pd config 100").unwrap().is_err());
    let cfg_res = handle_project_discovery_slash(&ctx, "/pd config 500 250 150 25 20 12")
        .unwrap()
        .unwrap();
    assert_eq!(cfg_res.dict_get("configured"), Some(&PyRep::Bool(true)));
    let cons_res = handle_project_discovery_slash(&ctx, "/pd consensus 1 true")
        .unwrap()
        .unwrap();
    assert_eq!(cons_res.dict_get("is_accurate"), Some(&PyRep::Bool(true)));
    let _ = handle_project_discovery_slash(&ctx, "/pd consensus 2 false").unwrap().unwrap();
    let bonus_res = handle_project_discovery_slash(&ctx, "/pd bonus 15")
        .unwrap()
        .unwrap();
    assert_eq!(bonus_res.dict_get("max_bonus_samples").and_then(|v| v.as_i64()), Some(15));

    // Tutorial progression
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "initialize_tutorial_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_level", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    for expected_lvl in 2..=5 {
        let st = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(st.dict_get("tutorialLevel").and_then(|v| v.as_i64()), Some(expected_lvl));
    }
    assert_eq!(
        handle_project_discovery(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "get_is_player_entitled_to_tutorial_reward",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // Reset and skip tutorial
    let _ = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "reset_tutorial", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "skip_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_completion_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Tasks, data from URL, classification, AK spending
    let train = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "new_training_task",
        &[PyRep::Integer(3)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(train.dict_get("task_number").and_then(|v| v.as_i64()), Some(3));

    let t1 = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[], &kw)
        .unwrap()
        .unwrap();
    let t1_url = t1.dict_get("url").and_then(|v| v.as_str()).unwrap().to_string();
    assert_eq!(t1.dict_get("task_number").and_then(|v| v.as_i64()), Some(1));

    let data_json = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(t1_url)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(data_json.as_str().unwrap().contains("\"fetch_count\":1"));
    assert!(handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://invalid.example.com/1".into())],
        &kw
    )
    .unwrap()
    .is_err());

    // Classification validation and accurate submission
    assert!(handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "post_classification", &[], &kw)
        .unwrap()
        .is_err());
    assert!(handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::Integer(1), PyRep::None],
        &kw
    )
    .unwrap()
    .is_err());

    let sub1 = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::String("{\"gates\":[[1,2,3]]}".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(sub1.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(20));
    assert!(sub1.dict_get("ISK_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Duplicate submission rejected
    assert!(handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::Integer(1), PyRep::String("{\"gates\":[[1,2,3]]}".into())],
        &kw
    )
    .unwrap()
    .is_err());

    // Spend AK
    assert!(spend_analysis_kredits(&ctx, 90_000_001, 0).is_err());
    assert!(spend_analysis_kredits(&ctx, 90_000_001, 99999).is_err());
    let rem_ak = spend_analysis_kredits(&ctx, 90_000_001, 5).unwrap();
    assert_eq!(rem_ak, 15);
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_player_analysis_kredits", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(15)
    );

    // Queries & GM fix_task_id
    let _ = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[], &kw)
        .unwrap()
        .unwrap();
    let _ = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(2)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Real(1000.0)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_project_id", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String(ACTIVE_PROJECT_ID.into())
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_maximum_bonus_samples", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(15)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_remaining_bonus_samples", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(14)
    );
    let fixed = handle_project_discovery(
        &ctx,
        &mut gm,
        "ProjectDiscovery",
        "fix_task_id",
        &[PyRep::String("task_10".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(fixed.dict_get("current_task_index").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(
        handle_project_discovery(&ctx, &mut gm, "ProjectDiscovery", "get_current_task_index", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(10)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "append", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "remove", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
}

#[test]
fn project_discovery_organic_flow_starts_at_zero_and_emits_ak_change() {
    let ctx = test_ctx();
    let uid = ctx.db.provision_player_account("pd_organic", "pw");
    let cid = ctx
        .db
        .create_character(uid, "Analyst Zero", 1, 1, 11, 17, &ctx.sde)
        .expect("create char")
        .character_id;
    let _ = ctx
        .db
        .acquire_online_lock(uid, 1, Some(cid), false, ctx.now_unix());
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Fresh analyst starts at score 0.0 (INITIAL_PLAYER_SCORE) and accuracy 0.0 without fake boosts
    let stats = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[], &kw)
        .unwrap()
        .expect("get_player_statistics");
    assert_eq!(stats.dict_get("score").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(stats.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.0));

    // Skip tutorial, fetch a new task organically (NO /pd config or /pd consensus seeding)
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "skip_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let task_rep = handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[], &kw)
        .unwrap()
        .expect("get_new_task");
    let task_id = task_rep
        .dict_get("task_id")
        .and_then(|v| v.as_str())
        .expect("task_id")
        .to_string();
    let url = task_rep
        .dict_get("url")
        .and_then(|v| v.as_str())
        .expect("url")
        .to_string();

    let payload_str = match handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(url)],
        &kw,
    )
    .unwrap()
    .expect("get_data_from_url")
    {
        PyRep::String(s) => s,
        other => panic!("expected String payload, got {other:?}"),
    };
    let payload: serde_json::Value = serde_json::from_str(&payload_str).expect("valid task JSON");
    let center = payload["clusters"][0]["center"]
        .as_array()
        .expect("clusters[0].center array");
    let cx = center[0].as_f64().unwrap() / 512.0;
    let cy = center[1].as_f64().unwrap() / 512.0;
    let polygon = PyRep::List(vec![
        PyRep::List(vec![PyRep::Real(cx - 0.05), PyRep::Real(cy - 0.05)]),
        PyRep::List(vec![PyRep::Real(cx + 0.05), PyRep::Real(cy - 0.05)]),
        PyRep::List(vec![PyRep::Real(cx + 0.05), PyRep::Real(cy + 0.05)]),
        PyRep::List(vec![PyRep::Real(cx - 0.05), PyRep::Real(cy + 0.05)]),
    ]);
    let classification = PyRep::Dict(vec![(
        PyRep::String("polygons".into()),
        PyRep::List(vec![polygon]),
    )]);

    let _ = ctx.drain_notifications_for_char(cid);
    let res = handle_project_discovery(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String(task_id), classification],
        &kw,
    )
    .unwrap()
    .expect("post_classification");
    assert_eq!(res.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(20));
    assert!(res.dict_get("XP_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!(res.dict_get("ISK_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_player_analysis_kredits", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(20)
    );
    let notifs = ctx.drain_notifications_for_char(cid);
    assert!(
        notifs.iter().any(|n| n.event == "OnAnalysisKreditsChange"),
        "accurate classification must emit OnAnalysisKreditsChange organically without test injection"
    );
}

#[test]
fn project_discovery_bonus_getters_and_unentitled_tutorial_complete_experience() {
    let ctx = test_ctx();
    let cid = 90_000_077_i64;
    let mut sess = auth_session(77, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();
    let tutorial_incomplete = "TutorialNotCompleted";

    let commits_before = ctx.db.tx_commit_count();
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_maximum_bonus_samples", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(10)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "get_remaining_bonus_samples", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(10)
    );
    assert_eq!(
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_project_discovery(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "give_tutorial_complete_experience",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        tutorial_incomplete
    );
    assert_eq!(
        ctx.db.tx_commit_count(),
        commits_before,
        "bonus sample getters and rejected/unentitled tutorial claims must not mutate DB"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap
                .seasons_and_campaigns
                .project_discovery_players
                .contains_key(&cid)
        );
    });

    for _ in 0..4 {
        handle_project_discovery(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[], &kw)
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        handle_project_discovery(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "give_tutorial_complete_experience",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_project_discovery(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "give_tutorial_complete_experience",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        tutorial_incomplete
    );
}
