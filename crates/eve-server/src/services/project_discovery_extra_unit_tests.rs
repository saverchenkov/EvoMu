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
    assert_eq!(project_discovery_total_needed_xp_from_store(&store, 2), 1_250.0);
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

    // Default unconfigured tutorial XP reward is 500.0 on get_tutorial_xp_reward
    assert_eq!(call_pd(&ctx, &mut sess, "get_tutorial_xp_reward", &[]).unwrap(), PyRep::Real(500.0));
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

    // Task 2: accurate classification with accuracy < 0.5 -> tier 0 ISK multiplier (500 * rank 6 on V24 XP curve = 3000), then ranks up and grants LOOT_CRATE_TYPE_ID
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd consensus 2 true").unwrap().unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub2 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(2), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub2.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(3_000.0));
    assert_eq!(
        sub2.dict_get("loot_crates"),
        Some(&PyRep::List(vec![
            PyRep::Integer(LOOT_CRATE_TYPE_ID as i64),
            PyRep::Integer(LOOT_CRATE_TYPE_ID as i64),
        ]))
    );

    // Jump rank past NEEDED_LEVEL_FOR_SUPERIOR_CRATES (25) granting SUPERIOR_LOOT_CRATE_TYPE_ID
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd config 100 250 2500 10 15").unwrap().unwrap();
    handle_project_discovery_slash(&ctx, "/pd consensus 3 true").unwrap().unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub3 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(3), PyRep::String("ok".into())]).unwrap();
    let crates3 = match sub3.dict_get("loot_crates") {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v,
        other => panic!("expected loot_crates list, got {other:?}"),
    };
    assert_eq!(
        crates3.last().and_then(|v| v.as_i64()),
        Some(SUPERIOR_LOOT_CRATE_TYPE_ID as i64)
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

#[test]
fn project_discovery_ema_accuracy_avoids_binary64_drift() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_888, 98_000_001, 30_000_142);

    // Task 1: accurate -> 1.0
    handle_project_discovery_slash(&ctx, "/pd consensus 1 true")
        .unwrap()
        .unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub1 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(1), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub1.dict_get("accuracy").and_then(|v| v.as_f64()), Some(1.0));

    // Task 2: inaccurate -> 0.9
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd consensus 2 false")
        .unwrap()
        .unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub2 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(2), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub2.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.9));

    // Task 3: accurate -> exact 0.91 and score 91.0 (not 0.9099999999999999)
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    handle_project_discovery_slash(&ctx, "/pd consensus 3 true")
        .unwrap()
        .unwrap();
    call_pd(&ctx, &mut sess, "get_new_task", &[]).unwrap();
    let sub3 = call_pd(&ctx, &mut sess, "submit", &[PyRep::Integer(3), PyRep::String("ok".into())]).unwrap();
    assert_eq!(sub3.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.91));
    assert_eq!(sub3.dict_get("score").and_then(|v| v.as_f64()), Some(91.0));

    let stats = call_pd(&ctx, &mut sess, "get_player_statistics", &[]).unwrap();
    assert_eq!(stats.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.91));
    assert_eq!(stats.dict_get("score").and_then(|v| v.as_f64()), Some(91.0));
}

#[test]
fn ops_crowd_consensus_control_grading_payload_centroids_omega_bonus_and_url_fallbacks_kill_mutants() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let cid_a = 90_100_001_i64;
    let cid_b = 90_100_002_i64;
    let cid_c = 90_100_003_i64;

    // 1. V24 progressive XP curve through rank 1..30 (tiers <=5, <=15, <=25, >25)
    let default_store = ProjectDiscoveryStore::default();
    let xp_r5 = project_discovery_total_needed_xp_from_store(&default_store, 5);
    let xp_r15 = project_discovery_total_needed_xp_from_store(&default_store, 15);
    let xp_r25 = project_discovery_total_needed_xp_from_store(&default_store, 25);
    let xp_r26 = project_discovery_total_needed_xp_from_store(&default_store, 26);
    assert!(xp_r5 > 0.0 && xp_r15 > xp_r5 && xp_r25 > xp_r15);
    assert_eq!(xp_r26 - xp_r25, 10_750.0);

    // 2. sync_bonus_samples shrinking cap clamps bonus_samples_left down
    let mut shrink_store = ProjectDiscoveryStore {
        max_bonus_samples: Some(20),
        ..Default::default()
    };
    let mut shrink_st = PlayerProjectState::default();
    sync_bonus_samples(&mut shrink_st, &shrink_store, 1_000_000);
    assert_eq!(shrink_st.bonus_samples_left, 20);
    shrink_store.max_bonus_samples = Some(5);
    sync_bonus_samples(&mut shrink_st, &shrink_store, 1_000_010);
    assert_eq!(shrink_st.max_bonus_samples, 5);
    assert_eq!(shrink_st.bonus_samples_left, 5);

    // 3. Omega clone gets 20 daily bonus samples vs Alpha 10
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&cid_a) {
            c.clone_grade = 2;
        } else {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = cid_a;
            ch.clone_grade = 2;
            snap.characters.insert(cid_a, ch);
        }
        for id in [cid_b, cid_c] {
            if !snap.characters.contains_key(&id) {
                let mut ch = snap.characters.values().next().unwrap().clone();
                ch.character_id = id;
                ch.clone_grade = 1;
                snap.characters.insert(id, ch);
            }
        }
    });
    let mut sess_a = auth_session(1, cid_a, 98_000_001, 30_000_142);
    let mut sess_b = auth_session(2, cid_b, 98_000_001, 30_000_142);
    let mut sess_c = auth_session(3, cid_c, 98_000_001, 30_000_142);

    assert_eq!(
        call_pd(&ctx, &mut sess_a, "get_maximum_bonus_samples", &[]).unwrap(),
        PyRep::Integer(20)
    );
    assert_eq!(
        call_pd(&ctx, &mut sess_a, "get_remaining_bonus_samples", &[]).unwrap(),
        PyRep::Integer(20)
    );
    assert_eq!(
        call_pd(&ctx, &mut sess_b, "get_maximum_bonus_samples", &[]).unwrap(),
        PyRep::Integer(10)
    );

    // 4. new_training_task both inside (1..=5) and outside (>5, falls back to generated UUID)
    let tr_1 = call_pd(&ctx, &mut sess_a, "new_training_task", &[PyRep::Integer(1)]).unwrap();
    let tr_6 = call_pd(&ctx, &mut sess_a, "new_training_task", &[PyRep::Integer(6)]).unwrap();
    assert_ne!(tr_1.dict_get("task_id"), tr_6.dict_get("task_id"));
    assert!(tr_1.dict_get("solution").is_some());

    // 5. Payload validation edge cases: JSON booleans/empty polygons, PyRep::Bytes, Rowset, Object, Dict polygons
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // task 1
    for invalid_pl in [
        PyRep::Bool(true),
        PyRep::Integer(5),
        PyRep::String("null".into()),
        PyRep::String("NONE".into()),
        PyRep::String("[]".into()),
        PyRep::String("{}".into()),
        PyRep::String("{\"polygons\":[]}".into()),
        PyRep::Bytes(vec![]),
        PyRep::Dict(vec![(PyRep::String("polygons".into()), PyRep::List(vec![]))]),
        PyRep::keyval([("gates", PyRep::Tuple(vec![]))]),
        PyRep::Rowset {
            columns: vec![],
            rows: vec![],
        },
        PyRep::Object {
            type_name: "G".into(),
            args: Box::new(PyRep::None),
        },
    ] {
        assert_eq!(
            call_pd(&ctx, &mut sess_a, "post_classification", &[PyRep::Integer(1), invalid_pl])
                .unwrap_err()
                .code,
            "InvalidClassification"
        );
    }

    let sample_center = |idx: i64| -> [f64; 2] {
        let task = generate_authentic_task_data(idx, ACTIVE_PROJECT_ID, 10);
        let v: serde_json::Value = serde_json::from_str(task.payload_json.as_deref().unwrap()).unwrap();
        let arr = v["clusters"][0]["center_norm"].as_array().unwrap();
        [arr[0].as_f64().unwrap(), arr[1].as_f64().unwrap()]
    };
    let sample_uuid = |idx: i64| -> String {
        let task = generate_authentic_task_data(idx, ACTIVE_PROJECT_ID, 10);
        let v: serde_json::Value = serde_json::from_str(task.payload_json.as_deref().unwrap()).unwrap();
        v["task_uuid"].as_str().unwrap().to_string()
    };

    // 6. Rate-limit TooFast + TaskAlreadySubmitted + non-control multi-player crowd consensus
    let target_1 = sample_center(1);
    let concordant_json_obj = PyRep::Object {
        type_name: "Classification".into(),
        args: Box::new(PyRep::String(format!(
            "{{\"gates\":[[[{:.4},{:.4}],[{:.4},{:.4}]]]}}",
            target_1[0], target_1[1], target_1[0], target_1[1]
        ))),
    };
    let sub_a1 = call_pd(&ctx, &mut sess_a, "submit_classification", &[PyRep::Integer(1), concordant_json_obj]).unwrap();
    assert_eq!(sub_a1.dict_get("accuracy").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(sub_a1.dict_get("gotBonusXP"), Some(&PyRep::Bool(true)));
    assert_eq!(sub_a1.dict_get("bonusSamplesAfterClassification"), Some(&PyRep::Integer(19)));

    // Immediate resubmit before MIN_SUBMISSION_INTERVAL_SECS -> ProjectDiscoverySubmissionTooFast
    assert_eq!(
        call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(1), PyRep::String("ok".into())])
            .unwrap_err()
            .code,
        "ProjectDiscoverySubmissionTooFast"
    );
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    // Still same current_task_index (1) -> TaskAlreadySubmitted
    assert_eq!(
        call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(1), PyRep::String("ok".into())])
            .unwrap_err()
            .code,
        "TaskAlreadySubmitted"
    );

    // Pilot B joins task 1 concordantly in pixel space -> confirms crowd consensus in ProjectDiscoveryStore
    call_pd(&ctx, &mut sess_b, "get_new_task", &[]).unwrap(); // task 1
    let px = (target_1[0] * 512.0).round();
    let py = (target_1[1] * 512.0).round() as i64;
    let pixel_tuple = PyRep::Tuple(vec![PyRep::List(vec![PyRep::Real(px), PyRep::Integer(py)])]);
    let sub_b1 = call_pd(&ctx, &mut sess_b, "submit", &[PyRep::Integer(1), pixel_tuple]).unwrap();
    assert_eq!(sub_b1.dict_get("accuracy").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(sub_b1.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(20));

    // Pilot C submits discordant coordinates to task 2 after Pilot A establishes consensus centroid
    let target_2 = sample_center(2);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // task 2
    let a2_payload = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Real(target_2[0]),
        PyRep::Real(target_2[1]),
    ])]);
    assert_eq!(
        call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(2), a2_payload])
            .unwrap()
            .dict_get("accuracy")
            .and_then(|v| v.as_f64()),
        Some(1.0)
    );
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_c, "get_new_task", &[]).unwrap(); // task 1 for C
    call_pd(&ctx, &mut sess_c, "submit", &[PyRep::Integer(1), PyRep::String("ok".into())]).unwrap();
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_c, "get_new_task", &[]).unwrap(); // task 2 for C
    // Far away in normalized coords (max_coord <= 1.5 so no /512 normalization) -> sample_acc = 0.2 -> EMA (1.0 * 9.0 + 0.2) / 10.0 = 0.92, AK_Reward = 0
    let far_c2 = PyRep::List(vec![PyRep::List(vec![PyRep::Real(1.45), PyRep::Real(1.45)])]);
    let sub_c2 = call_pd(&ctx, &mut sess_c, "submit", &[PyRep::Integer(2), far_c2]).unwrap();
    assert_eq!(sub_c2.dict_get("accuracy").and_then(|v| v.as_f64()), Some(0.92));
    assert_eq!(sub_c2.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(0));

    // Explicit flag overrides ("false" / "invalid" string and Dict/KeyVal explicit flags) + implausible first vote on task 3
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_c, "get_new_task", &[]).unwrap(); // task 3
    let bad_first_vote = PyRep::List(vec![PyRep::List(vec![PyRep::Real(1.45), PyRep::Real(1.45)])]);
    let sub_c3 = call_pd(&ctx, &mut sess_c, "submit", &[PyRep::Integer(3), bad_first_vote]).unwrap();
    assert_eq!(sub_c3.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(0));

    // Task 4: explicit string "invalid" & KeyVal consensus=false
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_c, "get_new_task", &[]).unwrap(); // task 4
    let kv_false = PyRep::keyval([
        ("consensus", PyRep::Bool(false)),
        ("gates", PyRep::List(vec![PyRep::List(vec![PyRep::Real(0.5), PyRep::Real(0.5)])])),
    ]);
    let sub_c4 = call_pd(&ctx, &mut sess_c, "submit", &[PyRep::Integer(4), kv_false]).unwrap();
    assert_eq!(sub_c4.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(0));

    // Task 5: Control task (`5 % 5 == 0`) graded against `sample_target_center_norm(5)` directly
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    let target_5 = sample_center(5);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // A on 3
    call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(3), PyRep::String("invalid".into())]).unwrap();
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // A on 4
    let dict_false = PyRep::Dict(vec![
        (PyRep::String("is_accurate".into()), PyRep::Bool(false)),
        (PyRep::String("accurate".into()), PyRep::Bool(false)),
        (
            PyRep::String("polygons".into()),
            PyRep::List(vec![PyRep::List(vec![PyRep::Real(0.5), PyRep::Real(0.5)])]),
        ),
    ]);
    call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(4), dict_false]).unwrap();
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // A on 5 (control)
    let near_5 = PyRep::List(vec![PyRep::List(vec![
        PyRep::Real(target_5[0] + 0.05),
        PyRep::Real(target_5[1] + 0.05),
    ])]);
    let sub_a5 = call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(5), near_5]).unwrap();
    assert_eq!(sub_a5.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(20));

    // Pilot B on control task 5 submits far away -> fails control check
    call_pd(&ctx, &mut sess_b, "get_new_task", &[]).unwrap();
    call_pd(&ctx, &mut sess_b, "submit", &[PyRep::Integer(2), PyRep::String("false".into())]).unwrap();
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    for t in 3..=4 {
        call_pd(&ctx, &mut sess_b, "get_new_task", &[]).unwrap();
        call_pd(&ctx, &mut sess_b, "submit", &[PyRep::Integer(t), PyRep::String("false".into())]).unwrap();
        ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    }
    call_pd(&ctx, &mut sess_b, "get_new_task", &[]).unwrap(); // B on 5 (control)
    let far_5 = PyRep::String("{\"accurate\":true,\"polygons\":[[[1.45,1.45]]]}".into());
    let sub_b5 = call_pd(&ctx, &mut sess_b, "submit", &[PyRep::Integer(5), far_5]).unwrap();
    assert_eq!(sub_b5.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(0));

    // 7. Exhaust all 20 bonus samples for A -> 21st submission gets gotBonusXP == false & 1x rewards + medium ISK tier (0.5..0.9)
    for t in 6..=20 {
        ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
        call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap();
        let res = call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(t), PyRep::String("ok".into())]).unwrap();
        assert_eq!(res.dict_get("gotBonusXP"), Some(&PyRep::Bool(true)));
    }
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap();
    let res_no_bonus = call_pd(&ctx, &mut sess_a, "submit", &[PyRep::Integer(21), PyRep::String("ok".into())]).unwrap();
    assert_eq!(res_no_bonus.dict_get("gotBonusXP"), Some(&PyRep::Bool(false)));
    assert_eq!(res_no_bonus.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(150.0));

    // 8. get_data_from_url: auto-create on current task not yet in char_tasks, UUID lookup, payload_json=None & invalid JSON fallbacks
    let mut gm = sess_a.clone();
    gm.role |= eve_machonet::service_roles::ROLE_GML;
    assert_eq!(
        call_pd(&ctx, &mut gm, "fix_task_id", &[PyRep::String("abc".into())])
            .unwrap_err()
            .code,
        "InvalidTaskId"
    );
    // Set current_task_index directly to 22 via snap without inserting char_task, then fetch via URL
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .project_discovery_players
            .get_mut(&cid_a)
            .unwrap()
            .current_task_index = 22;
        snap.seasons_and_campaigns
            .project_discovery_store
            .char_tasks
            .remove(&format!("{cid_a}:22"));
    });
    let auto_22 = call_pd(
        &ctx,
        &mut sess_a,
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/mcgill-polyseg/tasks/22".into())],
    )
    .unwrap();
    assert!(auto_22.as_str().unwrap().contains("\"task_number\":22"));

    // Lookup by sample UUID segment & fallback when payload_json is None or corrupt JSON
    let uuid_22 = sample_uuid(22);
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .project_discovery_store
            .char_tasks
            .get_mut(&format!("{cid_a}:22"))
            .unwrap()
            .payload_json = None;
    });
    let none_payload_rep = call_pd(
        &ctx,
        &mut sess_a,
        "get_data_from_url",
        &[PyRep::String(format!("https://projectdiscovery.eveonline.com/mcgill-polyseg/assets/{uuid_22}"))],
    )
    .unwrap();
    assert!(none_payload_rep.as_str().unwrap().contains("\"fetch_count\":2"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .project_discovery_store
            .char_tasks
            .get_mut(&format!("{cid_a}:22"))
            .unwrap()
            .payload_json = Some("{corrupt".into());
    });
    let corrupt_payload_rep = call_pd(
        &ctx,
        &mut sess_a,
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/mcgill-polyseg/tasks/22".into())],
    )
    .unwrap();
    assert!(corrupt_payload_rep.as_str().unwrap().contains("\"task_id\":\"task_22\""));

    // Tutorial reward denied when skipped or incomplete
    assert_eq!(
        call_pd(&ctx, &mut sess_c, "give_tutorial_rewards", &[]).unwrap(),
        PyRep::Bool(false)
    );
    call_pd(&ctx, &mut sess_c, "skip_tutorial", &[]).unwrap();
    assert_eq!(
        call_pd(&ctx, &mut sess_c, "get_is_player_entitled_to_tutorial_reward", &[]).unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_pd(&ctx, &mut sess_c, "give_tutorial_rewards", &[]).unwrap(),
        PyRep::Bool(false)
    );

    // /pd config with max_bonus (6th arg) and mmos zero inaccurate XP
    handle_project_discovery_slash(&ctx, "/pd config 200 100 120 0 15 12").unwrap().unwrap();
    ctx.advance_secs(MIN_SUBMISSION_INTERVAL_SECS);
    call_pd(&ctx, &mut sess_a, "get_new_task", &[]).unwrap(); // task 23
    let zero_inacc = call_pd(
        &ctx,
        &mut sess_a,
        "submit",
        &[PyRep::Integer(23), PyRep::String("invalid".into())],
    )
    .unwrap();
    assert_eq!(zero_inacc.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(0.0));
}
