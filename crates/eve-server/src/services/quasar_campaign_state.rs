//! `quasar_campaign_state` — persisted campaign / objective / track state and its lifecycle engine
//! for the V24 `eve_public.campaign.*` Quasar APIs
//! (`quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj`,
//!  `quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj`).
//!
//! Campaign definitions are server-side content (CCP ships none in the client), so they are
//! operator-authored through [`ProgressionExtraManager::register_campaign`] and persisted in the
//! `WorldStateSnapshot.quasar_state_blobs` (`quasar_campaigns`, `quasar_campaign_objectives`,
//! `quasar_campaign_tracks`). Objectives must reference an authentic contribution method from
//! [`AUTHENTIC_CONTRIBUTION_METHODS`]. Every state transition below (campaign start / completion /
//! expiry / definition update, objective + track contribution) is driven by the server clock or by a
//! real gameplay event and emits the matching V24 notice.

#![forbid(unsafe_code)]

use super::quasar_campaign_definitions::AUTHENTIC_CONTRIBUTION_METHODS;
use super::quasar_progression_extra::ProgressionExtraManager;
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX};
use std::collections::HashMap;

/// `eve_public.campaign.State` values used by the engine (0 = scheduled, not yet started).
pub const CAMPAIGN_STATE_ACTIVE: u32 = 1;
pub const CAMPAIGN_STATE_COMPLETED: u32 = 2;
pub const CAMPAIGN_STATE_EXPIRED: u32 = 3;
/// `eve_public.campaign.objective.State` shares the campaign enum numbering.
pub const OBJECTIVE_STATE_ACTIVE: u32 = 1;
pub const OBJECTIVE_STATE_COMPLETED: u32 = 2;
pub const OBJECTIVE_STATE_EXPIRED: u32 = 3;
/// `eve_public.campaign.track.State`.
pub const TRACK_STATE_STARTED: u32 = 1;
pub const TRACK_STATE_TARGET_REACHED: u32 = 2;
pub const TRACK_STATE_STOPPED: u32 = 3;
/// `eve_public.campaign.objective.CommitmentState` / `ParticipantState`.
pub const COMMITMENT_STATE_COMMITTED: u32 = 1;
pub const COMMITMENT_STATE_RESIGNED: u32 = 2;
pub const PARTICIPANT_STATE_CONTRIBUTION_COMPLETED: u32 = 3;
/// DERIVED: `GetLimitsResponse.max_active_objectives_per_participant` — CCP's live value is
/// server-side; EvoMu enforces this operator default on `CommitRequest`.
pub const MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT: u32 = 10;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CampaignRecord {
    pub id: u64,
    /// `campaign.Attributes.definition_name`.
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub scheduled_start_unix: u64,
    /// 0 = no scheduled expiry.
    #[serde(default)]
    pub scheduled_expiry_unix: u64,
    /// `campaign.State`; 0 while the campaign is scheduled but not yet started.
    #[serde(default)]
    pub state: u32,
    #[serde(default)]
    pub transition_unix: u64,
    /// `campaign.Version.etag`, bumped whenever the operator re-registers the definition.
    #[serde(default)]
    pub etag: u64,
    #[serde(default)]
    pub version_notice_pending: bool,
    pub objectives: Vec<u64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CampaignObjectiveRecord {
    pub id: u64,
    pub campaign_id: u64,
    /// `objective.Attributes.key`.
    pub name: String,
    pub target_progress: i64,
    pub current_progress: i64,
    pub committed_participants: Vec<u32>,
    #[serde(default)]
    pub resigned_participants: Vec<u32>,
    /// char_id -> (participant progress, unredeemed ISK reward intervals).
    pub contributors: HashMap<u32, (i64, i64)>,
    /// Name of an entry in [`AUTHENTIC_CONTRIBUTION_METHODS`].
    #[serde(default)]
    pub contribution_method: String,
    /// `objective.Attributes.max_progress_per_participant`; 0 = unlimited.
    #[serde(default)]
    pub max_progress_per_participant: i64,
    /// `objective.Rewards.IskReward.amount_per_interval` (whole ISK).
    #[serde(default)]
    pub isk_reward_per_interval: i64,
    /// `objective.Rewards.IskReward.progress_interval`.
    #[serde(default)]
    pub isk_reward_progress_interval: i64,
    #[serde(default)]
    pub state: u32,
    #[serde(default)]
    pub transition_unix: u64,
    /// `contributionmethod.Configuration.parameters`: matcher constraints on the event context.
    #[serde(default)]
    pub parameters: Vec<ObjectiveParameter>,
}

/// One `ParameterConfiguration { key; matcher { values } }` constraint of an objective.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ObjectiveParameter {
    /// Parameter key of the contribution method (e.g. `ore_type`, `solar_system`, `faction`).
    pub key: String,
    /// Accepted values (decimal ids).
    pub values: Vec<String>,
}

/// A concrete gameplay event offered to campaign objectives.
#[derive(Debug, Clone, Copy, Default)]
pub struct CampaignEvent<'a> {
    /// Exact contribution method name from [`AUTHENTIC_CONTRIBUTION_METHODS`].
    pub method: &'a str,
    pub type_id: Option<u32>,
    pub solar_system_id: Option<u32>,
    pub faction_id: Option<i64>,
}

impl CampaignEvent<'_> {
    /// Event value for a contribution-method parameter key (None = the event cannot satisfy it).
    pub fn param(&self, key: &str) -> Option<String> {
        match key {
            "ore_type" | "item_type" | "target_ship_type" | "ship" => self.type_id.map(|v| v.to_string()),
            "solar_system" | "origin" => self.solar_system_id.map(|v| v.to_string()),
            "faction" => self.faction_id.map(|v| v.to_string()),
            _ => None,
        }
    }

    /// True when `obj` uses exactly this method and every parameter constraint matches the event.
    pub fn matches(&self, obj: &CampaignObjectiveRecord) -> bool {
        obj.contribution_method == self.method
            && obj.parameters.iter().all(|p| self.param(&p.key).is_some_and(|v| p.values.contains(&v)))
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TickingEvaluation {
    pub key: String,
    /// `TickingContribution.Evaluation.constant`.
    pub constant: Option<i64>,
    /// `TickingContribution.Evaluation.per_unit_value`, multiplied by the committed participant count.
    pub per_unit_value: Option<i64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TickingContribution {
    pub key: String,
    pub interval_secs: u64,
    pub evaluations: Vec<TickingEvaluation>,
    #[serde(default)]
    pub last_tick_unix: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CampaignTrackRecord {
    pub id: u64,
    pub campaign_id: u64,
    pub key: String,
    /// 0 = `no_target_progress`.
    pub target_progress_value: u64,
    pub current_progress: u64,
    /// char_id -> participant contribution on this track.
    pub participants: HashMap<u32, u64>,
    /// `ParticipantContributionTrack.max_total_progress`; 0 = no contribution limit.
    #[serde(default)]
    pub max_participant_contribution: u64,
    /// `ParticipantContributionTrack.Reward.IskReward.amount_per_interval` (whole ISK).
    #[serde(default)]
    pub isk_reward_per_interval: i64,
    #[serde(default)]
    pub isk_reward_progress_interval: u64,
    #[serde(default)]
    pub redeemed_intervals: HashMap<u32, u64>,
    #[serde(default)]
    pub ticking: Vec<TickingContribution>,
    #[serde(default)]
    pub state: u32,
    #[serde(default)]
    pub transition_unix: u64,
}

pub type PendingNotices = Vec<(NoticeScope, GoogleAny)>;

pub fn campaign_id_to_uuid_bytes(id: u64) -> [u8; 16] {
    tagged_uuid(b"CAMPAIGN", id)
}
pub fn objective_id_to_uuid_bytes(id: u64) -> [u8; 16] {
    tagged_uuid(b"CAMPOBJ_", id)
}
pub fn track_id_to_uuid_bytes(id: u64) -> [u8; 16] {
    tagged_uuid(b"CAMPTRK_", id)
}
fn tagged_uuid(tag: &[u8; 8], id: u64) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(tag);
    uuid[8..16].copy_from_slice(&id.to_be_bytes());
    uuid
}

/// Encodes `{ <field 1>: Identifier { uuid = 1 } }` — the shared shape of every campaign notice id.
pub fn encode_identifier(uuid: &[u8; 16]) -> Vec<u8> {
    let mut id = Vec::new();
    encode_bytes_field(1, uuid, &mut id);
    id
}

fn notice(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
}

fn campaign_notice(type_name: &str, campaign_id: u64) -> GoogleAny {
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_identifier(&campaign_id_to_uuid_bytes(campaign_id)), &mut out);
    notice(type_name, out)
}

fn contributed_notice(type_name: &str, uuid: [u8; 16], total_participant_progress: u64) -> GoogleAny {
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_identifier(&uuid), &mut out);
    encode_varint_field(2, total_participant_progress, &mut out);
    notice(type_name, out)
}

/// `CurrentVersionNotice { versions: [CampaignVersion { campaign = 1, version = 2 { etag = 2 } }] }`.
fn current_version_notice(versions: &[(u64, u64)]) -> GoogleAny {
    let mut out = Vec::new();
    for (cid, etag) in versions {
        let mut ver = Vec::new();
        encode_varint_field(2, *etag, &mut ver);
        let mut entry = Vec::new();
        encode_bytes_field(1, &encode_identifier(&campaign_id_to_uuid_bytes(*cid)), &mut entry);
        encode_bytes_field(2, &ver, &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    notice("eve_public.campaign.api.CurrentVersionNotice", out)
}

/// Parameter list `(key, parameter type, icon)` of an authentic contribution method, if the name is known.
pub fn contribution_method_params(name: &str) -> Option<&'static [(&'static str, &'static str, &'static str)]> {
    AUTHENTIC_CONTRIBUTION_METHODS.iter().find(|m| m.0 == name).map(|m| m.4)
}

/// Value of one tick of `tc` given `units` committed participants.
pub fn ticking_value(tc: &TickingContribution, units: u64) -> i64 {
    tc.evaluations
        .iter()
        .map(|e| match (e.constant, e.per_unit_value) {
            (Some(c), _) => c,
            (None, Some(p)) => p.saturating_mul(units as i64),
            (None, None) => 0,
        })
        .fold(0i64, |a, b| a.saturating_add(b))
}

fn intervals(progress: i64, interval: i64) -> i64 {
    if interval > 0 {
        progress.max(0) / interval
    } else {
        0
    }
}

impl ProgressionExtraManager {
    /// Operator authoring entry point: validates and persists a campaign definition with its
    /// objectives and tracks. Re-registering an existing campaign id bumps its version etag, which
    /// the lifecycle engine broadcasts as `eve_public.campaign.api.CurrentVersionNotice`.
    pub fn register_campaign(
        &self,
        mut campaign: CampaignRecord,
        objectives: Vec<CampaignObjectiveRecord>,
        tracks: Vec<CampaignTrackRecord>,
    ) -> Result<(), String> {
        for obj in &objectives {
            if obj.campaign_id != campaign.id {
                return Err(format!("objective {} belongs to campaign {}", obj.id, obj.campaign_id));
            }
            let Some(params) = contribution_method_params(&obj.contribution_method) else {
                return Err(format!("objective {} has unknown contribution method '{}'", obj.id, obj.contribution_method));
            };
            if let Some(p) = obj.parameters.iter().find(|p| !params.iter().any(|(k, _, _)| *k == p.key)) {
                return Err(format!("objective {} parameter '{}' is not defined by {}", obj.id, p.key, obj.contribution_method));
            }
        }
        if let Some(t) = tracks.iter().find(|t| t.campaign_id != campaign.id) {
            return Err(format!("track {} belongs to campaign {}", t.id, t.campaign_id));
        }
        {
            let mut camps = self.campaigns.write().unwrap_or_else(|e| e.into_inner());
            if let Some(prev) = camps.get(&campaign.id) {
                campaign.etag = prev.etag.saturating_add(1);
                campaign.version_notice_pending = true;
                campaign.state = prev.state;
                campaign.transition_unix = prev.transition_unix;
            } else {
                campaign.etag = campaign.etag.max(1);
            }
            campaign.objectives = objectives.iter().map(|o| o.id).collect();
            camps.insert(campaign.id, campaign);
        }
        {
            let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
            for obj in objectives {
                let merged = match objs.get(&obj.id) {
                    Some(prev) => CampaignObjectiveRecord {
                        current_progress: prev.current_progress,
                        committed_participants: prev.committed_participants.clone(),
                        resigned_participants: prev.resigned_participants.clone(),
                        contributors: prev.contributors.clone(),
                        state: prev.state,
                        transition_unix: prev.transition_unix,
                        ..obj
                    },
                    None => obj,
                };
                objs.insert(merged.id, merged);
            }
        }
        {
            let mut trks = self.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
            for mut trk in tracks {
                if let Some(prev) = trks.get(&trk.id) {
                    for tc in &mut trk.ticking {
                        if let Some(p) = prev.ticking.iter().find(|p| p.key == tc.key) {
                            tc.last_tick_unix = p.last_tick_unix;
                        }
                    }
                }
                let merged = match trks.get(&trk.id) {
                    Some(prev) => CampaignTrackRecord {
                        current_progress: prev.current_progress,
                        participants: prev.participants.clone(),
                        redeemed_intervals: prev.redeemed_intervals.clone(),
                        state: prev.state,
                        transition_unix: prev.transition_unix,
                        ..trk
                    },
                    None => trk,
                };
                trks.insert(merged.id, merged);
            }
        }
        self.save_to_db();
        Ok(())
    }

    /// Distinct characters committed to any objective of `campaign_id` (the ticking `units_count`).
    pub fn campaign_committed_units(&self, campaign_id: u64) -> u64 {
        let objs = self.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
        let mut seen: Vec<u32> = objs
            .values()
            .filter(|o| o.campaign_id == campaign_id)
            .flat_map(|o| o.committed_participants.iter().copied())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len() as u64
    }

    /// Advances every campaign to `now`: scheduled start, ticking track contributions, expiry,
    /// completion (all objectives completed) and pending definition-version broadcasts.
    pub fn evaluate_campaign_lifecycle(&self, now: u64) -> PendingNotices {
        let mut out = PendingNotices::new();
        let camp_ids: Vec<u64> = self.campaigns.read().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
        let mut changed = false;
        let mut versions = Vec::new();
        for cid in camp_ids {
            let units = self.campaign_committed_units(cid);
            let mut camps = self.campaigns.write().unwrap_or_else(|e| e.into_inner());
            let Some(camp) = camps.get_mut(&cid) else { continue };
            if camp.version_notice_pending {
                camp.version_notice_pending = false;
                versions.push((cid, camp.etag));
                changed = true;
            }
            let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
            let mut trks = self.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
            if camp.state == 0 && now >= camp.scheduled_start_unix {
                camp.state = CAMPAIGN_STATE_ACTIVE;
                camp.transition_unix = camp.scheduled_start_unix;
                for o in objs.values_mut().filter(|o| o.campaign_id == cid && o.state == 0) {
                    o.state = OBJECTIVE_STATE_ACTIVE;
                    o.transition_unix = camp.scheduled_start_unix;
                }
                for t in trks.values_mut().filter(|t| t.campaign_id == cid && t.state == 0) {
                    t.state = TRACK_STATE_STARTED;
                    t.transition_unix = camp.scheduled_start_unix;
                    for tc in &mut t.ticking {
                        tc.last_tick_unix = camp.scheduled_start_unix;
                    }
                }
                out.push((NoticeScope::Broadcast, campaign_notice("eve_public.campaign.api.StartedNotice", cid)));
                changed = true;
            }
            if camp.state != CAMPAIGN_STATE_ACTIVE {
                continue;
            }
            let horizon = if camp.scheduled_expiry_unix > 0 { now.min(camp.scheduled_expiry_unix) } else { now };
            for t in trks.values_mut().filter(|t| t.campaign_id == cid && t.state == TRACK_STATE_STARTED) {
                for tc in &mut t.ticking {
                    let interval = tc.interval_secs.max(1);
                    let ticks = horizon.saturating_sub(tc.last_tick_unix) / interval;
                    if ticks == 0 {
                        continue;
                    }
                    tc.last_tick_unix += ticks * interval;
                    let gain = ticking_value(tc, units).max(0) as u64;
                    t.current_progress = t.current_progress.saturating_add(gain.saturating_mul(ticks));
                    changed = true;
                }
                if t.target_progress_value > 0 && t.current_progress >= t.target_progress_value {
                    t.current_progress = t.target_progress_value;
                    t.state = TRACK_STATE_TARGET_REACHED;
                    t.transition_unix = horizon;
                }
            }
            let camp_objs: Vec<&CampaignObjectiveRecord> = objs.values().filter(|o| o.campaign_id == cid).collect();
            if !camp_objs.is_empty() && camp_objs.iter().all(|o| o.state == OBJECTIVE_STATE_COMPLETED) {
                camp.state = CAMPAIGN_STATE_COMPLETED;
                camp.transition_unix = now;
                for t in trks.values_mut().filter(|t| t.campaign_id == cid && t.state == TRACK_STATE_STARTED) {
                    t.state = TRACK_STATE_STOPPED;
                    t.transition_unix = now;
                }
                out.push((NoticeScope::Broadcast, campaign_notice("eve_public.campaign.api.CompletedNotice", cid)));
                changed = true;
            } else if camp.scheduled_expiry_unix > 0 && now >= camp.scheduled_expiry_unix {
                camp.state = CAMPAIGN_STATE_EXPIRED;
                camp.transition_unix = camp.scheduled_expiry_unix;
                for o in objs.values_mut().filter(|o| o.campaign_id == cid && o.state == OBJECTIVE_STATE_ACTIVE) {
                    o.state = OBJECTIVE_STATE_EXPIRED;
                    o.transition_unix = camp.scheduled_expiry_unix;
                }
                for t in trks.values_mut().filter(|t| t.campaign_id == cid && t.state == TRACK_STATE_STARTED) {
                    t.state = TRACK_STATE_STOPPED;
                    t.transition_unix = camp.scheduled_expiry_unix;
                }
                out.push((NoticeScope::Broadcast, campaign_notice("eve_public.campaign.api.ExpiredNotice", cid)));
                changed = true;
            }
        }
        if !versions.is_empty() {
            versions.sort_unstable();
            out.push((NoticeScope::Broadcast, current_version_notice(&versions)));
        }
        if changed {
            self.save_to_db();
        }
        out
    }

    /// Credits a gameplay event to every active objective the character is committed to whose
    /// contribution method is exactly `event.method` and whose parameter constraints all match the
    /// event context, then to the campaign's tracks, emitting the objective / track
    /// `ContributedNotice`s and any lifecycle notices the progress causes.
    pub fn record_gameplay_contribution(&self, char_id: u32, event: &CampaignEvent<'_>, units: u64, now: u64) -> PendingNotices {
        let mut out = self.evaluate_campaign_lifecycle(now);
        if units == 0 {
            return out;
        }
        let active_camps: Vec<u64> = self
            .campaigns
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|c| c.state == CAMPAIGN_STATE_ACTIVE)
            .map(|c| c.id)
            .collect();
        let mut credited_any = false;
        let mut per_campaign: Vec<(u64, u64)> = Vec::new();
        {
            let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<u64> = objs.keys().copied().collect();
            ids.sort_unstable();
            for oid in ids {
                let Some(obj) = objs.get_mut(&oid) else { continue };
                if !active_camps.contains(&obj.campaign_id)
                    || obj.state != OBJECTIVE_STATE_ACTIVE
                    || !obj.committed_participants.contains(&char_id)
                    || !event.matches(obj)
                {
                    continue;
                }
                let entry = obj.contributors.entry(char_id).or_insert((0, 0));
                let mut credit = units.min(i64::MAX as u64) as i64;
                if obj.max_progress_per_participant > 0 {
                    credit = credit.min(obj.max_progress_per_participant - entry.0).max(0);
                }
                if obj.target_progress > 0 {
                    credit = credit.min(obj.target_progress - obj.current_progress).max(0);
                }
                if credit == 0 {
                    continue;
                }
                let before = intervals(entry.0, obj.isk_reward_progress_interval);
                entry.0 += credit;
                entry.1 += intervals(entry.0, obj.isk_reward_progress_interval) - before;
                let participant_total = entry.0 as u64;
                obj.current_progress += credit;
                if obj.target_progress > 0 && obj.current_progress >= obj.target_progress {
                    obj.state = OBJECTIVE_STATE_COMPLETED;
                    obj.transition_unix = now;
                }
                out.push((
                    NoticeScope::Character(char_id),
                    contributed_notice(
                        "eve_public.campaign.objective.api.ContributedNotice",
                        objective_id_to_uuid_bytes(oid),
                        participant_total,
                    ),
                ));
                per_campaign.push((obj.campaign_id, credit as u64));
                credited_any = true;
            }
        }
        {
            let mut trks = self.campaign_tracks.write().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<u64> = trks.keys().copied().collect();
            ids.sort_unstable();
            for (cid, credit) in per_campaign {
                for tid in &ids {
                    let Some(t) = trks.get_mut(tid) else { continue };
                    if t.campaign_id != cid || t.state != TRACK_STATE_STARTED {
                        continue;
                    }
                    let mine = t.participants.entry(char_id).or_insert(0);
                    let mut add = credit;
                    if t.max_participant_contribution > 0 {
                        add = add.min(t.max_participant_contribution.saturating_sub(*mine));
                    }
                    if add == 0 {
                        continue;
                    }
                    *mine += add;
                    let participant_total = *mine;
                    t.current_progress = t.current_progress.saturating_add(add);
                    if t.target_progress_value > 0 && t.current_progress >= t.target_progress_value {
                        t.current_progress = t.target_progress_value;
                        t.state = TRACK_STATE_TARGET_REACHED;
                        t.transition_unix = now;
                    }
                    out.push((
                        NoticeScope::Character(char_id),
                        contributed_notice(
                            "eve_public.campaign.track.api.ContributedNotice",
                            track_id_to_uuid_bytes(*tid),
                            participant_total,
                        ),
                    ));
                }
            }
        }
        if credited_any {
            self.save_to_db();
            out.extend(self.evaluate_campaign_lifecycle(now));
        }
        out
    }

    /// Gameplay hook used by mining / industry / exploration / FW code paths.
    pub fn on_campaign_gameplay_event(&self, char_id: u32, event: CampaignEvent<'_>, units: u64, now: u64, gateway: &PublicGatewayRouter) {
        for (scope, payload) in self.record_gameplay_contribution(char_id, &event, units, now) {
            let _ = gateway.publish_notice(scope, payload, now);
        }
    }

    /// Settles every elapsed tick at the participant count active so far (pushing any lifecycle
    /// notices into `notices`) before the new commitment can affect later ticks.
    pub fn commit_to_objective(&self, char_id: u32, oid: u64, now: u64, notices: &mut PendingNotices) -> Result<(), (u32, String)> {
        notices.extend(self.evaluate_campaign_lifecycle(now));
        let active_count = self
            .campaign_objectives
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|o| o.id != oid && o.state == OBJECTIVE_STATE_ACTIVE && o.committed_participants.contains(&char_id))
            .count() as u32;
        let camps = self.campaigns.read().unwrap_or_else(|e| e.into_inner());
        let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
        let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".to_string()))?;
        let camp_active = camps.get(&obj.campaign_id).is_some_and(|c| c.state == CAMPAIGN_STATE_ACTIVE);
        if !camp_active || obj.state != OBJECTIVE_STATE_ACTIVE {
            return Err((STATUS_PRECONDITION_FAILED, "Campaign objective is not active".into()));
        }
        if obj.committed_participants.contains(&char_id) {
            return Ok(());
        }
        if active_count >= MAX_ACTIVE_OBJECTIVES_PER_PARTICIPANT {
            return Err((STATUS_PRECONDITION_FAILED, "Maximum active objectives per participant reached".into()));
        }
        obj.committed_participants.push(char_id);
        obj.resigned_participants.retain(|c| *c != char_id);
        drop(objs);
        drop(camps);
        self.save_to_db();
        Ok(())
    }

    /// Settles elapsed ticks (see [`Self::commit_to_objective`]) before removing the commitment.
    pub fn resign_from_objective(&self, char_id: u32, oid: u64, now: u64, notices: &mut PendingNotices) -> Result<(), (u32, String)> {
        notices.extend(self.evaluate_campaign_lifecycle(now));
        let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
        let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".to_string()))?;
        if !obj.committed_participants.contains(&char_id) {
            return Err((STATUS_PRECONDITION_FAILED, "Character is not committed to this objective".into()));
        }
        obj.committed_participants.retain(|c| *c != char_id);
        if !obj.resigned_participants.contains(&char_id) {
            obj.resigned_participants.push(char_id);
        }
        drop(objs);
        self.save_to_db();
        Ok(())
    }

    /// `CommitmentState` of `char_id` on `oid` (None = never committed).
    pub fn objective_commitment(obj: &CampaignObjectiveRecord, char_id: u32) -> Option<u32> {
        if obj.committed_participants.contains(&char_id) {
            Some(COMMITMENT_STATE_COMMITTED)
        } else if obj.resigned_participants.contains(&char_id) {
            Some(COMMITMENT_STATE_RESIGNED)
        } else {
            None
        }
    }

    /// `objective.ParticipantState` of `char_id` (None = no participation).
    pub fn objective_participant_state(obj: &CampaignObjectiveRecord, char_id: u32) -> Option<u32> {
        let progress = obj.contributors.get(&char_id).map(|e| e.0).unwrap_or(0);
        if obj.max_progress_per_participant > 0 && progress >= obj.max_progress_per_participant {
            return Some(PARTICIPANT_STATE_CONTRIBUTION_COMPLETED);
        }
        Self::objective_commitment(obj, char_id)
    }

    /// Unredeemed ISK intervals a participant holds on a track.
    pub fn track_unredeemed_intervals(t: &CampaignTrackRecord, char_id: u32) -> u64 {
        if t.isk_reward_progress_interval == 0 {
            return 0;
        }
        let earned = t.participants.get(&char_id).copied().unwrap_or(0) / t.isk_reward_progress_interval;
        earned.saturating_sub(t.redeemed_intervals.get(&char_id).copied().unwrap_or(0))
    }

    fn credit_campaign_reward(&self, char_id: u32, isk: i64, reason: String) -> Result<(), (u32, String)> {
        let Some(ref db) = self.db else {
            return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
        };
        let amount = eve_db::IskCents::from_isk(isk);
        let mut credited = false;
        db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&(char_id as i64)) {
                if let Some(new_bal) = ch.balance.checked_add(amount) {
                    ch.balance = new_bal;
                    credited = true;
                    snap.append_journal(char_id as i64, "CampaignReward", amount, new_bal, reason);
                }
            }
        });
        if credited {
            Ok(())
        } else {
            Err((STATUS_PRECONDITION_FAILED, "Unable to credit campaign reward".into()))
        }
    }

    /// `RedeemRewardsRequest`: pays every unredeemed ISK interval of the objective into the wallet.
    pub fn redeem_objective_rewards(&self, char_id: u32, oid: u64) -> Result<i64, (u32, String)> {
        let (units, per) = {
            let objs = self.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".to_string()))?;
            (obj.contributors.get(&char_id).map(|e| e.1).unwrap_or(0), obj.isk_reward_per_interval)
        };
        if units <= 0 || per <= 0 {
            return Err((STATUS_PRECONDITION_FAILED, "No unredeemed campaign objective rewards".into()));
        }
        let payout = units.saturating_mul(per);
        self.credit_campaign_reward(char_id, payout, format!("Campaign objective {oid} reward ({units} intervals)"))?;
        if let Some(e) = self
            .campaign_objectives
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&oid)
            .and_then(|o| o.contributors.get_mut(&char_id))
        {
            e.1 = 0;
        }
        self.save_to_db();
        Ok(payout)
    }

    /// `RedeemParticipantRewardsRequest`: pays unredeemed participant-contribution intervals.
    pub fn redeem_track_rewards(&self, char_id: u32, tid: u64) -> Result<i64, (u32, String)> {
        let (units, per) = {
            let trks = self.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());
            let t = trks.get(&tid).ok_or((STATUS_NOT_FOUND, "Campaign track not found".to_string()))?;
            (Self::track_unredeemed_intervals(t, char_id), t.isk_reward_per_interval)
        };
        if units == 0 || per <= 0 {
            return Err((STATUS_PRECONDITION_FAILED, "No unredeemed campaign track rewards".into()));
        }
        let payout = (units as i64).saturating_mul(per);
        self.credit_campaign_reward(char_id, payout, format!("Campaign track {tid} participant reward ({units} intervals)"))?;
        if let Some(t) = self.campaign_tracks.write().unwrap_or_else(|e| e.into_inner()).get_mut(&tid) {
            *t.redeemed_intervals.entry(char_id).or_insert(0) += units;
        }
        self.save_to_db();
        Ok(payout)
    }
}

/// Fails with `STATUS_BAD_REQUEST` when an identifier is missing.
pub fn require_id(id: Option<u64>, what: &str) -> Result<u64, (u32, String)> {
    id.ok_or((STATUS_BAD_REQUEST, format!("Missing {what} identifier")))
}
