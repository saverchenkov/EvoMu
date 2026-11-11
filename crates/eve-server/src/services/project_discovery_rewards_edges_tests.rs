//! Behavioral edge-case unit tests for `services::project_discovery_rewards`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::project_discovery::{
    configure_project_discovery_bonus_samples, configure_project_discovery_mmos,
    set_project_discovery_task_consensus,
};
use eve_db::{IskCents, WorldRepository};

#[test]
fn bonus_samples_rank_xp_curve_and_crate_spawning_cover_all_tiers() {
    let mut store = ProjectDiscoveryStore::default();
    assert_eq!(effective_max_bonus_samples(&store, 1), ALPHA_BONUS_SAMPLES);
    assert_eq!(effective_max_bonus_samples(&store, 2), OMEGA_BONUS_SAMPLES);
    store.max_bonus_samples = Some(0);
    assert_eq!(effective_max_bonus_samples(&store, 1), ALPHA_BONUS_SAMPLES);
    store.max_bonus_samples = Some(15);
    assert_eq!(effective_max_bonus_samples(&store, 2), 15);

    // sync_player_bonus_samples: initial refill, no early refill, grade change, 24h refill
    store.max_bonus_samples = None;
    let mut st = PlayerProjectState::default();
    sync_player_bonus_samples(&mut st, &store, 1, 10_000);
    assert_eq!(st.max_bonus_samples, ALPHA_BONUS_SAMPLES);
    assert_eq!(st.bonus_samples_left, ALPHA_BONUS_SAMPLES);
    assert_eq!(st.last_bonus_refill_unix, 10_000);

    st.bonus_samples_left = 4;
    sync_player_bonus_samples(&mut st, &store, 1, 10_500);
    assert_eq!(st.bonus_samples_left, 4);

    // Upgrade to Omega (+10 diff)
    sync_player_bonus_samples(&mut st, &store, 2, 10_600);
    assert_eq!(st.max_bonus_samples, OMEGA_BONUS_SAMPLES);
    assert_eq!(st.bonus_samples_left, 14);

    // Downgrade to Alpha (-10 diff)
    sync_player_bonus_samples(&mut st, &store, 1, 10_700);
    assert_eq!(st.max_bonus_samples, ALPHA_BONUS_SAMPLES);
    assert_eq!(st.bonus_samples_left, 4);

    // 24h elapsed -> full refill
    sync_player_bonus_samples(&mut st, &store, 1, 10_000 + 86_400);
    assert_eq!(st.bonus_samples_left, ALPHA_BONUS_SAMPLES);

    // XP curves: rank <= 0, progressive tiers (1..=5, 6..=15, 16..=25, >25), linear override, per-rank map override
    let default_store = ProjectDiscoveryStore::default();
    assert_eq!(total_needed_xp_for_rank(&default_store, 0), 0.0);
    assert_eq!(total_needed_xp_for_rank(&default_store, -2), 0.0);
    assert_eq!(total_needed_xp_for_rank(&default_store, 1), 500.0);
    assert_eq!(total_needed_xp_for_rank(&default_store, 2), 1_250.0);
    let r5 = total_needed_xp_for_rank(&default_store, 5);
    let r6 = total_needed_xp_for_rank(&default_store, 6);
    assert!(r6 > r5);
    let r16 = total_needed_xp_for_rank(&default_store, 16);
    let r26 = total_needed_xp_for_rank(&default_store, 26);
    assert!(r26 > r16);

    assert_eq!(compute_rank_for_xp(&default_store, 499.0, 1), 1);
    assert_eq!(compute_rank_for_xp(&default_store, r6 + 1.0, 1), 7);

    let mut linear_store = ProjectDiscoveryStore {
        xp_per_rank: 100.0,
        ..Default::default()
    };
    assert_eq!(total_needed_xp_for_rank(&linear_store, 3), 300.0);
    linear_store.xp_curve_by_rank.insert(2, 0.0);
    assert_eq!(total_needed_xp_for_rank(&linear_store, 2), 0.0);
    assert_eq!(compute_rank_for_xp(&linear_store, 500.0, 1), 2);

    // format_player_statistics_rep
    let stats_rep = format_player_statistics_rep(&st);
    match stats_rep {
        PyRep::KeyVal(entries) => {
            assert!(entries.iter().any(|(k, _)| k == "projects"));
            assert!(entries.iter().any(|(k, _)| k == "project_list"));
            assert!(entries.iter().any(|(k, _)| k == "analysisKredits"));
        }
        other => panic!("expected KeyVal, got {other:?}"),
    }

    // spawn_rank_reward_crate: missing char, home station fallback, docked station
    let repo = WorldRepository::with_demo_seed();
    repo.with_mut_snapshot(|snap| {
        let count_before = snap.items.len();
        spawn_rank_reward_crate(snap, 999_999_999, LOOT_CRATE_TYPE_ID);
        assert_eq!(snap.items.len(), count_before);

        let cid = *snap.characters.keys().next().unwrap();
        let home = snap.characters[&cid].home_station_id;
        snap.characters.get_mut(&cid).unwrap().station_id = None;
        spawn_rank_reward_crate(snap, cid, LOOT_CRATE_TYPE_ID);
        assert!(snap.items.values().any(|i| i.owner_id == cid
            && i.type_id == LOOT_CRATE_TYPE_ID
            && i.location_id == home
            && i.flag == InventoryFlag::Hangar));

        snap.characters.get_mut(&cid).unwrap().station_id = Some(60_003_760);
        spawn_rank_reward_crate(snap, cid, SUPERIOR_LOOT_CRATE_TYPE_ID);
        assert!(snap.items.values().any(|i| i.owner_id == cid
            && i.type_id == SUPERIOR_LOOT_CRATE_TYPE_ID
            && i.location_id == 60_003_760));
    });
}

#[test]
fn execute_post_classification_exercises_guards_rewards_tiers_crates_and_mmos() {
    let repo = WorldRepository::with_demo_seed();
    // Seeded character 90_000_001 has Omega clone_grade == 2 (20 daily bonus samples).
    let cid = 90_000_001_i64;

    // 1. Submission rate limit, invalid task ID, and already submitted guards
    repo.with_mut_snapshot(|snap| {
        let mut st = PlayerProjectState {
            current_task_index: 5,
            last_submission_unix: 1_000,
            ..Default::default()
        };
        let err_fast =
            execute_post_classification(snap, &mut st, cid, 5, &PyRep::None, 1_000).unwrap_err();
        assert_eq!(err_fast.code, "ProjectDiscoverySubmissionTooFast");

        let err_invalid =
            execute_post_classification(snap, &mut st, cid, 4, &PyRep::None, 1_010).unwrap_err();
        assert_eq!(err_invalid.code, "InvalidTaskId");

        st.last_submitted_task_index = 5;
        let err_dup =
            execute_post_classification(snap, &mut st, cid, 5, &PyRep::None, 1_010).unwrap_err();
        assert_eq!(err_dup.code, "TaskAlreadySubmitted");
    });

    // 2. First accurate submission with bonus sample -> 2x XP & 2x ISK, rank up below 25 spawns LOOT_CRATE_TYPE_ID
    repo.with_mut_snapshot(|snap| {
        let start_bal = snap.characters[&cid].balance;
        let journal_before = snap.wallet_journal.len();
        configure_project_discovery_mmos(snap, 100.0, 500.0, 150.0, 25.0, 20);
        set_project_discovery_task_consensus(snap, 1, true);

        let mut st = PlayerProjectState {
            current_task_index: 1,
            last_submitted_task_index: 0,
            rank: 1,
            ..Default::default()
        };
        let out = execute_post_classification(snap, &mut st, cid, 1, &PyRep::None, 2_000).unwrap();
        assert_eq!(out.prev_rank, 1);
        assert_eq!(out.new_rank, 4); // 300 XP with 100/rank -> ranks 1->2->3->4
        assert_eq!(out.xp_gain, 300.0);
        assert_eq!(out.total_xp, 300.0);
        assert_eq!(out.ak_gain, 20);
        // Demo seed character 90_000_001 is Omega (clone_grade 2 -> 20 bonus samples - 1 = 19)
        assert_eq!(st.bonus_samples_left, 19);
        assert_eq!(st.accuracy, 1.0);
        assert_eq!(st.score, 100.0);
        // tier_idx 2 (acc >= 0.9 -> 1000) * rank 1 * 2 bonus = 2000 ISK
        assert_eq!(
            out.new_bal.unwrap(),
            start_bal.checked_add(IskCents::from_isk(2_000)).unwrap()
        );
        assert_eq!(snap.wallet_journal.len(), journal_before + 1);
        assert!(matches!(out.rep, PyRep::KeyVal(_)));

        // 3. No bonus samples left + medium accuracy tier (0.5..0.9) + rank >= 25 spawns SUPERIOR_LOOT_CRATE_TYPE_ID
        st.current_task_index = 2;
        st.bonus_samples_left = 0;
        st.accuracy = 0.6;
        st.rank = NEEDED_LEVEL_FOR_SUPERIOR_CRATES - 1;
        st.experience = ((NEEDED_LEVEL_FOR_SUPERIOR_CRATES - 2) as f64) * 100.0;
        set_project_discovery_task_consensus(snap, 2, true);
        let out2 = execute_post_classification(snap, &mut st, cid, 2, &PyRep::None, 2_010).unwrap();
        assert_eq!(out2.xp_gain, 150.0);
        assert!(out2.new_rank >= NEEDED_LEVEL_FOR_SUPERIOR_CRATES);
        assert!(snap
            .items
            .values()
            .any(|i| i.owner_id == cid && i.type_id == SUPERIOR_LOOT_CRATE_TYPE_ID));

        // 4. Low accuracy (<0.5) still accurate -> tier_idx 0 (500 * prev_rank)
        st.current_task_index = 3;
        st.accuracy = 0.1;
        let rank_before_low = st.rank;
        set_project_discovery_task_consensus(snap, 3, true);
        let out_low =
            execute_post_classification(snap, &mut st, cid, 3, &PyRep::None, 2_020).unwrap();
        if let PyRep::KeyVal(entries) = out_low.rep {
            let isk_r = entries
                .iter()
                .find(|(k, _)| *k == "ISK_Reward")
                .map(|(_, v)| v.clone());
            assert_eq!(isk_r, Some(PyRep::Real((500 * rank_before_low) as f64)));
        }

        // 5. Inaccurate submission -> MMOS inaccurate XP (25.0), 0 AK, 0 ISK
        st.current_task_index = 4;
        set_project_discovery_task_consensus(snap, 4, false);
        let out_inacc =
            execute_post_classification(snap, &mut st, cid, 4, &PyRep::None, 2_030).unwrap();
        assert_eq!(out_inacc.xp_gain, 25.0);
        assert_eq!(out_inacc.ak_gain, 0);
        assert!(out_inacc.new_bal.is_none());
    });

    // 6. Default (non-MMOS) store + missing character fallback
    let repo2 = WorldRepository::with_demo_seed();
    repo2.with_mut_snapshot(|snap| {
        configure_project_discovery_bonus_samples(snap, 12);
        // Reset mmos_configured to false via store reload check: test missing char with default XP
        let mut st = PlayerProjectState {
            current_task_index: 1,
            ..Default::default()
        };
        let out = execute_post_classification(snap, &mut st, 888_888_888, 1, &PyRep::None, 3_000)
            .unwrap();
        assert_eq!(out.xp_gain, 300.0); // 150.0 * 2.0 bonus
        assert_eq!(out.ak_gain, 20);
        assert_eq!(out.new_bal, Some(IskCents::from_isk(2_000)));
    });
}
