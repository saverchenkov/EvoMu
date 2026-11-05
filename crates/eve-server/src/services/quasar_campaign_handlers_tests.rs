use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_campaign_state::{
    ObjectiveParameter, TickingContribution, TickingEvaluation,
};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

fn id_dyn_msg(uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.set(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Message(Box::new(inner)));
    msg
}

#[test]
fn campaign_wire_encoders_and_gateway_request_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 501_u64;
    let oid = 601_u64;
    let tid = 701_u64;
    let char_id = 100_000_001_u32;

    let camp_uuid = campaign_uuid(cid);
    let id_msg = id_dyn_msg(camp_uuid);
    assert_eq!(extract_campaign_id(id_msg.get(1)), Some(cid));
    assert_eq!(extract_campaign_ids(id_msg.get(1)), vec![cid]);

    let camp = CampaignRecord {
        id: cid,
        name: "caldari insurgency".into(),
        description: "desc".into(),
        namespace: "pirate_insurgency".into(),
        scheduled_start_unix: 1_699_000_000,
        scheduled_expiry_unix: 1_710_000_000,
        state: 0,
        transition_unix: 0,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![oid],
    };
    let obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: cid,
        name: "mine asteroid".into(),
        target_progress: 100,
        current_progress: 0,
        committed_participants: vec![char_id],
        resigned_participants: Vec::new(),
        contributors: HashMap::from([(char_id, (25, 2))]),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 50,
        isk_reward_per_interval: 10_000,
        isk_reward_progress_interval: 10,
        state: 0,
        transition_unix: 0,
        parameters: vec![ObjectiveParameter {
            key: "solar_system".into(),
            values: vec!["30000142".into()],
        }],
    };
    let trk = CampaignTrackRecord {
        id: tid,
        campaign_id: cid,
        key: "suppression".into(),
        target_progress_value: 500,
        current_progress: 40,
        participants: HashMap::from([(char_id, 40)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 25_000,
        isk_reward_progress_interval: 20,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "passive".into(),
            interval_secs: 60,
            evaluations: vec![
                TickingEvaluation {
                    key: "base".into(),
                    constant: Some(5),
                    per_unit_value: None,
                },
                TickingEvaluation {
                    key: "per_pilot".into(),
                    constant: None,
                    per_unit_value: Some(2),
                },
            ],
            last_tick_unix: 1_699_000_000,
        }],
        state: 0,
        transition_unix: 0,
    };

    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_campaign_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));
    mgr.register_campaign(camp.clone(), vec![obj.clone()], vec![trk.clone()])
        .expect("register campaign");

    assert!(!encode_campaign_stats(&[&obj]).is_empty());
    assert!(!encode_campaign_attributes(&camp, &[&obj]).is_empty());
    assert!(!encode_objective_stats(&obj).is_empty());
    assert!(!encode_objective_attributes(&obj).is_empty());
    assert!(!encode_track_attributes(&trk).is_empty());
    assert!(!encode_track_stats(&trk).is_empty());
    assert!(!encode_track_participant_stats(&trk, char_id).is_empty());
    assert!(!encode_track_participant_stats(&trk, 999_999).is_empty());
    assert!(!encode_status_track(&trk).is_empty());
    assert!(!encode_ticking_prediction(&trk, 3).is_empty());

    let auth = AuthoritativeContext::for_session(
        Some(1),
        Some(char_id),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Req");
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    assert!(handlers["eve_public.campaign.api.GetAllRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllForNamespaceRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetStatusRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetStatsRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetLimitsRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](&auth, &any, &empty).is_ok());
}
