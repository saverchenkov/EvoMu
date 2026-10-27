//! `quasar_progression_extra` — V24.01 Quasar Community Tracks, Tales, Goals, Opportunities, Campaigns, Titles & Seasonal Events
//! (`feat:quasar.communitytrack`, `feat:quasar.tale`, `feat:quasar.goal`, `feat:quasar.opportunity`,
//!  `feat:quasar.campaign`, `feat:quasar.seasonalevent`, `Q-4`).
//!
//! Implements live backend semantics for:
//! - Community Progression Tracks (`eve_public.communitytrack.api`, `eve.communitytrack.api`).
//! - Narrative Tales (`eve.tale.api`, `eve_public.tale.api`).
//! - Generic Organization and Personal Goals (`eve_public.goal.api`, `eve_public.goal.contribution.api`, `eve.goal.api`).
//! - Job Board Opportunities (`eve_public.opportunity.api`).
//! - Character Cosmetic Titles (`eve_public.cosmetic.character.title.api`).
//! - Seasonal Challenges and Live Campaigns (`eve_public.campaign.api`, `eve.seasonalevent.challenge`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};

use super::quasar_goal_tale_notices as notices;
use super::quasar_social_chat::ctx_queue_notice;
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
    /// Admin-started tales: tale_id -> (spawn_template_id, solar_system_id, started_at_unix).
    pub active_tales: RwLock<HashMap<u32, (u32, u32, u64)>>,
    pub generic_goals: RwLock<HashMap<String, GenericGoalRecord>>,
    pub campaigns: RwLock<HashMap<u64, CampaignRecord>>,
    pub campaign_objectives: RwLock<HashMap<u64, CampaignObjectiveRecord>>,
    pub campaign_tracks: RwLock<HashMap<u64, CampaignTrackRecord>>,
    pub corp_state: Option<Arc<crate::services::quasar_social_corp::QuasarCorpState>>,
    pub db: Option<Arc<WorldRepository>>,
    pub sde: Option<Arc<eve_sde::SdeRegistry>>,
}

pub use super::quasar_campaign_state::{CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord};

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
        let (tracks_loaded, goals_loaded, tales_loaded, camps_loaded, objs_loaded, camp_tracks_loaded) = db.as_ref().map(|d| {
            d.with_snapshot(|s| {
                let t = s.quasar_state_blobs
                    .get("quasar_community_tracks")
                    .and_then(|json| serde_json::from_str::<HashMap<u32, CommunityTrackState>>(json).ok());
                let g = s.quasar_state_blobs
                    .get("quasar_generic_goals")
                    .and_then(|json| serde_json::from_str::<HashMap<String, GenericGoalRecord>>(json).ok());
                let tales = s.quasar_state_blobs
                    .get("quasar_active_tales")
                    .and_then(|json| serde_json::from_str::<HashMap<u32, (u32, u32, u64)>>(json).ok());
                let camps = s.quasar_state_blobs
                    .get("quasar_campaigns")
                    .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignRecord>>(json).ok());
                let objs = s.quasar_state_blobs
                    .get("quasar_campaign_objectives")
                    .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignObjectiveRecord>>(json).ok());
                let trks = s.quasar_state_blobs
                    .get("quasar_campaign_tracks")
                    .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignTrackRecord>>(json).ok());
                (t, g, tales, camps, objs, trks)
            })
        }).unwrap_or((None, None, None, None, None, None));

        Self {
            community_tracks: RwLock::new(tracks_loaded.unwrap_or_default()),
            active_tales: RwLock::new(tales_loaded.unwrap_or_default()),
            generic_goals: RwLock::new(goals_loaded.unwrap_or_default()),
            campaigns: RwLock::new(camps_loaded.unwrap_or_default()),
            campaign_objectives: RwLock::new(objs_loaded.unwrap_or_default()),
            campaign_tracks: RwLock::new(camp_tracks_loaded.unwrap_or_default()),
            corp_state: None,
            db,
            sde: None,
        }
    }

    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
        corp_state: Option<Arc<crate::services::quasar_social_corp::QuasarCorpState>>,
    ) -> Self {
        let mut mgr = Self::new(db);
        mgr.corp_state = corp_state;
        mgr.sde = sde.clone();
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

    /// Owning corporation of a goal backed by the corporation project ledger (`CORPGOAL` UUIDs).
    /// Must be read before a delete removes the ledger entry.
    pub fn goal_corp_id(&self, uuid: &[u8; 16]) -> Option<u32> {
        if &uuid[0..8] != notices::CORP_GOAL_UUID_PREFIX {
            return None;
        }
        let goal_id = u64::from_be_bytes(uuid[8..16].try_into().ok()?);
        let cs = self.corp_state.as_ref()?;
        let goals = cs.goals.read().unwrap_or_else(|e| e.into_inner());
        goals.get(&goal_id).map(|g| g.corp_id)
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let tracks = self.community_tracks.read().unwrap_or_else(|e| e.into_inner());
            let goals = self.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let tales = self.active_tales.read().unwrap_or_else(|e| e.into_inner());
            let camps = self.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let objs = self.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let trks = self.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Ok(json) = serde_json::to_string(&*trks) {
                    snap.quasar_state_blobs.insert("quasar_campaign_tracks".into(), json);
                }
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

/// Reads `Identifier { sequential = 1 }` nested at `field_num` (tale, spawntemplate, solarsystem).
fn seq_id_of(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<u64> {
    let Some(DynamicValue::Message(id_msg)) = dyn_msg.get(field_num) else { return None };
    match id_msg.get(1)? {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        _ => None,
    }
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
            // StartTaleRequest { spawn_template = 1 (Identifier.sequential), solar_system = 2 }
            let template_id = seq_id_of(dyn_msg, 1)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing spawn_template identifier".to_string()))?;
            let system_id = seq_id_of(dyn_msg, 2)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing solar_system identifier".to_string()))?;
            // A tale is anchored to a real solar system from the static data.
            if mgr.sde.as_ref().is_some_and(|sde| !sde.solar_systems.contains_key(&system_id)) {
                return Err((STATUS_NOT_FOUND, format!("Unknown solar system {system_id}")));
            }
            let now = ctx.now_unix();
            let tale_id = {
                let mut tales = mgr.active_tales.write().unwrap_or_else(|e| e.into_inner());
                let id = tales.keys().copied().max().unwrap_or(0).saturating_add(1);
                tales.insert(id, (template_id, system_id, now));
                id
            };
            mgr.save_to_db();
            // The client's tale service invalidates its per-template tale cache on StartedNotice.
            ctx_queue_notice(
                notices::tale_scope(),
                notices::tale_lifecycle(true, tale_id as u64, template_id as u64, system_id as u64),
                now,
            );
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
            // StopTalesRequest { spawn_template = 1 }: stops every running tale of that template.
            let template_id = seq_id_of(dyn_msg, 1)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing spawn_template identifier".to_string()))?;
            let stopped: Vec<(u32, u32)> = {
                let mut tales = mgr.active_tales.write().unwrap_or_else(|e| e.into_inner());
                let ids: Vec<u32> = tales.iter().filter(|(_, t)| t.0 == template_id).map(|(id, _)| *id).collect();
                ids.into_iter().filter_map(|id| tales.remove(&id).map(|t| (id, t.1))).collect()
            };
            if !stopped.is_empty() {
                mgr.save_to_db();
            }
            let now = ctx.now_unix();
            for (tale_id, system_id) in stopped {
                ctx_queue_notice(
                    notices::tale_scope(),
                    notices::tale_lifecycle(false, tale_id as u64, template_id as u64, system_id as u64),
                    now,
                );
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.GetActiveTalesCountRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            // GetActiveTalesCountRequest { spawn_template = 1 }; an absent template counts all tales.
            let filter = seq_id_of(dyn_msg, 1).and_then(|v| u32::try_from(v).ok());
            let count = mgr
                .active_tales
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|t| filter.is_none_or(|f| t.0 == f))
                .count() as u32;
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

            let raw_initial_prog = dyn_msg
                .get(5)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0)
                .min(target);
            if raw_initial_prog > 0 && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((
                    STATUS_FORBIDDEN,
                    "Non-admin callers cannot seed initial goal progress on CreateRequest".into(),
                ));
            }
            let initial_prog = raw_initial_prog;

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
            // Members of the owning corporation see new projects appear via CreatedNotice.
            let goal_corp = mgr.goal_corp_id(&uuid);
            let created = notices::goal_created(
                &uuid,
                &notices::GoalAttributesView {
                    created_unix: ctx.now_unix(),
                    creator_id: cid,
                    corp_id: goal_corp,
                    name: &rec.name,
                    description: &rec.description,
                    desired: target,
                    current: initial_prog,
                },
            );
            mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).insert(uuid_to_hex(&uuid), rec);
            mgr.save_to_db();
            ctx_queue_notice(notices::goal_scope(goal_corp, cid), created, ctx.now_unix());

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
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goal_corp = mgr.goal_corp_id(&target_uuid);
            if let (Some(cid), Some(gc)) = (ctx.effective_character_id(), goal_corp) {
                if let Some(ref db) = mgr.db {
                    let caller_corp = db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32));
                    if let Some(c_corp) = caller_corp {
                        if gc != 0 && gc != c_corp && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                            return Err((STATUS_FORBIDDEN, "Not a member of the goal's corporation".into()));
                        }
                    }
                }
            }
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
        Arc::new(move |ctx, _any, _dyn| {
            let caller_cid = ctx.effective_character_id();
            let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
            let caller_corp = caller_cid.and_then(|cid| {
                mgr.db.as_ref().and_then(|db| {
                    db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32))
                })
            });
            let goals = mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for g in goals.values() {
                if !is_admin {
                    if let (Some(gc), Some(cc)) = (mgr.goal_corp_id(&g.uuid), caller_corp) {
                        if gc != 0 && gc != cc && caller_cid != Some(g.creator_id) {
                            continue;
                        }
                    }
                }
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
            let goal_corp = mgr.goal_corp_id(&target_uuid);
            let is_corp_director = goal_corp.is_some_and(|gc| {
                gc != 0
                    && mgr.db.as_ref().is_some_and(|db| {
                        db.with_snapshot(|s| {
                            let caller_corp = s.characters.get(&(caller_id as i64)).map(|c| c.corporation_id as u32);
                            caller_corp == Some(gc)
                                && s.corporations.get(&(gc as i64)).is_some_and(|corp| {
                                    corp.ceo_id == caller_id as i64
                                        || (corp.member_roles.get(&(caller_id as i64)).copied().unwrap_or(0)
                                            & eve_db::corp_roles::ROLE_DIRECTOR)
                                            != 0
                                })
                        })
                    })
            });
            let (creator_id, was_open) = {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get_mut(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                if g.creator_id != caller_id && !is_corp_director && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator or corporation director".into()));
                }
                let was_open = !g.closed;
                g.closed = true;
                (g.creator_id, was_open)
            };
            mgr.save_to_db();
            if let Some(ref cs) = mgr.corp_state {
                if &target_uuid[0..8] == b"CORPGOAL" {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap());
                    cs.close_or_delete_goal(goal_id, false);
                }
            }
            // Closing is a one-way transition; re-closing a closed goal is a no-op without a notice.
            if was_open {
                ctx_queue_notice(
                    notices::goal_scope(goal_corp, creator_id),
                    notices::goal_closed(&target_uuid, caller_id),
                    ctx.now_unix(),
                );
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
            let (capped, prev, desired, creator_id) = {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get_mut(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                let prev = g.current_progress;
                let c = new_prog.min(g.desired_progress);
                g.current_progress = c;
                (c, prev, g.desired_progress, g.creator_id)
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
            // Manual progress set -> ProgressedNotice only when the value actually changed;
            // crossing the target also completes the goal.
            let scope = notices::goal_scope(mgr.goal_corp_id(&target_uuid), creator_id);
            if capped != prev {
                ctx_queue_notice(scope.clone(), notices::goal_progressed(&target_uuid, capped), ctx.now_unix());
            }
            if capped >= desired && prev < desired {
                ctx_queue_notice(scope, notices::goal_completed(&target_uuid), ctx.now_unix());
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
            // Resolve the audience before the corporation ledger entry is removed.
            let goal_corp = mgr.goal_corp_id(&target_uuid);
            let is_corp_director = goal_corp.is_some_and(|gc| {
                gc != 0
                    && mgr.db.as_ref().is_some_and(|db| {
                        db.with_snapshot(|s| {
                            let caller_corp = s.characters.get(&(caller_id as i64)).map(|c| c.corporation_id as u32);
                            caller_corp == Some(gc)
                                && s.corporations.get(&(gc as i64)).is_some_and(|corp| {
                                    corp.ceo_id == caller_id as i64
                                        || (corp.member_roles.get(&(caller_id as i64)).copied().unwrap_or(0)
                                            & eve_db::corp_roles::ROLE_DIRECTOR)
                                            != 0
                                })
                        })
                    })
            });
            let creator_id = {
                let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                let Some(g) = goals.get(&hex) else {
                    return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                };
                if g.creator_id != caller_id && !is_corp_director && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator or corporation director".into()));
                }
                let creator_id = g.creator_id;
                goals.remove(&hex);
                creator_id
            };
            mgr.save_to_db();
            if let Some(ref cs) = mgr.corp_state {
                if &target_uuid[0..8] == b"CORPGOAL" {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap());
                    cs.close_or_delete_goal(goal_id, true);
                }
            }
            ctx_queue_notice(
                notices::goal_scope(goal_corp, creator_id),
                notices::goal_deleted(&target_uuid),
                ctx.now_unix(),
            );
            Ok(GoogleEmpty.to_any())
        }),
    );

    // eve.goal.api.SetNameRequest / SetDescriptionRequest: administrative rename of a goal
    // ({ goal = 1, name|description = 2, corporation = 3 }). The client only learns about the new
    // text through NameChangedNotice / DescriptionChangedNotice (admin_designated_* fields).
    for (req_name, rsp_name, is_name) in [
        ("eve.goal.api.SetNameRequest", "eve.goal.api.SetNameResponse", true),
        ("eve.goal.api.SetDescriptionRequest", "eve.goal.api.SetDescriptionResponse", false),
    ] {
        let mgr = Arc::clone(&manager);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                require_admin_role(ctx)?;
                let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                    return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
                };
                let text = match dyn_msg.get(2) {
                    Some(DynamicValue::String(s)) => s.clone(),
                    _ => String::new(),
                };
                if is_name && text.trim().is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Goal name must not be empty".into()));
                }
                let creator_id = {
                    let mut goals = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner());
                    let Some(g) = goals.get_mut(&uuid_to_hex(&target_uuid)) else {
                        return Err((STATUS_NOT_FOUND, "Goal not found".into()));
                    };
                    if is_name { g.name = text.clone() } else { g.description = text.clone() }
                    g.creator_id
                };
                mgr.save_to_db();
                if let (Some(cs), true) = (&mgr.corp_state, &target_uuid[0..8] == notices::CORP_GOAL_UUID_PREFIX) {
                    let goal_id = u64::from_be_bytes(target_uuid[8..16].try_into().unwrap_or([0; 8]));
                    if let Some(cg) = cs.goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&goal_id) {
                        if is_name { cg.name = text.clone() } else { cg.description = text.clone() }
                    }
                    cs.sync_to_db();
                }
                let notice = if is_name {
                    notices::goal_name_changed(&target_uuid, &text)
                } else {
                    notices::goal_description_changed(&target_uuid, &text)
                };
                ctx_queue_notice(notices::goal_scope(mgr.goal_corp_id(&target_uuid), creator_id), notice, ctx.now_unix());
                Ok(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"), value: Vec::new() })
            }),
        );
    }

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

    // 3b. eve_public.goal.contribution.api.{GetContributorSummariesForGoalRequest,GetMyContributorSummaryForGoalRequest,GetMyContributorSummariesRequest}
    let encode_goal_contrib_summary = |cid: u32, progress: u64, uuid: &[u8; 16]| -> Vec<u8> {
        let (mut ch_b, mut goal_b, mut s_b) = (Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, cid as u64, &mut ch_b);
        encode_bytes_field(1, &ch_b, &mut s_b);
        encode_varint_field(2, progress, &mut s_b);
        encode_bytes_field(1, uuid, &mut goal_b);
        encode_bytes_field(3, &goal_b, &mut s_b);
        s_b
    };
    let resolve_goal_contribs = |mgr: &ProgressionExtraManager, g: &GenericGoalRecord| -> Vec<(u32, u64)> {
        if &g.uuid[0..8] == notices::CORP_GOAL_UUID_PREFIX {
            if let Some(ref cs) = mgr.corp_state {
                let gid = u64::from_be_bytes(g.uuid[8..16].try_into().unwrap_or([0; 8]));
                if let Some(cg) = cs.goals.read().unwrap_or_else(|e| e.into_inner()).get(&gid) {
                    let mut list: Vec<(u32, u64)> = cg
                        .contributors
                        .iter()
                        .map(|(&c, &(p, _))| (c, p.max(0) as u64))
                        .collect();
                    list.sort_by_key(|&(c, _)| c);
                    if !list.is_empty() {
                        return list;
                    }
                }
            }
        }
        if g.current_progress > 0 {
            vec![(g.creator_id, g.current_progress)]
        } else {
            Vec::new()
        }
    };

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let g = mgr
                .generic_goals
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&uuid_to_hex(&target_uuid))
                .cloned()
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
            if let Some(gc) = mgr.goal_corp_id(&target_uuid) {
                let caller_corp = mgr.db.as_ref().and_then(|db| {
                    db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32))
                });
                if gc != 0 && caller_corp != Some(gc) && g.creator_id != cid && require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Cross-corporation goal summary access denied".into()));
                }
            }
            let mut out = Vec::new();
            for (c_id, prog) in resolve_goal_contribs(&mgr, &g) {
                let s_b = encode_goal_contrib_summary(c_id, prog, &g.uuid);
                encode_bytes_field(1, &s_b, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.contribution.api.GetContributorSummariesForGoalResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let g = mgr
                .generic_goals
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&uuid_to_hex(&target_uuid))
                .cloned()
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
            let prog = resolve_goal_contribs(&mgr, &g)
                .into_iter()
                .find(|&(c, _)| c == cid)
                .map(|(_, p)| p)
                .unwrap_or(0);
            let s_b = encode_goal_contrib_summary(cid, prog, &g.uuid);
            let mut out = Vec::new();
            encode_bytes_field(1, &s_b, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.contribution.api.GetMyContributorSummaryForGoalResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals: Vec<GenericGoalRecord> = mgr
                .generic_goals
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .cloned()
                .collect();
            let mut out = Vec::new();
            for g in goals {
                if let Some((_, prog)) = resolve_goal_contribs(&mgr, &g).into_iter().find(|&(c, p)| c == cid && p > 0) {
                    let s_b = encode_goal_contrib_summary(cid, prog, &g.uuid);
                    encode_bytes_field(1, &s_b, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.contribution.api.GetMyContributorSummariesResponse"),
                value: out,
            })
        }),
    );

    // ------------------------------------------------------------------------
    // 4. Character Titles Definition Handler: delegated to `quasar_cosmetics_skinr`
    // ------------------------------------------------------------------------

    // ------------------------------------------------------------------------
    // 5. Seasonal Challenges
    // ------------------------------------------------------------------------
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_track_id(dyn_msg).map(|id| id as i64);
            let effective_cid = ctx.effective_character_id().map(|id| id as i64).unwrap_or(0);
            let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
            if let Some(rc) = req_cid {
                if effective_cid != 0 && rc != effective_cid && !is_admin {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Cannot query seasonal challenges of another character".into(),
                    ));
                }
            }
            let char_id = req_cid.unwrap_or(effective_cid);
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

    // ------------------------------------------------------------------------
    // 6. Live Campaigns, Objectives, Tracks & Definitions
    // ------------------------------------------------------------------------
    super::quasar_campaign_definitions::register_campaign_definition_handlers(router, Arc::clone(&manager));
    super::quasar_campaign_handlers::register_campaign_handlers(router, Arc::clone(&manager));
}

fn format_opportunity_id_value(v: &DynamicValue) -> String {
    match v {
        DynamicValue::String(s) => s.clone(),
        DynamicValue::Bytes(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        DynamicValue::Uint64(u) => u.to_string(),
        DynamicValue::Int64(i) => i.to_string(),
        DynamicValue::Uint32(u) => u.to_string(),
        DynamicValue::Int32(i) => i.to_string(),
        DynamicValue::Bool(b) => b.to_string(),
        DynamicValue::Enum(e) => e.to_string(),
        DynamicValue::Message(m) => m
            .fields
            .iter()
            .map(|(k, fv)| format!("{k}={}", format_opportunity_id_value(fv)))
            .collect::<Vec<_>>()
            .join(","),
        DynamicValue::Repeated(items) => items
            .iter()
            .map(format_opportunity_id_value)
            .collect::<Vec<_>>()
            .join(","),
        other => format!("{other:?}"),
    }
}

/// Consumes client-published `eve_public.opportunity.api.{Viewed,Tracked,Untracked,Completed,Discovered}`
/// events from `publicGatewaySvc.PublishEvent` / `PublishEventPayload`, updates live achievement/goal
/// progression state, and queues progression notices to the character's subscribed session.
pub fn handle_opportunity_event(
    manager: &crate::services::quasar_progression::ProgressionManagers,
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    session: &eve_machonet::SessionState,
    any: &GoogleAny,
    now_unix: u64,
) {
    let event_name = eve_proto::quasar::QuasarDescriptorRegistry::normalize_type_name(&any.type_url);
    if !matches!(
        event_name,
        "eve_public.opportunity.api.Viewed"
            | "eve_public.opportunity.api.Tracked"
            | "eve_public.opportunity.api.Untracked"
            | "eve_public.opportunity.api.Completed"
            | "eve_public.opportunity.api.Discovered"
    ) {
        return;
    }
    let Some(cid) = session.character_id.filter(|&c| c > 0).map(|c| c as u32) else { return };
    let Ok(dyn_msg) = descriptors.decode_dynamic(&any.type_url, &any.value) else { return };
    let mut delta = 1u64;
    let opp_key = if event_name.ends_with(".Discovered") {
        let Some(DynamicValue::Message(disc_opp)) = dyn_msg.get(1) else { return };
        if let Some(amt) = disc_opp.get(2).and_then(|v| match v {
            DynamicValue::Uint64(u) if *u > 0 => Some(*u),
            DynamicValue::Int64(i) if *i > 0 => Some(*i as u64),
            _ => None,
        }) {
            delta = amt;
        }
        let tag = match disc_opp.get(1) {
            Some(DynamicValue::Message(m)) => match m.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.clone(),
                Some(DynamicValue::Uint64(v)) if *v > 0 => v.to_string(),
                Some(DynamicValue::Int64(v)) if *v > 0 => v.to_string(),
                _ => String::new(),
            },
            Some(DynamicValue::String(s)) => s.clone(),
            _ => String::new(),
        };
        if tag.is_empty() { return; }
        format!("discovered:{tag}")
    } else {
        if let Some(DynamicValue::Message(ch_msg)) = dyn_msg.get(1) {
            let req_cid = match ch_msg.get(1) {
                Some(DynamicValue::Uint64(v)) => Some(*v as u32),
                Some(DynamicValue::Int64(v)) if *v > 0 => Some(*v as u32),
                Some(DynamicValue::Uint32(v)) => Some(*v),
                Some(DynamicValue::Int32(v)) if *v > 0 => Some(*v as u32),
                _ => None,
            };
            if req_cid.filter(|&c| c > 0).is_some_and(|rc| rc != cid) { return; }
        }
        let Some(DynamicValue::Message(opp_msg)) = dyn_msg.get(2) else { return };
        let Some(variant_num) = (1u32..=8).find(|&f| opp_msg.get(f).is_some()) else { return };
        let Some(variant_val) = opp_msg.get(variant_num) else { return };
        let id_str = format_opportunity_id_value(variant_val);
        if id_str.is_empty() { return; }
        format!("v{variant_num}:{id_str}")
    };
    if let Some((ach, done, ms)) = manager.achievements.add_progress(cid, 102, delta, now_unix) {
        manager.achievements.save_to_db();
        let cat_score = manager.achievements.category_score(cid, ach.def.category_id);
        crate::services::quasar_achievements::emit_achievement_notices(cid, ach.def.id, ach.def.category_id, ach.progress, cat_score, done, &ms, now_unix);
    }
    if let Some(ref db) = manager.extra.db {
        db.with_mut_snapshot(|snap| {
            let mut state: serde_json::Value = snap.quasar_state_blobs.get("quasar_opportunities").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_else(|| serde_json::json!({}));
            if !state.is_object() { state = serde_json::json!({}); }
            if let Some(obj) = state.as_object_mut() {
                let ch_entry = obj.entry(cid.to_string()).or_insert_with(|| serde_json::json!({"tracked": [], "history": []}));
                if event_name.ends_with(".Tracked") {
                    if let Some(arr) = ch_entry.get_mut("tracked").and_then(|v| v.as_array_mut()) {
                        if !arr.iter().any(|x| x.as_str() == Some(&opp_key)) {
                            if arr.len() >= 256 { arr.remove(0); }
                            arr.push(serde_json::Value::String(opp_key.clone()));
                        }
                    }
                } else if event_name.ends_with(".Untracked") {
                    if let Some(arr) = ch_entry.get_mut("tracked").and_then(|v| v.as_array_mut()) {
                        arr.retain(|x| x.as_str() != Some(&opp_key));
                    }
                }
                if let Some(hist) = ch_entry.get_mut("history").and_then(|v| v.as_array_mut()) {
                    if hist.len() >= 256 { hist.remove(0); }
                    hist.push(serde_json::json!({"event": event_name, "key": opp_key}));
                }
            }
            snap.quasar_state_blobs.insert("quasar_opportunities".into(), state.to_string());
        });
    }
}
