//! Live integration tests for `ProjectDiscovery` RPCs using real RPCs,
//! GM `/pd` slash commands, and live server state.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    client_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(client_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_project_discovery_tutorial_xp_and_training_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db
        .provision_gm_account("gm_pd_1", "gm-pass", 90_000_003, "GM Discovery");

    let mut pilot = login_pilot_char(&ctx, 71_001, "demo", "demo-secret", 90_000_001).await;
    let mut gm = login_pilot_char(&ctx, 71_002, "gm_pd_1", "gm-pass", 90_000_003).await;

    // 1. Default maximum bonus samples is 20 for Omega clone_grade
    let max_bonus_init = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_maximum_bonus_samples", &[])
        .await
        .expect("get_maximum_bonus_samples initial");
    assert_eq!(max_bonus_init, PyRep::Integer(20));

    // 2. new_training_task returns real training tasks with solution
    let train1 = rpc(&ctx, &mut pilot, "ProjectDiscovery", "new_training_task", &[])
        .await
        .expect("new_training_task default");
    assert_eq!(train1.dict_get("task_id").and_then(|v| v.as_str()), Some("ed134308-e080-49bc-be56-169a5f839c96"));
    assert_eq!(train1.dict_get("level").and_then(|v| v.as_i64()), Some(1));
    assert!(train1.dict_get("solution").is_some());

    let train3 = rpc(&ctx, &mut pilot, "ProjectDiscovery", "new_training_task", &[PyRep::Integer(3)])
        .await
        .expect("new_training_task 3");
    assert_eq!(train3.dict_get("task_id").and_then(|v| v.as_str()), Some("eb4c6403-e878-43cf-a009-9c8c566ae90f"));
    assert_eq!(train3.dict_get("task_number").and_then(|v| v.as_i64()), Some(3));

    // 3. give_tutorial_rewards is false before tutorial completion
    let early_reward = rpc(&ctx, &mut pilot, "ProjectDiscovery", "give_tutorial_rewards", &[])
        .await
        .expect("give_tutorial_rewards early");
    assert_eq!(early_reward, PyRep::Bool(false));

    // 4. GM configures MMOS XP curve, tutorial XP reward, and max bonus samples via /pd config
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/pd config 500 300 200 50 25 12".into())],
    )
    .await
    .expect("/pd config");

    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_tutorial_xp_reward", &[])
            .await
            .expect("get_tutorial_xp_reward"),
        PyRep::Real(300.0)
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(1)])
            .await
            .expect("get_total_needed_xp 1"),
        PyRep::Real(500.0)
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(2)])
            .await
            .expect("get_total_needed_xp 2"),
        PyRep::Real(1000.0)
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_maximum_bonus_samples", &[])
            .await
            .expect("get_maximum_bonus_samples updated"),
        PyRep::Integer(12)
    );

    // 5. Complete tutorial to level 5 and claim tutorial rewards idempotently
    for _ in 0..4 {
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "increase_tutorial_level", &[])
            .await
            .expect("increase_tutorial_level");
    }
    let claimed = rpc(&ctx, &mut pilot, "ProjectDiscovery", "give_tutorial_rewards", &[])
        .await
        .expect("give_tutorial_rewards claim");
    assert_eq!(claimed, PyRep::Bool(true));

    let state = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_player_state", &[])
        .await
        .expect("get_player_state");
    assert_eq!(state.dict_get("experience").and_then(|v| v.as_f64()), Some(300.0));

    let repeat = rpc(&ctx, &mut pilot, "ProjectDiscovery", "give_tutorial_rewards", &[])
        .await
        .expect("give_tutorial_rewards repeat");
    assert_eq!(repeat, PyRep::Bool(false));
}

#[tokio::test]
async fn test_project_discovery_tasks_url_classification_and_stats_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db
        .provision_gm_account("gm_pd_2", "gm-pass", 90_000_003, "GM Discovery");

    let mut pilot = login_pilot_char(&ctx, 72_001, "demo", "demo-secret", 90_000_001).await;
    let mut gm = login_pilot_char(&ctx, 72_002, "gm_pd_2", "gm-pass", 90_000_003).await;

    // Configure MMOS curve (300 XP/rank) and task consensus via GM slash commands
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/pd config 300 0 200 50 25 10".into())],
    )
    .await
    .expect("/pd config");
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/pd consensus 1 true".into())],
    )
    .await
    .expect("/pd consensus 1");
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/pd consensus 2 false".into())],
    )
    .await
    .expect("/pd consensus 2");
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/pd consensus 3 true".into())],
    )
    .await
    .expect("/pd consensus 3");

    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
            .await
            .expect("get_remaining_bonus_samples initial"),
        PyRep::Integer(10)
    );

    // 1. get_new_task & get_data_from_url
    let t1 = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task 1");
    assert_eq!(t1.dict_get("task_id").and_then(|v| v.as_str()), Some("task_1"));
    let url1 = t1
        .dict_get("url")
        .and_then(|v| v.as_str())
        .expect("task_1 url")
        .to_string();

    let payload_str = match rpc(
        &ctx,
        &mut pilot,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(url1.clone())],
    )
    .await
    .expect("get_data_from_url 1")
    {
        PyRep::String(s) => s,
        other => panic!("expected String payload, got {other:?}"),
    };
    let payload: serde_json::Value = serde_json::from_str(&payload_str).expect("valid JSON");
    assert_eq!(payload["task_id"], "task_1");
    assert_eq!(payload["fetch_count"], 1);
    assert!(payload["experiment"]["markers"].is_array());
    assert!(payload["clusters"].is_array());

    // Second fetch increments fetch_count
    let payload2_str = match rpc(
        &ctx,
        &mut pilot,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(url1)],
    )
    .await
    .expect("get_data_from_url 2")
    {
        PyRep::String(s) => s,
        other => panic!("expected String payload, got {other:?}"),
    };
    let payload2: serde_json::Value = serde_json::from_str(&payload2_str).expect("valid JSON");
    assert_eq!(payload2["fetch_count"], 2);

    // 2. post_classification on accurate task_1
    let post1 = rpc(
        &ctx,
        &mut pilot,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .expect("post_classification 1");
    assert_eq!(post1.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(400.0));
    assert_eq!(post1.dict_get("kredits_reward").and_then(|v| v.as_i64()), Some(25));
    assert_eq!(post1.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(2000.0));
    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
            .await
            .expect("get_remaining_bonus_samples after 1"),
        PyRep::Integer(9)
    );

    // 3. post_classification on inaccurate task_2 (2x bonus XP)
    ctx.advance_secs(5);
    let _t2 = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task 2");
    let post2 = rpc(
        &ctx,
        &mut pilot,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_2".into()), PyRep::List(vec![PyRep::Real(2.0)])],
    )
    .await
    .expect("post_classification 2");
    assert_eq!(post2.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(100.0));
    assert_eq!(post2.dict_get("kredits_reward").and_then(|v| v.as_i64()), Some(0));

    // 4. post_classification on accurate task_3 reaches 900 XP (>= 300/600/900 thresholds -> rank 4)
    ctx.advance_secs(5);
    let _t3 = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task 3");
    let post3 = rpc(
        &ctx,
        &mut pilot,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_3".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .expect("post_classification 3");
    assert_eq!(post3.dict_get("rank").and_then(|v| v.as_i64()), Some(4));

    // 5. get_player_statistics reflects rank, XP, accuracy, score, and tasks_submitted
    let stats = rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics");
    assert_eq!(stats.dict_get("rank").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(stats.dict_get("experience").and_then(|v| v.as_f64()), Some(900.0));
    assert_eq!(stats.dict_get("tasks_submitted").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(stats.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.91));
    assert_eq!(stats.dict_get("score").and_then(|v| v.as_f64()), Some(91.0));
    assert_eq!(
        rpc(&ctx, &mut pilot, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
            .await
            .expect("get_remaining_bonus_samples after 3"),
        PyRep::Integer(7)
    );
}
