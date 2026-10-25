//! `quasar_achievements` — V24.01 Quasar AIR Achievements & Milestones Service (`feat:quasar.achievement`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Achievements system:
//! - Category definitions: Combat, Exploration, Industry, Economy, Social.
//! - Per-character achievement tracking, milestone thresholds, score tallies.
//! - Milestone reward claim processing with live wallet/SP awards.
//! - Request handlers for `eve_public.achievement.api` and `eve.achievement.api`.
//! - Live notice publication (`ProgressedNotice`, `CompletedNotice`, `ReachedNotice`, `CheckVersionNotice`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
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

/// DERIVED: Authentic AIR achievement definitions based on the 68 SDE achievements
/// (res:/staticdata/achievements.static). Category bucketing (1..=10: Combat, 11..=20: Exploration,
/// 21..=40: Industry, 41..=60: Career, _ => Advanced), completion targets, score values,
/// milestone tiers, and rewards are synthesized server-side per AGENTS.md rules.
pub fn default_achievement_definitions() -> Vec<AchievementDef> {
    const SDE_ACHIEVEMENT_IDS: &[u32] = &[
        2, 3, 4, 9, 10, 12, 13, 14, 15, 16, 17, 18, 19, 20, 23, 24, 25, 26, 27, 28,
        29, 30, 31, 32, 36, 37, 38, 39, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51,
        52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 70, 71, 72, 73,
        74, 75, 76, 77, 78, 79, 81, 93, 94, 102,
    ];
    SDE_ACHIEVEMENT_IDS
        .iter()
        .map(|&id| {
            let cat_id = match id {
                1..=10 => 1,
                11..=20 => 2,
                21..=40 => 3,
                41..=60 => 4,
                _ => 5,
            };
            AchievementDef {
                id,
                category_id: cat_id,
                name: format!("AIR Achievement #{id}"),
                target: 10,
                score: 100,
                milestone_targets: vec![1, 5, 10],
                reward_isk: 100_000,
                reward_sp: 2_500,
            }
        })
        .collect()
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
    pub category_scores: RwLock<HashMap<(u32, u32), u32>>,
    pub sde_achievement_count: usize,
    pub sde_achievement_hash: String,
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
        let loaded_cat_scores = db.as_ref().map(|d| {
            d.with_snapshot(|s| {
                s.quasar_state_blobs
                    .get("quasar_achievement_cat_scores")
                    .and_then(|json| serde_json::from_str::<HashMap<String, u32>>(json).ok())
                    .map(|map| {
                        map.into_iter()
                            .filter_map(|(k, v)| {
                                let mut parts = k.split(':');
                                let c = parts.next()?.parse::<u32>().ok()?;
                                let cat = parts.next()?.parse::<u32>().ok()?;
                                Some(((c, cat), v))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
        }).unwrap_or_default();

        Self {
            definitions: defs,
            character_achievements: RwLock::new(loaded),
            category_scores: RwLock::new(loaded_cat_scores),
            sde_achievement_count: 68,
            sde_achievement_hash: "6b74c14cac4e3001".to_string(),
            db,
        }
    }

    /// DERIVED: Instantiates AchievementManager from SDE, strictly populating only the 68 SDE
    /// achievement definitions (res:/staticdata/achievements.static). Category bucketing,
    /// completion targets, score values, milestone tiers, and ISK/SP rewards are derived/synthesized
    /// server-side per AGENTS.md provenance rules.
    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
    ) -> Self {
        let mut mgr = Self::new(db);
        if let Some(sde) = sde {
            if let Ok(achs) = sde.load_achievements() {
                // Clear any synthetic definitions to strictly populate only the 68 SDE achievement definitions
                mgr.definitions.clear();
                mgr.sde_achievement_count = achs.len();
                let mut ids: Vec<u32> = achs.keys().copied().collect();
                ids.sort();
                mgr.sde_achievement_hash = sha256_hex(&serde_json::to_vec(&ids).unwrap_or_default())[..16].to_string();

                // DERIVED: Category bucketing, completion targets, score values, milestone tiers,
                // and ISK/SP reward amounts for the 68 SDE achievements are derived/synthesized
                // server-side per AGENTS.md provenance rules, mapping SDE achievement IDs
                // (res:/staticdata/achievements.static) to canonical career categories and reward tiers.
                for &id in &ids {
                    let cat_id = match id {
                        1..=10 => 1,
                        11..=20 => 2,
                        21..=40 => 3,
                        41..=60 => 4,
                        _ => 5,
                    };
                    mgr.definitions.push(AchievementDef {
                        id,
                        category_id: cat_id,
                        name: format!("AIR Achievement #{id}"),
                        target: 10,
                        score: 100,
                        milestone_targets: vec![1, 5, 10],
                        reward_isk: 100_000,
                        reward_sp: 2_500,
                    });
                }
            }
        }
        mgr
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let map = self.character_achievements.read().unwrap_or_else(|e| e.into_inner());
            let cat_scores = self.category_scores.read().unwrap_or_else(|e| e.into_inner());
            let cat_map: HashMap<String, u32> = cat_scores.iter().map(|((c, cat), v)| (format!("{c}:{cat}"), *v)).collect();
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Ok(json) = serde_json::to_string(&*map) {
                    snap.quasar_state_blobs.insert("quasar_achievements".into(), json);
                }
                if let Ok(cat_json) = serde_json::to_string(&cat_map) {
                    snap.quasar_state_blobs.insert("quasar_achievement_cat_scores".into(), cat_json);
                }
                Ok::<(), String>(())
            });
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

    pub fn reset_category(&self, character_id: u32, category_id: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).remove(&(character_id, category_id));
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
        drop(map);
        self.save_to_db();
    }

    pub fn set_category_score(&self, character_id: u32, category_id: u32, score: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).insert((character_id, category_id), score);
        self.save_to_db();
    }

    pub fn reset_all(&self, character_id: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).retain(|&(cid, _), _| cid != character_id);
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
        drop(map);
        self.save_to_db();
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
        let cat_scores = self.category_scores.read().unwrap_or_else(|e| e.into_inner());
        let mut total: u32 = cat_scores.iter().filter(|&(&(cid, _), _)| cid == character_id).map(|(_, &v)| v).sum();
        let map = self
            .character_achievements
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get(&character_id) {
            for a in list.iter().filter(|a| a.completed) {
                if !cat_scores.contains_key(&(character_id, a.def.category_id)) {
                    total += a.def.score;
                }
            }
        }
        total
    }

    pub fn category_score(&self, character_id: u32, category_id: u32) -> u32 {
        if let Some(&score) = self.category_scores.read().unwrap_or_else(|e| e.into_inner()).get(&(character_id, category_id)) {
            return score;
        }
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

pub(crate) fn encode_uuid_ident(field_no: u32, id: u32, out: &mut Vec<u8>) {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    let mut ident = Vec::new();
    encode_bytes_field(1, &uuid, &mut ident);
    encode_bytes_field(field_no, &ident, out);
}

pub(crate) fn emit_achievement_notices(
    cid: u32,
    aid: u32,
    cat_id: u32,
    progress: u64,
    cat_score: u32,
    completed: bool,
    milestones: &[u32],
    now: u64,
) {
    let mut not_bytes = Vec::new();
    encode_uuid_ident(1, aid, &mut not_bytes);
    encode_varint_field(2, progress, &mut not_bytes);
    super::quasar_social_chat::ctx_queue_notice(
        crate::quasar_gateway::NoticeScope::Character(cid),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.ProgressedNotice"),
            value: not_bytes,
        },
        now,
    );
    if completed {
        let mut comp_bytes = Vec::new();
        encode_uuid_ident(1, aid, &mut comp_bytes);
        super::quasar_social_chat::ctx_queue_notice(
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.CompletedNotice"),
                value: comp_bytes,
            },
            now,
        );
    }
    for m_id in milestones {
        let mut ms_bytes = Vec::new();
        encode_uuid_ident(1, aid, &mut ms_bytes);
        encode_uuid_ident(2, *m_id, &mut ms_bytes);
        super::quasar_social_chat::ctx_queue_notice(
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.milestone.api.ReachedNotice"),
                value: ms_bytes,
            },
            now,
        );

        if cat_id > 0 {
            let mut cat_ms_data = Vec::new();
            encode_uuid_ident(1, cat_id, &mut cat_ms_data);
            encode_uuid_ident(2, *m_id, &mut cat_ms_data);
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.milestone.api.ReachedNotice"),
                    value: cat_ms_data,
                },
                now,
            );
        }
    }

    if cat_id > 0 {
        let mut cat_prog = Vec::new();
        encode_uuid_ident(1, cat_id, &mut cat_prog);
        encode_varint_field(2, cat_score as u64, &mut cat_prog);
        super::quasar_social_chat::ctx_queue_notice(
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.ProgressedNotice"),
                value: cat_prog,
            },
            now,
        );
    }
}

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
                Some(_) => return Err((STATUS_BAD_REQUEST, "Invalid achievement ID".into())),
                None => return Err((STATUS_BAD_REQUEST, "Missing achievement ID".into())),
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
                encode_uuid_ident(1, *m_id, &mut rm);
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
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing category identifier".into()))?;
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
    let mgr_sc = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_cid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|id| *id > 0)
                .unwrap_or(cid);

            let score = mgr_sc.total_character_score(target_cid);
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

    // 6. eve_public.achievement.definition.api.cdn.GetLatestRequest -> GetLatestResponse
    let mgr_cdn = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let count = mgr_cdn.sde_achievement_count;
            let hash = &mgr_cdn.sde_achievement_hash;
            let url = format!("https://resources.eveonline.com/achievements/v24.1.{count}/{hash}.json");

            let mut checkpoint = Vec::new();
            encode_string_field(1, &url, &mut checkpoint);

            let mut spec = Vec::new();
            encode_varint_field(1, 24, &mut spec);
            encode_varint_field(2, 1, &mut spec);
            encode_varint_field(3, count as u64, &mut spec);
            encode_bytes_field(2, &spec, &mut checkpoint);

            let crc_val = crate::router::cache_notifications::crc32_ieee(hash.as_bytes()) as u32;
            let mut crc_msg = Vec::new();
            encode_varint_field(1, crc_val as u64, &mut crc_msg);
            encode_bytes_field(3, &crc_msg, &mut checkpoint);

            let mut out = Vec::new();
            encode_bytes_field(1, &checkpoint, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.api.cdn.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 7-11. Admin handlers (AddProgress, Complete, Reset, ResetAll, Category Reset)
    super::quasar_achievements_ops::register_achievement_admin_handlers(router, Arc::clone(&manager));

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
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing category identifier".into()))?;
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
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_cid = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|id| *id > 0)
                .unwrap_or(cid);
            let score = mgr.total_character_score(target_cid);
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

pub use super::quasar_achievements_ops::{
    publish_achievement_category_milestone_notice, publish_achievement_category_progress_notice,
    publish_achievement_milestone_reached_notice, publish_achievement_version_check_notice,
};
