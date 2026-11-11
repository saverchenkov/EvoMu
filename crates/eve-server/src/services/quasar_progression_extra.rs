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
pub struct CommunityTrackTemplateInfo {
    pub template_id: u32,
    pub name: String,
    pub target_progress: u64,
    #[serde(default)]
    pub tale_templates: Vec<u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommunityTrackState {
    pub track_id: u32,
    #[serde(default)]
    pub template_id: u32,
    pub name: String,
    pub target_progress: u64,
    pub current_progress: u64,
    pub active: bool,
    #[serde(default)]
    pub tale_templates: Vec<u32>,
}

pub fn uuid_to_hex(u: &[u8; 16]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

fn resolve_track_key(tracks: &HashMap<u32, CommunityTrackState>, id: u32) -> Option<u32> {
    tracks.contains_key(&id).then_some(id).or_else(|| tracks.iter().find(|(_, e)| e.template_id == id).map(|(&k, _)| k))
}

#[derive(Default)]
pub struct ProgressionExtraManager {
    pub community_track_templates: RwLock<HashMap<u32, CommunityTrackTemplateInfo>>,
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
    pub sde_event_tale_templates: std::collections::HashSet<u32>,
}

pub use super::quasar_campaign_state::{CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GenericGoalRecord {
    pub uuid: [u8; 16],
    pub creator_id: u32,
    pub name: String,
    pub description: String,
    pub desired_progress: u64,
    pub current_progress: u64,
    pub closed: bool,
    #[serde(default)]
    pub matcher_type: String,
    #[serde(default)]
    pub filter_id: Option<i64>,
    #[serde(default)]
    pub created_unix: u64,
    #[serde(default)]
    pub contribution_config_pb: Vec<u8>,
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
            community_track_templates: RwLock::new(HashMap::new()),
            community_tracks: RwLock::new(tracks_loaded.unwrap_or_default()),
            active_tales: RwLock::new(tales_loaded.unwrap_or_default()),
            generic_goals: RwLock::new(goals_loaded.unwrap_or_default()),
            campaigns: RwLock::new(camps_loaded.unwrap_or_default()),
            campaign_objectives: RwLock::new(objs_loaded.unwrap_or_default()),
            campaign_tracks: RwLock::new(camp_tracks_loaded.unwrap_or_default()),
            corp_state: None,
            db,
            sde: None,
            sde_event_tale_templates: std::collections::HashSet::new(),
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
                let mut templates = mgr.community_track_templates.write().unwrap_or_else(|e| e.into_inner());
                for (id_str, val) in records {
                    if let Ok(id) = id_str.parse::<u32>() {
                        let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("Community Track").to_string();
                        // DERIVED: Server-configured default goal (100) when a template in community_track_templates.fsdbinary omits an explicit non-zero per-instance goal.
                        let target = val.get("goal").and_then(|v| v.as_u64()).filter(|&g| g > 0).unwrap_or(100);
                        let tale_templates: Vec<u32> = val
                            .get("tale_templates")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|x| x.as_u64().and_then(|n| u32::try_from(n).ok())).collect())
                            .unwrap_or_default();
                        mgr.sde_event_tale_templates.extend(tale_templates.iter().copied());
                        templates.insert(id, CommunityTrackTemplateInfo { template_id: id, name, target_progress: target, tale_templates });
                    }
                }
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                let had_persisted = !tracks.is_empty();
                for t in tracks.values_mut() {
                    if t.template_id == 0 {
                        t.template_id = t.track_id;
                    }
                    if t.tale_templates.is_empty() {
                        if let Some(tpl) = templates.get(&t.template_id) {
                            t.tale_templates = tpl.tale_templates.clone();
                        }
                    }
                }
                drop(tracks);
                drop(templates);
                if had_persisted {
                    mgr.save_to_db();
                }
            }
        }
        mgr
    }

    /// Owning corporation of a goal backed by the corporation project ledger (`CORPGOAL` UUIDs).
    /// Must be read before a delete removes the ledger entry.
    pub fn goal_corp_id(&self, uuid: &[u8; 16]) -> Option<u32> {
        let pid = crate::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id(uuid)?;
        let db = self.db.as_ref().or_else(|| self.corp_state.as_ref()?.db.as_ref())?;
        db.with_snapshot(|s| {
            s.corp_projects
                .get(&pid)
                .filter(|_| !s.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.1))
                .and_then(|p| u32::try_from(p.corporation_id).ok())
                .filter(|&c| c > 0)
        })
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            if db.is_write_locked_by_current_thread() {
                return;
            }
            let tracks = self.community_tracks.read().unwrap_or_else(|e| e.into_inner());
            let goals = self.generic_goals.read().unwrap_or_else(|e| e.into_inner());
            let tales = self.active_tales.read().unwrap_or_else(|e| e.into_inner());
            let camps = self.campaigns.read().unwrap_or_else(|e| e.into_inner());
            let objs = self.campaign_objectives.read().unwrap_or_else(|e| e.into_inner());
            let trks = self.campaign_tracks.read().unwrap_or_else(|e| e.into_inner());
            let trks_j = serde_json::to_string(&*trks).ok();
            let tracks_j = serde_json::to_string(&*tracks).ok();
            let goals_j = serde_json::to_string(&*goals).ok();
            let tales_j = serde_json::to_string(&*tales).ok();
            let camps_j = serde_json::to_string(&*camps).ok();
            let objs_j = serde_json::to_string(&*objs).ok();
            let already = db.with_snapshot(|snap| {
                trks_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_campaign_tracks") == Some(j))
                    && tracks_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_community_tracks") == Some(j))
                    && goals_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_generic_goals") == Some(j))
                    && tales_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_active_tales") == Some(j))
                    && camps_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_campaigns") == Some(j))
                    && objs_j.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_campaign_objectives") == Some(j))
            });
            if already {
                return;
            }
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Some(json) = trks_j {
                    snap.quasar_state_blobs.insert("quasar_campaign_tracks".into(), json);
                }
                if let Some(json) = tracks_j {
                    snap.quasar_state_blobs.insert("quasar_community_tracks".into(), json);
                }
                if let Some(json) = goals_j {
                    snap.quasar_state_blobs.insert("quasar_generic_goals".into(), json);
                }
                if let Some(json) = tales_j {
                    snap.quasar_state_blobs.insert("quasar_active_tales".into(), json);
                }
                if let Some(json) = camps_j {
                    snap.quasar_state_blobs.insert("quasar_campaigns".into(), json);
                }
                if let Some(json) = objs_j {
                    snap.quasar_state_blobs.insert("quasar_campaign_objectives".into(), json);
                }
                Ok::<(), String>(())
            });
        }
    }

    pub fn rehydrate_from_db(&self) {
        let Some(ref db) = self.db else { return };
        let (
            tracks_loaded,
            goals_loaded,
            tales_loaded,
            camps_loaded,
            objs_loaded,
            camp_tracks_loaded,
        ) = db.with_snapshot(|s| {
            let t = s
                .quasar_state_blobs
                .get("quasar_community_tracks")
                .and_then(|json| serde_json::from_str::<HashMap<u32, CommunityTrackState>>(json).ok());
            let g = s
                .quasar_state_blobs
                .get("quasar_generic_goals")
                .and_then(|json| serde_json::from_str::<HashMap<String, GenericGoalRecord>>(json).ok());
            let tales = s
                .quasar_state_blobs
                .get("quasar_active_tales")
                .and_then(|json| serde_json::from_str::<HashMap<u32, (u32, u32, u64)>>(json).ok());
            let camps = s
                .quasar_state_blobs
                .get("quasar_campaigns")
                .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignRecord>>(json).ok());
            let objs = s
                .quasar_state_blobs
                .get("quasar_campaign_objectives")
                .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignObjectiveRecord>>(json).ok());
            let trks = s
                .quasar_state_blobs
                .get("quasar_campaign_tracks")
                .and_then(|json| serde_json::from_str::<HashMap<u64, CampaignTrackRecord>>(json).ok());
            (t, g, tales, camps, objs, trks)
        });
        let mut tracks = tracks_loaded.unwrap_or_default();
        let templates = self
            .community_track_templates
            .read()
            .unwrap_or_else(|e| e.into_inner());
        for t in tracks.values_mut() {
            if t.template_id == 0 {
                t.template_id = t.track_id;
            }
            if t.tale_templates.is_empty() {
                if let Some(tpl) = templates.get(&t.template_id) {
                    t.tale_templates = tpl.tale_templates.clone();
                }
            }
        }
        *self
            .community_tracks
            .write()
            .unwrap_or_else(|e| e.into_inner()) = tracks;
        *self
            .generic_goals
            .write()
            .unwrap_or_else(|e| e.into_inner()) = goals_loaded.unwrap_or_default();
        *self
            .active_tales
            .write()
            .unwrap_or_else(|e| e.into_inner()) = tales_loaded.unwrap_or_default();
        *self.campaigns.write().unwrap_or_else(|e| e.into_inner()) =
            camps_loaded.unwrap_or_default();
        *self
            .campaign_objectives
            .write()
            .unwrap_or_else(|e| e.into_inner()) = objs_loaded.unwrap_or_default();
        *self
            .campaign_tracks
            .write()
            .unwrap_or_else(|e| e.into_inner()) = camp_tracks_loaded.unwrap_or_default();
    }

    pub fn activate_community_track(&self, template_id: u32) -> Result<CommunityTrackState, (u32, String)> {
        let templates = self.community_track_templates.read().unwrap_or_else(|e| e.into_inner());
        let tpl = templates
            .get(&template_id)
            .ok_or_else(|| (STATUS_NOT_FOUND, format!("Community track template {template_id} not found")))?
            .clone();
        let max_tpl = templates.keys().copied().max().unwrap_or(0);
        drop(templates);
        let mut tracks = self.community_tracks.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = tracks.values_mut().find(|e| e.template_id == template_id) {
            existing.active = true;
            let state = existing.clone();
            drop(tracks);
            self.save_to_db();
            return Ok(state);
        }
        let track_id = tracks.keys().copied().max().unwrap_or(10_000).max(max_tpl).saturating_add(1);
        let state = CommunityTrackState {
            track_id,
            template_id,
            name: tpl.name,
            target_progress: tpl.target_progress,
            current_progress: 0,
            active: true,
            tale_templates: tpl.tale_templates,
        };
        tracks.insert(track_id, state.clone());
        drop(tracks);
        self.save_to_db();
        Ok(state)
    }

    pub fn get_community_track(&self, track_id: u32) -> Option<CommunityTrackState> {
        let map = self.community_tracks.read().unwrap_or_else(|e| e.into_inner());
        resolve_track_key(&map, track_id).and_then(|k| map.get(&k).cloned())
    }

    pub fn add_community_progress(&self, track_id: u32, delta: u64) -> Option<CommunityTrackState> {
        let mut map = self.community_tracks.write().unwrap_or_else(|e| e.into_inner());
        let key = resolve_track_key(&map, track_id)?;
        let track = map.get_mut(&key)?;
        track.current_progress = track.current_progress.saturating_add(delta).min(track.target_progress);
        Some(track.clone())
    }

    pub fn record_tale_contribution(&self, tale_template_id: u32, delta: u64) -> Vec<CommunityTrackState> {
        let mut map = self.community_tracks.write().unwrap_or_else(|e| e.into_inner());
        let mut updated = Vec::new();
        for track in map.values_mut() {
            if track.active && track.tale_templates.contains(&tale_template_id) {
                track.current_progress = track.current_progress.saturating_add(delta).min(track.target_progress);
                updated.push(track.clone());
            }
        }
        updated
    }

    pub fn on_system_tale_encounter_completed(
        &self,
        solar_system_id: u32,
        site_category: &str,
        dungeon_id: u32,
        delta: u64,
        now_unix: u64,
        gateway: &PublicGatewayRouter,
    ) -> Vec<CommunityTrackState> {
        notices::complete_system_tale_encounter(self, solar_system_id, site_category, dungeon_id, delta, now_unix, gateway)
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
        let parse_u32 = |v: &DynamicValue| match v {
            DynamicValue::Uint32(u) => Some(*u),
            DynamicValue::Int32(i) => u32::try_from(*i).ok(),
            DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
            DynamicValue::Int64(i) => u32::try_from(*i).ok(),
            _ => None,
        };
        dyn_msg
            .get(1)
            .and_then(|v| match v {
                DynamicValue::Message(m) => m.get(1).and_then(parse_u32),
                other => parse_u32(other),
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
            let def_tid = if track.template_id != 0 { track.template_id } else { track.track_id };
            encode_varint_field(1, def_tid as u64, &mut def_id);
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
            let updated = mgr
                .add_community_progress(tid, delta)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
            mgr.save_to_db();

            let mut not_bytes = Vec::new();
            encode_varint_field(1, updated.track_id as u64, &mut not_bytes);
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
            let found_existing = {
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                if let Some(key) = resolve_track_key(&tracks, tid) {
                    if let Some(entry) = tracks.get_mut(&key) {
                        entry.active = true;
                    }
                    true
                } else {
                    false
                }
            };
            if found_existing {
                mgr.save_to_db();
            } else {
                mgr.activate_community_track(tid)?;
            }
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
            {
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                let key = resolve_track_key(&tracks, tid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
                if let Some(track) = tracks.get_mut(&key) {
                    track.active = false;
                }
            }
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
            {
                let mut tracks = mgr.community_tracks.write().unwrap_or_else(|e| e.into_inner());
                let key = resolve_track_key(&tracks, tid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;
                if let Some(track) = tracks.get_mut(&key) {
                    track.current_progress = val.min(track.target_progress);
                }
            }
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
            let now = ctx.now_unix();
            if !stopped.is_empty() {
                let progressed = mgr.record_tale_contribution(template_id, stopped.len() as u64);
                mgr.save_to_db();
                for tr in progressed {
                    let mut not_bytes = Vec::new();
                    encode_varint_field(1, tr.track_id as u64, &mut not_bytes);
                    let notice = GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.communitytrack.api.ProgressedNotice"),
                        value: not_bytes,
                    };
                    ctx_queue_notice(crate::quasar_gateway::NoticeScope::Broadcast, notice, now);
                }
            }
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
    // 3. Generic Goal Handlers (unified onto canonical snap.corp_projects)
    // ------------------------------------------------------------------------
    use crate::services::social_corp_fleet::social_corp_projects as corp_projects;

    fn resolved_contrib_pb(
        contribution_config_pb: &[u8],
        matcher_type: &str,
        filter_id: Option<i64>,
    ) -> Vec<u8> {
        if !contribution_config_pb.is_empty() {
            contribution_config_pb.to_vec()
        } else {
            notices::encode_goal_contribution_config(matcher_type, filter_id)
        }
    }

    fn load_live_goal(
        mgr: &ProgressionExtraManager,
        target_uuid: &[u8; 16],
        now_unix: u64,
    ) -> Option<(GenericGoalRecord, u64)> {
        if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(target_uuid), &mgr.db) {
            let _ = db.with_mut_snapshot(|s| corp_projects::sweep_expired_corp_projects_in_snap(s, now_unix));
            let from_db = db.with_snapshot(|s| {
                let p = s.corp_projects.get(&pid)?;
                if s.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.1) {
                    return None;
                }
                let det = s.alliance_war_state.project_details.get(&pid).cloned().unwrap_or_default();
                let st = if det.expired || det.status == "Expired" {
                    4
                } else if det.closed || det.status == "Closed" {
                    2
                } else if p.current_quantity >= p.target_quantity || det.status == "Completed" {
                    3
                } else {
                    1
                };
                let matcher_type = s
                    .alliance_war_state
                    .project_meta
                    .get(&pid)
                    .map(|m| m.0.clone())
                    .unwrap_or_default();
                Some((
                    p.name.clone(),
                    det.description,
                    p.target_quantity,
                    p.current_quantity,
                    det.creator_id,
                    det.closed || det.status == "Closed",
                    st,
                    matcher_type,
                    det.filter_id,
                    det.created_unix,
                ))
            });
            if let Some((name, desc, desired, current, det_creator, closed, state, matcher_type, filter_id, created_unix)) = from_db {
                let cached = mgr
                    .generic_goals
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&uuid_to_hex(target_uuid))
                    .cloned();
                let creator_id = if det_creator > 0 {
                    det_creator as u32
                } else {
                    cached.as_ref().map_or(0, |g| g.creator_id)
                };
                let contribution_config_pb = cached
                    .as_ref()
                    .map(|g| g.contribution_config_pb.as_slice())
                    .unwrap_or(&[]);
                let contribution_config_pb =
                    resolved_contrib_pb(contribution_config_pb, &matcher_type, filter_id);
                return Some((
                    GenericGoalRecord {
                        uuid: *target_uuid,
                        creator_id,
                        name,
                        description: desc,
                        desired_progress: desired,
                        current_progress: current,
                        closed,
                        matcher_type,
                        filter_id,
                        created_unix,
                        contribution_config_pb,
                    },
                    state,
                ));
            }
        }
        mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner()).get(&uuid_to_hex(target_uuid)).cloned().map(|mut g| {
            if g.contribution_config_pb.is_empty() {
                g.contribution_config_pb = notices::encode_goal_contribution_config(&g.matcher_type, g.filter_id);
            }
            let st = if g.closed { 2 } else if g.current_progress >= g.desired_progress { 3 } else { 1 };
            (g, st)
        })
    }

    fn collect_all_live_goals(mgr: &ProgressionExtraManager, now_unix: u64) -> Vec<GenericGoalRecord> {
        let mut by_hex: HashMap<String, GenericGoalRecord> = mgr.generic_goals.read().unwrap_or_else(|e| e.into_inner()).clone();
        for g in by_hex.values_mut() {
            if g.contribution_config_pb.is_empty() {
                g.contribution_config_pb = notices::encode_goal_contribution_config(&g.matcher_type, g.filter_id);
            }
        }
        if let Some(ref db) = mgr.db {
            let _ = db.with_mut_snapshot(|s| corp_projects::sweep_expired_corp_projects_in_snap(s, now_unix));
            db.with_snapshot(|s| {
                for (&pid, p) in &s.corp_projects {
                    if s.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.1) {
                        continue;
                    }
                    let det = s.alliance_war_state.project_details.get(&pid).cloned().unwrap_or_default();
                    let uuid = corp_projects::corp_goal_uuid(pid);
                    let hex = uuid_to_hex(&uuid);
                    let prev = by_hex.get(&hex);
                    let matcher_type = s
                        .alliance_war_state
                        .project_meta
                        .get(&pid)
                        .map(|m| m.0.clone())
                        .unwrap_or_default();
                    let creator_id = if det.creator_id > 0 { det.creator_id as u32 } else { prev.map_or(0, |g| g.creator_id) };
                    let contribution_config_pb = resolved_contrib_pb(
                        prev.map(|g| g.contribution_config_pb.as_slice()).unwrap_or(&[]),
                        &matcher_type,
                        det.filter_id,
                    );
                    by_hex.insert(hex, GenericGoalRecord {
                        uuid,
                        creator_id,
                        name: p.name.clone(),
                        description: det.description,
                        desired_progress: p.target_quantity,
                        current_progress: p.current_quantity,
                        closed: det.closed || det.status == "Closed",
                        matcher_type,
                        filter_id: det.filter_id,
                        created_unix: det.created_unix,
                        contribution_config_pb,
                    });
                }
            });
        }
        let mut list: Vec<GenericGoalRecord> = by_hex.into_values().collect();
        list.sort_by_key(|g| g.uuid);
        list
    }

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.CreateRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let corpid = ctx.effective_corporation_id().or_else(|| {
                mgr.db.as_ref().and_then(|d| d.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32)))
            });
            let now_u = ctx.now_unix();
            let (is_ceo, corp_role, wallet_div) = corpid.zip(mgr.db.as_ref()).map_or((false, ctx.corp_role_mask, 1u8), |(c_id, d)| {
                d.with_snapshot(|s| {
                    let (ceo, eff) = corp_projects::effective_corp_member_roles(s, c_id as i64, cid as i64, ctx.corp_role_mask, now_u);
                    let acct = s.corp_governance.get(&(c_id as i64)).and_then(|g| g.member_account_keys.get(&(cid as i64)).copied()).unwrap_or(1000);
                    let div = if (1000..=1006).contains(&acct) { (acct - 999) as u8 } else { 1 };
                    (ceo, eff, div)
                })
            });
            let is_gm = require_admin_role(ctx).is_ok();
            let has_corp_role = (corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PROJECT_MANAGER)) != 0;
            if !is_ceo && !has_corp_role && !is_gm {
                return Err((STATUS_FORBIDDEN, "CorpRoleDenied: Insufficient corporation role to create corporation goal".into()));
            }

            let name = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.clone(),
                _ => "Organization Goal".to_string(),
            };
            let desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => String::new(),
            };
            let target = match dyn_msg.get(3) {
                Some(DynamicValue::Uint64(u)) if *u > 0 => *u,
                Some(DynamicValue::Int64(i)) if *i > 0 => *i as u64,
                _ => 100,
            };
            let initial_prog = match dyn_msg.get(5) {
                Some(DynamicValue::Uint64(u)) => (*u).min(target),
                Some(DynamicValue::Int64(i)) if *i >= 0 => (*i as u64).min(target),
                _ => 0,
            };
            if initial_prog > 0 && !is_gm {
                return Err((STATUS_FORBIDDEN, "Non-admin callers cannot seed initial goal progress on CreateRequest".into()));
            }
            let opts = corp_projects::parse_goal_create_options(dyn_msg, target)?;
            if opts.escrow.raw_cents() > 0 && !is_ceo && !is_gm && (corp_role & eve_db::corp_roles::ROLE_DIRECTOR) == 0 {
                let take_bit = eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[(wallet_div.clamp(1, 7) - 1) as usize];
                if (corp_role & take_bit) == 0 {
                    return Err((STATUS_FORBIDDEN, "CorpRoleDenied: Project Manager lacks wallet division take role for escrow".into()));
                }
            }

            let uuid = if let (Some(ref db), Some(c_id)) = (&mgr.db, corpid) {
                let min_id = mgr.corp_state.as_ref().map_or(101, |cs| cs.alloc_goal_id() as i64);
                let (pid, u) = corp_projects::create_canonical_project(
                    db,
                    corp_projects::CanonicalProjectSpec {
                        corp_id: c_id as i64,
                        creator_id: cid as i64,
                        name: name.clone(),
                        description: desc.clone(),
                        target_quantity: target,
                        initial_quantity: initial_prog,
                        escrow: opts.escrow,
                        isk_per_unit: opts.isk_per_unit,
                        wallet_division: wallet_div,
                        matcher_type: opts.matcher_type.clone(),
                        is_freelance: false,
                        auto_payout_on_contribution: opts.auto_payout,
                        max_per_character: opts.max_per_character,
                        expires_unix: opts.expires_unix,
                        filter_id: opts.filter_id,
                        batch_size: 1,
                        now_unix: now_u,
                        min_project_id: min_id,
                    },
                )?;
                if let Some(ref cs) = mgr.corp_state {
                    cs.id_counters.write().unwrap_or_else(|e| e.into_inner()).2 = (pid as u64) + 1;
                    cs.sync_to_db();
                }
                u
            } else {
                let gid = mgr.corp_state.as_ref().map(|cs| cs.alloc_goal_id()).unwrap_or_else(|| ctx.now_unix());
                corp_projects::corp_goal_uuid(gid as i64)
            };

            let contrib_pb = notices::extract_raw_length_delimited_field(&any.value, 4)
                .unwrap_or_else(|| notices::encode_goal_contribution_config(&opts.matcher_type, opts.filter_id));
            let rec = GenericGoalRecord {
                uuid,
                creator_id: cid,
                name,
                description: desc,
                desired_progress: target,
                current_progress: initial_prog,
                closed: false,
                matcher_type: opts.matcher_type.clone(),
                filter_id: opts.filter_id,
                created_unix: now_u,
                contribution_config_pb: contrib_pb,
            };
            let goal_corp = mgr.goal_corp_id(&uuid);
            let created = notices::goal_created(
                &uuid,
                &notices::GoalAttributesView {
                    created_unix: now_u,
                    creator_id: cid,
                    corp_id: goal_corp,
                    name: &rec.name,
                    description: &rec.description,
                    desired: target,
                    current: initial_prog,
                    state: None,
                    contribution_config_pb: &rec.contribution_config_pb,
                },
            );
            mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).insert(uuid_to_hex(&uuid), rec);
            mgr.save_to_db();
            ctx_queue_notice(notices::goal_scope(goal_corp, cid), created, now_u);

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
            let (g, state) = load_live_goal(&mgr, &target_uuid, ctx.now_unix())
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;

            let mut out = Vec::new();
            let contrib_pb = resolved_contrib_pb(&g.contribution_config_pb, &g.matcher_type, g.filter_id);
            let attr_bytes = notices::encode_goal_attributes(&notices::GoalAttributesView {
                created_unix: if g.created_unix > 0 { g.created_unix } else { ctx.now_unix() },
                creator_id: g.creator_id,
                corp_id: goal_corp,
                name: &g.name,
                description: &g.description,
                desired: g.desired_progress,
                current: g.current_progress,
                state: Some(state),
                contribution_config_pb: &contrib_pb,
            });
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
            let mut out = Vec::new();
            for g in collect_all_live_goals(&mgr, ctx.now_unix()) {
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

                let g_corp = mgr.goal_corp_id(&g.uuid);
                let g_state = if g.closed {
                    notices::GOAL_STATE_CLOSED
                } else if g.current_progress >= g.desired_progress {
                    notices::GOAL_STATE_COMPLETED
                } else {
                    notices::GOAL_STATE_ACTIVE
                };
                let contrib_pb = resolved_contrib_pb(&g.contribution_config_pb, &g.matcher_type, g.filter_id);
                let attr_bytes = notices::encode_goal_attributes(&notices::GoalAttributesView {
                    created_unix: if g.created_unix > 0 { g.created_unix } else { ctx.now_unix() },
                    creator_id: g.creator_id,
                    corp_id: g_corp,
                    name: &g.name,
                    description: &g.description,
                    desired: g.desired_progress,
                    current: g.current_progress,
                    state: Some(g_state),
                    contribution_config_pb: &contrib_pb,
                });
                encode_bytes_field(2, &attr_bytes, &mut goal_bytes);

                encode_bytes_field(3, &goal_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    let can_manage_corp_goal = |mgr: &ProgressionExtraManager, caller_id: u32, goal_corp: Option<u32>, ctx: &eve_proto::quasar::AuthoritativeContext| -> bool {
        goal_corp.is_some_and(|gc| {
            gc != 0
                && mgr.db.as_ref().is_some_and(|db| {
                    db.with_snapshot(|s| {
                        let caller_corp = s.characters.get(&(caller_id as i64)).map(|c| c.corporation_id as u32);
                        if caller_corp != Some(gc) {
                            return false;
                        }
                        let (ceo, eff) = corp_projects::effective_corp_member_roles(
                            s, gc as i64, caller_id as i64, ctx.corp_role_mask, ctx.now_unix(),
                        );
                        ceo || (eff & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PROJECT_MANAGER)) != 0
                    })
                })
        })
    };

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
            let now_u = ctx.now_unix();
            let hex = uuid_to_hex(&target_uuid);
            let goal_corp = mgr.goal_corp_id(&target_uuid);
            let is_corp_mgr = can_manage_corp_goal(&mgr, caller_id, goal_corp, ctx);
            let (live_g, _) = load_live_goal(&mgr, &target_uuid, now_u)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
            if live_g.creator_id != caller_id && !is_corp_mgr && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator or corporation project manager/director".into()));
            }
            let was_open = !live_g.closed;
            if let Some(g) = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&hex) {
                g.closed = true;
            }
            mgr.save_to_db();
            if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(&target_uuid), &mgr.db) {
                let _ = corp_projects::close_or_delete_canonical_project(db, pid, false, now_u);
            } else if let Some(ref cs) = mgr.corp_state {
                if let Some(pid) = corp_projects::goal_uuid_to_project_id(&target_uuid) {
                    cs.close_or_delete_goal(pid as u64, false, now_u);
                }
            }
            if was_open {
                ctx_queue_notice(
                    notices::goal_scope(goal_corp, live_g.creator_id),
                    notices::goal_closed(&target_uuid, caller_id),
                    now_u,
                );
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CloseResponse"),
                value: Vec::new(),
            })
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
            let expected_curr = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            let new_prog = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) if *i >= 0 => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            let now_u = ctx.now_unix();
            let hex = uuid_to_hex(&target_uuid);
            let (live_g, _) = load_live_goal(&mgr, &target_uuid, now_u)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
            if expected_curr != live_g.current_progress {
                return Err((
                    crate::quasar_gateway::STATUS_CONFLICT,
                    format!("Goal current_progress CAS mismatch: expected {expected_curr}, actual {}", live_g.current_progress),
                ));
            }
            let prev = live_g.current_progress;
            let desired = live_g.desired_progress;
            let capped = new_prog.min(desired);
            if let Some(g) = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&hex) {
                g.current_progress = capped;
            }
            mgr.save_to_db();
            if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(&target_uuid), &mgr.db) {
                db.with_mut_snapshot(|snap| {
                    if let Some(p) = snap.corp_projects.get_mut(&pid) {
                        p.current_quantity = capped.min(p.target_quantity);
                        let done = p.current_quantity >= p.target_quantity;
                        if let Some(m) = snap.alliance_war_state.project_meta.get_mut(&pid) {
                            m.2 = done;
                        }
                        if done {
                            if let Some(d) = snap.alliance_war_state.project_details.get_mut(&pid) {
                                if d.status == "Active" {
                                    d.status = "Completed".into();
                                }
                            }
                        }
                    }
                });
            }
            let scope = notices::goal_scope(mgr.goal_corp_id(&target_uuid), live_g.creator_id);
            if capped != prev {
                ctx_queue_notice(scope.clone(), notices::goal_progressed(&target_uuid, capped), now_u);
            }
            if capped >= desired && prev < desired {
                ctx_queue_notice(scope, notices::goal_completed(&target_uuid), now_u);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.SetCurrentProgressResponse"),
                value: Vec::new(),
            })
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
            let now_u = ctx.now_unix();
            let hex = uuid_to_hex(&target_uuid);
            let goal_corp = mgr.goal_corp_id(&target_uuid);
            let is_corp_mgr = can_manage_corp_goal(&mgr, caller_id, goal_corp, ctx);
            let (live_g, _) = load_live_goal(&mgr, &target_uuid, now_u)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
            if live_g.creator_id != caller_id && !is_corp_mgr && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Forbidden: not goal creator or corporation project manager/director".into()));
            }
            mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).remove(&hex);
            mgr.save_to_db();
            if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(&target_uuid), &mgr.db) {
                let _ = corp_projects::close_or_delete_canonical_project(db, pid, true, now_u);
            } else if let Some(ref cs) = mgr.corp_state {
                if let Some(pid) = corp_projects::goal_uuid_to_project_id(&target_uuid) {
                    cs.close_or_delete_goal(pid as u64, true, now_u);
                }
            }
            ctx_queue_notice(
                notices::goal_scope(goal_corp, live_g.creator_id),
                notices::goal_deleted(&target_uuid),
                now_u,
            );
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.DeleteResponse"),
                value: Vec::new(),
            })
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
                let now_u = ctx.now_unix();
                let (live_g, _) = load_live_goal(&mgr, &target_uuid, now_u)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Goal not found".into()))?;
                if let Some(g) = mgr.generic_goals.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid_to_hex(&target_uuid)) {
                    if is_name { g.name = text.clone() } else { g.description = text.clone() }
                }
                mgr.save_to_db();
                if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(&target_uuid), &mgr.db) {
                    db.with_mut_snapshot(|snap| {
                        if is_name {
                            if let Some(p) = snap.corp_projects.get_mut(&pid) {
                                p.name = text.clone();
                            }
                        } else if let Some(d) = snap.alliance_war_state.project_details.get_mut(&pid) {
                            d.description = text.clone();
                        }
                    });
                }
                let notice = if is_name {
                    notices::goal_name_changed(&target_uuid, &text)
                } else {
                    notices::goal_description_changed(&target_uuid, &text)
                };
                ctx_queue_notice(notices::goal_scope(mgr.goal_corp_id(&target_uuid), live_g.creator_id), notice, now_u);
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
            let count = collect_all_live_goals(&mgr, ctx.now_unix())
                .into_iter()
                .filter(|g| g.creator_id == cid && !g.closed)
                .count() as u64;
            let mut out = Vec::new();
            encode_varint_field(1, count, &mut out);
            encode_varint_field(2, 50, &mut out);
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
        if let (Some(pid), Some(ref db)) = (corp_projects::goal_uuid_to_project_id(&g.uuid), &mgr.db) {
            let list = db.with_snapshot(|s| {
                let mut v: Vec<(u32, u64)> = s
                    .alliance_war_state
                    .project_details
                    .get(&pid)
                    .map(|d| {
                        d.char_contributions
                            .iter()
                            .filter_map(|(&c, &p)| (c > 0 && p > 0).then_some((c as u32, p)))
                            .collect()
                    })
                    .unwrap_or_default();
                v.sort_by_key(|&(c, _)| c);
                v
            });
            if !list.is_empty() {
                return list;
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
            let (g, _) = load_live_goal(&mgr, &target_uuid, ctx.now_unix())
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
            let (g, _) = load_live_goal(&mgr, &target_uuid, ctx.now_unix())
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
            let mut out = Vec::new();
            for g in collect_all_live_goals(&mgr, ctx.now_unix()) {
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
                let now_ft = db.clock().now_blueos_filetime_ticks();
                db.with_snapshot(|snap| {
                    crate::services::season_manager::get_character_challenges_for_quasar(
                        snap,
                        mgr.sde.as_deref(),
                        now_ft,
                        char_id,
                    )
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

pub use super::quasar_goal_tale_notices::handle_opportunity_event;

#[cfg(test)]
#[path = "quasar_progression_extra_tests.rs"]
mod quasar_progression_extra_tests;

