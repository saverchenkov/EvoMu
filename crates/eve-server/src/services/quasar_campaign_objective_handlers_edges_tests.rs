//! Edge-case and error-path tests for `quasar_campaign_objective_handlers`.

use super::*;
use crate::quasar_gateway::STATUS_BAD_REQUEST;
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

fn ids_dyn_msg(uuids: &[[u8; 16]]) -> DynamicQuasarMessage {
    let items = uuids
        .iter()
        .map(|u| {
            let mut inner = DynamicQuasarMessage::new("Identifier");
            inner.set(1, DynamicValue::Bytes(u.to_vec()));
            DynamicValue::Message(Box::new(inner))
        })
        .collect();
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Repeated(items));
    msg
}

#[test]
fn campaign_objective_and_track_error_paths_and_participant_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 901_u64;
    let oid = 902_u64;
    let tid = 903_u64;
    let char_id = 90_000_001_u32;
    let other_char_id = 90_000_002_u32;

    let camp = CampaignRecord {
        id: cid,
        name: "caldari front".into(),
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
        name: "defend plex".into(),
        target_progress: 100,
        current_progress: 10,
        committed_participants: Vec::new(),
        resigned_participants: Vec::new(),
        contributors: HashMap::new(),
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
        current_progress: 10,
        participants: HashMap::new(),
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
    mgr.register_campaign(camp, vec![obj], vec![trk]).unwrap();

    let no_char_auth = AuthoritativeContext::for_session_at(
        Some(1),
        None,
        None,
        0,
        "tranquility",
        vec![],
        1_700_000_000,
    );
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
    let other_auth = AuthoritativeContext::for_session_at(
        Some(2),
        Some(other_char_id),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
        1_700_000_000,
    )
    .with_service_roles(u64::MAX);

    let any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Req");
    let obj_msg = id_dyn_msg(objective_id_to_uuid_bytes(oid));
    let missing_obj_msg = id_dyn_msg(objective_id_to_uuid_bytes(999_999));
    let trk_msg = id_dyn_msg(track_id_to_uuid_bytes(tid));
    let missing_trk_msg = id_dyn_msg(track_id_to_uuid_bytes(999_999));
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    // 1. Missing character -> STATUS_FORBIDDEN on character-scoped handlers
    for name in [
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        "eve_public.campaign.objective.api.CommitRequest",
        "eve_public.campaign.objective.api.ResignRequest",
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
    ] {
        let (code, _) = handlers[name](&no_char_auth, &any, &obj_msg).unwrap_err();
        assert_eq!(code, STATUS_FORBIDDEN, "{name} should require active character");
    }

    // 2. Missing/malformed UUID -> STATUS_INVALID_ARGUMENT across all 10 handlers
    for name in [
        "eve_public.campaign.objective.api.GetRequest",
        "eve_public.campaign.objective.api.GetStatusRequest",
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        "eve_public.campaign.objective.api.CommitRequest",
        "eve_public.campaign.objective.api.ResignRequest",
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        "eve_public.campaign.objective.api.GetStatsRequest",
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
    ] {
        let (code, _) = handlers[name](&auth, &any, &empty_msg).unwrap_err();
        assert_eq!(code, STATUS_BAD_REQUEST, "{name} should reject empty ID");
    }

    // 3. Unknown objective ID -> STATUS_NOT_FOUND
    for name in [
        "eve_public.campaign.objective.api.GetRequest",
        "eve_public.campaign.objective.api.GetStatusRequest",
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        "eve_public.campaign.objective.api.CommitRequest",
        "eve_public.campaign.objective.api.ResignRequest",
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        "eve_public.campaign.objective.api.GetStatsRequest",
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
    ] {
        let (code, _) = handlers[name](&auth, &any, &missing_obj_msg).unwrap_err();
        assert_eq!(code, STATUS_NOT_FOUND, "{name} should return NOT_FOUND");
    }

    // 4. Unknown track ID -> STATUS_NOT_FOUND
    for name in [
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
    ] {
        let (code, _) = handlers[name](&auth, &any, &missing_trk_msg).unwrap_err();
        assert_eq!(code, STATUS_NOT_FOUND, "{name} should return NOT_FOUND");
    }

    // 5. GetParticipantStatsRequest when NOT committed -> None arm (field 3)
    let stats_none = handlers["eve_public.campaign.objective.api.GetParticipantStatsRequest"](
        &other_auth,
        &any,
        &obj_msg,
    )
    .expect("uncommitted participant stats ok");
    assert_eq!(
        stats_none.type_url,
        "type.googleapis.com/eve_public.campaign.objective.api.GetParticipantStatsResponse"
    );

    // 6. Commit char_id -> GetParticipantStatusRequest hits Some(state) arm (field 1),
    //    and GetParticipantStatsRequest hits Some(state) with 0 contributions (.unwrap_or(0)).
    handlers["eve_public.campaign.objective.api.CommitRequest"](&auth, &any, &obj_msg).unwrap();
    let status_some = handlers["eve_public.campaign.objective.api.GetParticipantStatusRequest"](
        &auth, &any, &obj_msg,
    )
    .expect("committed participant status ok");
    assert_eq!(
        status_some.type_url,
        "type.googleapis.com/eve_public.campaign.objective.api.GetParticipantStatusResponse"
    );
    let stats_zero = handlers["eve_public.campaign.objective.api.GetParticipantStatsRequest"](
        &auth, &any, &obj_msg,
    )
    .expect("committed participant zero-progress stats ok");
    assert_eq!(
        stats_zero.type_url,
        "type.googleapis.com/eve_public.campaign.objective.api.GetParticipantStatsResponse"
    );

    // 7. Batch GetStatsRequest / GetParticipantStatsRequest with second ID missing -> NOT_FOUND
    let batch_missing = ids_dyn_msg(&[
        objective_id_to_uuid_bytes(oid),
        objective_id_to_uuid_bytes(999_998),
    ]);
    assert_eq!(
        handlers["eve_public.campaign.objective.api.GetStatsRequest"](&auth, &any, &batch_missing)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.campaign.objective.api.GetParticipantStatsRequest"](
            &auth,
            &any,
            &batch_missing
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 8. Resigning when not committed & redeeming with no eligible rewards propagate errors
    assert!(
        handlers["eve_public.campaign.objective.api.ResignRequest"](&other_auth, &any, &obj_msg)
            .is_err()
    );
    assert!(
        handlers["eve_public.campaign.objective.api.RedeemRewardsRequest"](
            &other_auth,
            &any,
            &obj_msg
        )
        .is_err()
    );
    assert!(
        handlers["eve_public.campaign.track.api.RedeemParticipantRewardsRequest"](
            &other_auth,
            &any,
            &trk_msg
        )
        .is_err()
    );
}
