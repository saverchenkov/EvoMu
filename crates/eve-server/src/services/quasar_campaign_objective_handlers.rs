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
    campaign_tracks, extract_campaign_id, extract_campaign_ids, lifecycle_builder,
    objective_attributes_builder, objective_stats_builder, queue_notices, ticking_prediction_builder,
    track_attributes_builder, track_participant_stats_builder, track_stats_builder,
};
use super::quasar_campaign_state::{objective_id_to_uuid_bytes, require_id, track_id_to_uuid_bytes, CampaignObjectiveRecord};
use super::quasar_progression_extra::ProgressionExtraManager;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_proto::pb;
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
            let mut resp = pb::message("eve_public.campaign.objective.api.GetResponse")
                .set("attributes", objective_attributes_builder(&obj));
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                resp = resp.add_with("tracks", |e| {
                    e.set("id.uuid", track_id_to_uuid_bytes(t.id))
                        .set("attributes", track_attributes_builder(&t))
                });
            }
            Ok(resp.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let obj = get_objective(&mgr, oid)?;
            let mut resp = pb::message("eve_public.campaign.objective.api.GetStatusResponse")
                .set(
                    "state",
                    lifecycle_builder(
                        "eve_public.campaign.objective.LifecycleState",
                        obj.state,
                        obj.transition_unix,
                    ),
                )
                .set("stats", objective_stats_builder(&obj));
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                resp = resp.add_with("tracks", |tr| {
                    tr.set("id.uuid", track_id_to_uuid_bytes(t.id))
                        .set(
                            "state",
                            lifecycle_builder(
                                "eve_public.campaign.track.LifecycleState",
                                t.state,
                                t.transition_unix,
                            ),
                        )
                        .set("stats", track_stats_builder(&t))
                });
            }
            Ok(resp.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            let obj = get_objective(&mgr, oid)?;
            let mut resp = pb::message("eve_public.campaign.objective.api.GetParticipantStatusResponse");
            resp = match ProgressionExtraManager::objective_commitment(&obj, char_id) {
                Some(state) => resp.set("commitment_state", state as i32),
                None => resp.set("no_commitment", true),
            };
            for t in campaign_tracks(&mgr, obj.campaign_id) {
                resp = resp.add_with("tracks", |e| {
                    e.set("id.uuid", track_id_to_uuid_bytes(t.id))
                        .set("stats", track_participant_stats_builder(&t, char_id))
                });
            }
            Ok(resp.must_to_any())
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
            Ok(pb::message("eve_public.campaign.objective.api.CommitResponse").must_to_any())
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
            Ok(pb::message("eve_public.campaign.objective.api.ResignResponse").must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let oid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign objective")?;
            mgr.redeem_objective_rewards(char_id, oid)?;
            Ok(pb::message("eve_public.campaign.objective.api.RedeemRewardsResponse").must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let ids = extract_campaign_ids(dyn_msg.get(1));
            require_id(ids.first().copied(), "campaign objective")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let mut resp = pb::message("eve_public.campaign.objective.api.GetStatsResponse");
            for oid in ids {
                let obj = get_objective(&mgr, oid)?;
                resp = resp.add_with("objective_stats", |e| {
                    e.set("objective.uuid", objective_id_to_uuid_bytes(oid))
                        .set("stats", objective_stats_builder(&obj))
                });
            }
            Ok(resp.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let ids = extract_campaign_ids(dyn_msg.get(1));
            require_id(ids.first().copied(), "campaign objective")?;
            let mut resp = pb::message("eve_public.campaign.objective.api.GetParticipantStatsResponse");
            for oid in ids {
                let obj = get_objective(&mgr, oid)?;
                resp = resp.add_with("participant_stats", |e| {
                    let e = e.set("objective.uuid", objective_id_to_uuid_bytes(oid));
                    match ProgressionExtraManager::objective_participant_state(&obj, char_id) {
                        Some(state) => {
                            let progress = obj.contributors.get(&char_id).map(|c| c.0.max(0) as u64).unwrap_or(0);
                            e.set_with("stats", |ps| {
                                ps.set("state", state as i32)
                                    .set("current_participant_progress", progress)
                            })
                        }
                        None => e.set("no_participation", true),
                    }
                });
            }
            Ok(resp.must_to_any())
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
            Ok(ticking_prediction_builder(&trk, units).must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let tid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign track")?;
            mgr.redeem_track_rewards(char_id, tid)?;
            Ok(pb::message("eve_public.campaign.track.api.RedeemParticipantRewardsResponse").must_to_any())
        }),
    );
}

#[cfg(test)] #[path = "quasar_campaign_objective_handlers_tests.rs"]
mod quasar_campaign_objective_handlers_tests;

#[cfg(test)]
#[path = "quasar_campaign_objective_handlers_edges_tests.rs"]
mod quasar_campaign_objective_handlers_edges_tests;

