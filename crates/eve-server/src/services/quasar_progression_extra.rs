//! `quasar_progression_extra` — V24.01 Quasar Community Tracks, Tales, Goals, Campaigns, Titles & Seasonal Events
//! (`feat:quasar.communitytrack`, `feat:quasar.tale`, `feat:quasar.goal`, `feat:quasar.campaign`,
//!  `feat:quasar.seasonalevent`, `Q-4`).
//!
//! Implements live backend semantics for:
//! - Community Progression Tracks (`eve_public.communitytrack.api`, `eve.communitytrack.api`).
//! - Narrative Tales (`eve.tale.api`, `eve_public.tale.api`).
//! - Generic Organization and Personal Goals (`eve_public.goal.api`, `eve.goal.api`).
//! - Character Cosmetic Titles (`eve_public.cosmetic.character.title.api`).
//! - Seasonal Challenges and Live Campaigns (`eve_public.campaign.api`, `eve.seasonalevent.challenge`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};

/// DERIVED: ISK reward per campaign contribution reward unit earned from FW/insurgency complex capture (10,000 ISK).
pub const CAMPAIGN_REWARD_ISK_PER_UNIT: i64 = 10_000;
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ============================================================================
// Community Track State
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommunityTrackState {
    pub track_id: u32,
    pub name: String,
    pub target_progress: u64,
    pub current_progress: u64,
    pub active: bool,
}

pub fn uuid_to_hex(u: &[u8; 16]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
pub struct ProgressionExtraManager {
    pub community_tracks: RwLock<HashMap<u32, CommunityTrackState>>,
    pub active_tales: RwLock<HashMap<u32, (u32, u64)>>, // tale_id -> (solar_system_id, started_at)
    pub generic_goals: RwLock<HashMap<String, GenericGoalRecord>>,
    pub campaigns: RwLock<HashMap<u64, CampaignRecord>>,
    pub campaign_objectives: RwLock<HashMap<u64, CampaignObjectiveRecord>>,
    pub corp_state: Option<Arc<crate::services::quasar_social_corp::QuasarCorpState>>,
    db: Option<Arc<WorldRepository>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CampaignRecord {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub objectives: Vec<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CampaignObjectiveRecord {
    pub id: u64,
    pub campaign_id: u64,
    pub name: String,
    pub target_progress: i64,
    pub current_progress: i64,
    pub committed_participants: Vec<u32>,
    pub contributors: HashMap<u32, (i64, i64)>, // char_id -> (progress, unredeemed_rewards)
    #[serde(default)]
    pub reward_pool_isk: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GenericGoalRecord {
    pub uuid: [u8; 16],
    pub creator_id: u32,
    pub name: String,
    pub description: String,
    pub desired_progress: u64,
    pub current_progress: u64,
    pub closed: bool,
}

impl ProgressionExtraManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        let (tracks_loaded, goals_loaded, tales_loaded, camps_loaded, objs_loaded) = db.as_ref().map(|d| {
            d.with_snapshot(|s| {
                let t = s.quasar_state_blobs
                    .get("quasar_community_tracks")
                    .and_then(|json| serde_json::from_str::<HashMap<u32, CommunityTrackState>>(json).ok());
                let g = s.quasar_state_blobs
                    .get("quasar_generic_goals")
                    .and_then(|json| serde_json::from_str::<HashMap<String, GenericGoalRecord>>(json).ok());
                let tales = s.quasar_state_blobs
                    .get("quasar_active_tales")
                    .and_then(|json| serde_json::from_str::<HashMap<u32, (u32, u64)>>(json).ok());
                let camps = s.quasar_state_blobs
                    .get("quasar_campaigns")
                    .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignRecord>>(json).ok());
                let objs = s.quasar_state_blobs
                    .get("quasar_campaign_objectives")
                    .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignObjectiveRecord>>(json).ok());
                (t, g, tales, camps, objs)
            })
        }).unwrap_or((None, None, None, None, None));

        Self {
            community_tracks: RwLock::new(tracks_loaded.unwrap_or_default()),
            active_tales: RwLock::new(tales_loaded.unwrap_or_default()),
            generic_goals: RwLock::new(goals_loaded.unwrap_or_default()),
            campaigns: RwLock::new(camps_loaded.unwrap_or_default()),
            campaign_objectives: RwLock::new(objs_loaded.unwrap_or_default()),
            corp_state: None,
            db,
        }
    }

    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
        corp_state: Option<Arc<crate::services::quasar_social_corp::QuasarCorpState>>,
    ) -> Self {
        let mut mgr = Self::new(db);
        mgr.corp_state = corp_state;
        if let Some(sde) = sde {
            if let Ok(records) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                for (id_str, val) in records {
                    if let Ok(id) = id_str.parse::<u32>() {
                        let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("Community Track").to_string();
                        let target = val.get("goal").and_then(|v| v.as_u64()).unwrap_or(1_000_000);
                        tracks.entry(id).or_insert(CommunityTrackState {
                            track_id: id,
                            name,
                            target_progress: target,
                            current_progress: 0,
                            active: true,
                        });
                    }
                }
            }
        }
        mgr
    }

    pub fn register_campaign(
        &self,
        campaign: CampaignRecord,
        objectives: Vec<CampaignObjectiveRecord>,
    ) {
        self.campaigns
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(campaign.id, campaign);
        let mut objs = self
            .campaign_objectives
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for obj in objectives {
            objs.insert(obj.id, obj);
        }
        drop(objs);
        self.save_to_db();
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let tracks = self.community_tracks.read().unwrap_or_else(|e| e.into_inner());
            let goals = self.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let tales = self.active_tales.read().unwrap_or_else(|e| e.into_inner());
            let camps = self.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let objs = self.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Ok(json) = serde_json::to_string(&*tracks) {
                    snap.quasar_state_blobs.insert("quasar_community_tracks".into(), json);
                }
                if let Ok(json) = serde_json::to_string(&*goals) {
                    snap.quasar_state_blobs.insert("quasar_generic_goals".into(), json);
                }
                if let Ok(json) = serde_json::to_string(&*tales) {
                    snap.quasar_state_blobs.insert("quasar_active_tales".into(), json);
                }
                if let Ok(json) = serde_json::to_string(&*camps) {
                    snap.quasar_state_blobs.insert("quasar_campaigns".into(), json);
                }
                if let Ok(json) = serde_json::to_string(&*objs) {
                    snap.quasar_state_blobs.insert("quasar_campaign_objectives".into(), json);
                }
                Ok::<(), String>(())
            });
        }
    }

    pub fn get_community_track(&self, track_id: u32) -> Option<CommunityTrackState> {
        self.community_tracks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&track_id)
            .cloned()
    }

    pub fn record_campaign_contribution(
        &self,
        objective_id: u64,
        char_id: u32,
        delta_progress: i64,
        unredeemed_rewards_delta: i64,
    ) -> Result<(), (u32, String)> {
        if delta_progress <= 0 {
            return Err((STATUS_BAD_REQUEST, "Contribution progress delta must be positive".into()));
        }
        {
            let mut objs = self.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
            let obj = objs
                .get_mut(&objective_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            if !obj.committed_participants.contains(&char_id) {
                obj.committed_participants.push(char_id);
            }
            obj.current_progress = obj.current_progress.saturating_add(delta_progress);
            let reward_units = unredeemed_rewards_delta.max(0);
            let entry = obj.contributors.entry(char_id).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(delta_progress);
            entry.1 = entry.1.saturating_add(reward_units);
            // DERIVED: Each campaign reward unit earned from FW/insurgency complex capture funds the objective reward pool by CAMPAIGN_REWARD_ISK_PER_UNIT.
            obj.reward_pool_isk = obj
                .reward_pool_isk
                .saturating_add(reward_units.saturating_mul(CAMPAIGN_REWARD_ISK_PER_UNIT));
        }
        self.save_to_db();
        Ok(())
    }

    pub fn on_participant_activity(&self, char_id: u32, delta_progress: i64, unredeemed_rewards_delta: i64) {
        if delta_progress <= 0 {
            return;
        }
        let committed_ids: Vec<u64> = self
            .campaign_objectives
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|o| o.committed_participants.contains(&char_id))
            .map(|o| o.id)
            .collect();
        for oid in committed_ids {
            let _ = self.record_campaign_contribution(oid, char_id, delta_progress, unredeemed_rewards_delta);
        }
    }

    pub fn add_community_progress(&self, track_id: u32, delta: u64) -> Option<CommunityTrackState> {
        let mut map = self
            .community_tracks
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(track) = map.get_mut(&track_id) {
            track.current_progress = track.current_progress.saturating_add(delta);
            return Some(track.clone());
        }
        None
    }
}

pub fn extract_uuid(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<[u8; 16]> {
    if let Some(DynamicValue::Message(id_msg)) = dyn_msg.get(field_num) {
        if let Some(DynamicValue::Bytes(b)) = id_msg.get(1) {
            if b.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(b);
                return Some(arr);
            }
        }
    }
    None
}

// ============================================================================
// Registration
// ============================================================================

pub fn register_progression_extra_handlers(
    router: &PublicGatewayRouter,
    _db: Arc<WorldRepository>,
    manager: Arc<ProgressionExtraManager>,
) {
    // ------------------------------------------------------------------------
    // 1. Community Track Handlers
    // ------------------------------------------------------------------------
    let extract_track_id = |dyn_msg: &DynamicQuasarMessage| -> Option<u32> {
        dyn_msg
            .get(1)
            .and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u32),
                DynamicValue::Uint64(u) => Some(*u as u32),
                DynamicValue::Int64(i) => Some(*i as u32),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Uint64(u) => Some(*u as u64 as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    _ => None,
                }),
                _ => None,
            })
            .filter(|&id| id != 0)
    };

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.communitytrack.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            let track = mgr
                .get_community_track(tid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            let mut def_id = Vec::new();
            encode_varint_field(1, track.track_id as u64, &mut def_id);
            encode_bytes_field(1, &def_id, &mut attr_bytes);
            encode_varint_field(2, track.current_progress, &mut attr_bytes);
            encode_varint_field(3, track.target_progress, &mut attr_bytes);
            encode_string_field(4, &track.name, &mut attr_bytes);
            encode_varint_field(5, if track.active { 1 } else { 0 }, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.communitytrack.api.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            let track = mgr
                .get_community_track(tid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_varint_field(1, track.track_id as u64, &mut attr_bytes);
            encode_string_field(2, &track.name, &mut attr_bytes);
            encode_varint_field(3, track.target_progress, &mut attr_bytes);
            encode_varint_field(4, track.current_progress, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.communitytrack.api.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.AddProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            let delta = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Int32(i) if *i > 0 => Some(*i as u64),
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i > 0 => Some(*i as u64),
                    _ => None,
                })
                .filter(|&d| d > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or non-positive community track progress delta".into()))?;
            let _ = mgr
                .add_community_progress(tid, delta)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
            mgr.save_to_db();

            let mut not_bytes = Vec::new();
            encode_varint_field(1, tid as u64, &mut not_bytes);
            let notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.communitytrack.api.ProgressedNotice"), value: not_bytes.clone() };
            super::quasar_social_chat::ctx_queue_notice(crate::quasar_gateway::NoticeScope::Broadcast, notice.clone(), ctx.now_unix());
            super::quasar_social_chat::ctx_queue_notice(crate::quasar_gateway::NoticeScope::Character(cid), notice, ctx.now_unix());

            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.StartRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            {
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                let entry = tracks
                    .get_mut(&tid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
                entry.active = true;
            }
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.StopRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            let mut track = mgr
                .get_community_track(tid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
            track.active = false;
            mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner()).insert(tid, track);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.admin.api.SetProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let tid = extract_track_id(dyn_msg)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing community track Identifier".into()))?;
            let val = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Int32(i) if *i >= 0 => Some(*i as u64),
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            let mut track = mgr
                .get_community_track(tid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
            track.current_progress = val.min(track.target_progress);
            mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner()).insert(tid, track);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // ------------------------------------------------------------------------
    // 2. Tales Handlers
    // ------------------------------------------------------------------------
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.StartTaleRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(_cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let tale_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            let system_id = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(30_000_142);
            mgr.active_tales
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(tale_id, (system_id, ctx.now_unix()));
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.StopTalesRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(_cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let tale_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            mgr.active_tales.write().unwrap_or_else(|e| e.into_inner()).remove(&tale_id);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.GetActiveTalesCountRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let count = mgr.active_tales.read().unwrap_or_else(|e| e.into_inner()).len() as u32;
            let mut out = Vec::new();
            encode_varint_field(1, count as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tale.api.GetActiveTalesCountResponse"),
                value: out,
            })
        }),
    );

    // ------------------------------------------------------------------------
    // 3. Generic Goal Handlers
    // ------------------------------------------------------------------------
    fn parse_isk_value(val: &DynamicValue) -> Result<i64, (u32, String)> {
        match val {
            DynamicValue::Sint64(v) | DynamicValue::Int64(v) => {
                if *v < 0 {
                    Err((STATUS_BAD_REQUEST, "Negative goal reward amount".into()))
                } else {
                    Ok(*v)
                }
            }
            DynamicValue::Uint64(u) => i64::try_from(*u).map_err(|_| (STATUS_BAD_REQUEST, "Goal reward overflow".into())),
            DynamicValue::Sint32(v) | DynamicValue::Int32(v) => {
                if *v < 0 {
                    Err((STATUS_BAD_REQUEST, "Negative goal reward amount".into()))
                } else {
                    Ok(*v as i64)
                }
            }
            DynamicValue::Uint32(u) => Ok(*u as i64),
            DynamicValue::Message(m) => {
                if let Some(sub2) = m.get(2) {
                    if let Ok(v) = parse_isk_value(sub2) {
                        if v > 0 {
                            return Ok(v);
                        }
                    }
                }
                if let Some(sub1) = m.get(1) {
                    return parse_isk_value(sub1);
                }
                Ok(0)
            }
            DynamicValue::Repeated(list) => {
                let mut sum = 0i64;
                for item in list {
                    let v = parse_isk_value(item)?;
                    sum = sum.checked_add(v).ok_or_else(|| (STATUS_BAD_REQUEST, "Goal reward overflow".into()))?;
                }
                Ok(sum)
            }
            _ => Ok(0),
        }
    }

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let corpid = ctx.effective_corporation_id().or_else(|| {
                mgr.db.as_ref().and_then(|d| {
                    d.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32))
                })
            });
            let corp_role = ctx
                .identity
                .as_ref()
                .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
                .unwrap_or(0);
            let is_ceo = corpid.zip(mgr.db.as_ref()).is_some_and(|(c_id, d)| {
                d.with_snapshot(|s| s.corporations.get(&(c_id as i64)).is_some_and(|c| c.ceo_id == cid as i64))
            });
            let has_corp_role = (corp_role
                & (eve_db::corp_roles::ROLE_DIRECTOR
                    | eve_db::corp_roles::ROLE_PROJECT_MANAGER
                    | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER))
                != 0;
            if !is_ceo && !has_corp_role && require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Insufficient corporation role to create corporation goal".into()));
            }

            let name = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::String(s) if !s.trim().is_empty() => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "Organization Goal".to_string());

            let desc = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            let target = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) if *u > 0 => Some(*u),
                    DynamicValue::Int64(i) if *i > 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(100);

            let initial_prog = dyn_msg
                .get(5)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0)
                .min(target);

            let mut escrow_isk = 0i64;
            if let Some(pools) = dyn_msg.get(8) {
                escrow_isk = escrow_isk.saturating_add(parse_isk_value(pools)?);
            }
            if escrow_isk == 0 {
                if let Some(rewards) = dyn_msg.get(7) {
                    escrow_isk = escrow_isk.saturating_add(parse_isk_value(rewards)?.saturating_mul(target as i64));
                }
            }

            let goal_id = if let (Some(ref cs), Some(c_id)) = (&mgr.corp_state, corpid) {
                let gid = cs.create_goal(c_id, name.clone(), desc.clone(), target as i64, escrow_isk)?;
                if initial_prog > 0 {
                    if let Some(cg) = cs.goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&gid) {
                        cg.current_progress = initial_prog as i64;
                    }
                    cs.sync_to_db();
                }
                gid
            } else {
                mgr.corp_state.as_ref().map(|cs| cs.alloc_goal_id()).unwrap_or_else(|| ctx.now_unix())
            };

            let mut uuid = [0u8; 16];
            uuid[0..8].copy_from_slice(b"CORPGOAL");
            uuid[8..16].copy_from_slice(&goal_id.to_be_bytes());

            let rec = GenericGoalRecord {
                uuid,
                creator_id: cid,
                name,
                description: desc,
                desired_progress: target,
                current_progress: initial_prog,
                closed: false,
            };
            mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).insert(uuid_to_hex(&uuid), rec);
            mgr.save_to_db();

            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CreateResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let g = goals
                .get(&uuid_to_hex(&target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_string_field(2, &g.name, &mut attr_bytes);
            encode_string_field(3, &g.description, &mut attr_bytes);

            // 5: progress
            let mut prog_bytes = Vec::new();
            encode_varint_field(1, g.desired_progress, &mut prog_bytes);
            encode_varint_field(2, g.current_progress, &mut prog_bytes);
            encode_bytes_field(5, &prog_bytes, &mut attr_bytes);

            encode_varint_field(6, if g.closed { 2 } else { 1 }, &mut attr_bytes); // state
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let goals = mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for g in goals.values() {
                let mut goal_bytes = Vec::new();
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &g.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut goal_bytes);

                let mut attr_bytes = Vec::new();
                encode_string_field(2, &g.name, &mut attr_bytes);
                encode_string_field(3, &g.description, &mut attr_bytes);
                encode_bytes_field(2, &attr_bytes, &mut goal_bytes);

                encode_bytes_field(3, &goal_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.CloseRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(caller_id) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Authentication required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let hex = uuid_to_hex(&target_uuid);
            {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get_mut(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                if g.creator_id != caller_id && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator".into()));
                }
                g.closed = true;
            }
            mgr.save_to_db();
            if let Some(ref cs) = mgr.corp_state {
                if &target_uuid[0..8] == b"CORPGOAL" {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap());
                    cs.close_or_delete_goal(goal_id, false);
                }
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.SetCurrentProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let new_prog = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            let hex = uuid_to_hex(&target_uuid);
            let capped = {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get_mut(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                let c = new_prog.min(g.desired_progress);
                g.current_progress = c;
                c
            };
            mgr.save_to_db();
            if let Some(ref cs) = mgr.corp_state {
                if &target_uuid[0..8] == b"CORPGOAL" {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap());
                    if let Some(cg) = cs.goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&goal_id) {
                        cg.current_progress = (capped as i64).clamp(0, cg.target_progress);
                    }
                    cs.sync_to_db();
                }
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(caller_id) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Authentication required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let hex = uuid_to_hex(&target_uuid);
            {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                if g.creator_id != caller_id && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator".into()));
                }
                goals.remove(&hex);
            }
            mgr.save_to_db();
            if let Some(ref cs) = mgr.corp_state {
                if &target_uuid[0..8] == b"CORPGOAL" {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap());
                    cs.close_or_delete_goal(goal_id, true);
                }
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.GetCapacityRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let count = mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|g| g.creator_id == cid && !g.closed)
                .count() as u64;
            let mut out = Vec::new();
            encode_varint_field(1, count, &mut out);  // count
            encode_varint_field(2, 50, &mut out); // capacity = 50
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetCapacityResponse"),
                value: out,
            })
        }),
    );

    // ------------------------------------------------------------------------
    // 4. Character Titles Definition Handler
    // ------------------------------------------------------------------------
    router.register_handler(
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-character-titles", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.definition.api.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // ------------------------------------------------------------------------
    // 5. Campaigns & Seasonal Events
    // ------------------------------------------------------------------------
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let camps = mgr.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for c in camps.values().filter(|c| c.active) {
                let mut id_bytes = Vec::new();
                encode_varint_field(1, c.id, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign ID".into()))?;
            let camps = mgr.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let camp = camps.get(&cid).ok_or((STATUS_NOT_FOUND, "Campaign not found".into()))?;
            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_string_field(1, &camp.name, &mut attr_bytes);
            encode_string_field(2, &camp.description, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = extract_track_id(dyn_msg)
                .or_else(|| ctx.effective_character_id())
                .map(|id| id as i64)
                .unwrap_or(0);
            if char_id <= 0 {
                return Err((STATUS_BAD_REQUEST, "Missing character ID".into()));
            }
            let challenges = if let Some(ref db) = mgr.db {
                db.with_snapshot(|snap| {
                    crate::services::season_manager::get_character_challenges_for_quasar(snap, char_id)
                })
            } else {
                Vec::new()
            };
            let mut out = Vec::new();
            for ch in challenges {
                let mut chal = Vec::new();
                encode_varint_field(1, ch.challenge_id as u64, &mut chal); // challenge_id
                encode_varint_field(2, ch.max_progress as u64, &mut chal); // target
                encode_varint_field(3, ch.progress as u64, &mut chal);     // current
                encode_bytes_field(1, &chal, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.seasonalevent.challenge.GetChallengesForCharacterResponse"
                ),
                value: out,
            })
        }),
    );

fn extract_campaign_id(val: Option<&DynamicValue>) -> Option<u64> {
    fn from_bytes(b: &[u8]) -> Option<u64> {
        if b.is_empty() || b.iter().all(|&x| x == 0) {
            return None;
        }
        if b.len() == 16 {
            let mut hi = [0u8; 8];
            let mut lo = [0u8; 8];
            hi.copy_from_slice(&b[0..8]);
            lo.copy_from_slice(&b[8..16]);
            if hi == [0u8; 8] {
                return Some(u64::from_be_bytes(lo)).filter(|&v| v != 0);
            }
            if lo == [0u8; 8] {
                return Some(u64::from_le_bytes(hi)).filter(|&v| v != 0);
            }
            return Some(u64::from_be_bytes(lo)).filter(|&v| v != 0);
        }
        if b.len() >= 8 {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&b[0..8]);
            let id = u64::from_be_bytes(arr);
            if id != 0 {
                return Some(id);
            }
        }
        if b.len() >= 4 {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(&b[0..4]);
            let id = u32::from_be_bytes(arr) as u64;
            if id != 0 {
                return Some(id);
            }
        }
        b.first().copied().map(u64::from).filter(|&v| v != 0)
    }

    match val {
        Some(DynamicValue::Uint64(u)) if *u != 0 => Some(*u),
        Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) if *u != 0 => Some(*u as u64),
        Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u64),
        Some(DynamicValue::Bytes(b)) => from_bytes(b),
        Some(DynamicValue::Message(m)) => match m.get(1) {
            Some(DynamicValue::Uint64(u)) if *u != 0 => Some(*u),
            Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) if *u != 0 => Some(*u as u64),
            Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u64),
            Some(DynamicValue::Bytes(b)) => from_bytes(b),
            Some(DynamicValue::Message(inner)) => {
                extract_campaign_id(Some(&DynamicValue::Message(inner.clone())))
            }
            _ => None,
        },
        _ => None,
    }
}

    router.register_handler(
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.contributionmethod.definition.api.GetAllResponse"),
                value: Vec::new(),
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            let mut attr = Vec::new();
            encode_string_field(1, &obj.name, &mut attr);
            encode_varint_field(2, obj.target_progress as u64, &mut attr);
            encode_varint_field(3, obj.current_progress as u64, &mut attr);
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatusRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            encode_varint_field(1, obj.current_progress as u64, &mut out);
            encode_varint_field(2, obj.target_progress as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetStatusResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatusRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let committed = obj.committed_participants.contains(&cid);
            let (prog, unredeemed) = obj.contributors.get(&cid).copied().unwrap_or((0, 0));
            let mut out = Vec::new();
            encode_varint_field(1, if committed { 1 } else { 0 }, &mut out);
            encode_varint_field(2, prog as u64, &mut out);
            encode_varint_field(3, unredeemed as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetParticipantStatusResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.CommitRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                if !obj.committed_participants.contains(&cid) {
                    obj.committed_participants.push(cid);
                }
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.CommitResponse"),
                value: Vec::new(),
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.ResignRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                obj.committed_participants.retain(|c| *c != cid);
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.ResignResponse"),
                value: Vec::new(),
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.RedeemRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let (redeemable_units, payout) = {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
                let pool = obj.reward_pool_isk;
                let unredeemed = obj.contributors.get(&cid).map(|e| e.1).unwrap_or(0);
                let redeemable_units = unredeemed.min(pool / CAMPAIGN_REWARD_ISK_PER_UNIT);
                if unredeemed <= 0 || redeemable_units <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "Campaign reward pool is empty or no rewards available".into()));
                }
                let payout = redeemable_units.saturating_mul(CAMPAIGN_REWARD_ISK_PER_UNIT);
                if let Some(entry) = obj.contributors.get_mut(&cid) {
                    entry.1 = entry.1.saturating_sub(redeemable_units);
                }
                obj.reward_pool_isk = obj.reward_pool_isk.saturating_sub(payout);
                (redeemable_units, payout)
            };
            let Some(ref db) = mgr.db else {
                return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
            };
            let reward_isk = eve_db::IskCents::from_isk(payout);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_isk) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "CampaignReward",
                            reward_isk,
                            new_bal,
                            format!("Campaign objective {oid} reward"),
                        );
                    }
                }
            });
            if !credited {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                if let Some(obj) = objs.get_mut(&oid) {
                    if let Some(entry) = obj.contributors.get_mut(&cid) {
                        entry.1 = entry.1.saturating_add(redeemable_units);
                    }
                    obj.reward_pool_isk = obj.reward_pool_isk.saturating_add(payout);
                }
                return Err((STATUS_PRECONDITION_FAILED, "Unable to credit campaign reward".into()));
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.RedeemRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetStatsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let mut out = Vec::new();
            encode_varint_field(1, obj.current_progress as u64, &mut out);
            encode_varint_field(2, obj.committed_participants.len() as u64, &mut out);
            encode_varint_field(3, obj.contributors.len() as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetStatsResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.objective.api.GetParticipantStatsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign objective ID".into()))?;
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let obj = objs.get(&oid).ok_or((STATUS_NOT_FOUND, "Campaign objective not found".into()))?;
            let prog = obj.contributors.get(&cid).map(|c| c.0).unwrap_or(0);
            let rank = if prog <= 0 {
                0
            } else {
                1 + obj.contributors.values().filter(|(score, _)| *score > prog).count() as u64
            };
            let mut out = Vec::new();
            encode_varint_field(1, prog as u64, &mut out);
            encode_varint_field(2, rank, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.GetParticipantStatsResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let objs = mgr.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let committed_count: u64 = objs.values().map(|o| o.committed_participants.len() as u64).sum();
            let total_progress: u64 = objs.values().flat_map(|o| o.contributors.values()).map(|(p, _)| (*p).max(0) as u64).sum();
            let mut out = Vec::new();
            if committed_count > 0 || total_progress > 0 {
                encode_varint_field(1, committed_count, &mut out);
                encode_varint_field(2, total_progress, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.campaign.track.api.RedeemParticipantRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let oid = extract_campaign_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing campaign track/objective ID".into()))?;
            let (redeemable_units, payout) = {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                let obj = objs.get_mut(&oid).ok_or((STATUS_NOT_FOUND, "Campaign track/objective not found".into()))?;
                let pool = obj.reward_pool_isk;
                let unredeemed = obj.contributors.get(&cid).map(|e| e.1).unwrap_or(0);
                let redeemable_units = unredeemed.min(pool / CAMPAIGN_REWARD_ISK_PER_UNIT);
                if unredeemed <= 0 || redeemable_units <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "Campaign reward pool is empty or no rewards available".into()));
                }
                let payout = redeemable_units.saturating_mul(CAMPAIGN_REWARD_ISK_PER_UNIT);
                if let Some(entry) = obj.contributors.get_mut(&cid) {
                    entry.1 = entry.1.saturating_sub(redeemable_units);
                }
                obj.reward_pool_isk = obj.reward_pool_isk.saturating_sub(payout);
                (redeemable_units, payout)
            };
            let Some(ref db) = mgr.db else {
                return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
            };
            let reward_isk = eve_db::IskCents::from_isk(payout);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_isk) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "CampaignReward",
                            reward_isk,
                            new_bal,
                            format!("Campaign track {oid} participant reward"),
                        );
                    }
                }
            });
            if !credited {
                let mut objs = mgr.campaign_objectives.write().unwrap_or_else(|e| e.into_inner());
                if let Some(obj) = objs.get_mut(&oid) {
                    if let Some(entry) = obj.contributors.get_mut(&cid) {
                        entry.1 = entry.1.saturating_add(redeemable_units);
                    }
                    obj.reward_pool_isk = obj.reward_pool_isk.saturating_add(payout);
                }
                return Err((STATUS_PRECONDITION_FAILED, "Unable to credit campaign reward".into()));
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.RedeemParticipantRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    router.register_handler(
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"),
                value: Vec::new(),
            })
        }),
    );
}
