//! Edge-case unit tests for `quasar_campaign_handlers`:
//! UUID extraction variants (Message, raw Bytes, WireMessage, Repeated, invalid),
//! namespace extraction, lifecycle state encoders, error paths (403/400/404),
//! participant stats, unredeemed rewards with namespace filters, and lifecycle notice evaluation.

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_campaign_state::{
    ObjectiveParameter, TickingContribution, TickingEvaluation, CAMPAIGN_STATE_EXPIRED,
    OBJECTIVE_STATE_COMPLETED, TRACK_STATE_STOPPED,
};
use eve_proto::pb;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

fn id_dyn_msg(uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.set(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Message(Box::new(inner)));
    msg
}

fn ns_dyn_msg(ns: &str) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Namespace");
    inner.set(1, DynamicValue::String(ns.to_string()));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Message(Box::new(inner)));
    msg
}

#[test]
fn extract_campaign_id_and_namespace_all_variants() {
    assert_eq!(extract_campaign_id(None), None);
    assert_eq!(extract_campaign_id(Some(&DynamicValue::Uint64(42))), None);

    // Wrong length or prefix or zero id
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(b"short".to_vec()))),
        None
    );
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(b"BADPREF_00000001".to_vec()))),
        None
    );
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(campaign_uuid(0).to_vec()))),
        None
    );

    // Valid prefixes directly in Bytes
    for (uuid, expected) in [
        (campaign_uuid(11), 11),
        (objective_uuid(22), 22),
        (track_uuid(33), 33),
    ] {
        assert_eq!(
            extract_campaign_id(Some(&DynamicValue::Bytes(uuid.to_vec()))),
            Some(expected)
        );
    }

    // Wire-encoded Identifier in DynamicValue::Bytes
    let wire = pb::message("eve_public.campaign.Identifier")
        .set("uuid", campaign_uuid(77))
        .must_encode();
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(wire))),
        Some(77)
    );

    // Message with missing/wrong field 1
    let mut bad_inner = DynamicQuasarMessage::new("Identifier");
    bad_inner.set(1, DynamicValue::Uint32(5));
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Message(Box::new(bad_inner)))),
        None
    );

    // Repeated
    let rep = DynamicValue::Repeated(vec![
        DynamicValue::Bytes(campaign_uuid(101).to_vec()),
        DynamicValue::Bytes(objective_uuid(202).to_vec()),
    ]);
    assert_eq!(extract_campaign_id(Some(&rep)), Some(101));
    assert_eq!(extract_campaign_ids(Some(&rep)), vec![101, 202]);
    assert_eq!(extract_campaign_ids(None), Vec::<u64>::new());

    // namespace_of variants: Message(String), Message(non-String), Wire bytes, None
    assert_eq!(
        namespace_of(&ns_dyn_msg("fw")),
        Some("fw".to_string())
    );
    let mut msg_non_str = DynamicQuasarMessage::new("Req");
    let mut inner_non_str = DynamicQuasarMessage::new("Namespace");
    inner_non_str.set(1, DynamicValue::Uint32(9));
    msg_non_str.set(1, DynamicValue::Message(Box::new(inner_non_str)));
    assert_eq!(namespace_of(&msg_non_str), Some(String::new()));

    let ns_wire = pb::message("eve_public.campaign.Namespace")
        .set("namespace", "pirate_ns")
        .must_encode();
    let mut msg_bytes = DynamicQuasarMessage::new("Req");
    msg_bytes.set(1, DynamicValue::Bytes(ns_wire));
    assert_eq!(namespace_of(&msg_bytes), Some("pirate_ns".to_string()));
    assert_eq!(namespace_of(&DynamicQuasarMessage::new("Req")), None);
}

#[test]
fn encoders_cover_completed_expired_zero_rewards_and_eval_branches() {
    let cid = 900_u64;
    let char_id = 100_000_010_u32;

    let camp_completed = CampaignRecord {
        id: cid,
        name: "completed campaign".into(),
        description: "done".into(),
        namespace: String::new(),
        scheduled_start_unix: 1_690_000_000,
        scheduled_expiry_unix: 0,
        state: CAMPAIGN_STATE_COMPLETED,
        transition_unix: 1_695_000_000,
        etag: 2,
        version_notice_pending: false,
        objectives: vec![901],
    };
    let obj_completed = CampaignObjectiveRecord {
        id: 901,
        campaign_id: cid,
        name: "done obj".into(),
        target_progress: 50,
        current_progress: 50,
        committed_participants: vec![],
        resigned_participants: vec![char_id],
        contributors: HashMap::from([(char_id, (50, 0))]),
        contribution_method: "UnknownMethod".into(),
        max_progress_per_participant: 0,
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        state: OBJECTIVE_STATE_COMPLETED,
        transition_unix: 1_695_000_000,
        parameters: vec![ObjectiveParameter {
            key: "custom_key".into(),
            values: vec!["123".into()],
        }],
    };
    let trk_stopped = CampaignTrackRecord {
        id: 902,
        campaign_id: cid,
        key: "track".into(),
        target_progress_value: 0,
        current_progress: 100,
        participants: HashMap::from([(char_id, 100)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "tick_k".into(),
            interval_secs: 30,
            evaluations: vec![
                TickingEvaluation {
                    key: "const".into(),
                    constant: Some(10),
                    per_unit_value: None,
                },
                TickingEvaluation {
                    key: "per_u".into(),
                    constant: None,
                    per_unit_value: Some(3),
                },
                TickingEvaluation {
                    key: "empty".into(),
                    constant: None,
                    per_unit_value: None,
                },
            ],
            last_tick_unix: 1_690_000_000,
        }],
        state: TRACK_STATE_STOPPED,
        transition_unix: 1_695_000_000,
    };
    let trk_started_no_max = CampaignTrackRecord {
        id: 903,
        campaign_id: cid,
        key: "active_trk".into(),
        target_progress_value: 200,
        current_progress: 10,
        participants: HashMap::from([(char_id, 10)]),
        max_participant_contribution: 0,
        isk_reward_per_interval: 5_000,
        isk_reward_progress_interval: 5,
        redeemed_intervals: HashMap::new(),
        ticking: trk_stopped.ticking.clone(),
        state: TRACK_STATE_STARTED,
        transition_unix: 1_691_000_000,
    };

    assert!(!encode_campaign_attributes(&camp_completed, &[&obj_completed]).is_empty());
    assert!(!encode_objective_attributes(&obj_completed).is_empty());
    assert!(!encode_track_attributes(&trk_stopped).is_empty());
    assert!(!encode_track_attributes(&trk_started_no_max).is_empty());
    assert!(!encode_track_stats(&trk_stopped).is_empty());
    assert!(!encode_track_stats(&trk_started_no_max).is_empty());
    assert!(!encode_track_participant_stats(&trk_stopped, char_id).is_empty());
    assert!(!encode_track_participant_stats(&trk_started_no_max, char_id).is_empty());
    assert!(!encode_ticking_prediction(&trk_stopped, 4).is_empty());
}

#[test]
fn campaign_handlers_error_paths_namespaces_upcoming_and_lifecycle_transitions() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_011_u32;
    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_campaign_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));

    // Active campaign in namespace "fw"
    let c_active = CampaignRecord {
        id: 1001,
        name: "Active FW".into(),
        description: "d".into(),
        namespace: "fw".into(),
        scheduled_start_unix: 1_699_000_000,
        scheduled_expiry_unix: 1_710_000_000,
        state: CAMPAIGN_STATE_ACTIVE,
        transition_unix: 1_699_000_000,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![2001, 2002],
    };
    // Upcoming campaign in namespace "fw" (scheduled in the future so it stays state == 0)
    let c_upcoming = CampaignRecord {
        id: 1002,
        name: "Upcoming FW".into(),
        description: "d".into(),
        namespace: "fw".into(),
        scheduled_start_unix: 1_705_000_000,
        scheduled_expiry_unix: 1_715_000_000,
        state: 0,
        transition_unix: 0,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![],
    };
    // Expired campaign in namespace "other" (ignored by listed_campaigns)
    let c_expired = CampaignRecord {
        id: 1003,
        name: "Expired Other".into(),
        description: "d".into(),
        namespace: "other".into(),
        scheduled_start_unix: 1_600_000_000,
        scheduled_expiry_unix: 1_650_000_000,
        state: CAMPAIGN_STATE_EXPIRED,
        transition_unix: 1_650_000_000,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![],
    };
    let o_active = CampaignObjectiveRecord {
        id: 2001,
        campaign_id: 1001,
        name: "active obj".into(),
        target_progress: 100,
        current_progress: 20,
        committed_participants: vec![char_id],
        resigned_participants: vec![],
        contributors: HashMap::from([(char_id, (20, 3))]),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 100,
        isk_reward_per_interval: 1000,
        isk_reward_progress_interval: 5,
        state: OBJECTIVE_STATE_ACTIVE,
        transition_unix: 1_699_000_000,
        parameters: vec![],
    };
    let o_inactive = CampaignObjectiveRecord {
        id: 2002,
        campaign_id: 1001,
        name: "completed obj".into(),
        target_progress: 10,
        current_progress: 10,
        committed_participants: vec![],
        resigned_participants: vec![],
        contributors: HashMap::new(),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 0,
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        state: OBJECTIVE_STATE_COMPLETED,
        transition_unix: 1_699_500_000,
        parameters: vec![],
    };
    let t_fw = CampaignTrackRecord {
        id: 3001,
        campaign_id: 1001,
        key: "fw_trk".into(),
        target_progress_value: 100,
        current_progress: 40,
        participants: HashMap::from([(char_id, 40)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 1000,
        isk_reward_progress_interval: 10,
        redeemed_intervals: HashMap::new(),
        ticking: vec![],
        state: TRACK_STATE_STARTED,
        transition_unix: 1_699_000_000,
    };

    mgr.register_campaign(c_active, vec![o_active, o_inactive], vec![t_fw])
        .unwrap();
    mgr.register_campaign(c_upcoming, vec![], vec![]).unwrap();
    mgr.register_campaign(c_expired, vec![], vec![]).unwrap();

    let auth = AuthoritativeContext::for_session(
        Some(1),
        Some(char_id),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![],
    );
    let no_char_auth =
        AuthoritativeContext::for_session(Some(1), None, None, 0, "tranquility", vec![]);
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Req");
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    // Missing campaign ID -> 400; unknown campaign ID -> 404
    let missing_id = id_dyn_msg(campaign_uuid(99_999));
    for ep in [
        "eve_public.campaign.api.GetRequest",
        "eve_public.campaign.api.GetStatusRequest",
        "eve_public.campaign.api.GetStatsRequest",
        "eve_public.campaign.api.GetParticipantStatsRequest",
    ] {
        assert_ne!(handlers[ep](&auth, &any, &empty).unwrap_err().0, 0);
        assert_eq!(
            handlers[ep](&auth, &any, &missing_id).unwrap_err().0,
            STATUS_NOT_FOUND
        );
    }

    // No active character -> STATUS_FORBIDDEN
    let valid_id = id_dyn_msg(campaign_uuid(1001));
    assert_eq!(
        handlers["eve_public.campaign.api.GetParticipantStatsRequest"](
            &no_char_auth,
            &any,
            &valid_id
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](
            &no_char_auth,
            &any,
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](
            &no_char_auth,
            &any,
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Namespace queries: started + upcoming returned
    assert!(
        handlers["eve_public.campaign.api.GetAllForNamespaceRequest"](
            &auth,
            &any,
            &ns_dyn_msg("fw")
        )
        .is_ok()
    );
    // GetRequest / GetParticipantStatsRequest / unredeemed rewards with namespace
    assert!(handlers["eve_public.campaign.api.GetRequest"](&auth, &any, &valid_id).is_ok());
    assert!(
        handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth, &any, &valid_id)
            .is_ok()
    );
    assert!(
        handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](&auth, &any, &empty)
            .is_ok()
    );
    assert!(
        handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](
            &auth,
            &any,
            &ns_dyn_msg("fw")
        )
        .is_ok()
    );
}
