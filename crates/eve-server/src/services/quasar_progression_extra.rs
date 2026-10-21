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
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
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

#[derive(Debug, Clone)]
pub struct CommunityTrackState {
    pub track_id: u32,
    pub name: String,
    pub target_progress: u64,
    pub current_progress: u64,
    pub active: bool,
}

#[derive(Debug, Default)]
pub struct ProgressionExtraManager {
    pub community_tracks: RwLock<HashMap<u32, CommunityTrackState>>,
    pub active_tales: RwLock<HashMap<u32, (u32, u64)>>, // tale_id -> (solar_system_id, started_at)
    pub equipped_titles: RwLock<HashMap<u32, u32>>,     // char_id -> title_id
    pub generic_goals: RwLock<HashMap<[u8; 16], GenericGoalRecord>>,
}

#[derive(Debug, Clone)]
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
    pub fn new() -> Self {
        let mgr = Self {
            community_tracks: RwLock::new(HashMap::new()),
            active_tales: RwLock::new(HashMap::new()),
            equipped_titles: RwLock::new(HashMap::new()),
            generic_goals: RwLock::new(HashMap::new()),
        };
        // Seed initial community track (DERIVED from standard server community events)
        mgr.community_tracks.write().unwrap().insert(
            1,
            CommunityTrackState {
                track_id: 1,
                name: "New Eden Defense Initiative".to_string(),
                target_progress: 1_000_000,
                current_progress: 350_000,
                active: true,
            },
        );
        // Seed initial generic goal
        let mut sample_uuid = [0u8; 16];
        sample_uuid[0..4].copy_from_slice(&0x474F414Cu32.to_be_bytes()); // "GOAL"
        mgr.generic_goals.write().unwrap().insert(
            sample_uuid,
            GenericGoalRecord {
                uuid: sample_uuid,
                creator_id: 90_000_001,
                name: "Alliance Strategic Deployment".to_string(),
                description: "Stock fuel and munitions in staging citadel.".to_string(),
                desired_progress: 100,
                current_progress: 45,
                closed: false,
            },
        );
        mgr
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
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.communitytrack.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            let track = mgr
                .get_community_track(tid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Community track not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_varint_field(1, track.track_id as u64, &mut attr_bytes);
            encode_string_field(2, &track.name, &mut attr_bytes);
            encode_varint_field(3, track.target_progress, &mut attr_bytes);
            encode_varint_field(4, track.current_progress, &mut attr_bytes);
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
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            let delta = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(100);
            let _ = mgr.add_community_progress(tid, delta);
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.StartRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            if let Some(mut track) = mgr.get_community_track(tid) {
                track.active = true;
                mgr.community_tracks.write().unwrap().insert(tid, track);
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.api.StopRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            if let Some(mut track) = mgr.get_community_track(tid) {
                track.active = false;
                mgr.community_tracks.write().unwrap().insert(tid, track);
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.communitytrack.admin.api.SetProgressRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            let val = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            if let Some(mut track) = mgr.get_community_track(tid) {
                track.current_progress = val;
                mgr.community_tracks.write().unwrap().insert(tid, track);
            }
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
                .unwrap()
                .insert(tale_id, (system_id, ctx.now_unix()));
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.StopTalesRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tale_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            mgr.active_tales.write().unwrap().remove(&tale_id);
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.tale.api.GetActiveTalesCountRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let count = mgr.active_tales.read().unwrap().len() as u32;
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
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let mut uuid = [0u8; 16];
            uuid[0..8].copy_from_slice(&(ctx.now_unix()).to_be_bytes());
            uuid[8..12].copy_from_slice(&cid.to_be_bytes());
            uuid[12..16].copy_from_slice(&0x474F414Cu32.to_be_bytes());

            let name = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::String(s) => Some(s.clone()),
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
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(100);

            let rec = GenericGoalRecord {
                uuid,
                creator_id: cid,
                name,
                description: desc,
                desired_progress: target,
                current_progress: 0,
                closed: false,
            };
            mgr.generic_goals.write().unwrap().insert(uuid, rec);

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
            let goals = mgr.generic_goals.read().unwrap();
            let g = goals
                .get(&target_uuid)
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
            let goals = mgr.generic_goals.read().unwrap();
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            if let Some(g) = mgr.generic_goals.write().unwrap().get_mut(&target_uuid) {
                g.closed = true;
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.SetCurrentProgressRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let new_prog = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(0);
            if let Some(g) = mgr.generic_goals.write().unwrap().get_mut(&target_uuid) {
                g.current_progress = new_prog;
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.goal.api.DeleteRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            mgr.generic_goals.write().unwrap().remove(&target_uuid);
            Ok(GoogleEmpty.to_any())
        }),
    );

    router.register_handler(
        "eve_public.goal.api.GetCapacityRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out);  // count = 1
            encode_varint_field(2, 50, &mut out); // capacity = 50
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetCapacityResponse"),
                value: out,
            })
        }),
    );

    // ------------------------------------------------------------------------
    // 4. Character Titles Handlers
    // ------------------------------------------------------------------------
    let _mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let sample_titles = [101u32, 102u32, 103u32]; // DERIVED title IDs
            let mut out = Vec::new();
            for tid in sample_titles {
                let mut id_bytes = Vec::new();
                encode_varint_field(1, tid as u64, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GetOwnedResponse"
                ),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.cosmetic.character.title.api.EquipRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let tid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(101);
            mgr.equipped_titles.write().unwrap().insert(cid, tid);
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.cosmetic.character.title.api.UnequipRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            mgr.equipped_titles.write().unwrap().remove(&cid);
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(90_000_001);

            let mut out = Vec::new();
            if let Some(tid) = mgr.equipped_titles.read().unwrap().get(&cid) {
                let mut id_bytes = Vec::new();
                encode_varint_field(1, *tid as u64, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            } else {
                encode_varint_field(2, 1, &mut out); // no_title = true
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GetEquippedForCharacterResponse"
                ),
                value: out,
            })
        }),
    );

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
    router.register_handler(
        "eve_public.campaign.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_varint_field(1, 1, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    router.register_handler(
        "eve_public.campaign.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_string_field(1, "Havoc Pirate Insurgency", &mut attr_bytes);
            encode_string_field(2, "Pirate insurgency campaigns in low-security space.", &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetResponse"),
                value: out,
            })
        }),
    );

    router.register_handler(
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            let mut chal = Vec::new();
            encode_varint_field(1, 1001, &mut chal); // challenge_id
            encode_varint_field(2, 5, &mut chal);    // target
            encode_varint_field(3, 2, &mut chal);    // current
            encode_bytes_field(1, &chal, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.seasonalevent.challenge.GetChallengesForCharacterResponse"
                ),
                value: out,
            })
        }),
    );
}
