//! `quasar_achievements` — V24.01 Quasar AIR Achievements & Milestones Service (`feat:quasar.achievement`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Achievements system:
//! - Category definitions: Combat, Exploration, Industry, Economy, Social.
//! - Per-character achievement tracking, milestone thresholds, score tallies.
//! - Milestone reward claim processing with live wallet/SP awards.
//! - Request handlers for `eve_public.achievement.api` and `eve.achievement.api`.
//! - Live notice publication (`ProgressedNotice`, `CompletedNotice`, `ReachedNotice`, `CheckVersionNotice`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_NOT_FOUND};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, sha256_hex, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::{HashMap, HashSet};
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
pub struct AchievementConditionDef {
    pub condition_id: u32,
    pub target_value: u64,
    pub statistic_event_id: u32,
    #[serde(default)]
    pub event_name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AchievementDef {
    pub id: u32,
    #[serde(default)]
    pub is_client_achievement: bool,
    pub category_id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub notification_text: String,
    pub target: u64,
    pub score: u32,
    pub milestone_targets: Vec<u64>,
    #[serde(default)]
    pub conditions: Vec<AchievementConditionDef>,
    #[serde(default, deserialize_with = "deserialize_i64_or_f64")]
    pub reward_isk: i64,
    #[serde(default, deserialize_with = "deserialize_i64_or_f64")]
    pub reward_sp: i64,
}

pub fn achievement_reward_uuid(achievement_id: u32, milestone_id: u32) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[0..4].copy_from_slice(b"ACHR");
    uuid[8..12].copy_from_slice(&achievement_id.to_be_bytes());
    uuid[12..16].copy_from_slice(&milestone_id.to_be_bytes());
    uuid
}

fn classify_achievement_category(
    id: u32,
    info: &eve_sde::AchievementInfo,
    conditions: &[AchievementConditionDef],
) -> u32 {
    let mut hay = info.name.to_ascii_lowercase();
    hay.push(' ');
    hay.push_str(&info.description.to_ascii_lowercase());
    for c in conditions {
        hay.push(' ');
        hay.push_str(&c.event_name.to_ascii_lowercase());
    }
    if hay.contains("kill") || hay.contains("damage") || hay.contains("combat") || hay.contains("destroy") || hay.contains("bounty") || hay.contains("weapon") {
        1
    } else if hay.contains("scan") || hay.contains("hack") || hay.contains("explor") || hay.contains("wormhole") || hay.contains("jump") || hay.contains("gate") || hay.contains("warp") {
        2
    } else if hay.contains("mine") || hay.contains("ore") || hay.contains("manufactur") || hay.contains("industr") || hay.contains("blueprint") || hay.contains("reprocess") {
        3
    } else if hay.contains("market") || hay.contains("trade") || hay.contains("contract") || hay.contains("wallet") || hay.contains("isk") || hay.contains("plex") {
        4
    } else if hay.contains("fleet") || hay.contains("corp") || hay.contains("mission") || hay.contains("loyalty") || hay.contains("social") || hay.contains("agent") {
        5
    } else {
        (id % 5) + 1
    }
}

fn build_definitions_from_sde(
    sde: &eve_sde::SdeRegistry,
) -> (Vec<AchievementDef>, HashSet<String>, usize, String, Vec<u8>) {
    let Ok(achs) = sde.load_achievements() else {
        let empty = b"[]".to_vec();
        return (Vec::new(), HashSet::new(), 0, sha256_hex(&empty)[..16].to_string(), empty);
    };
    let events = sde
        .load_cfsd_table("res:/staticdata/achievementevents.fsdbinary")
        .unwrap_or_default();
    let server_eids: HashSet<u32> = achs
        .values()
        .filter(|i| !i.is_client_achievement)
        .flat_map(|i| i.conditions.iter().map(|c| c.statistic_event_id))
        .collect();
    let client_eids: HashSet<u32> = achs
        .values()
        .filter(|i| i.is_enabled && i.is_client_achievement)
        .flat_map(|i| i.conditions.iter().map(|c| c.statistic_event_id))
        .collect();
    let mut allowed_client_keys = HashSet::new();
    for &eid in &client_eids {
        allowed_client_keys.insert(eid.to_string());
    }
    for (&eid_i64, row) in &events {
        let Ok(eid) = u32::try_from(eid_i64) else { continue };
        let name = row.str("name").unwrap_or("").trim();
        if client_eids.contains(&eid) || (name.starts_with("client_") && !server_eids.contains(&eid)) {
            allowed_client_keys.insert(eid.to_string());
            if !name.is_empty() {
                allowed_client_keys.insert(name.to_string());
            }
        }
    }

    let mut ids: Vec<u32> = achs.values().filter(|i| i.is_enabled).map(|i| i.achievement_id).collect();
    ids.sort_unstable();
    let count = ids.len();

    let mut defs = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(info) = achs.get(&id).filter(|info| info.is_enabled) else { continue };
        let conditions: Vec<AchievementConditionDef> = info
            .conditions
            .iter()
            .map(|c| {
                let event_name = events.get(&i64::from(c.statistic_event_id)).and_then(|r| r.str("name")).unwrap_or("").to_string();
                AchievementConditionDef {
                    condition_id: c.condition_id,
                    target_value: (c.target_value as u64).max(1),
                    statistic_event_id: c.statistic_event_id,
                    event_name,
                }
            })
            .collect();
        let target = if conditions.is_empty() {
            (info.target_value as u64).max(1)
        } else {
            conditions.iter().map(|c| c.target_value).sum::<u64>().max(1)
        };

        let category_id = classify_achievement_category(id, info, &conditions);
        let score = ((id % 5) + 1) * 10;
        let milestone_targets = if target >= 4 { vec![target / 2, target] } else { Vec::new() };
        let reward_isk = ((target.min(500)) as i64 * 10_000).clamp(100_000, 1_000_000);
        let reward_sp = ((target.min(500)) as i64 * 50).clamp(2_500, 10_000);
        defs.push(AchievementDef {
            id,
            is_client_achievement: info.is_client_achievement,
            category_id,
            name: info.name.clone(),
            description: info.description.clone(),
            notification_text: info.notification_text.clone(),
            target,
            score,
            milestone_targets,
            conditions,
            reward_isk,
            reward_sp,
        });
    }
    let json_bytes = serde_json::to_vec(&defs).unwrap_or_else(|_| b"[]".to_vec());
    let hash = sha256_hex(&json_bytes)[..16].to_string();
    (defs, allowed_client_keys, count, hash, json_bytes)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterAchievement {
    pub def: AchievementDef,
    pub progress: u64,
    #[serde(default)]
    pub condition_progress: HashMap<u32, u64>,
    pub completed: bool,
    pub completed_date: Option<u64>,
    pub reached_milestones: Vec<u32>,
    pub unclaimed_reward_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RewardClaimFilter {
    pub achievement_id: Option<u32>,
    pub category_id: Option<u32>,
    pub milestone_id: Option<u32>,
    pub selected_reward_uuids: Option<Vec<[u8; 16]>>,
}

pub struct AchievementManager {
    pub definitions: Vec<AchievementDef>,
    pub allowed_client_counter_keys: HashSet<String>,
    pub character_achievements: RwLock<HashMap<u32, Vec<CharacterAchievement>>>,
    pub category_scores: RwLock<HashMap<(u32, u32), u32>>,
    pub sde_achievement_count: usize,
    pub sde_achievement_hash: String,
    pub sde_definitions_json: Vec<u8>,
    db: Option<Arc<WorldRepository>>,
    sde: Option<Arc<eve_sde::SdeRegistry>>,
}

impl Default for AchievementManager {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AchievementManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        let default_sde = eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).ok().map(Arc::new);
        Self::new_with_sde(db, default_sde)
    }

    /// Instantiates AchievementManager from SDE (`res:/staticdata/achievements.static`,
    /// `res:/staticdata/achievementevents.fsdbinary`, and `res:/localizationfsd/localization_fsd_en-us.pickle`).
    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
    ) -> Self {
        let (defs, allowed_client_counter_keys, sde_achievement_count, sde_achievement_hash, sde_definitions_json) = sde
            .as_deref()
            .map(build_definitions_from_sde)
            .unwrap_or_else(|| {
                let empty = b"[]".to_vec();
                (Vec::new(), HashSet::new(), 0, sha256_hex(&empty)[..16].to_string(), empty)
            });

        let defs_by_id: HashMap<u32, &AchievementDef> = defs.iter().map(|d| (d.id, d)).collect();

        let mut loaded = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs
                        .get("quasar_achievements")
                        .and_then(|json| serde_json::from_str::<HashMap<u32, Vec<CharacterAchievement>>>(json).ok())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();

        if !defs_by_id.is_empty() {
            for list in loaded.values_mut() {
                for a in list.iter_mut() {
                    if let Some(fresh_def) = defs_by_id.get(&a.def.id) {
                        a.def = (*fresh_def).clone();
                    }
                }
            }
        }

        let loaded_cat_scores = db
            .as_ref()
            .map(|d| {
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
            })
            .unwrap_or_default();

        Self {
            definitions: defs,
            allowed_client_counter_keys,
            character_achievements: RwLock::new(loaded),
            category_scores: RwLock::new(loaded_cat_scores),
            sde_achievement_count,
            sde_achievement_hash,
            sde_definitions_json,
            db,
            sde,
        }
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

    fn init_list<'a>(
        defs: &[AchievementDef],
        map: &'a mut HashMap<u32, Vec<CharacterAchievement>>,
        character_id: u32,
    ) -> &'a mut Vec<CharacterAchievement> {
        map.entry(character_id).or_insert_with(|| {
            defs.iter()
                .cloned()
                .map(|def| CharacterAchievement {
                    def,
                    progress: 0,
                    condition_progress: HashMap::new(),
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
                let mut rem = a.progress;
                for cond in &a.def.conditions {
                    let take = rem.min(cond.target_value);
                    a.condition_progress.insert(cond.condition_id, take);
                    rem = rem.saturating_sub(take);
                }

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

    /// Advances all achievements listening to the given SDE `statistic_event_id`
    /// (`res:/staticdata/achievementevents.fsdbinary`), clamping each condition to its
    /// `target_value` so multi-condition achievements (e.g. achievement 60 requiring both
    /// `zoomIn` 138 and `zoomOut` 124) require every condition to be satisfied.
    pub fn is_client_achievement_id(&self, id: u32) -> bool {
        self.definitions.iter().any(|d| d.id == id && d.is_client_achievement)
    }

    pub fn is_client_allowed_counter_key(&self, key: &str) -> bool {
        !key.is_empty() && self.allowed_client_counter_keys.contains(key)
    }

    pub fn record_statistic_event(
        &self,
        character_id: u32,
        statistic_event_id: u32,
        delta: u64,
        now_unix: u64,
    ) -> Vec<(CharacterAchievement, bool, Vec<u32>)> {
        self.record_statistic_event_filtered(character_id, statistic_event_id, delta, now_unix, false)
    }

    fn record_statistic_event_filtered(
        &self,
        character_id: u32,
        statistic_event_id: u32,
        delta: u64,
        now_unix: u64,
        client_only: bool,
    ) -> Vec<(CharacterAchievement, bool, Vec<u32>)> {
        let mut map = self.character_achievements.write().unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);
        let mut updates = Vec::new();

        for a in list.iter_mut() {
            if client_only && !a.def.is_client_achievement {
                continue;
            }
            let matching_conds: Vec<(u32, u64)> = a
                .def
                .conditions
                .iter()
                .filter(|c| c.statistic_event_id == statistic_event_id)
                .map(|c| (c.condition_id, c.target_value))
                .collect();
            if matching_conds.is_empty() {
                continue;
            }
            for (cid, c_target) in matching_conds {
                let entry = a.condition_progress.entry(cid).or_insert(0);
                *entry = entry.saturating_add(delta).min(c_target);
            }
            let total_cond_progress: u64 = a
                .def
                .conditions
                .iter()
                .map(|c| a.condition_progress.get(&c.condition_id).copied().unwrap_or(0).min(c.target_value))
                .sum();
            let prev_completed = a.completed;
            a.progress = a.progress.max(total_cond_progress);

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
            updates.push((a.clone(), !prev_completed && a.completed, new_milestones));
        }
        updates
    }

    pub fn on_gameplay_event_by_name(
        &self,
        character_id: u32,
        event_names: &[&str],
        delta: u64,
        now_unix: u64,
        gateway: &PublicGatewayRouter,
    ) {
        if delta == 0 || event_names.is_empty() {
            return;
        }
        if let Some(ref db) = self.db {
            crate::services::quasar_goal_tale_notices::record_server_opportunity_event(db, &self.definitions, character_id, event_names, delta, now_unix);
            if let Some(ref sde) = self.sde {
                crate::services::operations_manager_helpers::advance_and_notify_server_operation_tasks(db, sde, gateway, i64::from(character_id), event_names, &[], delta, now_unix);
            }
        }
        let mut event_ids: Vec<u32> = self
            .definitions
            .iter()
            .flat_map(|d| d.conditions.iter())
            .filter(|c| event_names.contains(&c.event_name.as_str()))
            .map(|c| c.statistic_event_id)
            .collect();
        event_ids.sort_unstable();
        event_ids.dedup();
        self.apply_statistic_event_ids(character_id, &event_ids, delta, now_unix, false, gateway);
    }

    pub fn on_client_counter_event(
        &self,
        character_id: u32,
        key: &str,
        delta: u64,
        now_unix: u64,
        gateway: &PublicGatewayRouter,
    ) {
        if delta == 0 {
            return;
        }
        let parsed_id = key.parse::<u32>().ok();
        let mut event_ids: Vec<u32> = self
            .definitions
            .iter()
            .filter(|d| d.is_client_achievement)
            .flat_map(|d| d.conditions.iter())
            .filter(|c| (!c.event_name.is_empty() && c.event_name == key) || parsed_id == Some(c.statistic_event_id))
            .map(|c| c.statistic_event_id)
            .collect();
        event_ids.sort_unstable();
        event_ids.dedup();
        self.apply_statistic_event_ids(character_id, &event_ids, delta, now_unix, true, gateway);
    }

    pub fn apply_client_task_completions_and_counters(
        &self,
        char_id: u32,
        client_ids: &[u32],
        counter_deltas: &[(String, u64)],
        now_unix: u64,
        gateway: &PublicGatewayRouter,
    ) {
        let mut any_marked = false;
        for &aid in client_ids {
            if self.is_client_achievement_id(aid) {
                if let Ok(a) = self.mark_completed(char_id, aid, now_unix) {
                    any_marked = true;
                    emit_achievement_notices(char_id, a.def.id, a.def.category_id, a.progress, 0, true, &[], now_unix);
                }
            }
        }
        if any_marked {
            self.save_to_db();
        }
        for (k, delta) in counter_deltas {
            self.on_client_counter_event(char_id, k, *delta, now_unix, gateway);
        }
        for (scope, payload, ts) in crate::services::quasar_social_chat::drain_local_notices() {
            let _ = gateway.publish_notice(scope, payload, ts);
        }
    }

    fn apply_statistic_event_ids(
        &self,
        character_id: u32,
        event_ids: &[u32],
        delta: u64,
        now_unix: u64,
        client_only: bool,
        gateway: &PublicGatewayRouter,
    ) {
        let mut any_changed = false;
        for &eid in event_ids {
            let updates = self.record_statistic_event_filtered(character_id, eid, delta, now_unix, client_only);
            for (a, just_completed, milestones) in updates {
                any_changed = true;
                let cat_score = if a.def.category_id > 0 { self.category_score(character_id, a.def.category_id) } else { 0 };
                emit_achievement_notices(character_id, a.def.id, a.def.category_id, a.progress, cat_score, just_completed, &milestones, now_unix);
            }
        }
        if any_changed {
            self.save_to_db();
        }
        for (scope, payload, ts) in crate::services::quasar_social_chat::drain_local_notices() {
            let _ = gateway.publish_notice(scope, payload, ts);
        }
    }

    pub fn mark_completed(&self, character_id: u32, achievement_id: u32, now_unix: u64) -> Result<CharacterAchievement, (u32, String)> {
        let mut map = self.character_achievements.write().unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);
        for a in list.iter_mut() {
            if a.def.id == achievement_id {
                a.progress = a.def.target;
                for cond in &a.def.conditions {
                    a.condition_progress.insert(cond.condition_id, cond.target_value);
                }
                a.completed = true;
                a.completed_date = Some(now_unix);
                for (idx, _) in a.def.milestone_targets.iter().enumerate() {
                    let m_id = (idx + 1) as u32;
                    if !a.reached_milestones.contains(&m_id) {
                        a.reached_milestones.push(m_id);
                        a.unclaimed_reward_count += 1;
                    }
                }
                if a.def.milestone_targets.is_empty()
                    && (a.def.reward_isk > 0 || a.def.reward_sp > 0)
                    && !a.reached_milestones.contains(&1)
                {
                    a.reached_milestones.push(1);
                    a.unclaimed_reward_count += 1;
                }
                return Ok(a.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Achievement not found".into()))
    }

    pub fn reset_achievement(&self, character_id: u32, achievement_id: u32) -> Result<CharacterAchievement, (u32, String)> {
        let mut map = self.character_achievements.write().unwrap_or_else(|e| e.into_inner());
        let list = Self::init_list(&self.definitions, &mut map, character_id);
        for a in list.iter_mut() {
            if a.def.id == achievement_id {
                a.progress = 0;
                a.condition_progress.clear();
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
        let mut map = self.character_achievements.write().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get_mut(&character_id) {
            for a in list.iter_mut().filter(|a| a.def.category_id == category_id) {
                a.progress = 0;
                a.condition_progress.clear();
                a.completed = false;
                a.completed_date = None;
                a.reached_milestones.clear();
                a.unclaimed_reward_count = 0;
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
                a.condition_progress.clear();
                a.completed = false;
                a.completed_date = None;
                a.reached_milestones.clear();
                a.unclaimed_reward_count = 0;
            }
        }
        drop(map);
        self.save_to_db();
    }

    pub fn clear_character(&self, character_id: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).retain(|&(cid, _), _| cid != character_id);
        self.character_achievements.write().unwrap_or_else(|e| e.into_inner()).remove(&character_id);
        self.save_to_db();
    }

    pub fn claim_rewards(&self, character_id: u32, db: &WorldRepository) -> u32 {
        self.claim_rewards_filtered(character_id, db, &RewardClaimFilter::default())
    }

    pub fn claim_rewards_filtered(
        &self,
        character_id: u32,
        db: &WorldRepository,
        filter: &RewardClaimFilter,
    ) -> u32 {
        let mut map = self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut total_claimed = 0u32;
        let mut total_reward_isk = 0i64;
        let mut total_reward_sp = 0.0f64;
        let mut taken: Vec<(u32, u32)> = Vec::new();
        {
            let Some(list) = map.get_mut(&character_id) else {
                return 0;
            };

            for a in list.iter_mut() {
                if a.unclaimed_reward_count == 0 {
                    continue;
                }
                if let Some(req_aid) = filter.achievement_id {
                    if a.def.id != req_aid {
                        continue;
                    }
                }
                if let Some(req_cid) = filter.category_id {
                    if a.def.category_id != req_cid {
                        continue;
                    }
                }
                let unclaimed_ms: Vec<u32> = if !a.reached_milestones.is_empty() {
                    a.reached_milestones
                        .iter()
                        .rev()
                        .take(a.unclaimed_reward_count as usize)
                        .copied()
                        .rev()
                        .collect()
                } else {
                    (1..=a.unclaimed_reward_count).collect()
                };
                let matched_count = unclaimed_ms
                    .iter()
                    .filter(|&&m_id| {
                        let ms_ok = filter.milestone_id.is_none_or(|req_mid| req_mid == m_id);
                        let sel_ok = filter.selected_reward_uuids.as_ref().is_none_or(|uuids| {
                            let target_uuid = achievement_reward_uuid(a.def.id, m_id);
                            uuids.iter().any(|u| *u == target_uuid)
                        });
                        ms_ok && sel_ok
                    })
                    .count() as u32;
                let count = matched_count.min(a.unclaimed_reward_count);
                if count > 0 {
                    total_claimed += count;
                    total_reward_isk = total_reward_isk
                        .saturating_add(a.def.reward_isk.max(0).saturating_mul(i64::from(count)));
                    total_reward_sp += (a.def.reward_sp.max(0) as f64) * f64::from(count);
                    taken.push((a.def.id, count));
                    a.unclaimed_reward_count -= count;
                }
            }
        }

        if total_claimed > 0 {
            let Ok(json_state) = serde_json::to_string(&*map) else {
                if let Some(list) = map.get_mut(&character_id) {
                    for (ach_id, count) in taken {
                        if let Some(a) = list.iter_mut().find(|a| a.def.id == ach_id) {
                            a.unclaimed_reward_count = a.unclaimed_reward_count.saturating_add(count);
                        }
                    }
                }
                return 0;
            };

            let persisted = db
                .try_with_mut_snapshot(|snap| -> Result<(), ()> {
                    snap.quasar_state_blobs
                        .insert(ACHIEVEMENTS_STATE_KEY.to_string(), json_state);
                    if total_reward_isk > 0 || total_reward_sp > 0.0 {
                        let journal_info = snap.characters.get_mut(&(character_id as i64)).map(|ch| {
                            let delta = IskCents::from_isk(total_reward_isk);
                            let new_bal = IskCents(ch.balance.0.saturating_add(delta.0));
                            if total_reward_isk > 0 {
                                ch.balance = new_bal;
                            }
                            if total_reward_sp > 0.0 {
                                ch.free_skill_points += total_reward_sp;
                            }
                            (delta, new_bal)
                        });
                        if let Some((delta, new_bal)) = journal_info {
                            if total_reward_isk > 0 {
                                snap.append_journal(
                                    character_id as i64,
                                    "AIRAchievementReward",
                                    delta,
                                    new_bal,
                                    format!("AIR Achievement reward ({total_claimed}x)"),
                                );
                            }
                        }
                    }
                    Ok(())
                })
                .is_ok();

            if !persisted {
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

pub(crate) fn extract_u32_ident(val: &DynamicValue) -> Option<u32> {
    match val {
        DynamicValue::Uint32(u) => Some(*u),
        DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
        DynamicValue::Int32(i) => u32::try_from(*i).ok(),
        DynamicValue::Int64(i) => u32::try_from(*i).ok(),
        DynamicValue::Message(m) => m.get(1).and_then(|inner| match inner {
            DynamicValue::Uint32(u) => Some(*u),
            DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
            DynamicValue::Int32(i) => u32::try_from(*i).ok(),
            DynamicValue::Int64(i) => u32::try_from(*i).ok(),
            DynamicValue::Bytes(b) if b.len() >= 4 => {
                let mut arr = [0u8; 4];
                arr.copy_from_slice(&b[b.len() - 4..]);
                Some(u32::from_be_bytes(arr))
            }
            _ => None,
        }),
        DynamicValue::Bytes(b) if b.len() >= 4 => {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(&b[b.len() - 4..]);
            Some(u32::from_be_bytes(arr))
        }
        _ => None,
    }
}

pub(crate) fn extract_u64_val(val: &DynamicValue) -> Option<u64> {
    match val {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(u64::from(*u)),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        _ => None,
    }
}

pub use super::quasar_achievements_ops::{
    publish_achievement_category_milestone_notice, publish_achievement_category_progress_notice,
    publish_achievement_milestone_reached_notice, publish_achievement_version_check_notice,
    register_achievement_handlers,
};

/// Minimal valid FlatBuffer for `AchievementData` (`achievements` / `character_titles` disk cache):
/// - Root table (`Pos = 16`) with `slot0 = 0`, `slot1 = 4` (`Challenges`), `slot2 = 8` (`TimeLimited`).
/// - Both `Challenges` and `TimeLimited` point to `Category` tables whose `Id` (`slot0`) is a valid
///   UUID string (`00000000-0000-0000-0000-000000000001` and `...0002`) and all vector offsets are `0`.
pub(crate) fn empty_achievement_flatbuffer() -> Vec<u8> {
    let mut buf = Vec::with_capacity(140);
    // 0..4: root table offset = 16
    buf.extend_from_slice(&16u32.to_le_bytes());
    // 4..16: root vtable (vtable_len = 10, table_len = 12, slot0 = 0, slot1 = 4, slot2 = 8, pad = 0)
    for w in [10u16, 12, 0, 4, 8, 0] {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    // 16..28: root table (soffset = 12 -> vtable at 4; slot1 uoffset = 16 -> table at 36; slot2 uoffset = 20 -> table at 44)
    buf.extend_from_slice(&12i32.to_le_bytes());
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&20u32.to_le_bytes());
    // 28..36: Category vtable (vtable_len = 6, table_len = 8, slot0 = 4, pad = 0)
    for w in [6u16, 8, 4, 0] {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    // 36..44: Challenges Category table (soffset = 8 -> vtable at 28; slot0 uoffset = 12 -> string at 52)
    buf.extend_from_slice(&8i32.to_le_bytes());
    buf.extend_from_slice(&12u32.to_le_bytes());
    // 44..52: TimeLimited Category table (soffset = 16 -> vtable at 28; slot0 uoffset = 48 -> string at 96)
    buf.extend_from_slice(&16i32.to_le_bytes());
    buf.extend_from_slice(&48u32.to_le_bytes());
    // 52..96: UUID string 1 (36 bytes + 4 zero pad bytes)
    let u1 = b"00000000-0000-0000-0000-000000000001";
    buf.extend_from_slice(&(u1.len() as u32).to_le_bytes());
    buf.extend_from_slice(u1);
    buf.extend_from_slice(&[0u8; 4]);
    // 96..140: UUID string 2 (36 bytes + 4 zero pad bytes)
    let u2 = b"00000000-0000-0000-0000-000000000002";
    buf.extend_from_slice(&(u2.len() as u32).to_le_bytes());
    buf.extend_from_slice(u2);
    buf.extend_from_slice(&[0u8; 4]);
    buf
}

/// Minimal valid FlatBuffer root table with all field offsets `0` (for `content_map_tags` `Universe`).
pub(crate) fn empty_root_table_flatbuffer() -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(&12u32.to_le_bytes());
    for w in [4u16, 4, 0, 0] {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.extend_from_slice(&8i32.to_le_bytes());
    buf
}

/// Ensures `version.txt` and `data.bin` exist in any local EVE client `cache/Storage/<content_id>/`
/// directory so `VersionCachedFlatbufferRepository._request_update_task` can read `_disk_storage.get()`
/// when `FlatbufferContentProvider.fetch` returns `[None, None]` on first run without an external TLS CDN.
pub(crate) fn ensure_client_flatbuffer_storage_seeded(
    content_id: &str,
    version_str: &str,
    data_bytes: &[u8],
) {
    let mut candidate_roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidate_roots.push(
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("CCP")
                .join("EVE"),
        );
    }
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        candidate_roots.push(std::path::PathBuf::from(local_appdata).join("CCP").join("EVE"));
    }
    for eve_root in candidate_roots {
        let Ok(entries) = std::fs::read_dir(&eve_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let storage_root = entry.path().join("cache").join("Storage");
            if !storage_root.is_dir() {
                continue;
            }
            let content_dir = storage_root.join(content_id);
            let _ = std::fs::create_dir_all(&content_dir);
            let data_path = content_dir.join("data.bin");
            let ver_path = content_dir.join("version.txt");
            let needs_seed = std::fs::metadata(&data_path)
                .map(|m| m.len() == 0)
                .unwrap_or(true);
            if needs_seed {
                let _ = std::fs::write(&data_path, data_bytes);
                let _ = std::fs::write(&ver_path, version_str.as_bytes());
            }
        }
    }
}

#[cfg(test)] #[path = "quasar_achievements_tests.rs"]
mod quasar_achievements_tests;

#[path = "quasar_achievements_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_achievements_cov_unit_tests;

