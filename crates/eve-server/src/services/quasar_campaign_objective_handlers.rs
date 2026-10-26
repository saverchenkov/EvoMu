//! `quasar_campaign_objective_handlers` — V24 `eve_public.campaign.objective.api` and
//! `eve_public.campaign.track.api` request handlers
//! (`quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj`).
//!
//! The matching `ContributedNotice`s (`objective/api/notices_pb2.pyj`, `track/api/notices_pb2.pyj`)
//! are emitted by [`ProgressionExtraManager::record_gameplay_contribution`] when a committed
//! character's gameplay event credits progress.

#![forbid(unsafe_code)]

use super::quasar_campaign_handlers::{
    any, campaign_tracks, encode_id_field, encode_lifecycle, encode_objective_attributes, encode_objective_stats,
    encode_status_track, encode_ticking_prediction, encode_track_attributes, encode_track_participant_stats,
    extract_campaign_id, extract_campaign_ids, queue_notices,
};
use super::quasar_campaign_state::{objective_id_to_uuid_bytes, require_id, track_id_to_uuid_bytes, CampaignObjectiveRecord};
use super::quasar_progression_extra::ProgressionExtraManager;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use std::sync::Arc;

fn get_objective(mgr: &ProgressionExtraManager, oid: u64) -> Result<CampaignObjectiveRecord, (u32, String)> {
    mgr.campaign_objectives
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&oid)
        .cloned()
        .ok_or((STATUS_NOT_FOUND, "Campaign objective not found".to_string()))
}

/// Registers the objective and track request handlers on the public gateway router.
pub fn register_objective_and_track_handlers(router: &PublicGatewayRouter, manager: Arc<ProgressionExtraManager>) {
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let obj = get_objective(&mgr, oid)?;
            let mut out = Vec::new();
            encode_bytes_field(1, &encode_objective_attributes(&obj), &mut out);
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                let mut e = Vec::new();
                encode_id_field(1, track_id_to_uuid_bytes(t.id), &mut e);
                encode_bytes_field(2, &encode_track_attributes(&t), &mut e);
                encode_bytes_field(2, &e, &mut out);
            }
            Ok(any("eve_public.campaign.objective.api.GetResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let obj = get_objective(&mgr, oid)?;
            let mut out = Vec::new();
            encode_lifecycle(1, obj.state, obj.transition_unix, &mut out);
            encode_bytes_field(2, &encode_objective_stats(&obj), &mut out);
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                encode_bytes_field(3, &encode_status_track(&t), &mut out);
            }
            Ok(any("eve_public.campaign.objective.api.GetStatusResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            let obj = get_objective(&mgr, oid)?;
            let mut out = Vec::new();
            match ProgressionExtraManager::objective_commitment(&obj, char_id) {
                Some(state) => encode_varint_field(1, u64::from(state), &mut out),
                None => encode_varint_field(3, 1, &mut out),
            }
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                let mut e = Vec::new();
                encode_id_field(1, track_id_to_uuid_bytes(t.id), &mut e);
                encode_bytes_field(2, &encode_track_participant_stats(&t, char_id), &mut e);
                encode_bytes_field(2, &e, &mut out);
            }
            Ok(any("eve_public.campaign.objective.api.GetParticipantStatusResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.CommitRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            let mut notices = Vec::new();
            let res = mgr.commit_to_objective(char_id, oid, ctx.now_unix(), &mut notices);
            queue_notices(notices, ctx.now_unix());
            res?;
            Ok(any("eve_public.campaign.objective.api.CommitResponse", Vec::new()))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.ResignRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            let mut notices = Vec::new();
            let res = mgr.resign_from_objective(char_id, oid, ctx.now_unix(), &mut notices);
            queue_notices(notices, ctx.now_unix());
            res?;
            Ok(any("eve_public.campaign.objective.api.ResignResponse", Vec::new()))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            mgr.redeem_objective_rewards(char_id, oid)?;
            Ok(any("eve_public.campaign.objective.api.RedeemRewardsResponse", Vec::new()))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let ids = extract_campaign_ids(dyn_msg.get(1));
            require_id(ids.first().copied(), "campaign objective")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let mut out = Vec::new();
            for oid in ids {
                let obj = get_objective(&mgr, oid)?;
                let mut e = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(oid), &mut e);
                encode_bytes_field(2, &encode_objective_stats(&obj), &mut e);
                encode_bytes_field(1, &e, &mut out);
            }
            Ok(any("eve_public.campaign.objective.api.GetStatsResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let ids = extract_campaign_ids(dyn_msg.get(1));
            require_id(ids.first().copied(), "campaign objective")?;
            let mut out = Vec::new();
            for oid in ids {
                let obj = get_objective(&mgr, oid)?;
                let mut e = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(oid), &mut e);
                match ProgressionExtraManager::objective_participant_state(&obj, char_id) {
                    Some(state) => {
                        let mut ps = Vec::new();
                        encode_varint_field(1, u64::from(state), &mut ps);
                        let progress = obj.contributors.get(&char_id).map(|c| c.0.max(0) as u64).unwrap_or(0);
                        encode_varint_field(2, progress, &mut ps);
                        encode_bytes_field(2, &ps, &mut e);
                    }
                    None => encode_varint_field(3, 1, &mut e),
                }
                encode_bytes_field(1, &e, &mut out);
            }
            Ok(any("eve_public.campaign.objective.api.GetParticipantStatsResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let tid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign track")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let trk = mgr
                .campaign_tracks
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&tid)
                .cloned()
                .ok_or((STATUS_NOT_FOUND, "Campaign track not found".to_string()))?;
            let units = mgr.campaign_committed_units(trk.campaign_id);
            Ok(any(
                "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse",
                encode_ticking_prediction(&trk, units),
            ))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let tid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign track")?;
            mgr.redeem_track_rewards(char_id, tid)?;
            Ok(any("eve_public.campaign.track.api.RedeemParticipantRewardsResponse", Vec::new()))
        }),
    );
}
