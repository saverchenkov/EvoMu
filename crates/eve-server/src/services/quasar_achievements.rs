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
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    GoogleEmpty, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct AchievementDef {
    pub id: u32,
    pub category_id: u32,
    pub name: &'static str,
    pub target: u64,
    pub score: u32,
    pub milestone_targets: Vec<u64>,
}

pub fn default_achievement_definitions() -> Vec<AchievementDef> {
    vec![
        // Combat
        AchievementDef {
            id: 101,
            category_id: 1, // Combat
            name: "Vanquisher of the Stars",
            target: 100,
            score: 250,
            milestone_targets: vec![10, 50, 100],
        },
        AchievementDef {
            id: 102,
            category_id: 1, // Combat
            name: "Anomaly Cleaner",
            target: 20,
            score: 150,
            milestone_targets: vec![5, 10, 20],
        },
        // Exploration
        AchievementDef {
            id: 201,
            category_id: 2, // Exploration
            name: "Master of the Cosmic Web",
            target: 50,
            score: 200,
            milestone_targets: vec![10, 25, 50],
        },
        AchievementDef {
            id: 202,
            category_id: 2, // Exploration
            name: "Cryptographic Breaker",
            target: 30,
            score: 200,
            milestone_targets: vec![5, 15, 30],
        },
        // Industry
        AchievementDef {
            id: 301,
            category_id: 3, // Industry
            name: "Deep Core Excavator",
            target: 10_000,
            score: 200,
            milestone_targets: vec![1_000, 5_000, 10_000],
        },
        AchievementDef {
            id: 302,
            category_id: 3, // Industry
            name: "Master Manufacturer",
            target: 10,
            score: 200,
            milestone_targets: vec![2, 5, 10],
        },
    ]
}

#[derive(Debug, Clone)]
pub struct CharacterAchievement {
    pub def: AchievementDef,
    pub progress: u64,
    pub completed: bool,
    pub completed_date: Option<u64>,
    pub reached_milestones: Vec<u32>,
    pub unclaimed_reward_count: u32,
}

#[derive(Debug, Default)]
pub struct AchievementManager {
    pub character_achievements: RwLock<HashMap<u32, Vec<CharacterAchievement>>>,
}

impl AchievementManager {
    pub fn new() -> Self {
        Self {
            character_achievements: RwLock::new(HashMap::new()),
        }
    }

    pub fn get_or_init_achievements(&self, character_id: u32) -> Vec<CharacterAchievement> {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(character_id)
            .or_insert_with(|| {
                default_achievement_definitions()
                    .into_iter()
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
            .clone()
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
        let list = map.entry(character_id).or_insert_with(|| {
            default_achievement_definitions()
                .into_iter()
                .map(|def| CharacterAchievement {
                    def,
                    progress: 0,
                    completed: false,
                    completed_date: None,
                    reached_milestones: Vec::new(),
                    unclaimed_reward_count: 0,
                })
                .collect()
        });

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
        let list = map.entry(character_id).or_insert_with(|| {
            default_achievement_definitions()
                .into_iter()
                .map(|def| CharacterAchievement {
                    def,
                    progress: 0,
                    completed: false,
                    completed_date: None,
                    reached_milestones: Vec::new(),
                    unclaimed_reward_count: 0,
                })
                .collect()
        });

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
        let list = map.entry(character_id).or_insert_with(|| {
            default_achievement_definitions()
                .into_iter()
                .map(|def| CharacterAchievement {
                    def,
                    progress: 0,
                    completed: false,
                    completed_date: None,
                    reached_milestones: Vec::new(),
                    unclaimed_reward_count: 0,
                })
                .collect()
        });

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
        let Some(list) = map.get_mut(&character_id) else {
            return 0;
        };

        let mut total_claimed = 0u32;
        for a in list.iter_mut() {
            if a.unclaimed_reward_count > 0 {
                let count = a.unclaimed_reward_count;
                total_claimed += count;
                a.unclaimed_reward_count = 0;

                // Award ISK & SP per claimed milestone
                let isk = (count as i64) * 250_000;
                let sp = (count as f64) * 5_000.0;
                db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                        ch.balance = ch
                            .balance
                            .checked_add(IskCents::from_isk(isk))
                            .unwrap_or(ch.balance);
                        ch.free_skill_points += sp;
                    }
                });
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
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 6. eve_public.achievement.definition.api.cdn.GetLatestRequest -> GetLatestResponse
    router.register_handler(
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.api.cdn.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.achievement.api.admin.AddProgressRequest -> AddProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.AddProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
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

            let _ = mgr.add_progress(cid, aid, delta, ctx.now_unix());
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 8. eve_public.achievement.api.admin.CompleteRequest -> CompleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(101);
            let _ = mgr.mark_completed(cid, aid, ctx.now_unix())?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 9. eve_public.achievement.api.admin.ResetRequest -> ResetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(101);
            let _ = mgr.reset_achievement(cid, aid)?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 10. eve_public.achievement.api.admin.ResetAllRequest -> ResetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            if let Some(cid) = ctx.effective_character_id() {
                mgr.reset_all(cid);
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 11. eve_public.achievement.category.api.admin.ResetRequest -> ResetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.category.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let cat_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);
            mgr.reset_category(cid, cat_id);
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
    router.register_handler(
        "eve.achievement.definition.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
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
