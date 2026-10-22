//! Comprehensive integration tests for Project Discovery (`ProjectDiscovery`)
//! and Season Manager (`seasonManager`) services.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_project_discovery_state_and_tasks() {
    // parity: rpc:ProjectDiscovery.fix_task_id, rpc:ProjectDiscovery.get_current_task_index
    // parity: rpc:ProjectDiscovery.get_data_from_url, rpc:ProjectDiscovery.get_new_task
    // parity: rpc:ProjectDiscovery.get_player_state, rpc:ProjectDiscovery.get_player_statistics
    // parity: rpc:ProjectDiscovery.get_project_id, rpc:ProjectDiscovery.get_remaining_bonus_samples
    // parity: rpc:ProjectDiscovery.get_total_needed_xp, rpc:ProjectDiscovery.new_training_task
    // parity: rpc:ProjectDiscovery.post_classification
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.fix_task_id",
        "rpc:ProjectDiscovery.get_current_task_index",
        "rpc:ProjectDiscovery.get_data_from_url",
        "rpc:ProjectDiscovery.get_new_task",
        "rpc:ProjectDiscovery.get_player_state",
        "rpc:ProjectDiscovery.get_player_statistics",
        "rpc:ProjectDiscovery.get_project_id",
        "rpc:ProjectDiscovery.get_remaining_bonus_samples",
        "rpc:ProjectDiscovery.get_total_needed_xp",
        "rpc:ProjectDiscovery.new_training_task",
        "rpc:ProjectDiscovery.post_classification",
    ]);

    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 61_001, "demo", "demo-secret", P1).await;

    // 1. get_project_id
    let proj = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_project_id", &[])
        .await
        .expect("get_project_id");
    assert_eq!(proj, PyRep::String("covid".into()));

    // 2. get_player_state
    let state = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_state", &[])
        .await
        .expect("get_player_state");
    let PyRep::KeyVal(skv) = state else {
        panic!("expected KeyVal from get_player_state");
    };
    assert_eq!(skv.get("rank").and_then(|v| v.as_i64()), Some(1));
    assert!(skv.get("score").is_some());
    assert!(skv.get("accuracy").is_some());

    // 3. get_player_statistics
    let stats = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics");
    let PyRep::KeyVal(tkv) = stats else {
        panic!("expected KeyVal from get_player_statistics");
    };
    assert_eq!(tkv.get("tasks_submitted").and_then(|v| v.as_i64()), Some(0));

    // 4. get_total_needed_xp
    let needed = rpc(
        &ctx,
        &mut p1,
        "ProjectDiscovery",
        "get_total_needed_xp",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("get_total_needed_xp");
    assert_eq!(needed, PyRep::Real(1500.0));

    // 5. get_new_task
    let task = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let PyRep::KeyVal(nkv) = task else {
        panic!("expected KeyVal from get_new_task");
    };
    assert_eq!(
        nkv.get("task_id").and_then(|v| v.as_str()),
        Some("task_1")
    );
    assert_eq!(nkv.get("task_number").and_then(|v| v.as_i64()), Some(1));

    // 6. get_current_task_index
    let idx = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_current_task_index", &[])
        .await
        .expect("get_current_task_index");
    assert_eq!(idx, PyRep::Integer(1));

    // 7. get_data_from_url
    let data = rpc(
        &ctx,
        &mut p1,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/1".into())],
    )
    .await
    .expect("get_data_from_url");
    assert_eq!(data, PyRep::String("data_payload".into()));

    // 8. fix_task_id
    let fixed = rpc(
        &ctx,
        &mut p1,
        "ProjectDiscovery",
        "fix_task_id",
        &[PyRep::String("custom_99".into())],
    )
    .await
    .expect("fix_task_id");
    assert_eq!(fixed, PyRep::String("custom_99".into()));

    // 9. new_training_task
    let train = rpc(&ctx, &mut p1, "ProjectDiscovery", "new_training_task", &[])
        .await
        .expect("new_training_task");
    let PyRep::KeyVal(tr_kv) = train else {
        panic!("expected KeyVal from new_training_task");
    };
    assert_eq!(
        tr_kv.get("task_id").and_then(|v| v.as_str()),
        Some("training_task_01")
    );

    // 10. get_remaining_bonus_samples
    let rem = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
        .await
        .expect("get_remaining_bonus_samples");
    assert_eq!(rem, PyRep::Integer(5));

    // 11. post_classification
    let post = rpc(
        &ctx,
        &mut p1,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.23)])],
    )
    .await
    .expect("post_classification");
    let PyRep::KeyVal(pkv) = post else {
        panic!("expected KeyVal from post_classification");
    };
    assert_eq!(pkv.get("experience_reward").and_then(|v| v.as_f64()), Some(150.0));
    assert_eq!(pkv.get("kredits_reward").and_then(|v| v.as_i64()), Some(25));

    // Verify task submission reflected in statistics
    let stats_after = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics after");
    let PyRep::KeyVal(after_kv) = stats_after else {
        panic!("expected KeyVal");
    };
    assert_eq!(after_kv.get("tasks_submitted").and_then(|v| v.as_i64()), Some(1));
}

#[tokio::test]
async fn test_project_discovery_tutorial_and_rewards() {
    // parity: rpc:ProjectDiscovery.get_is_player_entitled_to_tutorial_reward
    // parity: rpc:ProjectDiscovery.get_maximum_bonus_samples
    // parity: rpc:ProjectDiscovery.get_player_analysis_kredits
    // parity: rpc:ProjectDiscovery.get_tutorial_completion_status
    // parity: rpc:ProjectDiscovery.get_tutorial_level
    // parity: rpc:ProjectDiscovery.get_tutorial_xp_reward
    // parity: rpc:ProjectDiscovery.give_tutorial_rewards
    // parity: rpc:ProjectDiscovery.increase_tutorial_level
    // parity: rpc:ProjectDiscovery.reset_tutorial
    // parity: rpc:ProjectDiscovery.skip_tutorial
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_is_player_entitled_to_tutorial_reward",
        "rpc:ProjectDiscovery.get_maximum_bonus_samples",
        "rpc:ProjectDiscovery.get_player_analysis_kredits",
        "rpc:ProjectDiscovery.get_tutorial_completion_status",
        "rpc:ProjectDiscovery.get_tutorial_level",
        "rpc:ProjectDiscovery.get_tutorial_xp_reward",
        "rpc:ProjectDiscovery.give_tutorial_rewards",
        "rpc:ProjectDiscovery.increase_tutorial_level",
        "rpc:ProjectDiscovery.reset_tutorial",
        "rpc:ProjectDiscovery.skip_tutorial",
    ]);

    let ctx = ServerContext::new();
    let mut p2 = select(&ctx, 61_002, "other", "other-secret", P2).await;

    // 1. get_maximum_bonus_samples
    let max_samples = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_maximum_bonus_samples", &[])
        .await
        .expect("get_maximum_bonus_samples");
    assert_eq!(max_samples, PyRep::Integer(10));

    // 2. get_player_analysis_kredits
    let ak = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits");
    assert_eq!(ak, PyRep::Integer(250));

    // 3. get_tutorial_completion_status
    let complete = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[])
        .await
        .expect("get_tutorial_completion_status");
    assert_eq!(complete, PyRep::Bool(false));

    // 4. get_is_player_entitled_to_tutorial_reward
    let entitled = rpc(
        &ctx,
        &mut p2,
        "ProjectDiscovery",
        "get_is_player_entitled_to_tutorial_reward",
        &[],
    )
    .await
    .expect("get_is_player_entitled_to_tutorial_reward");
    assert_eq!(entitled, PyRep::Bool(true));

    // 5. get_tutorial_level
    let lvl = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_level", &[])
        .await
        .expect("get_tutorial_level");
    assert_eq!(lvl, PyRep::Integer(1));

    // 6. increase_tutorial_level
    let inc = rpc(&ctx, &mut p2, "ProjectDiscovery", "increase_tutorial_level", &[])
        .await
        .expect("increase_tutorial_level");
    assert_eq!(inc, PyRep::Integer(2));

    // 7. get_tutorial_xp_reward
    let xp_rew = rpc(
        &ctx,
        &mut p2,
        "ProjectDiscovery",
        "get_tutorial_xp_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_tutorial_xp_reward");
    assert_eq!(xp_rew, PyRep::Real(500.0));

    // 8. reset_tutorial
    let reset = rpc(&ctx, &mut p2, "ProjectDiscovery", "reset_tutorial", &[])
        .await
        .expect("reset_tutorial");
    assert_eq!(reset, PyRep::Bool(true));
    let lvl_after_reset = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_level", &[])
        .await
        .expect("get_tutorial_level after reset");
    assert_eq!(lvl_after_reset, PyRep::Integer(1));

    // 9. skip_tutorial
    let skipped = rpc(&ctx, &mut p2, "ProjectDiscovery", "skip_tutorial", &[])
        .await
        .expect("skip_tutorial");
    assert_eq!(skipped, PyRep::Bool(true));
    let comp_after_skip = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[])
        .await
        .expect("get_tutorial_completion_status after skip");
    assert_eq!(comp_after_skip, PyRep::Bool(true));

    // 10. give_tutorial_rewards
    let rew = rpc(&ctx, &mut p2, "ProjectDiscovery", "give_tutorial_rewards", &[])
        .await
        .expect("give_tutorial_rewards");
    let PyRep::KeyVal(rkv) = rew else {
        panic!("expected KeyVal from give_tutorial_rewards");
    };
    assert_eq!(rkv.get("analysis_kredits").and_then(|v| v.as_i64()), Some(100));

    // Analysis kredits should now reflect reward (250 + 100 = 350)
    let ak_after = rpc(&ctx, &mut p2, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits after");
    assert_eq!(ak_after, PyRep::Integer(350));
}

#[tokio::test]
async fn test_season_manager_rpc_suite() {
    // parity: rpc:seasonManager.challenge_is_dormant
    // parity: rpc:seasonManager.claim_challenge_rewards
    // parity: rpc:seasonManager.fill_up_and_return_challenges_for_character
    // parity: rpc:seasonManager.get_challenge
    // parity: rpc:seasonManager.get_challenge_expiration_date
    // parity: rpc:seasonManager.get_loot_table_by_typeID
    // parity: rpc:seasonManager.get_max_active_challenges
    // parity: rpc:seasonManager.get_season_data_for_character
    // parity: rpc:seasonManager.get_season_end_time
    // parity: rpc:seasonManager.process_client_event
    // parity: rpc:seasonManager.reload_fsd_data
    // parity: rpc:seasonManager.select_season
    eve_testkit::attest(&[
        "rpc:seasonManager.challenge_is_dormant",
        "rpc:seasonManager.claim_challenge_rewards",
        "rpc:seasonManager.fill_up_and_return_challenges_for_character",
        "rpc:seasonManager.get_challenge",
        "rpc:seasonManager.get_challenge_expiration_date",
        "rpc:seasonManager.get_loot_table_by_typeID",
        "rpc:seasonManager.get_max_active_challenges",
        "rpc:seasonManager.get_season_data_for_character",
        "rpc:seasonManager.get_season_end_time",
        "rpc:seasonManager.process_client_event",
        "rpc:seasonManager.reload_fsd_data",
        "rpc:seasonManager.select_season",
    ]);

    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 61_003, "demo", "demo-secret", P1).await;

    // 1. get_max_active_challenges
    let max_ch = rpc(&ctx, &mut p1, "seasonManager", "get_max_active_challenges", &[])
        .await
        .expect("get_max_active_challenges");
    assert_eq!(max_ch, PyRep::Integer(4));

    // 2. get_season_end_time
    let end_time = rpc(&ctx, &mut p1, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time");
    let PyRep::Integer(end_ft) = end_time else {
        panic!("expected Integer filetime ticks for get_season_end_time");
    };
    assert!(end_ft > 0);

    // 3. select_season
    let sel = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "select_season",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("select_season");
    assert_eq!(sel, PyRep::Bool(true));

    // 4. get_season_data_for_character
    let season_data = rpc(&ctx, &mut p1, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character");
    assert!(matches!(season_data, PyRep::None | PyRep::KeyVal(_) | PyRep::Dict(_)));

    // 5. fill_up_and_return_challenges_for_character
    let filled = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    let PyRep::List(ch_list) = filled else {
        panic!("expected List of challenges");
    };
    assert_eq!(ch_list.len(), 4);

    // 6. get_challenge
    let ch = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("get_challenge");
    let PyRep::KeyVal(ch_kv) = ch else {
        panic!("expected KeyVal from get_challenge");
    };
    assert_eq!(ch_kv.get("challengeID").and_then(|v| v.as_i64()), Some(1002));
    assert_eq!(ch_kv.get("completed").and_then(|v| v.as_bool()), Some(true));

    // 7. challenge_is_dormant
    let dormant = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "challenge_is_dormant",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("challenge_is_dormant");
    assert_eq!(dormant, PyRep::Bool(false));

    // 8. get_challenge_expiration_date
    let exp_date = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("get_challenge_expiration_date");
    let PyRep::Integer(exp_ft) = exp_date else {
        panic!("expected Integer expiration date");
    };
    assert!(exp_ft > 0);

    // 9. claim_challenge_rewards
    let claim = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("claim_challenge_rewards");
    let PyRep::KeyVal(cl_kv) = claim else {
        panic!("expected KeyVal from claim_challenge_rewards");
    };
    assert_eq!(cl_kv.get("challengeID").and_then(|v| v.as_i64()), Some(1002));
    assert_eq!(cl_kv.get("claimed").and_then(|v| v.as_bool()), Some(true));
    assert!(cl_kv.get("seasonPoints").and_then(|v| v.as_i64()).unwrap_or(0) >= 50);

    // 10. get_loot_table_by_typeID
    let loot = rpc(
        &ctx,
        &mut p1,
        "seasonManager",
        "get_loot_table_by_typeID",
        &[PyRep::Integer(34)],
    )
    .await
    .expect("get_loot_table_by_typeID");
    let PyRep::List(loot_items) = loot else {
        panic!("expected List of loot items");
    };
    assert!(!loot_items.is_empty());

    // 11. reload_fsd_data
    let reload = rpc(&ctx, &mut p1, "seasonManager", "reload_fsd_data", &[])
        .await
        .expect("reload_fsd_data");
    assert_eq!(reload, PyRep::Bool(true));

    // 12. process_client_event
    let event_res = rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[])
        .await
        .expect("process_client_event");
    let PyRep::KeyVal(ekv) = event_res else {
        panic!("expected KeyVal from process_client_event");
    };
    assert_eq!(ekv.get("nodeGraphID").and_then(|v| v.as_i64()), Some(473));
    assert_eq!(ekv.get("currentNodeID").and_then(|v| v.as_i64()), Some(2));
}
