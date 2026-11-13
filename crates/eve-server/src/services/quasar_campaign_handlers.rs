//! `quasar_campaign_handlers` — V24 `eve_public.campaign.api` request handlers plus the shared wire
//! encoders for campaign / objective / track messages
//! (`quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj`).
//!
//! Field numbers follow the V24 descriptors in `data/quasar_descriptors.sqlite`
//! (`eve_public/campaign/campaign.proto`, `objective/objective.proto`, `track/track.proto`).
//! Every read handler first runs [`ProgressionExtraManager::evaluate_campaign_lifecycle`], so
//! campaign Started / Completed / Expired / CurrentVersion notices are queued from the real
//! state transition the server clock produced.

#![forbid(unsafe_code)]

use super::quasar_campaign_state::{
    campaign_id_to_uuid_bytes, contribution_method_params, encode_identifier, objective_id_to_uuid_bytes, require_id, ticking_value,
    track_id_to_uuid_bytes, CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord, PendingNotices,
    CAMPAIGN_STATE_ACTIVE, CAMPAIGN_STATE_COMPLETED, MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT, OBJECTIVE_STATE_ACTIVE,
    TRACK_STATE_STARTED,
};
use super::quasar_progression_extra::ProgressionExtraManager;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicValue, GoogleAny, GoogleDuration, GoogleTimestamp, WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::Arc;

pub use super::quasar_campaign_state::{
    campaign_id_to_uuid_bytes as campaign_uuid, objective_id_to_uuid_bytes as objective_uuid,
    track_id_to_uuid_bytes as track_uuid,
};

fn id_from_uuid(b: &[u8]) -> Option<u64> {
    if b.len() != 16 || !matches!(&b[0..8], b"CAMPAIGN" | b"CAMPOBJ_" | b"CAMPTRK_") {
        return None;
    }
    let mut lo = [0u8; 8];
    lo.copy_from_slice(&b[8..16]);
    Some(u64::from_be_bytes(lo)).filter(|v| *v != 0)
}

/// Extracts the numeric id from a decoded `{campaign,objective,track}.Identifier { bytes uuid = 1 }`
/// (or its raw wire bytes when no descriptor was available).
pub fn extract_campaign_id(val: Option<&DynamicValue>) -> Option<u64> {
    match val? {
        DynamicValue::Message(m) => match m.get(1) {
            Some(DynamicValue::Bytes(b)) => id_from_uuid(b),
            _ => None,
        },
        DynamicValue::Bytes(b) => id_from_uuid(b).or_else(|| {
            let wm = WireMessage::decode(b).ok()?;
            wm.fields.iter().find_map(|f| match (&f.value, f.field_number) {
                (WireValue::LengthDelimited(inner), 1) => id_from_uuid(inner),
                _ => None,
            })
        }),
        DynamicValue::Repeated(items) => extract_campaign_id(items.first()),
        _ => None,
    }
}

/// Every id of a repeated Identifier field.
pub fn extract_campaign_ids(val: Option<&DynamicValue>) -> Vec<u64> {
    match val {
        Some(DynamicValue::Repeated(items)) => items.iter().filter_map(|v| extract_campaign_id(Some(v))).collect(),
        other => extract_campaign_id(other).into_iter().collect(),
    }
}

pub fn queue_notices(notices: PendingNotices, now: u64) {
    for (scope, payload) in notices {
        crate::services::quasar_social_chat::ctx_queue_notice(scope, payload, now);
    }
}

pub fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
}

fn ts_unix(secs: u64) -> GoogleTimestamp {
    GoogleTimestamp::from_unix(secs as i64, 0)
}

fn id_builder(type_name: &str, uuid: [u8; 16]) -> ProtoBuilder {
    pb::message(type_name).set("uuid", uuid)
}

fn lifecycle_builder(type_name: &str, state: u32, transition_unix: u64) -> ProtoBuilder {
    pb::message(type_name)
        .set("state", state as i32)
        .when(transition_unix > 0, |b| b.set("transition_time", ts_unix(transition_unix)))
}

fn wrap_wire_bytes(field: u32, bytes: Vec<u8>, out: &mut Vec<u8>) {
    let msg = WireMessage { fields: vec![WireField { field_number: field, value: WireValue::LengthDelimited(bytes) }] };
    out.extend_from_slice(&msg.encode());
}

pub fn encode_id_field(field: u32, uuid: [u8; 16], out: &mut Vec<u8>) {
    wrap_wire_bytes(field, encode_identifier(&uuid), out);
}

/// `LifecycleState { state = 1; transition_time = 2 }` (same shape for campaign/objective/track).
pub fn encode_lifecycle(field: u32, state: u32, transition_unix: u64, out: &mut Vec<u8>) {
    wrap_wire_bytes(field, lifecycle_builder("eve_public.campaign.LifecycleState", state, transition_unix).must_encode(), out);
}

fn campaign_stats_builder(objs: &[&CampaignObjectiveRecord]) -> ProtoBuilder {
    let progress: u64 = objs.iter().map(|o| o.current_progress.max(0) as u64).sum();
    let mut contributors: Vec<u32> = objs.iter().flat_map(|o| o.contributors.keys().copied()).collect();
    contributors.sort_unstable();
    contributors.dedup();
    pb::message("eve_public.campaign.Stats")
        .set("current_campaign_progress", progress)
        .set("total_contributors", contributors.len() as u32)
}

/// `campaign.Stats { current_campaign_progress = 1; total_contributors = 2 }`.
pub fn encode_campaign_stats(objs: &[&CampaignObjectiveRecord]) -> Vec<u8> {
    campaign_stats_builder(objs).must_encode()
}

fn campaign_attributes_builder(c: &CampaignRecord, objs: &[&CampaignObjectiveRecord]) -> ProtoBuilder {
    let target_sum: u64 = objs.iter().map(|o| o.target_progress.max(0) as u64).sum();
    pb::message("eve_public.campaign.Attributes")
        .set("total_objectives", objs.len() as u32)
        .set("state", c.state as i32)
        .set("scheduled_start_time", ts_unix(c.scheduled_start_unix))
        .when(c.scheduled_expiry_unix > 0, |b| b.set("scheduled_expiry_time", ts_unix(c.scheduled_expiry_unix)))
        .when(c.state == CAMPAIGN_STATE_ACTIVE || c.state == 0, |b| b.set("not_finished", true))
        .when(!(c.state == CAMPAIGN_STATE_ACTIVE || c.state == 0), |b| b.set("finish_time", ts_unix(c.transition_unix)))
        .set("target_progress", target_sum)
        .set("definition_name", &c.name)
        .when(!c.namespace.is_empty(), |b| b.set("namespace.namespace", &c.namespace))
        .set("lifecycle_state", lifecycle_builder("eve_public.campaign.LifecycleState", c.state, c.transition_unix))
}

/// `campaign.Attributes` (fields 14, 16-21, 23, 24, 29).
pub fn encode_campaign_attributes(c: &CampaignRecord, objs: &[&CampaignObjectiveRecord]) -> Vec<u8> {
    campaign_attributes_builder(c, objs).must_encode()
}

/// `objective.Stats { current_objective_progress = 1; committed_participants = 2;
/// total_participants = 3; total_contributors = 4 }`.
pub fn encode_objective_stats(o: &CampaignObjectiveRecord) -> Vec<u8> {
    pb::message("eve_public.campaign.objective.Stats")
        .set("current_objective_progress", o.current_progress.max(0) as u64)
        .set("committed_participants", o.committed_participants.len() as u32)
        .set("total_participants", (o.committed_participants.len() + o.resigned_participants.len()) as u32)
        .set("total_contributors", o.contributors.len() as u32)
        .must_encode()
}

fn objective_attributes_builder(o: &CampaignObjectiveRecord) -> ProtoBuilder {
    let types = contribution_method_params(&o.contribution_method).unwrap_or(&[]);
    let mut b = pb::message("eve_public.campaign.objective.Attributes")
        .set("state", o.state as i32)
        .when(o.state == OBJECTIVE_STATE_ACTIVE || o.state == 0, |b| b.set("not_finished", true))
        .when(!(o.state == OBJECTIVE_STATE_ACTIVE || o.state == 0), |b| b.set("finish_time", ts_unix(o.transition_unix)))
        .set("target_progress", o.target_progress.max(0) as u64)
        .when(o.max_progress_per_participant > 0, |b| {
            b.set("max_progress_per_participant", o.max_progress_per_participant as u64)
        })
        .set("contribution_method_configuration.contribution_method.name", &o.contribution_method);
    for p in &o.parameters {
        let val_type = types.iter().find(|t| t.0 == p.key).map(|t| t.1).unwrap_or("");
        b = b.add_with("contribution_method_configuration.parameters", |pc| {
            pc.set("key", &p.key).add_with("matcher.values", |ve| {
                let mut ve = ve.set("value_type", val_type);
                for v in &p.values {
                    ve = ve.add("values", v);
                }
                ve
            })
        });
    }
    b.when(o.isk_reward_per_interval > 0 && o.isk_reward_progress_interval > 0, |b| {
        b.set("rewards.isk_reward.amount_per_interval.units", o.isk_reward_per_interval)
            .set("rewards.isk_reward.progress_interval", o.isk_reward_progress_interval as u64)
    })
    .when(!(o.isk_reward_per_interval > 0 && o.isk_reward_progress_interval > 0), |b| b.set("rewards.no_isk_reward", true))
    .set("rewards.no_lp_reward", true)
    .set("rewards.no_standing_reward", true)
    .set("key", &o.name)
    .set("lifecycle_state", lifecycle_builder("eve_public.campaign.objective.LifecycleState", o.state, o.transition_unix))
}

/// `objective.Attributes` (fields 6, 8/9, 11-14, 16, 22).
pub fn encode_objective_attributes(o: &CampaignObjectiveRecord) -> Vec<u8> {
    objective_attributes_builder(o).must_encode()
}

fn track_attributes_builder(t: &CampaignTrackRecord) -> ProtoBuilder {
    let mut b = pb::message("eve_public.campaign.track.Attributes")
        .set("key", &t.key)
        .when(t.target_progress_value > 0, |b| b.set("target_progress_value", t.target_progress_value))
        .when(t.target_progress_value == 0, |b| b.set("no_target_progress", true))
        .when(t.isk_reward_per_interval > 0 && t.isk_reward_progress_interval > 0, |b| {
            b.set_with("participant_contribution_track", |pct| {
                pct.when(t.max_participant_contribution > 0, |pct| {
                    pct.set("max_total_progress", t.max_participant_contribution)
                })
                .when(t.max_participant_contribution == 0, |pct| pct.set("no_contribution_limit", true))
                .add_with("rewards", |rw| {
                    rw.set("key", "isk")
                        .set("progress_interval", t.isk_reward_progress_interval)
                        .set("isk_reward.amount_per_interval.units", t.isk_reward_per_interval)
                })
            })
        })
        .when(!(t.isk_reward_per_interval > 0 && t.isk_reward_progress_interval > 0), |b| {
            b.set("no_participant_contribution_track", true)
        });
    for tc in &t.ticking {
        b = b.add_with("contributions", |contrib| {
            contrib.set_with("ticking_contribution", |tick| {
                let mut tick = tick
                    .set("key", &tc.key)
                    .set("interval", GoogleDuration { seconds: tc.interval_secs as i64, nanos: 0 });
                for e in &tc.evaluations {
                    tick = tick.add_with("evaluations", |ev| {
                        let ev = ev.set("key", &e.key);
                        match (e.constant, e.per_unit_value) {
                            (Some(c), _) => ev.set("constant", c),
                            (None, Some(p)) => ev.set("per_unit_value", p),
                            _ => ev,
                        }
                    });
                }
                tick
            })
        });
    }
    b.set("lifecycle_state", lifecycle_builder("eve_public.campaign.track.LifecycleState", t.state, t.transition_unix))
}

/// `track.Attributes` (fields 1, 3/4, 5/6, 8, 11).
pub fn encode_track_attributes(t: &CampaignTrackRecord) -> Vec<u8> {
    track_attributes_builder(t).must_encode()
}

fn track_stats_builder(t: &CampaignTrackRecord) -> ProtoBuilder {
    let mut b = pb::message("eve_public.campaign.track.Stats").set("current_progress", t.current_progress);
    for tc in &t.ticking {
        b = b.add_with("ticking_contributions", |tick| {
            let tick = tick.set("key", &tc.key);
            if t.state == TRACK_STATE_STARTED {
                tick.set("next_tick", ts_unix(tc.last_tick_unix + tc.interval_secs.max(1)))
            } else {
                tick.set("stopped", true)
            }
        });
    }
    b
}

/// `track.Stats { current_progress = 1; ticking_contributions = 2 [{ key, stopped | next_tick }] }`.
pub fn encode_track_stats(t: &CampaignTrackRecord) -> Vec<u8> {
    track_stats_builder(t).must_encode()
}

/// `track.ParticipantStats { participant_track = 1 { current = 1; max_reached = 2 } | no_participant_track = 2 }`.
pub fn encode_track_participant_stats(t: &CampaignTrackRecord, char_id: u32) -> Vec<u8> {
    let b = pb::message("eve_public.campaign.track.ParticipantStats");
    match t.participants.get(&char_id) {
        Some(&mine) => {
            let reached = t.max_participant_contribution > 0 && mine >= t.max_participant_contribution;
            b.set_with("participant_track", |pt| {
                pt.set("current_participant_contribution", mine).set("max_participant_contribution_reached", reached)
            })
        }
        None => b.set("no_participant_track", true),
    }
    .must_encode()
}

fn status_track_builder(t: &CampaignTrackRecord) -> ProtoBuilder {
    pb::message("eve_public.campaign.api.GetStatusResponse.Track")
        .set("id", id_builder("eve_public.campaign.track.Identifier", track_id_to_uuid_bytes(t.id)))
        .set("state", lifecycle_builder("eve_public.campaign.track.LifecycleState", t.state, t.transition_unix))
        .set("stats", track_stats_builder(t))
}

/// `GetStatusResponse.Track { id = 1; state = 2; stats = 3 }` (campaign and objective APIs).
pub fn encode_status_track(t: &CampaignTrackRecord) -> Vec<u8> {
    status_track_builder(t).must_encode()
}

pub fn campaign_tracks(mgr: &ProgressionExtraManager, cid: u64) -> Vec<CampaignTrackRecord> {
    let mut v: Vec<CampaignTrackRecord> = mgr
        .campaign_tracks
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|t| t.campaign_id == cid)
        .cloned()
        .collect();
    v.sort_by_key(|t| t.id);
    v
}

fn campaign_objectives(mgr: &ProgressionExtraManager, cid: u64) -> Vec<CampaignObjectiveRecord> {
    let mut v: Vec<CampaignObjectiveRecord> = mgr
        .campaign_objectives
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|o| o.campaign_id == cid)
        .cloned()
        .collect();
    v.sort_by_key(|o| o.id);
    v
}

fn get_campaign(mgr: &ProgressionExtraManager, cid: u64) -> Result<CampaignRecord, (u32, String)> {
    mgr.campaigns
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cid)
        .cloned()
        .ok_or((STATUS_NOT_FOUND, "Campaign not found".to_string()))
}

fn listed_campaigns(mgr: &ProgressionExtraManager, namespace: Option<&str>) -> (Vec<u64>, Vec<CampaignRecord>) {
    let camps = mgr.campaigns.read().unwrap_or_else(|e| e.into_inner());
    let mut started: Vec<u64> = Vec::new();
    let mut upcoming: Vec<CampaignRecord> = Vec::new();
    for c in camps.values().filter(|c| namespace.is_none_or(|ns| c.namespace == ns)) {
        match c.state {
            0 => upcoming.push(c.clone()),
            CAMPAIGN_STATE_ACTIVE | CAMPAIGN_STATE_COMPLETED => started.push(c.id),
            _ => {}
        }
    }
    started.sort_unstable();
    upcoming.sort_by_key(|c| c.id);
    (started, upcoming)
}

fn namespace_of(dyn_msg: &eve_proto::quasar::DynamicQuasarMessage) -> Option<String> {
    match dyn_msg.get(1) {
        Some(DynamicValue::Message(m)) => match m.get(1) {
            Some(DynamicValue::String(s)) => Some(s.clone()),
            _ => Some(String::new()),
        },
        Some(DynamicValue::Bytes(b)) => WireMessage::decode(b).ok().and_then(|wm| {
            wm.fields.iter().find_map(|f| match &f.value {
                WireValue::LengthDelimited(s) if f.field_number == 1 => String::from_utf8(s.clone()).ok(),
                _ => None,
            })
        }),
        _ => None,
    }
}

/// Registers the `eve_public.campaign.api` request handlers, then the objective / track handlers.
pub fn register_campaign_handlers(router: &PublicGatewayRouter, manager: Arc<ProgressionExtraManager>) {
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let mut b = pb::message("eve_public.campaign.api.GetAllResponse");
            for cid in listed_campaigns(&mgr, None).0 {
                b = b.add("campaigns", id_builder("eve_public.campaign.Identifier", campaign_id_to_uuid_bytes(cid)));
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllForNamespaceRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let ns = namespace_of(dyn_msg).unwrap_or_default();
            let (started, upcoming) = listed_campaigns(&mgr, Some(&ns));
            let mut b = pb::message("eve_public.campaign.api.GetAllForNamespaceResponse");
            for cid in started {
                b = b.add("campaigns", id_builder("eve_public.campaign.Identifier", campaign_id_to_uuid_bytes(cid)));
            }
            for c in upcoming {
                b = b.add_with("upcoming_campaigns", |up| {
                    up.set("definition_name", &c.name).set("scheduled_start_time", ts_unix(c.scheduled_start_unix))
                });
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let camp = get_campaign(&mgr, cid)?;
            let objs = campaign_objectives(&mgr, cid);
            let refs: Vec<&CampaignObjectiveRecord> = objs.iter().collect();
            let mut b = pb::message("eve_public.campaign.api.GetResponse")
                .set("attributes", campaign_attributes_builder(&camp, &refs))
                .set("stats", campaign_stats_builder(&refs));
            for o in objs.iter().filter(|o| o.state == OBJECTIVE_STATE_ACTIVE) {
                b = b.add_with("available_objectives", |avail| {
                    avail
                        .set("objective", id_builder("eve_public.campaign.objective.Identifier", objective_id_to_uuid_bytes(o.id)))
                        .set("attributes", objective_attributes_builder(o))
                });
            }
            for t in campaign_tracks(&mgr, cid) {
                b = b.add_with("tracks", |e| {
                    e.set("id", id_builder("eve_public.campaign.track.Identifier", track_id_to_uuid_bytes(t.id)))
                        .set("attributes", track_attributes_builder(&t))
                        .set("stats", track_stats_builder(&t))
                });
            }
            for o in &objs {
                b = b.add("objectives", id_builder("eve_public.campaign.objective.Identifier", objective_id_to_uuid_bytes(o.id)));
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let camp = get_campaign(&mgr, cid)?;
            let objs = campaign_objectives(&mgr, cid);
            let refs: Vec<&CampaignObjectiveRecord> = objs.iter().collect();
            let mut b = pb::message("eve_public.campaign.api.GetStatusResponse")
                .set("state", lifecycle_builder("eve_public.campaign.LifecycleState", camp.state, camp.transition_unix))
                .set("stats", campaign_stats_builder(&refs));
            for t in campaign_tracks(&mgr, cid) {
                b = b.add("tracks", status_track_builder(&t));
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign")?;
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            get_campaign(&mgr, cid)?;
            let objs = campaign_objectives(&mgr, cid);
            let refs: Vec<&CampaignObjectiveRecord> = objs.iter().collect();
            let mut b = pb::message("eve_public.campaign.api.GetStatsResponse").set("campaign_stats", campaign_stats_builder(&refs));
            for o in &objs {
                b = b.add_with("objective_stats", |e| {
                    e.set("objective", id_builder("eve_public.campaign.objective.Identifier", objective_id_to_uuid_bytes(o.id)))
                        .set_raw_message("stats", &encode_objective_stats(o))
                });
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let cid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign")?;
            get_campaign(&mgr, cid)?;
            let mut b = pb::message("eve_public.campaign.api.GetParticipantStatsResponse");
            for o in campaign_objectives(&mgr, cid) {
                let Some(state) = ProgressionExtraManager::objective_participant_state(&o, char_id) else { continue };
                let progress = o.contributors.get(&char_id).map(|e| e.0.max(0) as u64).unwrap_or(0);
                b = b.add_with("participant_stats", |e| {
                    e.set("objective", id_builder("eve_public.campaign.objective.Identifier", objective_id_to_uuid_bytes(o.id)))
                        .set_with("stats", |ps| {
                            ps.set("state", state as i32).set("current_participant_progress", progress)
                        })
                });
            }
            Ok(b.must_to_any())
        }),
    );

    router.register_handler(
        "eve_public.campaign.api.GetLimitsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Ok(pb::message("eve_public.campaign.api.GetLimitsResponse")
                .set("max_active_objectives_per_participant", MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT)
                .must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllUnredeemedRewardsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<(u64, i64)> = objs
                .values()
                .filter_map(|o| o.contributors.get(&char_id).filter(|e| e.1 > 0).map(|e| (o.id, e.1)))
                .collect();
            ids.sort_unstable();
            let mut b = pb::message("eve_public.campaign.api.GetAllUnredeemedRewardsResponse");
            for (oid, units) in ids {
                b = b.add_with("objectives", |e| {
                    e.set("objective", id_builder("eve_public.campaign.objective.Identifier", objective_id_to_uuid_bytes(oid)))
                        .set("isk_intervals_unredeemed", units as u64)
                        .set("no_lp_unredeemed", true)
                });
            }
            Ok(b.must_to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let ns = namespace_of(dyn_msg);
            let camp_ns: HashMap<u64, String> = mgr
                .campaigns
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .map(|c| (c.id, c.namespace.clone()))
                .collect();
            let trks = mgr.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<(u64, u64)> = trks
                .values()
                .filter(|t| ns.as_ref().is_none_or(|n| camp_ns.get(&t.campaign_id) == Some(n)))
                .map(|t| (t.id, ProgressionExtraManager::track_unredeemed_intervals(t, char_id)))
                .filter(|(_, n)| *n > 0)
                .collect();
            rows.sort_unstable();
            let mut b = pb::message("eve_public.campaign.api.GetAllUnredeemedParticipantRewardsResponse");
            for (tid, units) in rows {
                b = b.add_with("tracks", |e| {
                    e.set("track", id_builder("eve_public.campaign.track.Identifier", track_id_to_uuid_bytes(tid)))
                        .set("isk_intervals_unredeemed", units)
                        .set("no_lp_unredeemed", true)
                });
            }
            Ok(b.must_to_any())
        }),
    );

    super::quasar_campaign_objective_handlers::register_objective_and_track_handlers(router, manager);
}

/// Predicted value of the next tick of each ticking contribution on `t`.
pub fn encode_ticking_prediction(t: &CampaignTrackRecord, units: u64) -> Vec<u8> {
    let mut b = pb::message("eve_public.campaign.track.api.GetTickingContributionsPredictionResponse");
    for tc in &t.ticking {
        b = b.add_with("contributions", |c| {
            let mut c = c.set("key", &tc.key);
            for e in &tc.evaluations {
                c = c.add_with("evaluations", |ev| {
                    let ev = ev.set("key", &e.key);
                    match (e.constant, e.per_unit_value) {
                        (Some(k), _) => ev.set("constant", k),
                        (None, Some(_)) => ev.set("units_count", units),
                        _ => ev,
                    }
                });
            }
            c.set("value", ticking_value(tc, units).max(0) as u64)
        });
    }
    b.must_encode()
}

#[cfg(test)] #[path = "quasar_campaign_handlers_tests.rs"]
mod quasar_campaign_handlers_tests;

#[cfg(test)]
#[path = "quasar_campaign_handlers_edges_tests.rs"]
mod quasar_campaign_handlers_edges_tests;

