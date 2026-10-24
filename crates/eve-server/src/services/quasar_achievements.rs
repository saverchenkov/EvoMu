//! `quasar_achievements` — V24.01 Quasar AIR Achievements & Milestones Service (`feat:quasar.achievement`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Achievements system:
//! - Category definitions: Combat, Exploration, Industry, Economy, Social.
//! - Per-character achievement tracking, milestone thresholds, score tallies.
//! - Milestone reward claim processing with live wallet/SP awards.
//! - Request handlers for `eve_public.achievement.api` and `eve.achievement.api`.
//! - Live notice publication (`ProgressedNotice`, `CompletedNotice`, `ReachedNotice`, `CheckVersionNotice`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, sha256_hex, DynamicValue, GoogleAny,
    GoogleEmpty, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub const ACHIEVEMENTS_STATE_KEY: &str = "quasar_achievements";

fn deserialize_i64_or_f64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct I64OrF64Visitor;
    impl<'de> serde::de::Visitor<'de> for I64OrF64Visitor {
        type Value = i64;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an integer or floating point number")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(v)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(v as i64)
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(v.round() as i64)
        }
    }
    deserializer.deserialize_any(I64OrF64Visitor)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AchievementDef {
    pub id: u32,
    pub category_id: u32,
    pub name: String,
    pub target: u64,
    pub score: u32,
    pub milestone_targets: Vec<u64>,
    #[serde(default, deserialize_with = "deserialize_i64_or_f64")]
    pub reward_isk: i64,
    #[serde(default, deserialize_with = "deserialize_i64_or_f64")]
    pub reward_sp: i64,
}

// DERIVED: V24 achievement definitions are server-delivered via achievement.definition.api.cdn.GetLatestRequest (not bundled in client FSD); canonical combat/exploration/industry career milestone definitions:
pub fn default_achievement_definitions() -> Vec<AchievementDef> {
    vec![
        // Combat
        AchievementDef {
            id: 101,
            category_id: 1, // Combat
            name: "Destroy Hostile Vessels".to_string(),
            target: 100,
            score: 250,
            milestone_targets: vec![10, 50, 100],
            reward_isk: 250_000,
            reward_sp: 5_000,
        },
        AchievementDef {
            id: 102,
            category_id: 1, // Combat
            name: "Clear Combat Anomalies".to_string(),
            target: 20,
            score: 150,
            milestone_targets: vec![5, 10, 20],
            reward_isk: 150_000,
            reward_sp: 3_000,
        },
        // Exploration
        AchievementDef {
            id: 201,
            category_id: 2, // Exploration
            name: "Scan Cosmic Signatures".to_string(),
            target: 50,
            score: 200,
            milestone_targets: vec![10, 25, 50],
            reward_isk: 200_000,
            reward_sp: 4_000,
        },
        AchievementDef {
            id: 202,
            category_id: 2, // Exploration
            name: "Hack Relic and Data Containers".to_string(),
            target: 30,
            score: 200,
            milestone_targets: vec![5, 15, 30],
            reward_isk: 200_000,
            reward_sp: 4_000,
        },
        // Industry
        AchievementDef {
            id: 301,
            category_id: 3, // Industry
            name: "Harvest Asteroid Ore".to_string(),
            target: 10_000,
            score: 200,
            milestone_targets: vec![1_000, 5_000, 10_000],
            reward_isk: 300_000,
            reward_sp: 6_000,
        },
        AchievementDef {
            id: 302,
            category_id: 3, // Industry
            name: "Complete Manufacturing Jobs".to_string(),
            target: 10,
            score: 200,
            milestone_targets: vec![2, 5, 10],
            reward_isk: 250_000,
            reward_sp: 5_000,
        },
    ]
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterAchievement {
    pub def: AchievementDef,
    pub progress: u64,
    pub completed: bool,
    pub completed_date: Option<u64>,
    pub reached_milestones: Vec<u32>,
    pub unclaimed_reward_count: u32,
}

pub struct AchievementManager {
    pub definitions: Vec<AchievementDef>,
    pub character_achievements: RwLock<HashMap<u32, Vec<CharacterAchievement>>>,
    db: Option<Arc<WorldRepository>>,
}

impl Default for AchievementManager {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AchievementManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        let defs = default_achievement_definitions();
        let loaded = db.as_ref().map(|d| {
            d.with_snapshot(|s| {
                s.quasar_state_blobs
                    .get("quasar_achievements")
                    .and_then(|json| serde_json::from_str::<HashMap<u32, Vec<CharacterAchievement>>>(json).ok())
                    .unwrap_or_default()
            })
        }).unwrap_or_default();

        Self {
            definitions: defs,
            character_achievements: RwLock::new(loaded),
            db,
        }
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let map = self.character_achievements.read().unwrap_or_else(|e| e.into_inner());
            if let Ok(json) = serde_json::to_string(&*map) {
                let _ = db.try_with_mut_snapshot(|snap| {
                    snap.quasar_state_blobs.insert("quasar_achievements".into(), json);
                    Ok::<(), String>(())
                });
            }
        }
    }

    fn init_list<'a>(defs: &[AchievementDef], map: &'a mut HashMap<u32, Vec<CharacterAchievement>>, character_id: u32) -> &'a mut Vec<CharacterAchievement> {
        map.entry(character_id).or_insert_with(|| {
            defs.iter()
                .cloned()
                .map(|def| CharacterAchievement {
                    def,
                    progress: 0,
                    completed: false,
                    completed_date: None,
                    reached_milestones: Vec::new(),
                    unclaimed_reward_count: 0,
                })
                .collect()
        })
    }

    pub fn get_or_init_achievements(&self, character_id: u32) -> Vec<CharacterAchievement> {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        Self::init_list(&self.definitions, &mut map, character_id).clone()
    }

    pub fn unlock_achievement(&self, character_id: u32, achievement_id: u32) {
        self.add_progress(character_id, achievement_id, 10_000_000, 1_700_000_000);
        self.save_to_db();
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        achievement_id: u32,
        delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterAchievement, bool, Vec<u32>)> {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);

        for a in list.iter_mut() {
            if a.def.id == achievement_id {
                let prev_completed = a.completed;
                a.progress = a.progress.saturating_add(delta);

                let mut new_milestones = Vec::new();
                for (idx, target) in a.def.milestone_targets.iter().enumerate() {
                    let m_id = (idx + 1) as u32;
                    if a.progress >= *target && !a.reached_milestones.contains(&m_id) {
                        a.reached_milestones.push(m_id);
                        a.unclaimed_reward_count += 1;
                        new_milestones.push(m_id);
                    }
                }

                if a.progress >= a.def.target && !a.completed {
                    a.completed = true;
                    a.completed_date = Some(now_unix);
                }
                let just_completed = !prev_completed && a.completed;
                return Some((a.clone(), just_completed, new_milestones));
            }
        }
        None
    }

    pub fn mark_completed(
        &self,
        character_id: u32,
        achievement_id: u32,
        now_unix: u64,
    ) -> Result<CharacterAchievement, (u32, String)> {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);

        for a in list.iter_mut() {
            if a.def.id == achievement_id {
                a.progress = a.def.target;
                a.completed = true;
                a.completed_date = Some(now_unix);
                for (idx, _) in a.def.milestone_targets.iter().enumerate() {
                    let m_id = (idx + 1) as u32;
                    if !a.reached_milestones.contains(&m_id) {
                        a.reached_milestones.push(m_id);
                        a.unclaimed_reward_count += 1;
                    }
                }
                return Ok(a.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Achievement not found".into()))
    }

    pub fn reset_achievement(
        &self,
        character_id: u32,
        achievement_id: u32,
    ) -> Result<CharacterAchievement, (u32, String)> {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);

        for a in list.iter_mut() {
            if a.def.id == achievement_id {
                a.progress = 0;
                a.completed = false;
                a.completed_date = None;
                a.reached_milestones.clear();
                a.unclaimed_reward_count = 0;
                return Ok(a.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Achievement not found".into()))
    }

    pub fn reset_all(&self, character_id: u32) {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get_mut(&character_id) {
            for a in list.iter_mut() {
                a.progress = 0;
                a.completed = false;
                a.completed_date = None;
                a.reached_milestones.clear();
                a.unclaimed_reward_count = 0;
            }
        }
    }

    pub fn reset_category(&self, character_id: u32, category_id: u32) {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get_mut(&character_id) {
            for a in list.iter_mut() {
                if a.def.category_id == category_id {
                    a.progress = 0;
                    a.completed = false;
                    a.completed_date = None;
                    a.reached_milestones.clear();
                    a.unclaimed_reward_count = 0;
                }
            }
        }
    }

    pub fn claim_rewards(
        &self,
        character_id: u32,
        db: &WorldRepository,
    ) -> u32 {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut total_claimed = 0u32;
        let mut total_isk = 0i64;
        let mut total_sp = 0i64;
        let mut taken: Vec<(u32, u32)> = Vec::new();
        {
            let Some(list) = map.get_mut(&character_id) else {
                return 0;
            };

            for a in list.iter_mut() {
                if a.unclaimed_reward_count > 0 {
                    let count = a.unclaimed_reward_count;
                    total_claimed += count;
                    total_isk += (count as i64) * a.def.reward_isk;
                    total_sp += (count as i64) * a.def.reward_sp;
                    taken.push((a.def.id, count));
                    a.unclaimed_reward_count = 0;
                }
            }
        }

        if total_claimed > 0 {
            let json_state = match serde_json::to_string(&*map) {
                Ok(j) => j,
                Err(_) => {
                    if let Some(list) = map.get_mut(&character_id) {
                        for (ach_id, count) in taken {
                            if let Some(a) = list.iter_mut().find(|a| a.def.id == ach_id) {
                                a.unclaimed_reward_count = a.unclaimed_reward_count.saturating_add(count);
                            }
                        }
                    }
                    return 0;
                }
            };

            let isk_cents = IskCents::from_isk(total_isk);
            let credited = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
                let ch = snap.characters.get_mut(&(character_id as i64)).ok_or(())?;
                let new_bal = ch.balance.checked_add(isk_cents).ok_or(())?;
                ch.balance = new_bal;
                ch.free_skill_points += total_sp as f64;
                let balance_after = ch.balance;

                snap.next_entity_id += 1;
                let entry_id = snap.next_entity_id;
                snap.wallet_journal.push(eve_db::WalletJournalEntry {
                    entry_id,
                    character_id: character_id as i64,
                    entry_type: "achievement_reward".to_string(),
                    amount: isk_cents,
                    balance_after,
                    reason: "AIR achievement rewards claimed".to_string(),
                });
                snap.quasar_state_blobs.insert(ACHIEVEMENTS_STATE_KEY.to_string(), json_state);
                Ok(())
            }).is_ok();

            if !credited {
                if let Some(list) = map.get_mut(&character_id) {
                    for (ach_id, count) in taken {
                        if let Some(a) = list.iter_mut().find(|a| a.def.id == ach_id) {
                            a.unclaimed_reward_count = a.unclaimed_reward_count.saturating_add(count);
                        }
                    }
                }
                return 0;
            }
        }
        total_claimed
    }

    pub fn total_character_score(&self, character_id: u32) -> u32 {
        let map = self
            .character_achievements
            .read()
            .unwrap_or_else(|e| e.into_inner());
        map.get(&character_id)
            .map(|list| {
                list.iter()
                    .filter(|a| a.completed)
                    .map(|a| a.def.score)
                    .sum()
            })
            .unwrap_or(0)
    }

    pub fn category_score(&self, character_id: u32, category_id: u32) -> u32 {
        let map = self
            .character_achievements
            .read()
            .unwrap_or_else(|e| e.into_inner());
        map.get(&character_id)
            .map(|list| {
                list.iter()
                    .filter(|a| a.def.category_id == category_id && a.completed)
                    .map(|a| a.def.score)
                    .sum()
            })
            .unwrap_or(0)
    }
}

// ============================================================================
// Registration
// ============================================================================

pub fn register_achievement_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<AchievementManager>,
) {
    // 1. eve_public.achievement.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid_opt = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                });
            let aid = match aid_opt {
                Some(id) if id > 0 => {
                    if !mgr.definitions.iter().any(|d| d.id == id) {
                        return Err((STATUS_NOT_FOUND, "Achievement not found".into()));
                    }
                    id
                }
                _ => 101,
            };

            let achievements = mgr.get_or_init_achievements(cid);
            let a = achievements
                .iter()
                .find(|x| x.def.id == aid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Achievement not found".into()))?;

            let mut out = Vec::new();
            if let Some(ts) = a.completed_date {
                let ts_proto = GoogleTimestamp {
                    seconds: ts as i64,
                    nanos: 0,
                };
                encode_bytes_field(1, &ts_proto.encode_proto(), &mut out);
            }
            encode_varint_field(2, if a.completed { 0 } else { 1 }, &mut out); // not_completed
            encode_varint_field(3, a.progress, &mut out); // progress_value

            for m_id in &a.reached_milestones {
                let mut rm = Vec::new();
                encode_varint_field(1, *m_id as u64, &mut rm);
                encode_bytes_field(5, &rm, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.achievement.category.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.category.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let cat_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(1);
            let score = mgr.category_score(cid, cat_id);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve_public.achievement.showcase.api.GetCharacterScoreRequest -> GetCharacterScoreResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let score = mgr.total_character_score(cid);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.showcase.api.GetCharacterScoreResponse"
                ),
                value: out,
            })
        }),
    );

    // 4. eve_public.achievement.reward.api.GetUnclaimedRequest -> GetUnclaimedResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let achievements = mgr.get_or_init_achievements(cid);
            let mut out = Vec::new();
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let mut rew = Vec::new();
                    encode_varint_field(1, a.def.id as u64, &mut rew);
                    encode_varint_field(2, a.unclaimed_reward_count as u64, &mut rew);
                    encode_bytes_field(1, &rew, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.reward.api.GetUnclaimedResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve_public.achievement.reward.api.ClaimRequest -> ClaimResponse
    let mgr = Arc::clone(&manager);
    let db_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.achievement.reward.api.ClaimRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let _ = mgr.claim_rewards(cid, &db_claim);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    fn emit_achievement_notices(
        cid: u32,
        aid: u32,
        completed: bool,
        milestones: &[u32],
        now: u64,
    ) {
        let mut not_bytes = Vec::new();
        encode_varint_field(1, aid as u64, &mut not_bytes);
        super::quasar_social_chat::ctx_queue_notice(
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.ProgressedNotice"),
                value: not_bytes.clone(),
            },
            now,
        );
        if completed {
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.CompletedNotice"),
                    value: not_bytes,
                },
                now,
            );
        }
        for m_id in milestones {
            let mut ms_bytes = Vec::new();
            encode_varint_field(1, aid as u64, &mut ms_bytes);
            encode_varint_field(2, *m_id as u64, &mut ms_bytes);
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.milestone.api.ReachedNotice"),
                    value: ms_bytes,
                },
                now,
            );
        }
    }

    // 6. eve_public.achievement.definition.api.cdn.GetLatestRequest -> GetLatestResponse
    let mgr_cdn = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let json_bytes = serde_json::to_vec(&mgr_cdn.definitions).unwrap_or_default();
            let hash = sha256_hex(&json_bytes);
            let mut out = Vec::new();
            encode_string_field(1, &hash, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.api.cdn.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.achievement.api.admin.AddProgressRequest -> AddProgressResponse
    let mgr_prog = Arc::clone(&manager);
    let progress_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        crate::quasar_gateway::require_admin_role(ctx)?;
        let Some(cid) = ctx.effective_character_id() else {
            return Err((STATUS_FORBIDDEN, "Active character required".into()));
        };
        let aid = dyn_msg
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
        let delta = dyn_msg
            .get(2)
            .and_then(|v| match v {
                DynamicValue::Uint64(u) => Some(*u),
                DynamicValue::Int64(i) => Some(*i as u64),
                _ => None,
            })
            .unwrap_or(1);

        let outcome = mgr_prog.add_progress(cid, aid, delta, ctx.now_unix());
        mgr_prog.save_to_db();

        if let Some((_, just_completed, new_milestones)) = outcome {
            emit_achievement_notices(cid, aid, just_completed, &new_milestones, ctx.now_unix());
        }

        Ok(GoogleEmpty.to_any())
    });
    router.register_handler(
        "eve_public.achievement.api.admin.AddProgressRequest",
        Arc::clone(&progress_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.ProgressAchievementRequest",
        progress_handler,
    );

    // 8. eve_public.achievement.api.admin.CompleteRequest -> CompleteResponse
    let mgr_comp = Arc::clone(&manager);
    let complete_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        crate::quasar_gateway::require_admin_role(ctx)?;
        let Some(cid) = ctx.effective_character_id() else {
            return Err((STATUS_FORBIDDEN, "Active character required".into()));
        };
        let aid = dyn_msg
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
        let ach = mgr_comp.mark_completed(cid, aid, ctx.now_unix())?;
        mgr_comp.save_to_db();

        let ms: Vec<u32> = (1..=ach.def.milestone_targets.len() as u32).collect();
        emit_achievement_notices(cid, aid, true, &ms, ctx.now_unix());

        Ok(GoogleEmpty.to_any())
    });
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteRequest",
        Arc::clone(&complete_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteAchievementRequest",
        Arc::clone(&complete_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.UnlockAchievementRequest",
        complete_handler,
    );

    // 9. eve_public.achievement.api.admin.ResetRequest -> ResetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(101);
            let _ = mgr.reset_achievement(cid, aid)?;
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 10. eve_public.achievement.api.admin.ResetAllRequest -> ResetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            if let Some(cid) = ctx.effective_character_id() {
                mgr.reset_all(cid);
                mgr.save_to_db();
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 11. eve_public.achievement.category.api.admin.ResetRequest -> ResetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.category.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let cat_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(1);
            mgr.reset_category(cid, cat_id);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 12. eve.achievement.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let score = ctx
                .effective_character_id()
                .map(|cid| mgr.total_character_score(cid))
                .unwrap_or(0);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 13. eve.achievement.category.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.category.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cat_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(1);
            let score = ctx
                .effective_character_id()
                .map(|cid| mgr.category_score(cid, cat_id))
                .unwrap_or(0);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.category.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 14. eve.achievement.definition.api.GetLatestRequest -> GetLatestResponse
    let mgr_cdn_eve = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.definition.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let json_bytes = serde_json::to_vec(&mgr_cdn_eve.definitions).unwrap_or_default();
            let hash = sha256_hex(&json_bytes);
            let mut out = Vec::new();
            encode_string_field(1, &hash, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.api.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 15. eve.achievement.definition.version.api.ListRequest -> ListResponse
    router.register_handler(
        "eve.achievement.definition.version.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.version.api.ListResponse"
                ),
                value: out,
            })
        }),
    );

    // 16. eve.achievement.definition.version.api.GetDefaultVersionRequest -> GetDefaultVersionResponse
    router.register_handler(
        "eve.achievement.definition.version.api.GetDefaultVersionRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.version.api.GetDefaultVersionResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.achievement.reward.api.GetUnclaimedRequest -> GetUnclaimedResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.reward.api.GetUnclaimedRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let achievements = mgr.get_or_init_achievements(cid);
            let mut out = Vec::new();
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let mut rew = Vec::new();
                    encode_varint_field(1, a.def.id as u64, &mut rew);
                    encode_varint_field(2, a.unclaimed_reward_count as u64, &mut rew);
                    encode_bytes_field(1, &rew, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.reward.api.GetUnclaimedResponse"
                ),
                value: out,
            })
        }),
    );

    // 18. eve.achievement.showcase.api.GetCharacterScoreRequest -> GetCharacterScoreResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.showcase.api.GetCharacterScoreRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let score = mgr.total_character_score(cid);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.showcase.api.GetCharacterScoreResponse"
                ),
                value: out,
            })
        }),
    );
}
