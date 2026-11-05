use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_campaign_state::{
    CampaignRecord, CampaignTrackRecord, TickingContribution, TickingEvaluation,
};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny};
use std::collections::HashMap;

fn id_dyn_msg(uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.set(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Message(Box::new(inner)));
    msg
}

#[test]
fn campaign_objective_and_track_gateway_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 801_u64;
    let oid = 802_u64;
    let tid = 803_u64;
    let char_id = 90_000_001_u32;

    let camp = CampaignRecord {
        id: cid,
        name: "amarr front".into(),
        description: "desc".into(),
        namespace: "fw".into(),
        scheduled_start_unix: 1_699_000_000,
        scheduled_expiry_unix: 0,
        state: 1,
        transition_unix: 0,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![oid],
    };
    let obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: cid,
        name: "capture plex".into(),
        target_progress: 100,
        current_progress: 20,
        committed_participants: Vec::new(),
        resigned_participants: Vec::new(),
        contributors: HashMap::from([(char_id, (20, 1))]),
        contribution_method: "CaptureFWComplex".into(),
        max_progress_per_participant: 0,
        isk_reward_per_interval: 50_000,
        isk_reward_progress_interval: 10,
        state: 1,
        transition_unix: 0,
        parameters: Vec::new(),
    };
    let trk = CampaignTrackRecord {
        id: tid,
        campaign_id: cid,
        key: "frontline".into(),
        target_progress_value: 200,
        current_progress: 20,
        participants: HashMap::from([(char_id, 20)]),
        max_participant_contribution: 0,
        isk_reward_per_interval: 100_000,
        isk_reward_progress_interval: 10,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "tick".into(),
            interval_secs: 60,
            evaluations: vec![TickingEvaluation {
                key: "c".into(),
                constant: Some(1),
                per_unit_value: None,
            }],
            last_tick_unix: 1_699_000_000,
        }],
        state: 1,
        transition_unix: 0,
    };
    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_objective_and_track_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));
    mgr.register_campaign(camp, vec![obj], vec![trk])
        .unwrap();

    let auth = AuthoritativeContext::for_session_at(
        Some(1),
        Some(char_id),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
        1_700_000_000,
    )
    .with_service_roles(u64::MAX);
    let any = GoogleAny::default();
    let obj_msg = id_dyn_msg(objective_id_to_uuid_bytes(oid));
    let trk_msg = id_dyn_msg(track_id_to_uuid_bytes(tid));
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    assert!(handlers["eve_public.campaign.objective.api.GetRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.GetStatusRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.GetParticipantStatusRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.CommitRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.GetStatsRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.GetParticipantStatsRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.RedeemRewardsRequest"](&auth, &any, &obj_msg).is_ok());
    assert!(handlers["eve_public.campaign.objective.api.ResignRequest"](&auth, &any, &obj_msg).is_ok());

    assert!(handlers["eve_public.campaign.track.api.GetTickingContributionsPredictionRequest"](&auth, &any, &trk_msg).is_ok());
    assert!(handlers["eve_public.campaign.track.api.RedeemParticipantRewardsRequest"](&auth, &any, &trk_msg).is_ok());
}
