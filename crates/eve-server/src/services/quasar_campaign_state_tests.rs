use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

#[test]
fn campaign_state_lifecycle_contributions_commit_resign_and_redemption() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mgr = ProgressionExtraManager::new(Some(std::sync::Arc::clone(&ctx.db)));
    let char_id = 90_000_001_u32;
    let cid = 901_u64;
    let oid = 902_u64;
    let tid = 903_u64;

    assert!(contribution_method_params("MineAsteroid").is_some());
    assert!(contribution_method_params("UnknownMethod").is_none());
    assert_eq!(require_id(Some(42), "camp").unwrap(), 42);
    assert!(require_id(None, "camp").is_err());

    let tc = TickingContribution {
        key: "tick".into(),
        interval_secs: 60,
        evaluations: vec![
            TickingEvaluation {
                key: "c".into(),
                constant: Some(10),
                per_unit_value: None,
            },
            TickingEvaluation {
                key: "u".into(),
                constant: None,
                per_unit_value: Some(5),
            },
            TickingEvaluation {
                key: "z".into(),
                constant: None,
                per_unit_value: None,
            },
        ],
        last_tick_unix: 1000,
    };
    assert_eq!(ticking_value(&tc, 3), 25);

    // Invalid objective campaign_id / method / parameter rejected
    let camp = CampaignRecord {
        id: cid,
        name: "mining op".into(),
        description: "desc".into(),
        namespace: "mining".into(),
        scheduled_start_unix: 1000,
        scheduled_expiry_unix: 2000,
        ..Default::default()
    };
    let mut bad_obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: 999,
        name: "mine".into(),
        target_progress: 50,
        contribution_method: "MineAsteroid".into(),
        ..Default::default()
    };
    assert!(mgr.register_campaign(camp.clone(), vec![bad_obj.clone()], vec![]).is_err());
    bad_obj.campaign_id = cid;
    bad_obj.contribution_method = "NotAMethod".into();
    assert!(mgr.register_campaign(camp.clone(), vec![bad_obj.clone()], vec![]).is_err());

    let obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: cid,
        name: "mine".into(),
        target_progress: 50,
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 30,
        isk_reward_per_interval: 10_000,
        isk_reward_progress_interval: 10,
        parameters: vec![ObjectiveParameter {
            key: "solar_system".into(),
            values: vec!["30000142".into()],
        }],
        ..Default::default()
    };
    let trk = CampaignTrackRecord {
        id: tid,
        campaign_id: cid,
        key: "main".into(),
        target_progress_value: 100,
        max_participant_contribution: 40,
        isk_reward_per_interval: 20_000,
        isk_reward_progress_interval: 10,
        ticking: vec![tc],
        ..Default::default()
    };
    mgr.register_campaign(camp.clone(), vec![obj], vec![trk]).unwrap();
    // Re-registering bumps etag and queues version notice
    let prev_obj = mgr.campaign_objectives.read().unwrap()[&oid].clone();
    let prev_trk = mgr.campaign_tracks.read().unwrap()[&tid].clone();
    mgr.register_campaign(camp, vec![prev_obj], vec![prev_trk])
        .unwrap();

    // Lifecycle at t=1060 starts campaign, ticks track once, and emits StartedNotice + CurrentVersionNotice
    let notices = mgr.evaluate_campaign_lifecycle(1060);
    assert!(notices.len() >= 2);

    let mut commit_notices = Vec::new();
    mgr.commit_to_objective(char_id, oid, 1060, &mut commit_notices).unwrap();
    assert_eq!(mgr.campaign_committed_units(cid), 1);

    // Matching gameplay event credits objective (capped at max_progress_per_participant=30) and track
    let ev = CampaignEvent {
        method: "MineAsteroid",
        type_id: Some(1228),
        solar_system_id: Some(30_000_142),
        faction_id: None,
    };
    mgr.on_campaign_gameplay_event(char_id, ev, 40, 1120, &ctx.quasar_gateway);

    let paid_obj = mgr.redeem_objective_rewards(char_id, oid).unwrap();
    assert_eq!(paid_obj, 30_000);
    assert!(mgr.redeem_objective_rewards(char_id, oid).is_err());

    let paid_trk = mgr.redeem_track_rewards(char_id, tid).unwrap();
    assert_eq!(paid_trk, 60_000);
    assert!(mgr.redeem_track_rewards(char_id, tid).is_err());

    mgr.resign_from_objective(char_id, oid, 1120, &mut commit_notices).unwrap();
    assert!(mgr.resign_from_objective(char_id, oid, 1120, &mut commit_notices).is_err());

    // Advance past expiry (t=2500) -> expires campaign
    let exp_notices = mgr.evaluate_campaign_lifecycle(2500);
    assert!(!exp_notices.is_empty());
    assert_eq!(mgr.campaigns.read().unwrap()[&cid].state, CAMPAIGN_STATE_EXPIRED);
}
