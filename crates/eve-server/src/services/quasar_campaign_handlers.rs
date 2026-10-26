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
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64, DynamicValue, GoogleAny,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
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

pub fn encode_timestamp(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut ts = Vec::new();
    encode_varint_field(1, secs, &mut ts);
    encode_bytes_field(field, &ts, out);
}

pub fn encode_id_field(field: u32, uuid: [u8; 16], out: &mut Vec<u8>) {
    encode_bytes_field(field, &encode_identifier(&uuid), out);
}

/// `LifecycleState { state = 1; transition_time = 2 }` (same shape for campaign/objective/track).
pub fn encode_lifecycle(field: u32, state: u32, transition_unix: u64, out: &mut Vec<u8>) {
    let mut ls = Vec::new();
    encode_varint_field(1, u64::from(state), &mut ls);
    if transition_unix > 0 {
        encode_timestamp(2, transition_unix, &mut ls);
    }
    encode_bytes_field(field, &ls, out);
}

fn encode_isk_currency(field: u32, isk: i64, out: &mut Vec<u8>) {
    let mut cur = Vec::new();
    encode_varint_field(1, encode_zigzag64(isk), &mut cur);
    encode_bytes_field(field, &cur, out);
}

/// `campaign.Stats { current_campaign_progress = 1; total_contributors = 2 }`.
pub fn encode_campaign_stats(objs: &[&CampaignObjectiveRecord]) -> Vec<u8> {
    let progress: u64 = objs.iter().map(|o| o.current_progress.max(0) as u64).sum();
    let mut contributors: Vec<u32> = objs.iter().flat_map(|o| o.contributors.keys().copied()).collect();
    contributors.sort_unstable();
    contributors.dedup();
    let mut s = Vec::new();
    encode_varint_field(1, progress, &mut s);
    encode_varint_field(2, contributors.len() as u64, &mut s);
    s
}

/// `campaign.Attributes` (fields 14, 16-21, 23, 24, 29).
pub fn encode_campaign_attributes(c: &CampaignRecord, objs: &[&CampaignObjectiveRecord]) -> Vec<u8> {
    let mut a = Vec::new();
    encode_varint_field(14, objs.len() as u64, &mut a);
    encode_varint_field(16, u64::from(c.state), &mut a);
    encode_timestamp(17, c.scheduled_start_unix, &mut a);
    if c.scheduled_expiry_unix > 0 {
        encode_timestamp(18, c.scheduled_expiry_unix, &mut a);
    }
    if c.state == CAMPAIGN_STATE_ACTIVE || c.state == 0 {
        encode_varint_field(19, 1, &mut a);
    } else {
        encode_timestamp(20, c.transition_unix, &mut a);
    }
    encode_varint_field(21, objs.iter().map(|o| o.target_progress.max(0) as u64).sum(), &mut a);
    encode_string_field(23, &c.name, &mut a);
    if !c.namespace.is_empty() {
        let mut ns = Vec::new();
        encode_string_field(1, &c.namespace, &mut ns);
        encode_bytes_field(24, &ns, &mut a);
    }
    encode_lifecycle(29, c.state, c.transition_unix, &mut a);
    a
}

/// `objective.Stats { current_objective_progress = 1; committed_participants = 2;
/// total_participants = 3; total_contributors = 4 }`.
pub fn encode_objective_stats(o: &CampaignObjectiveRecord) -> Vec<u8> {
    let mut s = Vec::new();
    encode_varint_field(1, o.current_progress.max(0) as u64, &mut s);
    encode_varint_field(2, o.committed_participants.len() as u64, &mut s);
    encode_varint_field(3, (o.committed_participants.len() + o.resigned_participants.len()) as u64, &mut s);
    encode_varint_field(4, o.contributors.len() as u64, &mut s);
    s
}

/// `objective.Attributes` (fields 6, 8/9, 11-14, 16, 22).
pub fn encode_objective_attributes(o: &CampaignObjectiveRecord) -> Vec<u8> {
    let mut a = Vec::new();
    encode_varint_field(6, u64::from(o.state), &mut a);
    if o.state == OBJECTIVE_STATE_ACTIVE || o.state == 0 {
        encode_varint_field(8, 1, &mut a);
    } else {
        encode_timestamp(9, o.transition_unix, &mut a);
    }
    encode_varint_field(11, o.target_progress.max(0) as u64, &mut a);
    if o.max_progress_per_participant > 0 {
        encode_varint_field(12, o.max_progress_per_participant as u64, &mut a);
    }
    let mut method = Vec::new();
    encode_string_field(1, &o.contribution_method, &mut method);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &method, &mut cfg);
    let types = contribution_method_params(&o.contribution_method).unwrap_or(&[]);
    for p in &o.parameters {
        let mut entry = Vec::new();
        encode_string_field(1, types.iter().find(|t| t.0 == p.key).map(|t| t.1).unwrap_or(""), &mut entry);
        for v in &p.values {
            encode_string_field(2, v, &mut entry);
        }
        let mut matcher = Vec::new();
        encode_bytes_field(1, &entry, &mut matcher);
        let mut pc = Vec::new();
        encode_string_field(1, &p.key, &mut pc);
        encode_bytes_field(2, &matcher, &mut pc);
        encode_bytes_field(2, &pc, &mut cfg);
    }
    encode_bytes_field(13, &cfg, &mut a);
    let mut rewards = Vec::new();
    if o.isk_reward_per_interval > 0 && o.isk_reward_progress_interval > 0 {
        let mut isk = Vec::new();
        encode_isk_currency(1, o.isk_reward_per_interval, &mut isk);
        encode_varint_field(2, o.isk_reward_progress_interval as u64, &mut isk);
        encode_bytes_field(1, &isk, &mut rewards);
    } else {
        encode_varint_field(2, 1, &mut rewards);
    }
    encode_varint_field(4, 1, &mut rewards);
    encode_varint_field(6, 1, &mut rewards);
    encode_bytes_field(14, &rewards, &mut a);
    encode_string_field(16, &o.name, &mut a);
    encode_lifecycle(22, o.state, o.transition_unix, &mut a);
    a
}

/// `track.Attributes` (fields 1, 3/4, 5/6, 8, 11).
pub fn encode_track_attributes(t: &CampaignTrackRecord) -> Vec<u8> {
    let mut a = Vec::new();
    encode_string_field(1, &t.key, &mut a);
    if t.target_progress_value > 0 {
        encode_varint_field(3, t.target_progress_value, &mut a);
    } else {
        encode_varint_field(4, 1, &mut a);
    }
    if t.isk_reward_per_interval > 0 && t.isk_reward_progress_interval > 0 {
        let mut pct = Vec::new();
        if t.max_participant_contribution > 0 {
            encode_varint_field(2, t.max_participant_contribution, &mut pct);
        } else {
            encode_varint_field(3, 1, &mut pct);
        }
        let mut reward = Vec::new();
        encode_string_field(1, "isk", &mut reward);
        encode_varint_field(3, t.isk_reward_progress_interval, &mut reward);
        let mut isk = Vec::new();
        encode_isk_currency(1, t.isk_reward_per_interval, &mut isk);
        encode_bytes_field(4, &isk, &mut reward);
        encode_bytes_field(4, &reward, &mut pct);
        encode_bytes_field(5, &pct, &mut a);
    } else {
        encode_varint_field(6, 1, &mut a);
    }
    for tc in &t.ticking {
        let mut tick = Vec::new();
        encode_string_field(1, &tc.key, &mut tick);
        let mut dur = Vec::new();
        encode_varint_field(1, tc.interval_secs, &mut dur);
        encode_bytes_field(3, &dur, &mut tick);
        for e in &tc.evaluations {
            let mut ev = Vec::new();
            encode_string_field(1, &e.key, &mut ev);
            match (e.constant, e.per_unit_value) {
                (Some(c), _) => encode_varint_field(3, encode_zigzag64(c), &mut ev),
                (None, Some(p)) => encode_varint_field(4, encode_zigzag64(p), &mut ev),
                _ => {}
            }
            encode_bytes_field(4, &ev, &mut tick);
        }
        let mut contrib = Vec::new();
        encode_bytes_field(2, &tick, &mut contrib);
        encode_bytes_field(8, &contrib, &mut a);
    }
    encode_lifecycle(11, t.state, t.transition_unix, &mut a);
    a
}

/// `track.Stats { current_progress = 1; ticking_contributions = 2 [{ key, stopped | next_tick }] }`.
pub fn encode_track_stats(t: &CampaignTrackRecord) -> Vec<u8> {
    let mut s = Vec::new();
    encode_varint_field(1, t.current_progress, &mut s);
    for tc in &t.ticking {
        let mut tick = Vec::new();
        encode_string_field(1, &tc.key, &mut tick);
        if t.state == TRACK_STATE_STARTED {
            encode_timestamp(3, tc.last_tick_unix + tc.interval_secs.max(1), &mut tick);
        } else {
            encode_varint_field(2, 1, &mut tick);
        }
        encode_bytes_field(2, &tick, &mut s);
    }
    s
}

/// `track.ParticipantStats { participant_track = 1 { current = 1; max_reached = 2 } | no_participant_track = 2 }`.
pub fn encode_track_participant_stats(t: &CampaignTrackRecord, char_id: u32) -> Vec<u8> {
    let mut s = Vec::new();
    match t.participants.get(&char_id) {
        Some(&mine) => {
            let mut pt = Vec::new();
            encode_varint_field(1, mine, &mut pt);
            let reached = t.max_participant_contribution > 0 && mine >= t.max_participant_contribution;
            encode_varint_field(2, u64::from(reached), &mut pt);
            encode_bytes_field(1, &pt, &mut s);
        }
        None => encode_varint_field(2, 1, &mut s),
    }
    s
}

/// `GetStatusResponse.Track { id = 1; state = 2; stats = 3 }` (campaign and objective APIs).
pub fn encode_status_track(t: &CampaignTrackRecord) -> Vec<u8> {
    let mut e = Vec::new();
    encode_id_field(1, track_id_to_uuid_bytes(t.id), &mut e);
    encode_lifecycle(2, t.state, t.transition_unix, &mut e);
    encode_bytes_field(3, &encode_track_stats(t), &mut e);
    e
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
            let mut out = Vec::new();
            for cid in listed_campaigns(&mgr, None).0 {
                encode_id_field(1, campaign_id_to_uuid_bytes(cid), &mut out);
            }
            Ok(any("eve_public.campaign.api.GetAllResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllForNamespaceRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            queue_notices(mgr.evaluate_campaign_lifecycle(ctx.now_unix()), ctx.now_unix());
            let ns = namespace_of(dyn_msg).unwrap_or_default();
            let (started, upcoming) = listed_campaigns(&mgr, Some(&ns));
            let mut out = Vec::new();
            for cid in started {
                encode_id_field(1, campaign_id_to_uuid_bytes(cid), &mut out);
            }
            for c in upcoming {
                let mut up = Vec::new();
                encode_string_field(1, &c.name, &mut up);
                encode_timestamp(3, c.scheduled_start_unix, &mut up);
                encode_bytes_field(2, &up, &mut out);
            }
            Ok(any("eve_public.campaign.api.GetAllForNamespaceResponse", out))
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
            let mut out = Vec::new();
            encode_bytes_field(1, &encode_campaign_attributes(&camp, &refs), &mut out);
            for o in objs.iter().filter(|o| o.state == OBJECTIVE_STATE_ACTIVE) {
                let mut avail = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(o.id), &mut avail);
                encode_bytes_field(2, &encode_objective_attributes(o), &mut avail);
                encode_bytes_field(2, &avail, &mut out);
            }
            encode_bytes_field(4, &encode_campaign_stats(&refs), &mut out);
            for t in campaign_tracks(&mgr, cid) {
                let mut e = Vec::new();
                encode_id_field(1, track_id_to_uuid_bytes(t.id), &mut e);
                encode_bytes_field(2, &encode_track_attributes(&t), &mut e);
                encode_bytes_field(3, &encode_track_stats(&t), &mut e);
                encode_bytes_field(5, &e, &mut out);
            }
            for o in &objs {
                encode_id_field(6, objective_id_to_uuid_bytes(o.id), &mut out);
            }
            Ok(any("eve_public.campaign.api.GetResponse", out))
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
            let mut out = Vec::new();
            encode_lifecycle(1, camp.state, camp.transition_unix, &mut out);
            encode_bytes_field(2, &encode_campaign_stats(&refs), &mut out);
            for t in campaign_tracks(&mgr, cid) {
                encode_bytes_field(3, &encode_status_track(&t), &mut out);
            }
            Ok(any("eve_public.campaign.api.GetStatusResponse", out))
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
            let mut out = Vec::new();
            encode_bytes_field(1, &encode_campaign_stats(&refs), &mut out);
            for o in &objs {
                let mut e = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(o.id), &mut e);
                encode_bytes_field(2, &encode_objective_stats(o), &mut e);
                encode_bytes_field(2, &e, &mut out);
            }
            Ok(any("eve_public.campaign.api.GetStatsResponse", out))
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx.effective_character_id().ok_or((STATUS_FORBIDDEN, "Active character required".to_string()))?;
            let cid = require_id(extract_campaign_id(dyn_msg.get(1)), "campaign")?;
            get_campaign(&mgr, cid)?;
            let mut out = Vec::new();
            for o in campaign_objectives(&mgr, cid) {
                let Some(state) = ProgressionExtraManager::objective_participant_state(&o, char_id) else { continue };
                let mut ps = Vec::new();
                encode_varint_field(1, u64::from(state), &mut ps);
                encode_varint_field(2, o.contributors.get(&char_id).map(|e| e.0.max(0) as u64).unwrap_or(0), &mut ps);
                let mut e = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(o.id), &mut e);
                encode_bytes_field(2, &ps, &mut e);
                encode_bytes_field(1, &e, &mut out);
            }
            Ok(any("eve_public.campaign.api.GetParticipantStatsResponse", out))
        }),
    );

    router.register_handler(
        "eve_public.campaign.api.GetLimitsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_varint_field(1, u64::from(MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT), &mut out);
            Ok(any("eve_public.campaign.api.GetLimitsResponse", out))
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
            let mut out = Vec::new();
            for (oid, units) in ids {
                let mut e = Vec::new();
                encode_id_field(1, objective_id_to_uuid_bytes(oid), &mut e);
                encode_varint_field(2, units as u64, &mut e);
                encode_varint_field(5, 1, &mut e);
                encode_bytes_field(1, &e, &mut out);
            }
            Ok(any("eve_public.campaign.api.GetAllUnredeemedRewardsResponse", out))
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
            let mut out = Vec::new();
            for (tid, units) in rows {
                let mut e = Vec::new();
                encode_id_field(1, track_id_to_uuid_bytes(tid), &mut e);
                encode_varint_field(2, units, &mut e);
                encode_varint_field(5, 1, &mut e);
                encode_bytes_field(1, &e, &mut out);
            }
            Ok(any("eve_public.campaign.api.GetAllUnredeemedParticipantRewardsResponse", out))
        }),
    );

    super::quasar_campaign_objective_handlers::register_objective_and_track_handlers(router, manager);
}

/// Predicted value of the next tick of each ticking contribution on `t`.
pub fn encode_ticking_prediction(t: &CampaignTrackRecord, units: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for tc in &t.ticking {
        let mut c = Vec::new();
        encode_string_field(1, &tc.key, &mut c);
        for e in &tc.evaluations {
            let mut ev = Vec::new();
            encode_string_field(1, &e.key, &mut ev);
            match (e.constant, e.per_unit_value) {
                (Some(k), _) => encode_varint_field(2, encode_zigzag64(k), &mut ev),
                (None, Some(_)) => encode_varint_field(3, units, &mut ev),
                _ => {}
            }
            encode_bytes_field(2, &ev, &mut c);
        }
        encode_varint_field(3, ticking_value(tc, units).max(0) as u64, &mut c);
        encode_bytes_field(1, &c, &mut out);
    }
    out
}
