//! Extra branch-coverage and mutation-killing unit tests for `quasar_campaign_state`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_campaign_state::{
    campaign_id_to_uuid_bytes, contribution_method_params, encode_identifier,
    objective_id_to_uuid_bytes, require_id, ticking_value, track_id_to_uuid_bytes, CampaignEvent,
    CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord, ObjectiveParameter,
    TickingContribution, TickingEvaluation, CAMPAIGN_STATE_ACTIVE, CAMPAIGN_STATE_COMPLETED,
    COMMITMENT_STATE_COMMITTED, COMMITMENT_STATE_RESIGNED, MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT,
    OBJECTIVE_STATE_COMPLETED, PARTICIPANT_STATE_CONTRIBUTION_COMPLETED, TRACK_STATE_STOPPED,
    TRACK_STATE_TARGET_REACHED,
};
use crate::services::quasar_progression_extra::ProgressionExtraManager;
use std::sync::Arc;

#[test]
fn uuid_encoding_event_parameters_and_registration_validation() {
    let cid_uuid = campaign_id_to_uuid_bytes(0x0102_0304_0506_0708);
    assert_eq!(&cid_uuid[0..8], b"CAMPAIGN");
    assert_eq!(&cid_uuid[8..16], &[1, 2, 3, 4, 5, 6, 7, 8]);

    let oid_uuid = objective_id_to_uuid_bytes(7);
    assert_eq!(&oid_uuid[0..8], b"CAMPOBJ_");
    assert_eq!(oid_uuid[15], 7);

    let tid_uuid = track_id_to_uuid_bytes(9);
    assert_eq!(&tid_uuid[0..8], b"CAMPTRK_");
    assert_eq!(tid_uuid[15], 9);

    let enc = encode_identifier(&cid_uuid);
    assert_eq!(enc.len(), 18);

    assert_eq!(require_id(None, "track").unwrap_err().0, STATUS_BAD_REQUEST);
    assert!(contribution_method_params("DamageShip").is_some());

    // CampaignEvent::param and matches across all parameter key categories
    let ev = CampaignEvent {
        method: "MineAsteroid",
        type_id: Some(1228),
        solar_system_id: Some(30_000_142),
        faction_id: Some(500_001),
    };
    for k in ["ore_type", "item_type", "target_ship_type", "ship"] {
        assert_eq!(ev.param(k).as_deref(), Some("1228"));
    }
    for k in ["solar_system", "origin"] {
        assert_eq!(ev.param(k).as_deref(), Some("30000142"));
    }
    assert_eq!(ev.param("faction").as_deref(), Some("500001"));
    assert!(ev.param("unknown_param").is_none());

    let mut obj = CampaignObjectiveRecord {
        id: 1,
        campaign_id: 10,
        name: "mine".into(),
        contribution_method: "MineAsteroid".into(),
        parameters: vec![
            ObjectiveParameter {
                key: "ore_type".into(),
                values: vec!["1228".into()],
            },
            ObjectiveParameter {
                key: "solar_system".into(),
                values: vec!["30000142".into()],
            },
        ],
        ..Default::default()
    };
    assert!(ev.matches(&obj));
    obj.parameters[0].values = vec!["18".into()];
    assert!(!ev.matches(&obj));

    // Registration validation: invalid objective parameter key and mismatched track campaign_id
    let mgr = ProgressionExtraManager::new(None);
    let camp = CampaignRecord {
        id: 10,
        name: "test".into(),
        ..Default::default()
    };
    let bad_param_obj = CampaignObjectiveRecord {
        id: 1,
        campaign_id: 10,
        contribution_method: "MineAsteroid".into(),
        parameters: vec![ObjectiveParameter {
            key: "not_a_valid_param".into(),
            values: vec!["1".into()],
        }],
        ..Default::default()
    };
    assert!(mgr
        .register_campaign(camp.clone(), vec![bad_param_obj], vec![])
        .unwrap_err()
        .contains("not defined by"));

    let bad_trk = CampaignTrackRecord {
        id: 2,
        campaign_id: 999,
        ..Default::default()
    };
    assert!(mgr
        .register_campaign(camp, vec![], vec![bad_trk])
        .unwrap_err()
        .contains("belongs to campaign"));
}

#[test]
fn campaign_completion_track_caps_commitment_limit_and_reward_rollback() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mgr = ProgressionExtraManager::new(Some(Arc::clone(&ctx.db)));
    let char_id = 90_000_001_u32;
    let char2 = 90_000_002_u32;

    let cid = 500_u64;
    let oid1 = 501_u64;
    let oid2 = 502_u64;
    let tid1 = 503_u64;
    let tid2 = 504_u64;

    let camp = CampaignRecord {
        id: cid,
        name: "completion camp".into(),
        scheduled_start_unix: 1_000,
        scheduled_expiry_unix: 0, // no expiry
        ..Default::default()
    };
    let obj1 = CampaignObjectiveRecord {
        id: oid1,
        campaign_id: cid,
        name: "o1".into(),
        target_progress: 20,
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 20,
        isk_reward_per_interval: 5_000,
        isk_reward_progress_interval: 10,
        ..Default::default()
    };
    let obj2 = CampaignObjectiveRecord {
        id: oid2,
        campaign_id: cid,
        name: "o2".into(),
        target_progress: 15,
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 0, // unlimited per participant
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        ..Default::default()
    };
    let trk1 = CampaignTrackRecord {
        id: tid1,
        campaign_id: cid,
        key: "t1".into(),
        target_progress_value: 25,
        max_participant_contribution: 30,
        isk_reward_per_interval: 10_000,
        isk_reward_progress_interval: 10,
        ticking: vec![TickingContribution {
            key: "tk".into(),
            interval_secs: 50,
            evaluations: vec![TickingEvaluation {
                key: "u".into(),
                constant: None,
                per_unit_value: Some(2),
            }],
            last_tick_unix: 0,
        }],
        ..Default::default()
    };
    let trk2 = CampaignTrackRecord {
        id: tid2,
        campaign_id: cid,
        key: "t2_unlimited".into(),
        target_progress_value: 0, // stays STARTED until campaign completes -> STOPPED
        isk_reward_progress_interval: 0,
        ..Default::default()
    };

    mgr.register_campaign(camp, vec![obj1, obj2], vec![trk1, trk2])
        .unwrap();

    // Before scheduled_start_unix (t=500): campaign remains in state 0, commit fails with PRECONDITION_FAILED
    assert!(mgr.evaluate_campaign_lifecycle(500).is_empty());
    let mut notices = Vec::new();
    assert_eq!(
        mgr.commit_to_objective(char_id, oid1, 500, &mut notices)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        mgr.commit_to_objective(char_id, 999_999, 500, &mut notices)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // At t=1000: campaign starts; commit char_id to both oid1 and oid2 (deduped committed_units == 1)
    mgr.commit_to_objective(char_id, oid1, 1_000, &mut notices)
        .unwrap();
    // Idempotent duplicate commit
    mgr.commit_to_objective(char_id, oid1, 1_000, &mut notices)
        .unwrap();
    mgr.commit_to_objective(char_id, oid2, 1_000, &mut notices)
        .unwrap();
    mgr.commit_to_objective(char2, oid1, 1_000, &mut notices)
        .unwrap();
    assert_eq!(mgr.campaign_committed_units(cid), 2);

    // Resign and re-commit char2 to test commitment state transitions
    mgr.resign_from_objective(char2, oid1, 1_000, &mut notices)
        .unwrap();
    {
        let objs = mgr.campaign_objectives.read().unwrap();
        assert_eq!(
            ProgressionExtraManager::objective_commitment(&objs[&oid1], char2),
            Some(COMMITMENT_STATE_RESIGNED)
        );
        assert_eq!(
            ProgressionExtraManager::objective_commitment(&objs[&oid1], 777_777),
            None
        );
        assert_eq!(
            ProgressionExtraManager::objective_commitment(&objs[&oid1], char_id),
            Some(COMMITMENT_STATE_COMMITTED)
        );
    }
    mgr.commit_to_objective(char2, oid1, 1_000, &mut notices)
        .unwrap();

    // Record 0 units -> no-op
    let ev = CampaignEvent {
        method: "MineAsteroid",
        type_id: Some(1228),
        solar_system_id: Some(30_000_142),
        faction_id: None,
    };
    assert!(mgr.record_gameplay_contribution(char_id, &ev, 0, 1_000).is_empty());

    // Record 20 units at t=1050 -> completesboth oid1 (target 20) and oid2 (target 15),
    // reaches trk1 target_progress_value (25), completes campaign, and stops trk2!
    let contrib_notices = mgr.record_gameplay_contribution(char_id, &ev, 20, 1_050);
    assert!(contrib_notices
        .iter()
        .any(|(_, a)| a.type_url.ends_with("campaign.api.CompletedNotice")));

    {
        let camps = mgr.campaigns.read().unwrap();
        assert_eq!(camps[&cid].state, CAMPAIGN_STATE_COMPLETED);
        let objs = mgr.campaign_objectives.read().unwrap();
        assert_eq!(objs[&oid1].state, OBJECTIVE_STATE_COMPLETED);
        assert_eq!(objs[&oid2].state, OBJECTIVE_STATE_COMPLETED);
        assert_eq!(
            ProgressionExtraManager::objective_participant_state(&objs[&oid1], char_id),
            Some(PARTICIPANT_STATE_CONTRIBUTION_COMPLETED)
        );
        let trks = mgr.campaign_tracks.read().unwrap();
        assert_eq!(trks[&tid1].state, TRACK_STATE_TARGET_REACHED);
        assert_eq!(trks[&tid2].state, TRACK_STATE_STOPPED);
        assert_eq!(
            ProgressionExtraManager::track_unredeemed_intervals(&trks[&tid2], char_id),
            0
        );
    }

    // Reward redemption rollback when database is unavailable (ProgressionExtraManager::new(None))
    let db_less_mgr = ProgressionExtraManager::new(None);
    db_less_mgr
        .campaign_objectives
        .write()
        .unwrap()
        .insert(oid1, mgr.campaign_objectives.read().unwrap()[&oid1].clone());
    db_less_mgr
        .campaign_tracks
        .write()
        .unwrap()
        .insert(tid1, mgr.campaign_tracks.read().unwrap()[&tid1].clone());

    assert_eq!(
        db_less_mgr
            .redeem_objective_rewards(char_id, 999_999)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        db_less_mgr
            .redeem_track_rewards(char_id, 999_999)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    assert_eq!(
        db_less_mgr
            .redeem_objective_rewards(char_id, oid1)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
    // Verify unredeemed objective intervals were restored after failed payout
    assert_eq!(
        db_less_mgr.campaign_objectives.read().unwrap()[&oid1].contributors[&char_id].1,
        2
    );

    assert_eq!(
        db_less_mgr
            .redeem_track_rewards(char_id, tid1)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
    // Verify redeemed track intervals were rolled back to 0 after failed payout
    assert_eq!(
        db_less_mgr.campaign_tracks.read().unwrap()[&tid1]
            .redeemed_intervals
            .get(&char_id)
            .copied()
            .unwrap_or(0),
        0
    );

    // Enforce MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT (10)
    let mut cap_objs = Vec::new();
    for i in 0..=MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT {
        cap_objs.push(CampaignObjectiveRecord {
            id: 600 + i as u64,
            campaign_id: 600,
            name: format!("o{i}"),
            target_progress: 100,
            contribution_method: "MineAsteroid".into(),
            state: 1,
            ..Default::default()
        });
    }
    let cap_camp = CampaignRecord {
        id: 600,
        name: "cap".into(),
        state: CAMPAIGN_STATE_ACTIVE,
        ..Default::default()
    };
    db_less_mgr
        .register_campaign(cap_camp, cap_objs, vec![])
        .unwrap();
    db_less_mgr.campaigns.write().unwrap().get_mut(&600).unwrap().state = CAMPAIGN_STATE_ACTIVE;
    for o in db_less_mgr.campaign_objectives.write().unwrap().values_mut() {
        if o.campaign_id == 600 {
            o.state = 1;
        }
    }
    for i in 0..MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT {
        db_less_mgr
            .commit_to_objective(char_id, 600 + i as u64, 2_000, &mut notices)
            .unwrap();
    }
    assert_eq!(
        db_less_mgr
            .commit_to_objective(
                char_id,
                600 + MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT as u64,
                2_000,
                &mut notices
            )
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        ticking_value(
            &TickingContribution {
                key: "k".into(),
                interval_secs: 10,
                evaluations: vec![],
                last_tick_unix: 0,
            },
            5
        ),
        0
    );
}
