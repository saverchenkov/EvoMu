//! Comprehensive integration tests for Project Discovery (`ProjectDiscovery`)
//! and Season Manager (`seasonManager`) services.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    s
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
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
    let proj = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_project_id", &[]).await.expect("get_project_id");
    assert_eq!(proj, PyRep::String("flow_cytometry".into()));

    // 2. get_player_state
    let state = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_state", &[]).await.expect("get_player_state");
    let PyRep::KeyVal(skv) = state else { panic!("expected KeyVal from get_player_state") };
    assert_eq!(skv.get("rank").and_then(|v| v.as_i64()), Some(1));
    assert!(skv.get("score").is_some());
    assert!(skv.get("accuracy").is_some());

    // 3. get_player_statistics
    let stats = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_statistics", &[]).await.expect("get_player_statistics");
    let PyRep::KeyVal(tkv) = stats else { panic!("expected KeyVal from get_player_statistics") };
    assert_eq!(tkv.get("tasks_submitted").and_then(|v| v.as_i64()), Some(0));

    // 4. get_total_needed_xp (SYNTHETIC placeholder curve, not imported data)
    let needed = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(1)]).await.expect("get_total_needed_xp");
    assert_eq!(needed, PyRep::Real(2000.0));
    let needed_r2 = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(2)]).await.expect("get_total_needed_xp r2");
    assert_eq!(needed_r2, PyRep::Real(4000.0));

    // 5. get_new_task
    let task = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task");
    let PyRep::KeyVal(nkv) = task else { panic!("expected KeyVal from get_new_task") };
    assert_eq!(nkv.get("task_id").and_then(|v| v.as_str()), Some("task_1"));
    assert_eq!(nkv.get("task_number").and_then(|v| v.as_i64()), Some(1));

    // 6. get_current_task_index
    let idx = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_current_task_index", &[]).await.expect("get_current_task_index");
    assert_eq!(idx, PyRep::Integer(1));

    // 7. get_data_from_url (rejects empty or unissued task URLs; persists fetch_count in ProjectDiscoveryStore)
    assert!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("".into())]).await.is_err());
    assert!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/9999".into())]).await.is_err());
    let data = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/1".into())]).await.expect("get_data_from_url");
    let PyRep::String(ref data_json) = data else { panic!("expected String from get_data_from_url") };
    let parsed_json: serde_json::Value = serde_json::from_str(data_json).expect("valid json");
    assert_eq!(parsed_json["task_id"], "task_1");
    assert_eq!(parsed_json["project_id"], "flow_cytometry");
    assert_eq!(parsed_json["samples"], 10);
    assert_eq!(parsed_json["fetch_count"], 1);
    let data2 = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/1".into())]).await.expect("get_data_from_url 2");
    let parsed_json2: serde_json::Value = serde_json::from_str(data2.as_str().unwrap()).expect("valid json 2");
    assert_eq!(parsed_json2["fetch_count"], 2);

    // 8. fix_task_id (should fail without GM role)
    let non_gm_res = rpc(&ctx, &mut p1, "ProjectDiscovery", "fix_task_id", &[PyRep::String("custom_99".into())]).await;
    assert!(non_gm_res.is_err());

    // Grant GM role to session
    p1.role |= eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let fixed = rpc(&ctx, &mut p1, "ProjectDiscovery", "fix_task_id", &[PyRep::String("custom_99".into())]).await.expect("fix_task_id");
    assert_eq!(fixed.dict_get("fixed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(fixed.dict_get("task_id").and_then(|v| v.as_str()), Some("custom_99"));
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_current_task_index", &[]).await.unwrap(), PyRep::Integer(99));

    // Reject non-positive or unparseable task ID
    let err_fix = rpc(&ctx, &mut p1, "ProjectDiscovery", "fix_task_id", &[PyRep::String("invalid".into())]).await;
    assert_eq!(err_fix.unwrap_err().code.to_ascii_lowercase(), "invalidtaskid");

    // Reset task index back to 1 for subsequent task_1 tests
    let fb = rpc(&ctx, &mut p1, "ProjectDiscovery", "fix_task_id", &[PyRep::String("task_1".into())])
        .await
        .expect("fix_task_id back");
    let PyRep::KeyVal(fb_kv) = fb else { panic!("expected KeyVal") };
    assert_eq!(fb_kv.get("current_task_index").and_then(|v| v.as_i64()), Some(1));
    let idx_back = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_current_task_index", &[])
        .await
        .expect("get_current_task_index back");
    assert_eq!(idx_back, PyRep::Integer(1));

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
    assert_eq!(rem, PyRep::Integer(10));

    // 11. post_classification negative checks (empty/whitespace string and empty list)
    let err_str = rpc(&ctx, &mut p1, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::String("   ".into())]).await;
    assert_eq!(err_str.unwrap_err().code.to_ascii_lowercase(), "invalidclassification");

    let err_list = rpc(&ctx, &mut p1, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::List(vec![])]).await;
    assert_eq!(err_list.unwrap_err().code.to_ascii_lowercase(), "invalidclassification");

    let state_pre = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_state", &[]).await.expect("get_player_state pre");
    assert_eq!(state_pre.dict_get("experience").and_then(|v| v.as_f64()), Some(0.0));
    let kredits_pre = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("get_player_analysis_kredits pre");
    assert_eq!(kredits_pre, PyRep::Integer(0));

    // Valid post_classification
    let post = rpc(&ctx, &mut p1, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.23)])]).await.expect("post_classification");
    assert_eq!(post.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(150.0));
    assert_eq!(post.dict_get("kredits_reward").and_then(|v| v.as_i64()), Some(50));

    let kredits_post = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits post");
    assert_eq!(kredits_post, PyRep::Integer(50));
    assert!(eve_server::services::project_discovery::spend_analysis_kredits(&ctx, P1, 100).is_err());
    assert_eq!(eve_server::services::project_discovery::spend_analysis_kredits(&ctx, P1, 20).expect("spend AK"), 30);
    let rem_post = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
        .await
        .expect("get_remaining_bonus_samples post");
    assert_eq!(rem_post, PyRep::Integer(9));

    // Verify task submission reflected in statistics
    let stats_after = rpc(&ctx, &mut p1, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics after");
    let PyRep::KeyVal(after_kv) = stats_after else {
        panic!("expected KeyVal");
    };
    assert_eq!(after_kv.get("tasks_submitted").and_then(|v| v.as_i64()), Some(1));
    assert!(after_kv.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) > 100.0);
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

    let db_path = std::env::temp_dir().join(format!("evomu_pd_tut_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let ctx = ServerContext::with_repository(std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite")));
    ctx.db.with_mut_snapshot(|snap| *snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(Clone::clone));

    // Fresh character skip test before any reward claim (R-BF3-01 / R-BF4-02)
    let mut p1 = select(&ctx, 61_001, "demo", "demo-secret", P1).await;
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent initial"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("rew initial"), PyRep::Bool(false));
    let _ = rpc(&ctx, &mut p1, "ProjectDiscovery", "skip_tutorial", &[]).await.expect("skip");
    let inc = rpc(&ctx, &mut p1, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("inc");
    assert_eq!(inc, PyRep::Integer(6));
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("rew"), PyRep::Bool(false));
    let _ = rpc(&ctx, &mut p1, "ProjectDiscovery", "reset_tutorial", &[]).await.expect("reset");
    // Right after skip -> reset (at level 1), player is NOT entitled and cannot claim rewards!
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent rst"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p1, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("rew rst"), PyRep::Bool(false));

    let mut p2 = select(&ctx, 61_002, "other", "other-secret", P2).await;

    // 1. get_maximum_bonus_samples & 2. get_player_analysis_kredits
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_maximum_bonus_samples", &[]).await.expect("get_maximum_bonus_samples"), PyRep::Integer(10));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("get_player_analysis_kredits"), PyRep::Integer(0));

    // 3. get_tutorial_completion_status & 4. get_is_player_entitled_to_tutorial_reward (false at level 1)
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.expect("get_tutorial_completion_status"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent lvl 1"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_level", &[]).await.expect("get_tutorial_level"), PyRep::Integer(1));

    // 6. increase_tutorial_level to level 2 — still incomplete and not entitled to tutorial reward
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("inc lvl 2"), PyRep::Integer(2));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.expect("comp lvl 2"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent lvl 2"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("rew lvl 2"), PyRep::Bool(false));

    // 7. get_tutorial_xp_reward
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_xp_reward", &[PyRep::Integer(2)]).await.expect("get_tutorial_xp_reward"), PyRep::Real(250.0));

    // Complete levels 3, 4, 5 to finish tutorial without skipping
    for expected_lvl in 3..=5 {
        assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("inc to 5"), PyRep::Integer(expected_lvl));
    }
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.expect("comp lvl 5"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent lvl 5"), PyRep::Bool(true));

    // 8. give_tutorial_rewards (claimed after completing level 5)
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards repeat"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("ent after"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("ak after"), PyRep::Integer(50));

    // Verify tutorial_level (5) and analysis_kredits (50) persist across SQLite reload
    drop(ctx);
    let ctx = ServerContext::with_repository(std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite")));
    let mut p2 = select(&ctx, 61_012, "other", "other-secret", P2).await;
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_level", &[]).await.expect("lvl reloaded"), PyRep::Integer(5));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("ak reloaded"), PyRep::Integer(50));

    // 9. reset_tutorial & 10. skip_tutorial
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "reset_tutorial", &[]).await.expect("reset_tutorial"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_level", &[]).await.expect("lvl after reset"), PyRep::Integer(1));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "skip_tutorial", &[]).await.expect("skip_tutorial"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.expect("comp_after_skip"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut p2, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("rew after skip"), PyRep::Bool(false));
    let _ = std::fs::remove_file(&db_path);
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

    // Negative tests: non-client RPCs are not exposed
    let err_claim = rpc(&ctx, &mut p1, "seasonManager", "claim_reward", &[PyRep::Integer(1)]).await;
    assert!(err_claim.is_err());
    let err_comp = rpc(&ctx, &mut p1, "seasonManager", "complete_challenge_for_character_id", &[PyRep::Integer(1003)]).await;
    assert!(err_comp.is_err());

    // 1. get_max_active_challenges (SYNTHETIC default config, overridable via the state blob)
    let max_ch = rpc(&ctx, &mut p1, "seasonManager", "get_max_active_challenges", &[]).await.expect("get_max_active_challenges");
    assert_eq!(max_ch, PyRep::Integer(4));
    ctx.db.with_mut_snapshot(|s| s.service_state_blobs.insert("season_manager:season_configuration".into(), r#"{"season_id":2,"max_active_challenges":6,"max_season_points":1000}"#.into()));
    assert_eq!(rpc(&ctx, &mut p1, "seasonManager", "get_max_active_challenges", &[]).await.unwrap(), PyRep::Integer(6));

    // 2. get_season_end_time
    let end_time = rpc(&ctx, &mut p1, "seasonManager", "get_season_end_time", &[]).await.expect("get_season_end_time");
    let PyRep::Integer(end_ft) = end_time else { panic!("expected Integer filetime ticks") };
    assert!(end_ft > 0);

    // 3. select_season (rejects unknown season ID)
    assert!(rpc(&ctx, &mut p1, "seasonManager", "select_season", &[PyRep::Integer(999)]).await.is_err());
    let sel = rpc(&ctx, &mut p1, "seasonManager", "select_season", &[PyRep::Integer(2)]).await.expect("select_season");
    assert_eq!(sel, PyRep::Bool(true));

    // 4. get_season_data_for_character: seasonservice.pyj `_fetch_season_data` treats a list as the
    //    season-selection list, so a challenge list is wrong; with no seasons FSD data imported the
    //    server reports "no active season" through the client's `season_data is None` path.
    let season_data = rpc(&ctx, &mut p1, "seasonManager", "get_season_data_for_character", &[]).await.expect("get_season_data_for_character");
    assert_eq!(season_data, PyRep::None);

    // 5. fill_up_and_return_challenges_for_character
    let filled = rpc(&ctx, &mut p1, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await.expect("fill_up_and_return_challenges");
    let PyRep::List(ch_list) = filled else { panic!("expected List of challenges") };
    assert_eq!(ch_list.len(), 4);

    // 6. get_challenge (rejects unknown challenge ID)
    assert!(rpc(&ctx, &mut p1, "seasonManager", "get_challenge", &[PyRep::Integer(9999)]).await.is_err());
    let ch = rpc(&ctx, &mut p1, "seasonManager", "get_challenge", &[PyRep::Integer(1002)]).await.expect("get_challenge");
    assert_eq!(ch.dict_get("challengeID").and_then(|v| v.as_i64()), Some(1002));
    assert_eq!(ch.dict_get("completed").and_then(|v| v.as_bool()), Some(false));

    // 7. challenge_is_dormant
    let dormant = rpc(&ctx, &mut p1, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(1002)]).await.expect("challenge_is_dormant");
    assert_eq!(dormant, PyRep::Bool(false));

    // 8. get_challenge_expiration_date
    let exp_date = rpc(&ctx, &mut p1, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(1002)]).await.expect("get_challenge_expiration_date");
    let PyRep::Integer(exp_ft) = exp_date else { panic!("expected Integer expiration date") };
    assert!(exp_ft > 0);

    // 9. claim_challenge_rewards (uncompleted challenge fails)
    let err_uncomp = rpc(&ctx, &mut p1, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1002)]).await;
    assert!(err_uncomp.is_err());

    // BF-SEASON-TRUST-01: Server-observable challenges reject client event progress
    let err_client_1002 = rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[PyRep::Integer(1002), PyRep::Integer(5)]).await;
    assert_eq!(err_client_1002.unwrap_err().code, "InvalidChallengeEvent"); // rpc-lint: allow error code assertion

    // Progress server-driven combat challenge 1002 via NPC killmails
    ctx.db.with_mut_snapshot(|s| {
        for i in 1..=5 {
            s.killmails.insert(900_000 + i, eve_db::KillmailRecord { kill_id: 900_000 + i, victim_character_id: 500_000_100, attacker_character_id: P1, ..Default::default() });
        }
    });

    let claim = rpc(&ctx, &mut p1, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1002)]).await.expect("claim_challenge_rewards");
    assert_eq!(claim.dict_get("challengeID").and_then(|v| v.as_i64()), Some(1002));
    assert_eq!(claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    assert!(claim.dict_get("seasonPoints").and_then(|v| v.as_i64()).unwrap_or(0) >= 50);
    assert_eq!(claim.dict_get("deliveredItemID"), Some(&PyRep::None));

    // Repeat claim fails
    let err_claimed = rpc(&ctx, &mut p1, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1002)]).await;
    assert!(err_claimed.is_err());

    // BF-SEASON-TRUST-01: Mining challenge 1001 rejects client event progress
    let err_client_1001 = rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[PyRep::Integer(1001), PyRep::Integer(1)]).await;
    assert_eq!(err_client_1001.unwrap_err().code, "InvalidChallengeEvent"); // rpc-lint: allow error code assertion

    // Complete server-side mining challenge 1001 and verify docked reward item delivery
    ctx.db.with_mut_snapshot(|s| s.mining_ledger.push(eve_db::MiningLedgerRecord {
        character_id: P1, solar_system_id: 30000142, type_id: 1230, quantity: 1000, date: "2026-10-24".into(), timestamp_unix: ctx.now_unix(),
    }));
    let claim_1001 = rpc(&ctx, &mut p1, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1001)]).await.expect("claim ch 1001");
    let reward_item_id = claim_1001.dict_get("deliveredItemID").and_then(|v| v.as_i64()).expect("deliveredItemID");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&reward_item_id)));

    // BF-SEASON-TRUST-01: Exploration challenge 1003 rejects client event progress; progress via server state
    let err_client_1003 = rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[PyRep::Integer(1003), PyRep::Integer(2)]).await;
    assert_eq!(err_client_1003.unwrap_err().code, "InvalidChallengeEvent"); // rpc-lint: allow error code assertion
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert(format!("hacking_wins:{P1}"), "2".into());
    });

    // 10. get_loot_table_by_typeID
    let loot = rpc(&ctx, &mut p1, "seasonManager", "get_loot_table_by_typeID", &[PyRep::Integer(587)]).await.expect("get_loot_table_by_typeID");
    let PyRep::List(loot_items) = loot else { panic!("expected List of loot items") };
    assert!(!loot_items.is_empty());

    // 11. reload_fsd_data (requires GM role)
    assert!(rpc(&ctx, &mut p1, "seasonManager", "reload_fsd_data", &[]).await.is_err());
    p1.role |= eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let reload = rpc(&ctx, &mut p1, "seasonManager", "reload_fsd_data", &[]).await.expect("reload_fsd_data");
    assert_eq!(reload, PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| s.service_state_blobs.contains_key("season_manager:challenge_definitions")));

    // 12. process_client_event (BF-SEASON-CONST-01, BF-SEASON-PCE-TEST-01: canonical string event and kwargs)
    assert_eq!(rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[]).await.unwrap_err().code, "ChallengeNotFound"); // rpc-lint: allow error code assertion
    ctx.db.with_mut_snapshot(|s| {
        let mut defs = eve_server::services::season_manager::default_seasonal_challenge_definitions();
        defs.push(eve_server::services::season_manager::SeasonalChallengeDef { challenge_id: 2001, season_id: 1, event_type: "client".into(), title: "C".into(), description: "D".into(), points: 10, max_progress: 10, client_event_driven: true, reward_type_id: 0, reward_quantity: 0, is_dormant: false });
        defs.push(eve_server::services::season_manager::SeasonalChallengeDef { challenge_id: 2002, season_id: 1, event_type: "client".into(), title: "D".into(), description: "D".into(), points: 10, max_progress: 10, client_event_driven: true, reward_type_id: 0, reward_quantity: 0, is_dormant: true });
        s.service_state_blobs.insert("season_manager:challenge_definitions".into(), serde_json::to_string(&defs).unwrap());
    });
    assert_eq!(rpc(&ctx, &mut p1, "seasonManager", "process_client_event", &[PyRep::Integer(2001), PyRep::Integer(1)]).await.expect("client-driven progress"), PyRep::None);
    let pce_kw = HashMap::from([("amount".into(), PyRep::Integer(5)), ("extra_data".into(), PyRep::Dict(vec![]))]);
    ctx.dispatch_rpc(&mut p1, "seasonManager", None, "process_client_event", &[PyRep::String("client".into())], &pce_kw, None).await.expect("canonical kwargs pce");
    let ch2001 = rpc(&ctx, &mut p1, "seasonManager", "get_challenge", &[PyRep::Integer(2001)]).await.expect("get_challenge");
    assert_eq!(ch2001.dict_get("progress").and_then(|v| v.as_i64()), Some(6));

    // 13. Character-specific challenge tracking & SQLite persistence across restart
    // BF-SEASON-SEM-01: Seed P2 with prior mining ledger and active industry job before challenge creation
    ctx.db.with_mut_snapshot(|s| {
        s.mining_ledger.push(eve_db::MiningLedgerRecord {
            character_id: P2, solar_system_id: 30000142, type_id: 1230, quantity: 500, date: "2026-10-24".into(), timestamp_unix: ctx.now_unix(),
        });
        s.industry_jobs.insert(88001, eve_db::IndustryJobRecord {
            job_id: 88001, installer_id: P2, facility_id: 60003760, blueprint_item_id: 7001, blueprint_type_id: 681,
            activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds: 3600, start_unix: ctx.now_unix(),
        });
    });
    let mut p2 = select(&ctx, 61_004, "other", "other-secret", P2).await;
    let p2_ch = rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(1002)]).await.expect("p2 get_challenge");
    assert_eq!(p2_ch.dict_get("progress").and_then(|v| v.as_i64()), Some(0), "P2 progress must start at 0 independent of P1");
    assert_eq!(p2_ch.dict_get("completed").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(rpc(&ctx, &mut p2, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(1002)]).await.expect("dormant"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut p2, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(2002)]).await.expect("dormant"), PyRep::Bool(true));
    assert!(matches!(rpc(&ctx, &mut p2, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(1002)]).await, Ok(PyRep::Integer(t)) if t > 0));
    assert!(rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(9999)]).await.is_err());
    assert!(rpc(&ctx, &mut p2, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(9999)]).await.is_err());
    assert!(rpc(&ctx, &mut p2, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(9999)]).await.is_err());
    assert!(rpc(&ctx, &mut p2, "seasonManager", "process_client_event", &[PyRep::Integer(9999), PyRep::Integer(1)]).await.is_err());
    assert!(!ctx.db.with_snapshot(|s| s.service_state_blobs.contains_key("season_state:90000002")));

    // BF-SEASON-SEM-01: Initialize challenges for P2 locking in baseline; active job is not counted
    rpc(&ctx, &mut p2, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await.expect("p2 init");
    let p2_min = rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(1001)]).await.expect("p2 get 1001");
    assert_eq!(p2_min.dict_get("progress").and_then(|v| v.as_i64()), Some(0));
    let p2_ind = rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(1004)]).await.expect("p2 get 1004");
    assert_eq!(p2_ind.dict_get("progress").and_then(|v| v.as_i64()), Some(0));

    // Deliver industry job and add 300 post-baseline ore: progress updates correctly
    ctx.db.with_mut_snapshot(|s| {
        if let Some(j) = s.industry_jobs.get_mut(&88001) { j.status = "Delivered".into(); }
        s.mining_ledger.push(eve_db::MiningLedgerRecord {
            character_id: P2, solar_system_id: 30000142, type_id: 1230, quantity: 300, date: "2026-10-24".into(), timestamp_unix: ctx.now_unix(),
        });
    });
    let p2_ind2 = rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(1004)]).await.expect("p2 get 1004 del");
    assert_eq!(p2_ind2.dict_get("progress").and_then(|v| v.as_i64()), Some(1));
    let p2_min2 = rpc(&ctx, &mut p2, "seasonManager", "get_challenge", &[PyRep::Integer(1001)]).await.expect("p2 get 1001 post");
    assert_eq!(p2_min2.dict_get("progress").and_then(|v| v.as_i64()), Some(300));

    let tmp_db = std::env::temp_dir().join(format!("evomu_seasons_roundtrip_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);
    ctx.db.save_to_sqlite(&tmp_db).expect("save_to_sqlite");
    let ctx_reloaded = ServerContext::from_sqlite(&tmp_db).expect("from_sqlite");
    let _ = std::fs::remove_file(&tmp_db);

    let mut p1_reloaded = select(&ctx_reloaded, 61_091, "demo", "demo-secret", P1).await;
    let ch1003_reloaded = rpc(&ctx_reloaded, &mut p1_reloaded, "seasonManager", "get_challenge", &[PyRep::Integer(1003)]).await.expect("p1 get_challenge 1003");
    assert_eq!(ch1003_reloaded.dict_get("progress").and_then(|v| v.as_i64()), Some(2), "P1 challenge 1003 progress must survive SQLite reload");

    let ch1002_reloaded = rpc(&ctx_reloaded, &mut p1_reloaded, "seasonManager", "get_challenge", &[PyRep::Integer(1002)]).await.expect("p1 get_challenge 1002");
    assert_eq!(ch1002_reloaded.dict_get("claimed").and_then(|v| v.as_bool()), Some(true), "P1 challenge 1002 claimed state must survive SQLite reload");
}

#[tokio::test]
async fn test_pvp_filament_mgr_queue_and_stats() {
    // parity: rpc:pvpFilamentMgr.GetActiveEvents, rpc:pvpFilamentMgr.GetAllEvents
    // parity: rpc:pvpFilamentMgr.GetCharacterStatistics, rpc:pvpFilamentMgr.GetLeaderboard
    // parity: rpc:pvpFilamentMgr.GetMostRecentEvent, rpc:pvpFilamentMgr.GetNextEventDate
    // parity: rpc:pvpFilamentMgr.JoinPVPQueue, rpc:pvpFilamentMgr.LeavePVPQueue
    eve_testkit::attest(&[
        "rpc:pvpFilamentMgr.GetActiveEvents",
        "rpc:pvpFilamentMgr.GetAllEvents",
        "rpc:pvpFilamentMgr.GetCharacterStatistics",
        "rpc:pvpFilamentMgr.GetLeaderboard",
        "rpc:pvpFilamentMgr.GetMostRecentEvent",
        "rpc:pvpFilamentMgr.GetNextEventDate",
        "rpc:pvpFilamentMgr.JoinPVPQueue",
        "rpc:pvpFilamentMgr.LeavePVPQueue",
    ]);

    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 61_004, "demo", "demo-secret", P1).await;
    let mut p2 = select(&ctx, 61_006, "other", "other-secret", P2).await;

    let f1 = 99_001_001;
    let f2 = 99_001_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(f1, eve_db::InventoryItemRecord {
            item_id: f1, type_id: 53965, owner_id: P1, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 2, singleton: false, ..Default::default()
        });
        snap.items.insert(f2, eve_db::InventoryItemRecord {
            item_id: f2, type_id: 53965, owner_id: P2, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: false, ..Default::default()
        });
    });

    // Negative test: activating filament while docked fails
    let err_docked = rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(53965)]).await;
    assert!(matches!(err_docked, Err(ref e) if e.code == "MustBeInSpace"));

    // Undock pilots and move filaments into active ship cargo
    let ship1 = 99_001_101;
    let ship2 = 99_001_102;
    p1.station_id = None; p1.structure_id = None; p1.ship_id = Some(ship1);
    p2.station_id = None; p2.structure_id = None; p2.ship_id = Some(ship2);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) { c.station_id = None; c.structure_id = None; c.active_ship_id = ship1; }
        if let Some(c) = snap.characters.get_mut(&P2) { c.station_id = None; c.structure_id = None; c.active_ship_id = ship2; }
        snap.items.insert(ship1, eve_db::InventoryItemRecord { item_id: ship1, type_id: 606, owner_id: P1, location_id: 30000142, flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(ship2, eve_db::InventoryItemRecord { item_id: ship2, type_id: 606, owner_id: P2, location_id: 30000142, flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        if let Some(it) = snap.items.get_mut(&f1) { it.location_id = ship1; it.flag = eve_db::InventoryFlag::Cargo; }
        if let Some(it) = snap.items.get_mut(&f2) { it.location_id = ship2; it.flag = eve_db::InventoryFlag::Cargo; }
        snap.items.insert(99_001_009, eve_db::InventoryItemRecord {
            item_id: 99_001_009, type_id: 47765, owner_id: P1, location_id: ship1,
            flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, ..Default::default()
        });
    });

    // Reject Abyssal PvE filament (group 1979) on JoinPVPQueue
    let err_abyssal = rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(99_001_009), PyRep::Integer(47765)]).await;
    assert_eq!(err_abyssal.unwrap_err().code, "InvalidFilamentType");

    let res = rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(53965)]).await.expect("JoinPVPQueue");
    assert_eq!(res, PyRep::Integer(30));

    // BF-1 check: resolution fails if only one pilot is queued
    let err_single = eve_server::services::pvp_filament_mgr::record_proving_grounds_combat_result(&ctx, P1, P2, 1500.0);
    assert_eq!(err_single.unwrap_err().code, "NotInPvpQueue");

    // Duplicate join returns AlreadyInPvpQueue
    let res2 = rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(53965)]).await;
    assert!(matches!(res2, Err(ref e) if e.code == "AlreadyInPvpQueue"));

    // LeavePVPQueue with wrong filament item fails
    let err_leave = rpc(&ctx, &mut p1, "pvpFilamentMgr", "LeavePVPQueue", &[PyRep::Integer(999_999)]).await;
    assert!(err_leave.is_err());

    // LeavePVPQueue with valid filament stack ID leaves queue and returns PyRep::None
    let leave_res = rpc(&ctx, &mut p1, "pvpFilamentMgr", "LeavePVPQueue", &[PyRep::Integer(f1)]).await.expect("LeavePVPQueue");
    assert_eq!(leave_res, PyRep::None);
    let is_queued = ctx.db.with_snapshot(|s| eve_server::services::pvp_filament_mgr::load_pvp_store(s).queue.contains_key(&P1));
    assert!(!is_queued);

    // Re-join PVP queue
    let res_rejoin = rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(53965)]).await.expect("Re-join");
    assert_eq!(res_rejoin, PyRep::Integer(30));

    // Event queries inspecting actual fields and active vs historical filtering
    let active = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetActiveEvents", &[]).await.expect("GetActiveEvents");
    let PyRep::Dict(act_dict) = active else { panic!("expected Dict") };
    assert_eq!(act_dict.len(), 1);
    assert_eq!(act_dict[0].1.dict_get("scheduleID").and_then(|v| v.as_i64()), Some(1001));
    assert_eq!(act_dict[0].1.dict_get("name").and_then(|v| v.as_str()), Some("1v1 Cruiser Proving Grounds"));
    assert_eq!(act_dict[0].1.dict_get("typeIDs"), Some(&PyRep::List(vec![PyRep::Integer(53965)])));
    assert!(act_dict[0].1.dict_get("startDate").and_then(|v| v.as_i64()).unwrap() > 100_000_000_000_000_000);
    assert!(act_dict[0].1.dict_get("endDate").and_then(|v| v.as_i64()).unwrap() > 100_000_000_000_000_000);

    let all = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetAllEvents", &[]).await.expect("GetAllEvents");
    let PyRep::Dict(all_dict) = all else { panic!("expected Dict") };
    assert_eq!(all_dict.len(), 3);
    assert!(all_dict[0].1.dict_get("startDate").and_then(|v| v.as_i64()).unwrap() > 100_000_000_000_000_000);

    let recent = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetMostRecentEvent", &[]).await.expect("GetMostRecentEvent");
    assert_eq!(recent.dict_get("scheduleID").and_then(|v| v.as_i64()), Some(1001));

    let next_d = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetNextEventDate", &[]).await.expect("GetNextEventDate");
    let exp_ft = ctx.db.with_snapshot(|s| eve_server::services::pvp_filament_mgr::load_pvp_store(s).events.iter().filter(|e| e.start_unix > ctx.now_unix() as i64).map(|e| e.start_unix).min().unwrap() * 10_000_000 + 116_444_736_000_000_000);
    assert!(exp_ft > 100_000_000_000_000_000 && next_d == PyRep::Integer(exp_ft));

    // Gate activation fails outside arena (p1 is still in origin system 30000142)
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)]).await.unwrap_err().code, "NotInArena");

    // Second pilot joins and forms a 1v1 match instance (dynamic wait time 5s)
    let res_p2 = rpc(&ctx, &mut p2, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f2), PyRep::Integer(53965)]).await.expect("P2 Join");
    assert_eq!(res_p2, PyRep::Integer(5));
    assert_eq!(p2.solar_system_id, Some(32000001));
    ctx.sync_session(&mut p1);
    assert_eq!(p1.solar_system_id, Some(32000001));
    let has_match = ctx.db.with_snapshot(|s| eve_server::services::pvp_filament_mgr::load_pvp_store(s).active_matches.values().any(|m| m.active && m.team_a.contains(&P1) && m.team_b.contains(&P2) && m.arena_solar_system_id == 32000001));
    assert!(has_match);

    // In arena, gate activation fails before match conclusion
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)]).await.unwrap_err().code, "MatchNotConcluded");

    // Resolve via the destruction-pipeline hook (real weapon kill covered in combat_entropic_pvp_atomicity.rs)
    eve_server::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(&ctx, P1, P2, 32000001, 1500.0);

    // After conclusion, gate activation fails when target gate is invalid or beyond 50km
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(99999)]).await.unwrap_err().code, "TooFarFromGate");

    // Both pilots exit through AbyssalPVPEndGateActivation to origin systems
    let gate_res = rpc(&ctx, &mut p1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)]).await.expect("gate p1");
    assert_eq!(gate_res.dict_get("activated").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(p1.solar_system_id, Some(30000142));

    let gate_res_p2 = rpc(&ctx, &mut p2, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)]).await.expect("gate p2");
    assert_eq!(gate_res_p2.dict_get("activated").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(p2.solar_system_id, Some(30000142));

    // Second resolution fails as pilots were dequeued
    let err_again = eve_server::services::pvp_filament_mgr::record_proving_grounds_combat_result(&ctx, P1, P2, 1500.0);
    assert_eq!(err_again.unwrap_err().code, "NotInPvpQueue");

    // Both players are dequeued and match is completed
    let (q1, q2, match_active) = ctx.db.with_snapshot(|s| {
        let store = eve_server::services::pvp_filament_mgr::load_pvp_store(s);
        (store.queue.contains_key(&P1), store.queue.contains_key(&P2), store.active_matches.values().any(|m| m.active))
    });
    assert!(!q1 && !q2 && !match_active);

    // Leaderboard reflects post-match stats sorted descending by score with sequential ranks 1 and 2
    let lb = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetLeaderboard", &[PyRep::Integer(2), PyRep::Integer(0)]).await.expect("GetLeaderboard");
    let PyRep::List(lb_rows) = lb else { panic!("expected List") };
    assert_eq!(lb_rows.len(), 2);
    assert_eq!((lb_rows[0].dict_get("characterID").and_then(|v| v.as_i64()), lb_rows[0].dict_get("rank").and_then(|v| v.as_i64()), lb_rows[0].dict_get("score").and_then(|v| v.as_i64())), (Some(P1), Some(1), Some(100)));
    assert_eq!((lb_rows[1].dict_get("characterID").and_then(|v| v.as_i64()), lb_rows[1].dict_get("rank").and_then(|v| v.as_i64()), lb_rows[1].dict_get("score").and_then(|v| v.as_i64())), (Some(P2), Some(2), Some(25)));

    // Stats reflect match outcome and rank (scoped by eventID and characterID)
    let s1 = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(2), PyRep::Integer(P1)]).await.expect("P1 stats");
    let PyRep::KeyVal(kv1) = s1 else { panic!("expected KeyVal") };
    assert_eq!((kv1.get("rank").and_then(|v| v.as_i64()), kv1.get("wins").and_then(|v| v.as_i64()), kv1.get("score").and_then(|v| v.as_i64()), kv1.get("damageDealt").and_then(|v| v.as_f64())), (Some(1), Some(1), Some(100), Some(1500.0)));

    let s2 = rpc(&ctx, &mut p2, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(2), PyRep::Integer(P2)]).await.expect("P2 stats");
    let PyRep::KeyVal(kv2) = s2 else { panic!("expected KeyVal") };
    assert_eq!((kv2.get("rank").and_then(|v| v.as_i64()), kv2.get("losses").and_then(|v| v.as_i64()), kv2.get("score").and_then(|v| v.as_i64())), (Some(2), Some(1), Some(25)));

    // Advancing time rolls schedule forward so events never permanently expire
    ctx.advance_time_seconds(86400 * 30);
    let next_rolled = rpc(&ctx, &mut p1, "pvpFilamentMgr", "GetNextEventDate", &[]).await.expect("GetNextEventDate rolled");
    assert!(next_rolled.as_i64().unwrap() > 100_000_000_000_000_000);
}

#[tokio::test]
async fn test_employment_history_and_transfers() {
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 61_005, "demo", "demo-secret", P1).await;

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(P1).or_default();
        skills.insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 });
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.balance = eve_db::IskCents::from_isk(5_000_000);
        }
    });

    let corp_res = rpc(&ctx, &mut p1, "corpRegistry", "AddCorporation", &[PyRep::String("Frontier Vanguard Corp".into()), PyRep::String("FVC".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let new_corp_id = corp_res.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| corp_res.as_i64()).expect("expected Integer corp ID");
    assert!(new_corp_id > 0);

    let emp_transfers = rpc(&ctx, &mut p1, "corporationSvc", "GetEmployementRecordAndCharacterTransfers", &[PyRep::Integer(P1)]).await.expect("GetEmployementRecordAndCharacterTransfers");
    let PyRep::Rowset { rows, .. } = emp_transfers else { panic!("expected Rowset") };
    assert_eq!((rows.len(), &rows[0][1], &rows[1][1]), (2, &PyRep::Integer(1_000_170), &PyRep::Integer(new_corp_id)));

    let emp_record = rpc(&ctx, &mut p1, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(P1)]).await.expect("GetEmploymentRecord");
    let PyRep::Rowset { rows: rec_rows, .. } = emp_record else { panic!("expected Rowset") };
    assert_eq!((rec_rows.len(), &rec_rows[0][0], &rec_rows[1][0]), (2, &PyRep::Integer(1_000_170), &PyRep::Integer(new_corp_id)));
}
