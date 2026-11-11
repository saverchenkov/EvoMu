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
