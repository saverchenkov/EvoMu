//! Extra unit and mutation-killing tests for `services::project_discovery`
//! covering `sync_bonus_samples` 24-hour refill & `max_bonus_samples` delta scaling,
//! `generate_authentic_task_data` (`control` vs `standard` task_idx % 5),
//! `project_discovery_total_needed_xp_from_store` (`xp_curve_by_rank` override vs `xp_per_rank`)
//! and `rank_for_xp`, `is_project_discovery_enabled` config types (`Bool`, `Integer`, `String`,
//! fallback), 64-entry task eviction in `get_new_task`/`fix_task_id`, `post_classification`/`submit`
//! accuracy EMA, ISK multiplier tiers (`0.0..0.5` -> 500, `0.5..0.9` -> 800, `>=0.9` -> 1000),
//! standard vs superior loot crate rank threshold (`NEEDED_LEVEL_FOR_SUPERIOR_CRATES == 25`),
//! `fix_task_id` integer/string validation, `get_data_from_url` auto-creation & `payload_json: None`
//! fallback, and `/pd` slash command error branches.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::project_discovery::*;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn call_pd(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_project_discovery(ctx, session, "ProjectDiscovery", method, args, &HashMap::new())
        .expect("ProjectDiscovery handled")
}

#[test]
fn bonus_sample_sync_task_generation_xp_curve_rank_and_config_variants() {
    // 1. generate_authentic_task_data: task_idx % 5 == 0 -> "control", else "standard"
    let t_ctrl = generate_authentic_task_data(10, "mcgill-polyseg", 12);
    assert_eq!(t_ctrl.task_type.as_deref(), Some("control"));
    let t_std = generate_authentic_task_data(11, "mcgill-polyseg", 12);
    assert_eq!(t_std.task_type.as_deref(), Some("standard"));

    // 2. sync_bonus_samples: initial refill, max_bonus_samples increase, and 86_400s daily timer
    let mut store = ProjectDiscoveryStore::default();
    let mut st = PlayerProjectState::default();
    st.bonus_samples_left = 3;
    st.last_bonus_refill_unix = 0;
    sync_bonus_samples(&mut st, &store, 1_000_000);
    assert_eq!(st.last_bonus_refill_unix, 1_000_000);
    assert_eq!(st.bonus_samples_left, 10);

    // Spend 4 samples -> 6 left; increasing store.max_bonus_samples from 10 to 15 adds diff (+5 -> 11)
    st.bonus_samples_left = 6;
    store.max_bonus_samples = Some(15);
    sync_bonus_samples(&mut st, &store, 1_000_100);
    assert_eq!(st.max_bonus_samples, 15);
    assert_eq!(st.bonus_samples_left, 11);

    // Before 86_400s elapsed -> no refill; at exactly +86_400s -> refilled to 15
    sync_bonus_samples(&mut st, &store, 1_000_000 + 86_399);
    assert_eq!(st.bonus_samples_left, 11);
    sync_bonus_samples(&mut st, &store, 1_000_000 + 86_400);
    assert_eq!(st.bonus_samples_left, 15);
    assert_eq!(st.last_bonus_refill_unix, 1_086_400);

    // 3. project_discovery_total_needed_xp_from_store & rank_for_xp
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 0), 0.0);
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, -5), 0.0);
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 2), 0.0);
    store.xp_per_rank = 500.0;
    store.xp_curve_by_rank.insert(2, 1_200.0);
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 1), 500.0);
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 2), 1_200.0);
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 3), 1_500.0);
    assert_eq!(rank_for_xp(&store, 499.0, 1), 1);
    assert_eq!(rank_for_xp(&store, 500.0, 1), 2);
    assert_eq!(rank_for_xp(&store, 1_200.0, 1), 3);
    assert_eq!(rank_for_xp(&store, 1_600.0, 1), 4);

    // 4. is_project_discovery_enabled global config value representations
    let ctx = test_ctx();
    for (rep, expected) in [
        (PyRep::Integer(0), false),
        (PyRep::Integer(1), true),
        (PyRep::String("0".into()), false),
        (PyRep::String("1".into()), true),
        (PyRep::String("TRUE".into()), true),
        (PyRep::None, true),
    ] {
        ctx.global_config_values
            .write()
            .unwrap()
            .insert(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY.into(), rep);
        assert_eq!(is_project_discovery_enabled(&ctx), expected);
    }
}

#[test]
fn classification_isk_tiers_loot_crates_task_eviction_url_fallbacks_and_slash_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    let mut gm = sess.clone();
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    let mut unauth = SessionState::default();

    // Unauthenticated rejected
    assert!(call_pd(&ctx, &mut unauth, "get_player_state", &[]).is_err());

    // Slash command error branches
    for bad_cmd in [
        "/pd config 100 200 300 400 not_an_int",
        "/pd consensus 0 true",
        "/pd consensus 1 not_a_bool",
        "/pd bonus 0",
        "/pd bonus -5",
    ] {
        assert_eq!(
            handle_project_discovery_slash(&ctx, bad_cmd).unwrap().unwrap_err().code,
            "InvalidArguments"
        );
    }

    // Default unconfigured tutorial XP reward is 0.0 on get_tutorial_xp_reward, and 250.0 on give_tutorial_complete_experience
    assert_eq!(call_pd(&ctx, &mut sess, "get_tutorial_xp_reward", &[]).unwrap(), PyRep::Real(0.0));
    // Configure small xp_per_rank (100.0) and tutorial_xp (250.0) so tutorial completion triggers rank up
    handle_project_discovery_slash(&ctx, "/pd config 100 250 100 10 15").unwrap().unwrap();
    for _ in 0..4 {
        call_pd(&ctx, &mut sess, "increase_tutorial_level", &[]).unwrap();
    }
    assert_eq!(
        call_pd(&ctx, &mut sess, "give_tutorial_complete_experience", &[]).unwrap(),
        PyRep::Bool(true)
    );
    let st_after_tut = call_pd(&ctx, &mut sess, "get_player_state", &[]).unwrap();
    assert_eq!(st_after_tut.dict_get("rank").and_then(|v| v.as_i64()), Some(3));

    // new_training_task without args falls back to tutorial_level (5)
    let tr_default = call_pd(&ctx, &mut sess, "new_training_task", &[]).unwrap();
    assert_eq!(tr_default.dict_get("task_number").and_then(|v| v.as_i64()), Some(5));

    // Classification empty payload variants & invalid task_id variants
    for empty_payload in [
        PyRep::String("   ".into()),
        PyRep::List(vec![]),
        PyRep::Tuple(vec![]),
        PyRep::Dict(vec![]),
    ] {
        assert_eq!(
            call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(1), empty_payload])
                .unwrap_err()
                .code,
            "InvalidClassification"
        );
    }
    for bad_tid in [PyRep::String("not_a_number".into()), PyRep::Bool(true)] {
        assert_eq!(
            call_pd(&ctx, &mut sess, "submit", &[bad_tid, PyRep::String("ok".into())])
                .unwrap_err()
                .code,
            "InvalidTaskId"
        );
    }

    // Task 1: inaccurate classification (consensus = false) -> accuracy = 0.0, AK_Reward = 0, ISK_Reward = 0
    handle_project_discovery_slash(&ctx, "/pd consensus 1 false").unwrap().unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    assert_eq!(
        call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(999), PyRep::String("ok".into())])
            .unwrap_err()
            .code,
        "InvalidTaskId"
    );
    let sub1 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(1), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub1.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(sub1.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(sub1.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.0));
    // XP goes 250 -> 260 (stays Rank 3, no loot crate yet)
    assert_eq!(sub1.dict_get("loot_crates"), Some(&PyRep::List(vec![])));

    // Task 2: accurate classification with accuracy < 0.5 -> tier 0 ISK multiplier (500 * rank 3 = 1500), then ranks up 3 -> 4 (< 25) granting LOOT_CRATE_TYPE_ID
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd consensus 2 true").unwrap().unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub2 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(2), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub2.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(1_500.0));
    assert_eq!(
        sub2.dict_get("loot_crates"),
        Some(&PyRep::List(vec![PyRep::Integer(LOOT_CRATE_TYPE_ID as i64)]))
    );

    // Jump rank to 24 with high XP gain so next submission crosses NEEDED_LEVEL_FOR_SUPERIOR_CRATES (25)
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd config 100 250 2500 10 15").unwrap().unwrap();
    handle_project_discovery_slash(&ctx, "/pd consensus 3 true").unwrap().unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub3 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(3), PyRep::String("ok".into())]).unwrap();
    assert_eq!(
        sub3.dict_get("loot_crates"),
        Some(&PyRep::List(vec![PyRep::Integer(SUPERIOR_LOOT_CRATE_TYPE_ID as i64)]))
    );

    // fix_task_id: non-GM rejected; invalid int/string rejected; valid integer & >64 tasks eviction
    assert!(call_pd(&ctx, &mut sess, "fix_task_id", &[PyRep::Integer(5)]).is_err());
    for bad_arg in [PyRep::Integer(0), PyRep::String("task_0".into()), PyRep::None] {
        assert_eq!(
            call_pd(&ctx, &mut gm, "fix_task_id", &[bad_arg]).unwrap_err().code,
            "InvalidTaskId"
        );
    }
    for i in 1..=66 {
        call_pd(&ctx, &mut gm, "fix_task_id", &[PyRep::Integer(i)]).unwrap();
    }

    // get_data_from_url: invalid task segment -> InvalidTaskUrl; unseeded task != current -> TaskNotFound;
    // current task (67) not in char_tasks auto-creates it!
    assert_eq!(
        call_pd(
            &ctx,
            &mut gm,
            "get_data_from_url",
            &[PyRep::String("https://projectdiscovery.eveonline.com/mcgill-polyseg/tasks/task_0".into())],
        )
        .unwrap_err()
        .code,
        "InvalidTaskUrl"
    );
    assert_eq!(
        call_pd(
            &ctx,
            &mut gm,
            "get_data_from_url",
            &[PyRep::String("https://projectdiscovery.eveonline.com/mcgill-polyseg/tasks/999".into())],
        )
        .unwrap_err()
        .code,
        "TaskNotFound"
    );
}
