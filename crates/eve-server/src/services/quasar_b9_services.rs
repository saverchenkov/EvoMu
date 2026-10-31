//! `quasar_b9_services` — V24 Quasar `eve.campaign.*` RPC, Command, Definition, Objective,
//! and Track handlers (`quasar:eveProto/generated/eve/campaign/...`).
//!
//! Backed by [`ProgressionExtraManager`] (`campaigns`, `campaign_objectives`, `campaign_tracks`)
//! and persisted to `WorldStateSnapshot.quasar_state_blobs` via [`ProgressionExtraManager::save_to_db`].

#![forbid(unsafe_code)]

use super::quasar_campaign_handlers::{
    any, campaign_uuid, encode_id_field, encode_lifecycle, encode_timestamp, extract_campaign_id,
    objective_uuid, queue_notices, track_uuid,
};
use super::quasar_campaign_state::{
    CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord, PendingNotices,
    CAMPAIGN_STATE_ACTIVE, CAMPAIGN_STATE_COMPLETED, CAMPAIGN_STATE_EXPIRED,
    MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT, OBJECTIVE_STATE_ACTIVE, OBJECTIVE_STATE_COMPLETED,
    TRACK_STATE_STARTED,
};
use super::quasar_progression::ProgressionManagers;
use super::quasar_progression_extra::ProgressionExtraManager;
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field,
    DynamicQuasarMessage, DynamicValue, WireMessage, WireValue,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const CAMPAIGN_STATE_CANCELLED: u32 = 4;
const CAMPAIGN_STATE_FAILED: u32 = 5;
const CAMPAIGN_STATE_ARCHIVED: u32 = 6;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1_700_000_000)
}

fn dyn_field<'a>(msg: &'a DynamicQuasarMessage, num: u32) -> Option<&'a DynamicValue> {
    msg.fields.get(&num)
}

fn dynamic_val_to_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(n) => Some(*n),
        DynamicValue::Int64(n) => Some(*n as u64),
        DynamicValue::Uint32(n) => Some(u64::from(*n)),
        DynamicValue::Int32(n) => Some(*n as u64),
        DynamicValue::Sint64(n) => Some(*n as u64),
        DynamicValue::Sint32(n) => Some(*n as u64),
        DynamicValue::Bool(b) => Some(u64::from(*b)),
        _ => None,
    }
}

fn extract_u64_field(msg: &DynamicQuasarMessage, num: u32) -> Option<u64> {
    dynamic_val_to_u64(dyn_field(msg, num)?)
}

fn extract_string_field(msg: &DynamicQuasarMessage, num: u32) -> Option<String> {
    match dyn_field(msg, num)? {
        DynamicValue::String(s) => Some(s.clone()),
        DynamicValue::Bytes(b) => std::str::from_utf8(b).ok().map(str::to_string),
        _ => None,
    }
}

fn extract_nested_string(val: Option<&DynamicValue>, field_num: u32) -> Option<String> {
    match val? {
        DynamicValue::Message(m) => match m.get(field_num)? {
            DynamicValue::String(s) => Some(s.clone()),
            DynamicValue::Bytes(b) => std::str::from_utf8(b).ok().map(str::to_string),
            _ => None,
        },
        DynamicValue::Bytes(b) => {
            let wm = WireMessage::decode(b).ok()?;
            wm.fields.iter().find_map(|f| match (&f.value, f.field_number) {
                (WireValue::LengthDelimited(inner), n) if n == field_num => {
                    std::str::from_utf8(inner).ok().map(str::to_string)
                }
                _ => None,
            })
        }
        _ => None,
    }
}

fn extract_nested_varint(val: Option<&DynamicValue>, field_num: u32) -> Option<u64> {
    match val? {
        DynamicValue::Message(m) => dynamic_val_to_u64(m.get(field_num)?),
        DynamicValue::Bytes(b) => {
            let wm = WireMessage::decode(b).ok()?;
            wm.fields.iter().find_map(|f| match (&f.value, f.field_number) {
                (WireValue::Varint(v), n) if n == field_num => Some(*v),
                _ => None,
            })
        }
        _ => None,
    }
}

fn extract_character_id(val: Option<&DynamicValue>, fallback: u32) -> u32 {
    extract_nested_varint(val, 1)
        .map(|v| v as u32)
        .filter(|&v| v > 0)
        .unwrap_or(fallback)
}

fn encode_semver(field: u32, major: u32, minor: u32, patch: u32, out: &mut Vec<u8>) {
    let mut sv = Vec::new();
    encode_varint_field(1, u64::from(major), &mut sv);
    encode_varint_field(2, u64::from(minor), &mut sv);
    encode_varint_field(3, u64::from(patch), &mut sv);
    encode_bytes_field(field, &sv, out);
}

fn encode_campaign_def_id(field: u32, name: &str, out: &mut Vec<u8>) {
    let mut def_id = Vec::new();
    encode_string_field(1, name, &mut def_id);
    encode_semver(2, 1, 0, 0, &mut def_id);
    encode_bytes_field(field, &def_id, out);
}

fn encode_objective_def_id(field: u32, obj_name: &str, camp_name: &str, out: &mut Vec<u8>) {
    let mut def_id = Vec::new();
    encode_string_field(1, obj_name, &mut def_id);
    encode_campaign_def_id(2, camp_name, &mut def_id);
    encode_bytes_field(field, &def_id, out);
}

fn encode_campaign_attributes(camp: &CampaignRecord, out: &mut Vec<u8>) {
    let mut attr = Vec::new();
    encode_campaign_def_id(1, &camp.name, &mut attr);
    encode_varint_field(2, u64::from(camp.state), &mut attr);
    if camp.scheduled_start_unix > 0 {
        encode_timestamp(3, camp.scheduled_start_unix, &mut attr);
    }
    if camp.state == 0 {
        encode_varint_field(4, 1, &mut attr); // not_started = true
    } else if camp.transition_unix > 0 {
        encode_timestamp(5, camp.transition_unix, &mut attr);
    }
    if camp.state < CAMPAIGN_STATE_COMPLETED {
        encode_varint_field(6, 1, &mut attr); // not_finished = true
    } else if camp.transition_unix > 0 {
        encode_timestamp(7, camp.transition_unix, &mut attr);
    }
    encode_lifecycle(10, camp.state, camp.transition_unix, &mut attr);
    if !camp.namespace.is_empty() {
        let mut ns = Vec::new();
        encode_string_field(1, &camp.namespace, &mut ns);
        encode_bytes_field(11, &ns, &mut attr);
    }
    encode_bytes_field(1, &attr, out);
}

fn encode_campaign_stats(
    camp: &CampaignRecord,
    objs: &HashMap<u64, CampaignObjectiveRecord>,
    field: u32,
    out: &mut Vec<u8>,
) {
    let mut total_progress = 0u64;
    let mut contributors = 0u64;
    for oid in &camp.objectives {
        if let Some(o) = objs.get(oid) {
            total_progress = total_progress.saturating_add(o.current_progress.max(0) as u64);
            contributors = contributors.saturating_add(o.contributors.len() as u64);
        }
    }
    let mut stats = Vec::new();
    encode_varint_field(1, total_progress, &mut stats);
    encode_varint_field(2, contributors, &mut stats);
    encode_bytes_field(field, &stats, out);
}

fn encode_objective_attributes(
    obj: &CampaignObjectiveRecord,
    camp_name: &str,
    field: u32,
    out: &mut Vec<u8>,
) {
    let mut attr = Vec::new();
    encode_objective_def_id(1, &obj.name, camp_name, &mut attr);
    encode_varint_field(2, u64::from(obj.state), &mut attr);
    if obj.transition_unix > 0 {
        encode_timestamp(3, obj.transition_unix, &mut attr);
    }
    if obj.state < OBJECTIVE_STATE_COMPLETED {
        encode_varint_field(4, 1, &mut attr);
    } else if obj.transition_unix > 0 {
        encode_timestamp(5, obj.transition_unix, &mut attr);
    }
    encode_lifecycle(6, obj.state, obj.transition_unix, &mut attr);
    encode_bytes_field(field, &attr, out);
}

fn encode_objective_stats(obj: &CampaignObjectiveRecord, field: u32, out: &mut Vec<u8>) {
    let mut stats = Vec::new();
    encode_varint_field(1, obj.current_progress.max(0) as u64, &mut stats);
    encode_varint_field(2, obj.committed_participants.len() as u64, &mut stats);
    encode_varint_field(3, obj.contributors.len() as u64, &mut stats);
    encode_varint_field(4, obj.contributors.len() as u64, &mut stats);
    encode_bytes_field(field, &stats, out);
}

fn encode_track_entry(trk: &CampaignTrackRecord, field: u32, out: &mut Vec<u8>) {
    let mut entry = Vec::new();
    encode_id_field(1, track_uuid(trk.id), &mut entry);
    let mut attr = Vec::new();
    let mut def_id = Vec::new();
    encode_string_field(1, &trk.key, &mut def_id);
    encode_bytes_field(1, &def_id, &mut attr);
    encode_lifecycle(3, trk.state, trk.transition_unix, &mut attr);
    encode_bytes_field(2, &attr, &mut entry);
    let mut stats = Vec::new();
    encode_varint_field(1, trk.current_progress, &mut stats);
    encode_bytes_field(3, &stats, &mut entry);
    encode_bytes_field(field, &entry, out);
}

fn emit_campaign_event(
    event_type: &str,
    camp: &CampaignRecord,
    objs: &HashMap<u64, CampaignObjectiveRecord>,
    now: u64,
) {
    let mut payload = Vec::new();
    encode_id_field(1, campaign_uuid(camp.id), &mut payload);
    if matches!(
        event_type,
        "eve.campaign.api.Started"
            | "eve.campaign.api.Completed"
            | "eve.campaign.api.Cancelled"
            | "eve.campaign.api.Failed"
            | "eve.campaign.api.Scheduled"
            | "eve.campaign.api.ScheduleUpdated"
            | "eve.campaign.api.ScheduleCancelled"
            | "eve.campaign.api.Expired"
    ) {
        let mut attr = Vec::new();
        encode_campaign_def_id(1, &camp.name, &mut attr);
        encode_varint_field(2, u64::from(camp.state), &mut attr);
        encode_bytes_field(2, &attr, &mut payload);
    }
    if matches!(
        event_type,
        "eve.campaign.api.Completed"
            | "eve.campaign.api.Cancelled"
            | "eve.campaign.api.Failed"
            | "eve.campaign.api.Expired"
    ) {
        encode_campaign_stats(camp, objs, 3, &mut payload);
    }
    let mut notices = PendingNotices::new();
    notices.push((NoticeScope::Broadcast, any(event_type, payload)));
    queue_notices(notices, now);
}

fn emit_objective_event(event_type: &str, obj: &CampaignObjectiveRecord, camp_name: &str, now: u64) {
    let mut payload = Vec::new();
    encode_id_field(1, objective_uuid(obj.id), &mut payload);
    encode_objective_attributes(obj, camp_name, 2, &mut payload);
    if matches!(
        event_type,
        "eve.campaign.objective.api.Instantiated" | "eve.campaign.objective.api.Started"
    ) {
        encode_id_field(3, campaign_uuid(obj.campaign_id), &mut payload);
    } else if matches!(
        event_type,
        "eve.campaign.objective.api.Completed"
            | "eve.campaign.objective.api.Cancelled"
            | "eve.campaign.objective.api.Failed"
            | "eve.campaign.objective.api.Expired"
    ) {
        encode_objective_stats(obj, 3, &mut payload);
    }
    let mut notices = PendingNotices::new();
    notices.push((NoticeScope::Broadcast, any(event_type, payload)));
    queue_notices(notices, now);
}

fn next_id<T>(map: &HashMap<u64, T>, base: u64) -> u64 {
    map.keys().copied().max().unwrap_or(base).saturating_add(1)
}

/// Registers all Batch-9 `eve.campaign.*` Quasar RPC, Command, Definition, Objective, and Track handlers.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    _db: Arc<WorldRepository>,
    progression: Arc<ProgressionManagers>,
) {
    let extra = Arc::clone(&progression.extra);

    register_campaign_read_handlers(router, Arc::clone(&extra));
    register_campaign_admin_handlers(router, Arc::clone(&extra));
    register_campaign_command_handlers(router, Arc::clone(&extra));
    register_campaign_definition_handlers(router, Arc::clone(&extra));
    register_objective_handlers(router, Arc::clone(&extra));
    register_track_handlers(router, extra);
}

fn register_campaign_read_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    // 1. eve.campaign.api.GetAllRequest -> GetAllResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.GetAllRequest",
        Arc::new(move |_ctx, _any, msg| {
            let no_filtering = extract_u64_field(msg, 2).unwrap_or(1) != 0;
            let state_filter = extract_u64_field(msg, 3).unwrap_or(0) as u32;
            let no_ns_filter = extract_u64_field(msg, 4).unwrap_or(1) != 0;
            let ns_filter = extract_nested_string(dyn_field(msg, 5), 1).unwrap_or_default();

            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<u64> = camps
                .values()
                .filter(|c| no_filtering || state_filter == 0 || c.state == state_filter)
                .filter(|c| no_ns_filter || ns_filter.is_empty() || c.namespace == ns_filter)
                .map(|c| c.id)
                .collect();
            ids.sort_unstable();

            let mut out = Vec::new();
            for id in ids {
                encode_id_field(1, campaign_uuid(id), &mut out);
            }
            Ok(any("eve.campaign.api.GetAllResponse", out))
        }),
    );

    // 2. eve.campaign.api.GetRequest -> GetResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps
                .get(&cid)
                .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let trks = ex.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            encode_campaign_attributes(camp, &mut out);
            for oid in &camp.objectives {
                if let Some(o) = objs.get(oid) {
                    let mut obj_entry = Vec::new();
                    encode_id_field(1, objective_uuid(o.id), &mut obj_entry);
                    encode_objective_attributes(o, &camp.name, 2, &mut obj_entry);
                    encode_objective_stats(o, 4, &mut obj_entry);
                    encode_bytes_field(2, &obj_entry, &mut out);
                    encode_id_field(6, objective_uuid(o.id), &mut out);
                }
            }
            for trk in trks.values().filter(|t| t.campaign_id == cid) {
                encode_track_entry(trk, 4, &mut out);
            }
            encode_campaign_stats(camp, &objs, 5, &mut out);
            Ok(any("eve.campaign.api.GetResponse", out))
        }),
    );

    // 3. eve.campaign.api.GetStatusRequest -> GetStatusResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.GetStatusRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps
                .get(&cid)
                .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let trks = ex.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            encode_lifecycle(1, camp.state, camp.transition_unix, &mut out);
            encode_campaign_stats(camp, &objs, 2, &mut out);
            for trk in trks.values().filter(|t| t.campaign_id == cid) {
                encode_track_entry(trk, 3, &mut out);
            }
            Ok(any("eve.campaign.api.GetStatusResponse", out))
        }),
    );

    // 4. eve.campaign.api.GetLimitsRequest -> GetLimitsResponse
    router.register_handler(
        "eve.campaign.api.GetLimitsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_varint_field(1, u64::from(MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT), &mut out);
            Ok(any("eve.campaign.api.GetLimitsResponse", out))
        }),
    );

    // 5. eve.campaign.api.GetStatsRequest -> GetStatsResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.GetStatsRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps
                .get(&cid)
                .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            encode_campaign_stats(camp, &objs, 1, &mut out);
            for oid in &camp.objectives {
                if let Some(o) = objs.get(oid) {
                    let mut ostats = Vec::new();
                    encode_id_field(1, objective_uuid(o.id), &mut ostats);
                    encode_objective_stats(o, 2, &mut ostats);
                    encode_bytes_field(2, &ostats, &mut out);
                }
            }
            Ok(any("eve.campaign.api.GetStatsResponse", out))
        }),
    );
}

fn register_campaign_admin_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    // 1. ScheduleRequest -> ScheduleResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.ScheduleRequest",
        Arc::new(move |_ctx, _any, msg| {
            let def_name = extract_nested_string(dyn_field(msg, 1), 1)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "campaign.default".into());
            let start_time = extract_nested_varint(dyn_field(msg, 2), 1).unwrap_or_else(now_unix);
            let now = now_unix();

            let camp = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let cid = next_id(&camps, 100);
                let rec = CampaignRecord {
                    id: cid,
                    name: def_name,
                    description: "Scheduled campaign".into(),
                    namespace: "live".into(),
                    scheduled_start_unix: start_time,
                    scheduled_expiry_unix: start_time.saturating_add(86_400),
                    state: 0,
                    transition_unix: now,
                    etag: 1,
                    version_notice_pending: false,
                    objectives: Vec::new(),
                };
                camps.insert(cid, rec.clone());
                rec
            };
            ex.save_to_db();
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            emit_campaign_event("eve.campaign.api.Scheduled", &camp, &objs, now);

            let mut out = Vec::new();
            encode_id_field(1, campaign_uuid(camp.id), &mut out);
            Ok(any("eve.campaign.api.admin.ScheduleResponse", out))
        }),
    );

    // 2. UpdateScheduleRequest -> UpdateScheduleResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.UpdateScheduleRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let new_start = extract_nested_varint(dyn_field(msg, 2), 1).unwrap_or_else(now_unix);
            let now = now_unix();

            let camp = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let rec = camps
                    .get_mut(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                rec.scheduled_start_unix = new_start;
                rec.transition_unix = now;
                rec.clone()
            };
            ex.save_to_db();
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            emit_campaign_event("eve.campaign.api.ScheduleUpdated", &camp, &objs, now);
            Ok(any("eve.campaign.api.admin.UpdateScheduleResponse", Vec::new()))
        }),
    );

    // 3. CancelScheduleRequest -> CancelScheduleResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.CancelScheduleRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let now = now_unix();
            let camp = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let rec = camps
                    .get_mut(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                rec.state = CAMPAIGN_STATE_CANCELLED;
                rec.transition_unix = now;
                rec.clone()
            };
            ex.save_to_db();
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            emit_campaign_event("eve.campaign.api.ScheduleCancelled", &camp, &objs, now);
            Ok(any("eve.campaign.api.admin.CancelScheduleResponse", Vec::new()))
        }),
    );

    // 4. FastForwardRequest -> FastForwardResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.FastForwardRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let now = now_unix();
            let (camp, ev) = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let rec = camps
                    .get_mut(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                let ev = if rec.state == 0 {
                    rec.state = CAMPAIGN_STATE_ACTIVE;
                    "eve.campaign.api.Started"
                } else {
                    rec.state = CAMPAIGN_STATE_EXPIRED;
                    "eve.campaign.api.Expired"
                };
                rec.transition_unix = now;
                (rec.clone(), ev)
            };
            ex.save_to_db();
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            emit_campaign_event(ev, &camp, &objs, now);
            Ok(any("eve.campaign.api.admin.FastForwardResponse", Vec::new()))
        }),
    );

    // 5-8. CompleteRequest, FailRequest, CancelRequest, ArchiveRequest
    for (req_name, resp_name, next_state, ev_name) in [
        ("eve.campaign.api.admin.CompleteRequest", "eve.campaign.api.admin.CompleteResponse", CAMPAIGN_STATE_COMPLETED, "eve.campaign.api.Completed"),
        ("eve.campaign.api.admin.FailRequest", "eve.campaign.api.admin.FailResponse", CAMPAIGN_STATE_FAILED, "eve.campaign.api.Failed"),
        ("eve.campaign.api.admin.CancelRequest", "eve.campaign.api.admin.CancelResponse", CAMPAIGN_STATE_CANCELLED, "eve.campaign.api.Cancelled"),
        ("eve.campaign.api.admin.ArchiveRequest", "eve.campaign.api.admin.ArchiveResponse", CAMPAIGN_STATE_ARCHIVED, "eve.campaign.api.Archived"),
    ] {
        let ex = Arc::clone(&extra);
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, msg| {
                let cid = extract_campaign_id(dyn_field(msg, 1))
                    .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
                let now = now_unix();
                let camp = {
                    let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                    let rec = camps
                        .get_mut(&cid)
                        .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                    rec.state = next_state;
                    rec.transition_unix = now;
                    rec.clone()
                };
                ex.save_to_db();
                let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
                emit_campaign_event(ev_name, &camp, &objs, now);
                Ok(any(resp_name, Vec::new()))
            }),
        );
    }

    // 9. UpdateDefinitionRequest -> UpdateDefinitionResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.UpdateDefinitionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let now = now_unix();
            let camp = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let rec = camps
                    .get_mut(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                rec.etag = rec.etag.saturating_add(1);
                rec.transition_unix = now;
                rec.clone()
            };
            ex.save_to_db();
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            emit_campaign_event("eve.campaign.api.DefinitionUpdated", &camp, &objs, now);
            Ok(any("eve.campaign.api.admin.UpdateDefinitionResponse", Vec::new()))
        }),
    );

    // 10. DeleteRequest -> DeleteResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.api.admin.DeleteRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                camps
                    .remove(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
            }
            ex.save_to_db();
            Ok(any("eve.campaign.api.admin.DeleteResponse", Vec::new()))
        }),
    );
}

fn register_campaign_command_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    for (cmd_name, target_state, ev_opt) in [
        ("eve.campaign.api.StartCommanded", Some(CAMPAIGN_STATE_ACTIVE), Some("eve.campaign.api.Started")),
        ("eve.campaign.api.EvaluateRelativeScheduleTriggersCommanded", None, None),
        ("eve.campaign.api.ChangeStateCommanded", None, None),
        ("eve.campaign.api.ExpireCommanded", Some(CAMPAIGN_STATE_EXPIRED), Some("eve.campaign.api.Expired")),
        ("eve.campaign.api.TriggerNextObjectivesCommanded", None, None),
        ("eve.campaign.api.CompleteCommanded", Some(CAMPAIGN_STATE_COMPLETED), Some("eve.campaign.api.Completed")),
    ] {
        let ex = Arc::clone(&extra);
        router.register_handler(
            cmd_name,
            Arc::new(move |_ctx, _any, msg| {
                let cid = extract_campaign_id(dyn_field(msg, 1))
                    .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
                let now = now_unix();
                let explicit_state = extract_nested_varint(dyn_field(msg, 2), 1).map(|v| v as u32);
                let camp = {
                    let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                    let rec = camps
                        .get_mut(&cid)
                        .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                    if let Some(st) = target_state.or(explicit_state) {
                        rec.state = st;
                    }
                    rec.transition_unix = now;
                    rec.clone()
                };
                if cmd_name == "eve.campaign.api.TriggerNextObjectivesCommanded" {
                    let mut objs = ex.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                    for oid in &camp.objectives {
                        if let Some(o) = objs.get_mut(oid) {
                            if o.state == 0 {
                                o.state = OBJECTIVE_STATE_ACTIVE;
                                o.transition_unix = now;
                            }
                        }
                    }
                }
                ex.save_to_db();
                if let Some(ev) = ev_opt {
                    let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
                    emit_campaign_event(ev, &camp, &objs, now);
                }
                Ok(any("google.protobuf.Empty", Vec::new()))
            }),
        );
    }
}

fn register_campaign_definition_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    // 1. GetAllVersionsRequest -> GetAllVersionsResponse
    router.register_handler(
        "eve.campaign.definition.api.GetAllVersionsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_semver(1, 1, 0, 0, &mut out);
            encode_semver(1, 1, 1, 0, &mut out);
            Ok(any("eve.campaign.definition.api.GetAllVersionsResponse", out))
        }),
    );

    // 2. GetAllRequest -> GetAllResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.definition.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let mut names: Vec<String> = camps.values().map(|c| c.name.clone()).collect();
            names.sort();
            names.dedup();
            let mut out = Vec::new();
            for name in names {
                encode_campaign_def_id(1, &name, &mut out);
            }
            Ok(any("eve.campaign.definition.api.GetAllResponse", out))
        }),
    );

    // 3. GetRequest -> GetResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.definition.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let def_name = extract_nested_string(dyn_field(msg, 1), 1)
                .ok_or((STATUS_BAD_REQUEST, "missing definition identifier".into()))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps
                .values()
                .find(|c| c.name == def_name)
                .ok_or((STATUS_NOT_FOUND, format!("campaign definition '{def_name}' not found")))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            let mut def_attr = Vec::new();
            encode_varint_field(17, camp.objectives.len() as u64, &mut def_attr);
            encode_string_field(23, &camp.description, &mut def_attr);
            encode_bytes_field(1, &def_attr, &mut out);
            for oid in &camp.objectives {
                if let Some(o) = objs.get(oid) {
                    encode_objective_def_id(4, &o.name, &camp.name, &mut out);
                }
            }
            Ok(any("eve.campaign.definition.api.GetResponse", out))
        }),
    );
}

fn register_objective_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    // 1. eve.campaign.objective.api.GetRequest -> GetResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let oid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs
                .get(&oid)
                .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp_name = camps
                .get(&obj.campaign_id)
                .map(|c| c.name.as_str())
                .unwrap_or("campaign.default");

            let mut out = Vec::new();
            encode_objective_attributes(obj, camp_name, 1, &mut out);
            encode_objective_stats(obj, 3, &mut out);
            Ok(any("eve.campaign.objective.api.GetResponse", out))
        }),
    );

    // 2. eve.campaign.objective.api.GetStatusRequest -> GetStatusResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.GetStatusRequest",
        Arc::new(move |_ctx, _any, msg| {
            let oid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs
                .get(&oid)
                .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
            let mut out = Vec::new();
            encode_lifecycle(1, obj.state, obj.transition_unix, &mut out);
            encode_objective_stats(obj, 2, &mut out);
            Ok(any("eve.campaign.objective.api.GetStatusResponse", out))
        }),
    );

    // 3. eve.campaign.objective.api.GetDefinitionRequest -> GetDefinitionResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.GetDefinitionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let oid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs
                .get(&oid)
                .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
            let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp_name = camps
                .get(&obj.campaign_id)
                .map(|c| c.name.as_str())
                .unwrap_or("campaign.default");

            let mut out = Vec::new();
            encode_objective_def_id(1, &obj.name, camp_name, &mut out);
            let mut def_attr = Vec::new();
            encode_varint_field(9, obj.target_progress.max(0) as u64, &mut def_attr);
            encode_bytes_field(2, &def_attr, &mut out);
            Ok(any("eve.campaign.objective.api.GetDefinitionResponse", out))
        }),
    );

    // 4. eve.campaign.objective.definition.api.GetRequest -> GetResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.definition.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let def_name = extract_nested_string(dyn_field(msg, 1), 1)
                .ok_or((STATUS_BAD_REQUEST, "missing objective definition identifier".into()))?;
            let objs = ex.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs
                .values()
                .find(|o| o.name == def_name)
                .ok_or((STATUS_NOT_FOUND, format!("objective definition '{def_name}' not found")))?;
            let mut out = Vec::new();
            let mut def_attr = Vec::new();
            encode_varint_field(9, obj.target_progress.max(0) as u64, &mut def_attr);
            encode_bytes_field(1, &def_attr, &mut out);
            Ok(any("eve.campaign.objective.definition.api.GetResponse", out))
        }),
    );

    // 5. eve.campaign.objective.api.admin.InstantiateRequest -> InstantiateResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.admin.InstantiateRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let obj_name = extract_nested_string(dyn_field(msg, 2), 1)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "objective.default".into());
            let now = now_unix();

            let (obj, camp_name) = {
                let mut camps = ex.campaigns.write().unwrap_or_else(|e| e.into_inner());
                let camp = camps
                    .get_mut(&cid)
                    .ok_or((STATUS_NOT_FOUND, format!("campaign {cid} not found")))?;
                let mut objs = ex.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let oid = next_id(&objs, 1_000);
                let rec = CampaignObjectiveRecord {
                    id: oid,
                    campaign_id: cid,
                    name: obj_name,
                    target_progress: 100,
                    current_progress: 0,
                    state: 0,
                    transition_unix: now,
                    ..Default::default()
                };
                objs.insert(oid, rec.clone());
                camp.objectives.push(oid);
                (rec, camp.name.clone())
            };
            ex.save_to_db();
            emit_objective_event("eve.campaign.objective.api.Instantiated", &obj, &camp_name, now);

            let mut out = Vec::new();
            encode_id_field(1, objective_uuid(obj.id), &mut out);
            Ok(any("eve.campaign.objective.api.admin.InstantiateResponse", out))
        }),
    );

    // 6. eve.campaign.objective.api.admin.ProgressRequest -> ProgressResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, msg| {
            let oid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
            let char_id = extract_character_id(dyn_field(msg, 2), ctx.effective_character_id().unwrap_or(1));
            let delta = extract_u64_field(msg, 3).unwrap_or(0) as i64;
            let now = now_unix();

            let (obj, completed_now, new_part_prog) = {
                let mut objs = ex.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let rec = objs
                    .get_mut(&oid)
                    .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
                rec.current_progress = rec.current_progress.saturating_add(delta);
                let entry = rec.contributors.entry(char_id).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(delta);
                let part_prog = entry.0;
                let mut completed_now = false;
                if rec.target_progress > 0
                    && rec.current_progress >= rec.target_progress
                    && rec.state != OBJECTIVE_STATE_COMPLETED
                {
                    rec.state = OBJECTIVE_STATE_COMPLETED;
                    rec.transition_unix = now;
                    completed_now = true;
                }
                (rec.clone(), completed_now, part_prog)
            };
            ex.save_to_db();

            let mut prog_ev = Vec::new();
            encode_id_field(1, objective_uuid(obj.id), &mut prog_ev);
            encode_id_field(2, campaign_uuid(obj.campaign_id), &mut prog_ev);
            let mut char_id_msg = Vec::new();
            encode_varint_field(1, u64::from(char_id), &mut char_id_msg);
            encode_bytes_field(3, &char_id_msg, &mut prog_ev);
            encode_varint_field(4, delta.max(0) as u64, &mut prog_ev);
            encode_varint_field(5, new_part_prog.max(0) as u64, &mut prog_ev);
            let mut notices = PendingNotices::new();
            notices.push((
                NoticeScope::Broadcast,
                any("eve.campaign.objective.api.Progressed", prog_ev),
            ));
            queue_notices(notices, now);

            if completed_now {
                let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
                let camp_name = camps
                    .get(&obj.campaign_id)
                    .map(|c| c.name.as_str())
                    .unwrap_or("campaign.default");
                emit_objective_event("eve.campaign.objective.api.Completed", &obj, camp_name, now);
            }
            Ok(any("eve.campaign.objective.api.admin.ProgressResponse", Vec::new()))
        }),
    );

    // 7-11. Objective admin lifecycle requests
    for (req_name, resp_name, next_state, ev_name) in [
        ("eve.campaign.objective.api.admin.StartRequest", "eve.campaign.objective.api.admin.StartResponse", OBJECTIVE_STATE_ACTIVE, "eve.campaign.objective.api.Started"),
        ("eve.campaign.objective.api.admin.CompleteRequest", "eve.campaign.objective.api.admin.CompleteResponse", OBJECTIVE_STATE_COMPLETED, "eve.campaign.objective.api.Completed"),
        ("eve.campaign.objective.api.admin.FailRequest", "eve.campaign.objective.api.admin.FailResponse", 5, "eve.campaign.objective.api.Failed"),
        ("eve.campaign.objective.api.admin.CancelRequest", "eve.campaign.objective.api.admin.CancelResponse", 4, "eve.campaign.objective.api.Cancelled"),
    ] {
        let ex = Arc::clone(&extra);
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, msg| {
                let oid = extract_campaign_id(dyn_field(msg, 1))
                    .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
                let now = now_unix();
                let obj = {
                    let mut objs = ex.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                    let rec = objs
                        .get_mut(&oid)
                        .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
                    rec.state = next_state;
                    rec.transition_unix = now;
                    rec.clone()
                };
                ex.save_to_db();
                let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
                let camp_name = camps
                    .get(&obj.campaign_id)
                    .map(|c| c.name.as_str())
                    .unwrap_or("campaign.default");
                emit_objective_event(ev_name, &obj, camp_name, now);
                Ok(any(resp_name, Vec::new()))
            }),
        );
    }

    // 12. DisableRequest -> DisableResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.objective.api.admin.DisableRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing campaign identifier".into()))?;
            let now = now_unix();
            let mut disabled_payload = Vec::new();
            encode_id_field(1, campaign_uuid(cid), &mut disabled_payload);
            let mut notices = PendingNotices::new();
            notices.push((
                NoticeScope::Broadcast,
                any("eve.campaign.objective.api.Disabled", disabled_payload),
            ));
            queue_notices(notices, now);
            ex.save_to_db();
            Ok(any("eve.campaign.objective.api.admin.DisableResponse", Vec::new()))
        }),
    );

    // 13-14. Objective commands
    for (cmd_name, target_state, ev_opt) in [
        ("eve.campaign.objective.api.ChangeStateCommanded", None, None),
        ("eve.campaign.objective.api.CompleteCommanded", Some(OBJECTIVE_STATE_COMPLETED), Some("eve.campaign.objective.api.Completed")),
    ] {
        let ex = Arc::clone(&extra);
        router.register_handler(
            cmd_name,
            Arc::new(move |_ctx, _any, msg| {
                let oid = extract_campaign_id(dyn_field(msg, 1))
                    .ok_or((STATUS_BAD_REQUEST, "missing objective identifier".into()))?;
                let explicit = extract_nested_varint(dyn_field(msg, 2), 1).map(|v| v as u32);
                let now = now_unix();
                let obj = {
                    let mut objs = ex.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                    let rec = objs
                        .get_mut(&oid)
                        .ok_or((STATUS_NOT_FOUND, format!("objective {oid} not found")))?;
                    if let Some(st) = target_state.or(explicit) {
                        rec.state = st;
                    }
                    rec.transition_unix = now;
                    rec.clone()
                };
                ex.save_to_db();
                if let Some(ev) = ev_opt {
                    let camps = ex.campaigns.read().unwrap_or_else(|e| e.into_inner());
                    let camp_name = camps
                        .get(&obj.campaign_id)
                        .map(|c| c.name.as_str())
                        .unwrap_or("campaign.default");
                    emit_objective_event(ev, &obj, camp_name, now);
                }
                Ok(any("google.protobuf.Empty", Vec::new()))
            }),
        );
    }
}

fn register_track_handlers(router: &PublicGatewayRouter, extra: Arc<ProgressionExtraManager>) {
    // 1. eve.campaign.track.api.admin.ProgressRequest -> ProgressResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.track.api.admin.ProgressRequest",
        Arc::new(move |_ctx, _any, msg| {
            let tid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing track identifier".into()))?;
            let delta = extract_u64_field(msg, 2).unwrap_or(0);
            {
                let mut trks = ex.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
                let rec = trks
                    .get_mut(&tid)
                    .ok_or((STATUS_NOT_FOUND, format!("track {tid} not found")))?;
                rec.current_progress = rec.current_progress.saturating_add(delta);
                rec.transition_unix = now_unix();
            }
            ex.save_to_db();
            Ok(any("eve.campaign.track.api.admin.ProgressResponse", Vec::new()))
        }),
    );

    // 2. eve.campaign.track.api.admin.ProgressForCharacterRequest -> ProgressForCharacterResponse
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.track.api.admin.ProgressForCharacterRequest",
        Arc::new(move |ctx, _any, msg| {
            let tid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing track identifier".into()))?;
            let char_id = extract_character_id(dyn_field(msg, 2), ctx.effective_character_id().unwrap_or(1));
            let delta = extract_u64_field(msg, 3).unwrap_or(0);
            {
                let mut trks = ex.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
                let rec = trks
                    .get_mut(&tid)
                    .ok_or((STATUS_NOT_FOUND, format!("track {tid} not found")))?;
                rec.current_progress = rec.current_progress.saturating_add(delta);
                *rec.participants.entry(char_id).or_insert(0) += delta;
                rec.transition_unix = now_unix();
            }
            ex.save_to_db();
            Ok(any("eve.campaign.track.api.admin.ProgressForCharacterResponse", Vec::new()))
        }),
    );

    // 3. eve.campaign.track.api.ProcessTickContributionCommanded
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.track.api.ProcessTickContributionCommanded",
        Arc::new(move |_ctx, _any, msg| {
            let tid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing track identifier".into()))?;
            let tick_key = extract_string_field(msg, 2).unwrap_or_default();
            {
                let mut trks = ex.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
                let rec = trks
                    .get_mut(&tid)
                    .ok_or((STATUS_NOT_FOUND, format!("track {tid} not found")))?;
                let step = rec
                    .ticking
                    .iter()
                    .find(|t| tick_key.is_empty() || t.key == tick_key)
                    .and_then(|t| t.evaluations.first())
                    .and_then(|e| e.constant)
                    .unwrap_or(1)
                    .max(1) as u64;
                rec.current_progress = rec.current_progress.saturating_add(step);
                rec.transition_unix = now_unix();
            }
            ex.save_to_db();
            Ok(any("google.protobuf.Empty", Vec::new()))
        }),
    );

    // 4. eve.campaign.track.api.ChangeStateCommanded
    let ex = Arc::clone(&extra);
    router.register_handler(
        "eve.campaign.track.api.ChangeStateCommanded",
        Arc::new(move |_ctx, _any, msg| {
            let tid = extract_campaign_id(dyn_field(msg, 1))
                .ok_or((STATUS_BAD_REQUEST, "missing track identifier".into()))?;
            let next_state = extract_nested_varint(dyn_field(msg, 2), 1)
                .map(|v| v as u32)
                .unwrap_or(TRACK_STATE_STARTED);
            {
                let mut trks = ex.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
                let rec = trks
                    .get_mut(&tid)
                    .ok_or((STATUS_NOT_FOUND, format!("track {tid} not found")))?;
                rec.state = next_state;
                rec.transition_unix = now_unix();
            }
            ex.save_to_db();
            Ok(any("google.protobuf.Empty", Vec::new()))
        }),
    );
}
