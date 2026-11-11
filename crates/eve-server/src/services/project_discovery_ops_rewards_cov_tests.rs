//! Direct unit tests covering `project_discovery_ops` and `project_discovery_rewards`.

#![forbid(unsafe_code)]

use super::super::ops::*;
use super::super::rewards::*;
use super::super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_proto::PyRep;

#[test]
fn training_tasks_grading_formats_control_and_multiplayer_consensus() {
    let ctx = test_ctx();
    // V24 training task UUIDs for 1..=5 and deterministic fallback for 6
    let t1 = build_training_task_response(1, 2);
    assert_eq!(
        t1.dict_get("task_id").and_then(|v| v.as_str()),
        Some("ed134308-e080-49bc-be56-169a5f839c96")
    );
    assert_eq!(t1.dict_get("level").and_then(|v| v.as_i64()), Some(2));
    let t6 = build_training_task_response(6, 5);
    assert_eq!(
        t6.dict_get("task_id").and_then(|v| v.as_str()),
        Some(sample_uuid_for_index(10_006).as_str())
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = ProjectDiscoveryStore::default();

        // Explicit rejection strings & Dict/JSON flags
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_001, 1, &PyRep::String("false".into())),
            (0.0, false)
        );
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_001, 1, &PyRep::String("INVALID".into())),
            (0.0, false)
        );
        assert_eq!(
            grade_and_record_classification(
                snap,
                &mut store,
                90_000_001,
                1,
                &PyRep::String(r#"{"accurate": false, "gates": [[0.5, 0.5]]}"#.into()),
            ),
            (0.0, false)
        );
        assert_eq!(
            grade_and_record_classification(
                snap,
                &mut store,
                90_000_001,
                1,
                &PyRep::Dict(vec![(PyRep::String("is_accurate".into()), PyRep::Bool(false))]),
            ),
            (0.0, false)
        );

        // Control task (index 10 % 5 == 0): None centroid -> (1.0, true); close pixel centroid -> accurate; far -> inaccurate
        let [tx, ty] = sample_target_center_norm(10);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_001, 10, &PyRep::List(vec![])),
            (1.0, true)
        );
        let close_px = PyRep::Tuple(vec![PyRep::List(vec![
            PyRep::Real(tx * 512.0),
            PyRep::Integer((ty * 512.0).round() as i64),
        ])]);
        let (acc_close, ok_close) =
            grade_and_record_classification(snap, &mut store, 90_000_001, 10, &close_px);
        assert!(ok_close && acc_close >= 0.75);

        let far_pt = PyRep::Object {
            type_name: "Gate".into(),
            args: Box::new(PyRep::KeyVal(HashMap::from([
                ("consensus".into(), PyRep::Bool(true)),
                (
                    "pts".into(),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(if tx > 0.5 { 0 } else { 1 }),
                        PyRep::Real(if ty > 0.5 { 0.0 } else { 1.0 }),
                    ])]),
                ),
            ]))),
        };
        let (acc_far, ok_far) =
            grade_and_record_classification(snap, &mut store, 90_000_002, 10, &far_pt);
        assert!(!ok_far && acc_far <= 0.35);

        // Standard task (index 11): multi-player crowd consensus
        let [sx, sy] = sample_target_center_norm(11);
        // First responder near target -> plausible (1.0, true), establishes consensus_centroid
        let first_sub = PyRep::String(format!(r#"{{"gates": [[{sx:.3}, {sy:.3}]]}}"#));
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_001, 11, &first_sub),
            (1.0, true)
        );
        // Second responder far from consensus_centroid -> discordant (0.2, false)
        let far_sub = PyRep::List(vec![PyRep::List(vec![
            PyRep::Real(if sx > 0.5 { 0.0 } else { 1.0 }),
            PyRep::Real(if sy > 0.5 { 0.0 } else { 1.0 }),
        ])]);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_002, 11, &far_sub),
            (0.2, false)
        );
        // Second concordant responder establishes task_consensus_by_index = true
        let concordant_sub = PyRep::List(vec![PyRep::List(vec![
            PyRep::Real(sx + 0.02),
            PyRep::Real(sy - 0.02),
        ])]);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_003, 11, &concordant_sub),
            (1.0, true)
        );
        assert_eq!(store.task_consensus_by_index.get(&11), Some(&true));
        // Subsequent responder hits cached consensus early return
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_004, 11, &far_sub),
            (1.0, true)
        );

        // Standard task (index 12): first responder implausible (> 0.65 away from target) -> (0.2, false)
        let [t12x, t12y] = sample_target_center_norm(12);
        let implausible = PyRep::List(vec![PyRep::List(vec![
            PyRep::Real(if t12x < 0.5 { 0.99 } else { 0.01 }),
            PyRep::Real(if t12y < 0.5 { 0.99 } else { 0.01 }),
        ])]);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 90_000_001, 12, &implausible),
            (0.2, false)
        );
        assert!(!snap.seasons_and_campaigns.project_discovery_votes.is_empty());
    });
}

#[test]
fn rewards_bonus_alpha_omega_xp_curve_crate_spawn_and_execute_post_classification() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let _sess = auth_session(1, char_id, 98_000_001, 30_000_142);

    // 1. effective_max_bonus_samples & sync_player_bonus_samples
    let mut store = ProjectDiscoveryStore::default();
    assert_eq!(effective_max_bonus_samples(&store, 1), ALPHA_BONUS_SAMPLES);
    assert_eq!(effective_max_bonus_samples(&store, 2), OMEGA_BONUS_SAMPLES);
    store.max_bonus_samples = Some(0);
    assert_eq!(effective_max_bonus_samples(&store, 1), ALPHA_BONUS_SAMPLES);
    store.max_bonus_samples = Some(12);
    assert_eq!(effective_max_bonus_samples(&store, 2), 12);
    store.max_bonus_samples = None;

    let mut st = PlayerProjectState::default();
    sync_player_bonus_samples(&mut st, &store, 1, 1_000_000);
    assert_eq!(st.bonus_samples_left, 10);
    // Upgrade Alpha -> Omega adds +10 bonus samples immediately
    sync_player_bonus_samples(&mut st, &store, 2, 1_000_010);
    assert_eq!(st.max_bonus_samples, 20);
    assert_eq!(st.bonus_samples_left, 20);
    // Downgrade Omega -> Alpha clamps bonus samples to 10
    sync_player_bonus_samples(&mut st, &store, 1, 1_000_020);
    assert_eq!(st.max_bonus_samples, 10);
    assert_eq!(st.bonus_samples_left, 10);

    // 2. Progressive total_needed_xp_for_rank across all 4 rank tiers
    assert_eq!(total_needed_xp_for_rank(&store, 1), 500.0);
    assert_eq!(total_needed_xp_for_rank(&store, 5), 5_000.0);
    assert!(total_needed_xp_for_rank(&store, 6) > total_needed_xp_for_rank(&store, 5));
    assert!(total_needed_xp_for_rank(&store, 16) > total_needed_xp_for_rank(&store, 15));
    assert!(total_needed_xp_for_rank(&store, 26) > total_needed_xp_for_rank(&store, 25));

    // 3. format_player_statistics_rep & spawn_rank_reward_crate
    let stats_rep = format_player_statistics_rep(&st);
    assert_eq!(stats_rep.dict_get("rank").and_then(|v| v.as_i64()), Some(1));
    assert!(stats_rep.dict_get("projects").is_some());
    assert!(stats_rep.dict_get("project_list").is_some());

    ctx.db.with_mut_snapshot(|snap| {
        let items_before = snap.items.len();
        // Unknown character -> no-op
        spawn_rank_reward_crate(snap, 999_999_999, LOOT_CRATE_TYPE_ID);
        assert_eq!(snap.items.len(), items_before);

        // Undocked character falls back to home_station_id
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.home_station_id = 60_003_760;
        }
        spawn_rank_reward_crate(snap, char_id, LOOT_CRATE_TYPE_ID);
        assert_eq!(snap.items.len(), items_before + 1);

        // Character with home_station_id == 0 and station_id == None -> no-op
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.home_station_id = 0;
        }
        spawn_rank_reward_crate(snap, char_id, LOOT_CRATE_TYPE_ID);
        assert_eq!(snap.items.len(), items_before + 1);

        // 4. execute_post_classification error paths & no-bonus + medium/high ISK tiers
        let mut pstate = PlayerProjectState {
            project_id: ACTIVE_PROJECT_ID.into(),
            rank: 1,
            current_task_index: 1,
            last_submitted_task_index: 0,
            last_submission_unix: 1_000,
            last_bonus_refill_unix: 1_000,
            max_bonus_samples: 20,
            bonus_samples_left: 0,
            ..Default::default()
        };
        assert_eq!(
            execute_post_classification(snap, &mut pstate, char_id, 1, &PyRep::List(vec![]), 1_000)
                .unwrap_err()
                .code,
            "ProjectDiscoverySubmissionTooFast"
        );
        assert_eq!(
            execute_post_classification(snap, &mut pstate, char_id, 2, &PyRep::List(vec![]), 1_010)
                .unwrap_err()
                .code,
            "InvalidTaskId"
        );
        pstate.last_submitted_task_index = 1;
        assert_eq!(
            execute_post_classification(snap, &mut pstate, char_id, 1, &PyRep::List(vec![]), 1_010)
                .unwrap_err()
                .code,
            "TaskAlreadySubmitted"
        );

        // Accurate submission with bonus_samples_left == 0 -> gotBonusXP = false, 1x ISK (1000 * rank 1 = 1000)
        pstate.last_submitted_task_index = 0;
        let out =
            execute_post_classification(snap, &mut pstate, char_id, 1, &PyRep::List(vec![]), 1_010)
                .unwrap();
        assert_eq!(out.rep.dict_get("gotBonusXP"), Some(&PyRep::Bool(false)));
        assert_eq!(out.rep.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(1_000.0));
        assert_eq!(out.xp_gain, 150.0);
        assert_eq!(out.ak_gain, 20);
        assert!(out.new_bal.is_some());

        // Second submission on task 2 with inaccurate grade drops EMA accuracy into [0.5, 0.9) tier
        // (1.0 * 9 + 0.0) / 10 = 0.9; force accuracy to 0.7 -> tier 1 (800 ISK)
        pstate.current_task_index = 2;
        pstate.accuracy = 0.65;
        let out2 =
            execute_post_classification(snap, &mut pstate, char_id, 2, &PyRep::List(vec![]), 1_020)
                .unwrap();
        // (0.65 * 9 + 1.0) / 10 = 0.685 -> tier 1 -> 800.0 ISK * rank 1
        assert_eq!(out2.rep.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(800.0));
    });
}
