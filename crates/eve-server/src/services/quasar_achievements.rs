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
use eve_proto::pb;
use eve_proto::quasar::{sha256_hex, DynamicValue, QuasarDescriptorRegistry};
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CategoryMilestoneDef {
    pub id: u32,
    pub target_score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReachedCategoryMilestone {
    pub milestone_id: u32,
    pub reached_date: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct AchievementUpdateOutcome {
    pub achievement: CharacterAchievement,
    pub just_completed: bool,
    pub new_ach_milestones: Vec<u32>,
    pub cat_score: u32,
    pub cat_score_changed: bool,
    pub new_cat_milestones: Vec<u32>,
}

pub fn achievement_reward_uuid(achievement_id: u32, milestone_id: u32) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[0..4].copy_from_slice(b"ACHR");
    uuid[8..12].copy_from_slice(&achievement_id.to_be_bytes());
    uuid[12..16].copy_from_slice(&milestone_id.to_be_bytes());
    uuid
}

/// DERIVED server category bucketing (`1=combat, 2=exploration, 3=industry, 4=economy/goals, 5=social`):
/// `achievements.static` omits server-authored CDN category trees, so each achievement's primary
/// statistic event deterministically selects a non-zero category bucket so category APIs (`GetRequest`,
/// `category.api.admin.ResetRequest`) operate independently per category.
fn derived_achievement_category_id(
    primary_event_id: u32,
    conditions: &[AchievementConditionDef],
) -> u32 {
    let eid = if primary_event_id != 0 {
        primary_event_id
    } else {
        conditions.first().map_or(0, |c| c.statistic_event_id)
    };
    match eid {
        // 1: Combat
        1 | 18 | 102 | 103 | 112 | 113 | 115 => 1,
        // 2: Exploration & Navigation
        15 | 34 | 74 | 90 | 95 | 110 | 123 | 124 | 127 | 128 | 129 | 138 | 139 | 142 | 146 | 149
        | 150 | 151 | 152 | 153 => 2,
        // 3: Industry, Mining, Reprocessing & Salvage
        59 | 86 | 104 | 107 | 116 | 118 | 119 | 120 | 125 | 145 | 148 => 3,
        // 4: Goals, Missions, Market, Fitting & Skills
        11 | 45 | 92 | 93 | 96 | 97 | 99 | 106 | 117 | 122 | 130 | 135 | 136 | 137 | 154 => 4,
        // 5: Social, Contacts, Chat & Corporations
        101 | 126 | 131 | 140 | 147 => 5,
        _ => {
            let name = conditions
                .first()
                .map_or("", |c| c.event_name.as_str())
                .to_ascii_lowercase();
            if name.contains("hostile") || name.contains("gun") || name.contains("combat") || name.contains("loss") {
                1
            } else if name.contains("probe") || name.contains("wormhole") || name.contains("warp") || name.contains("jump") || name.contains("dock") || name.contains("map") {
                2
            } else if name.contains("mine") || name.contains("asteroid") || name.contains("industry") || name.contains("refine") || name.contains("salvage") || name.contains("blueprint") || name.contains("loot") {
                3
            } else if name.contains("chat") || name.contains("convo") || name.contains("corp") || name.contains("contact") {
                5
            } else {
                4
            }
        }
    }
}

fn build_definitions_from_sde(
    sde: &eve_sde::SdeRegistry,
) -> (Vec<AchievementDef>, HashSet<String>, usize, String, Vec<u8>, Vec<u8>) {
    // Prefer V24 SQLite SDE tables (`sde_achievements` + `sde_achievement_events`); fall back to
    // reading `res:/staticdata/achievements.static` directly only for synthetic in-memory registries.
    let static_rows: Vec<eve_sde::AchievementStaticDef> = if !sde.achievements_static.is_empty() {
        sde.achievements_static.clone()
    } else if let Ok(achs) = sde.load_achievements() {
        let cfsd_events = sde
            .load_cfsd_table("res:/staticdata/achievementevents.fsdbinary")
            .unwrap_or_default();
        achs.into_values()
            .map(|info| {
                let simple_conditions = info
                    .conditions
                    .iter()
                    .map(|c| eve_sde::AchievementConditionDef {
                        condition_index: c.condition_id,
                        statistic: c.statistic_event_id,
                        target_value: (c.target_value as u64).max(1),
                    })
                    .collect();
                let primary_event_id = info
                    .conditions
                    .first()
                    .map_or(0, |c| c.statistic_event_id);
                let _ = &cfsd_events;
                eve_sde::AchievementStaticDef {
                    achievement_id: info.achievement_id,
                    name_id: info.name_id,
                    name: info.name,
                    description_id: info.description_id,
                    description: info.description,
                    notification_text_id: info.notification_text_id,
                    notification_text: info.notification_text,
                    is_client_achievement: info.is_client_achievement,
                    is_enabled: info.is_enabled,
                    simple_conditions,
                    target_value: info.target_value as u64,
                    primary_event_id,
                }
            })
            .collect()
    } else {
        let empty = b"[]".to_vec();
        let empty_fb = super::quasar_achievements_flatbuffer::encode_achievement_data_flatbuffer(
            &[],
            &HashMap::new(),
            &sde.achievement_category_labels,
        );
        return (
            Vec::new(),
            HashSet::new(),
            0,
            sha256_hex(&empty)[..16].to_string(),
            empty,
            empty_fb,
        );
    };

    let event_names: HashMap<u32, String> = if !sde.achievement_events_static.is_empty() {
        sde.achievement_events_static
            .iter()
            .map(|(&eid, ev)| (eid, ev.name.clone()))
            .collect()
    } else {
        sde.load_cfsd_table("res:/staticdata/achievementevents.fsdbinary")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(eid_i64, row)| {
                let eid = u32::try_from(eid_i64).ok()?;
                let name = row.str("name").unwrap_or("").trim().to_string();
                Some((eid, name))
            })
            .collect()
    };

    let server_eids: HashSet<u32> = static_rows
        .iter()
        .filter(|i| !i.is_client_achievement)
        .flat_map(|i| i.simple_conditions.iter().map(|c| c.statistic))
        .collect();
    let client_eids: HashSet<u32> = static_rows
        .iter()
        .filter(|i| i.is_enabled && i.is_client_achievement)
        .flat_map(|i| i.simple_conditions.iter().map(|c| c.statistic))
        .collect();
    let mut allowed_client_keys = HashSet::new();
    for &eid in &client_eids {
        allowed_client_keys.insert(eid.to_string());
    }
    for (&eid, name) in &event_names {
        let trimmed = name.trim();
        if client_eids.contains(&eid) || (trimmed.starts_with("client_") && !server_eids.contains(&eid)) {
            allowed_client_keys.insert(eid.to_string());
            if !trimmed.is_empty() {
                allowed_client_keys.insert(trimmed.to_string());
            }
        }
    }

    let mut enabled_rows: Vec<&eve_sde::AchievementStaticDef> =
        static_rows.iter().filter(|i| i.is_enabled).collect();
    enabled_rows.sort_by_key(|i| i.achievement_id);
    let count = enabled_rows.len();

    let mut defs = Vec::with_capacity(count);
    let mut loc_ids: HashMap<u32, (u32, u32)> = HashMap::with_capacity(count);
    for info in enabled_rows {
        let id = info.achievement_id;
        loc_ids.insert(id, (info.name_id, info.description_id));
        let conditions: Vec<AchievementConditionDef> = info
            .simple_conditions
            .iter()
            .map(|c| {
                let event_name = event_names.get(&c.statistic).cloned().unwrap_or_default();
                AchievementConditionDef {
                    condition_id: c.condition_index,
                    target_value: c.target_value.max(1),
                    statistic_event_id: c.statistic,
                    event_name,
                }
            })
            .collect();
        let target = if conditions.is_empty() {
            info.target_value.max(1)
        } else {
            conditions.iter().map(|c| c.target_value).sum::<u64>().max(1)
        };

        // Client `achievements.static` omits server-authored CDN score weights, milestone checkpoints,
        // and ISK/SP payouts; keep those 0 / empty unless operator-configured, while bucketing each
        // achievement into a DERIVED non-zero category (1..=5) so category reset/query APIs work.
        let category_id = derived_achievement_category_id(info.primary_event_id, &conditions);
        defs.push(AchievementDef {
            id,
            is_client_achievement: info.is_client_achievement,
            category_id,
            name: info.name.clone(),
            description: info.description.clone(),
            notification_text: info.notification_text.clone(),
            target,
            score: 0,
            milestone_targets: Vec::new(),
            conditions,
            reward_isk: 0,
            reward_sp: 0,
        });
    }
    let json_bytes = serde_json::to_vec(&defs).unwrap_or_else(|_| b"[]".to_vec());
    let hash = sha256_hex(&json_bytes)[..16].to_string();
    let fb_bytes = super::quasar_achievements_flatbuffer::encode_achievement_data_flatbuffer(
        &defs,
        &loc_ids,
        &sde.achievement_category_labels,
    );
    (defs, allowed_client_keys, count, hash, json_bytes, fb_bytes)
}

fn advance_achievement_milestones_and_completion(
    a: &mut CharacterAchievement,
    now_unix: u64,
) -> (bool, Vec<u32>) {
    let has_configured_payout = a.def.reward_isk > 0 || a.def.reward_sp > 0;
    let mut new_milestones = Vec::new();
    for (idx, target) in a.def.milestone_targets.iter().enumerate() {
        let m_id = (idx + 1) as u32;
        if a.progress >= *target && !a.reached_milestones.contains(&m_id) {
            a.reached_milestones.push(m_id);
            if has_configured_payout {
                a.unclaimed_reward_count += 1;
            }
            new_milestones.push(m_id);
        }
    }
    let prev_completed = a.completed;
    if a.progress >= a.def.target && !a.completed {
        a.completed = true;
        a.completed_date = Some(now_unix);
        if a.def.milestone_targets.is_empty()
            && has_configured_payout
            && !a.reached_milestones.contains(&1)
        {
            a.reached_milestones.push(1);
            a.unclaimed_reward_count += 1;
        }
    }
    (!prev_completed && a.completed, new_milestones)
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
    pub category_milestone_defs: RwLock<HashMap<u32, Vec<CategoryMilestoneDef>>>,
    pub reached_category_milestones: RwLock<HashMap<(u32, u32), Vec<ReachedCategoryMilestone>>>,
    pub sde_achievement_count: usize,
    pub sde_achievement_hash: String,
    pub sde_definitions_json: Vec<u8>,
    pub sde_definitions_flatbuffer: Vec<u8>,
    db: Option<Arc<WorldRepository>>,
    sde: Option<Arc<eve_sde::SdeRegistry>>,
}

fn parse_char_cat_map<T: serde::de::DeserializeOwned>(json: &str) -> HashMap<(u32, u32), T> {
    serde_json::from_str::<HashMap<String, T>>(json)
        .ok()
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
}

fn encode_char_cat_map<T: serde::Serialize>(map: &HashMap<(u32, u32), T>) -> Option<String> {
    let str_map: HashMap<String, &T> =
        map.iter().map(|((c, cat), v)| (format!("{c}:{cat}"), v)).collect();
    serde_json::to_string(&str_map).ok()
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
        type CachedDefs = (Vec<AchievementDef>, HashSet<String>, usize, String, Vec<u8>, Vec<u8>);
        struct CachedAchievementDefs(CachedDefs);

        let (
            defs,
            allowed_client_counter_keys,
            sde_achievement_count,
            sde_achievement_hash,
            sde_definitions_json,
            sde_definitions_flatbuffer,
        ) = sde
            .as_deref()
            .map(|s| {
                s.get_or_init_derived::<CachedAchievementDefs>(|| {
                    CachedAchievementDefs(build_definitions_from_sde(s))
                })
                .0
                .clone()
            })
            .unwrap_or_else(|| {
                let empty = b"[]".to_vec();
                let empty_fb =
                    super::quasar_achievements_flatbuffer::encode_achievement_data_flatbuffer(
                        &[],
                        &HashMap::new(),
                        &[],
                    );
                (
                    Vec::new(),
                    HashSet::new(),
                    0,
                    sha256_hex(&empty)[..16].to_string(),
                    empty,
                    empty_fb,
                )
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

        let (loaded_cat_scores, loaded_cat_milestones) = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    let scores = s
                        .quasar_state_blobs
                        .get("quasar_achievement_cat_scores")
                        .map(|json| parse_char_cat_map::<u32>(json))
                        .unwrap_or_default();
                    let milestones = s
                        .quasar_state_blobs
                        .get("quasar_achievement_cat_milestones")
                        .map(|json| parse_char_cat_map::<Vec<ReachedCategoryMilestone>>(json))
                        .unwrap_or_default();
                    (scores, milestones)
                })
            })
            .unwrap_or_default();

        Self {
            definitions: defs,
            allowed_client_counter_keys,
            character_achievements: RwLock::new(loaded),
            category_scores: RwLock::new(loaded_cat_scores),
            category_milestone_defs: RwLock::new(HashMap::new()),
            reached_category_milestones: RwLock::new(loaded_cat_milestones),
            sde_achievement_count,
            sde_achievement_hash,
            sde_definitions_json,
            sde_definitions_flatbuffer,
            db,
            sde,
        }
    }

    /// Returns the encoded `AchievementData` FlatBuffer matching `self.definitions`.
    pub fn flatbuffer_bytes(&self) -> Vec<u8> {
        if self.sde.is_none()
            || self.sde_definitions_flatbuffer.is_empty()
            || self.definitions.len() != self.sde_achievement_count
        {
            let cat_labels = self
                .sde
                .as_ref()
                .map_or(&[][..], |s| s.achievement_category_labels.as_slice());
            return super::quasar_achievements_flatbuffer::encode_achievement_data_flatbuffer(
                &self.definitions,
                &HashMap::new(),
                cat_labels,
            );
        }
        self.sde_definitions_flatbuffer.clone()
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            if db.is_write_locked_by_current_thread() {
                return;
            }
            let a_json = {
                let map = self.character_achievements.read().unwrap_or_else(|e| e.into_inner());
                serde_json::to_string(&*map).ok()
            };
            let c_json = {
                let cat_scores = self.category_scores.read().unwrap_or_else(|e| e.into_inner());
                encode_char_cat_map(&*cat_scores)
            };
            let cm_json = {
                let cat_milestones = self
                    .reached_category_milestones
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                encode_char_cat_map(&*cat_milestones)
            };
            let already = db.with_snapshot(|snap| {
                a_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_achievements") == Some(j))
                    && c_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_achievement_cat_scores") == Some(j))
                    && cm_json.as_ref().is_none_or(|j| snap.quasar_state_blobs.get("quasar_achievement_cat_milestones") == Some(j))
            });
            if already {
                return;
            }
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Some(json) = a_json {
                    snap.quasar_state_blobs.insert("quasar_achievements".into(), json);
                }
                if let Some(cat_json) = c_json {
                    snap.quasar_state_blobs.insert("quasar_achievement_cat_scores".into(), cat_json);
                }
                if let Some(cat_ms_json) = cm_json {
                    snap.quasar_state_blobs.insert("quasar_achievement_cat_milestones".into(), cat_ms_json);
                }
                Ok::<(), String>(())
            });
        }
    }

    pub fn rehydrate_from_db(&self) {
        let Some(ref db) = self.db else { return };
        let (mut loaded, loaded_cat_scores, loaded_cat_milestones) = db.with_snapshot(|s| {
            let loaded = s
                .quasar_state_blobs
                .get("quasar_achievements")
                .and_then(|json| serde_json::from_str::<HashMap<u32, Vec<CharacterAchievement>>>(json).ok())
                .unwrap_or_default();
            let cat_scores = s
                .quasar_state_blobs
                .get("quasar_achievement_cat_scores")
                .map(|json| parse_char_cat_map::<u32>(json))
                .unwrap_or_default();
            let cat_milestones = s
                .quasar_state_blobs
                .get("quasar_achievement_cat_milestones")
                .map(|json| parse_char_cat_map::<Vec<ReachedCategoryMilestone>>(json))
                .unwrap_or_default();
            (loaded, cat_scores, cat_milestones)
        });
        let defs_by_id: HashMap<u32, &AchievementDef> =
            self.definitions.iter().map(|d| (d.id, d)).collect();
        if !defs_by_id.is_empty() {
            for list in loaded.values_mut() {
                for a in list.iter_mut() {
                    if let Some(fresh_def) = defs_by_id.get(&a.def.id) {
                        a.def = (*fresh_def).clone();
                    }
                }
            }
        }
        *self
            .character_achievements
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded;
        *self
            .category_scores
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded_cat_scores;
        *self
            .reached_category_milestones
            .write()
            .unwrap_or_else(|e| e.into_inner()) = loaded_cat_milestones;
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

    pub fn set_category_milestones(&self, category_id: u32, mut milestones: Vec<CategoryMilestoneDef>) {
        milestones.sort_by_key(|m| (m.target_score, m.id));
        self.category_milestone_defs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(category_id, milestones);
    }

    pub fn reached_category_milestones(
        &self,
        character_id: u32,
        category_id: u32,
    ) -> Vec<ReachedCategoryMilestone> {
        self.reached_category_milestones
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(character_id, category_id))
            .cloned()
            .unwrap_or_default()
    }

    fn evaluate_category_milestones(
        &self,
        character_id: u32,
        category_id: u32,
        cat_score: u32,
        now_unix: u64,
    ) -> Vec<u32> {
        if category_id == 0 {
            return Vec::new();
        }
        let defs = {
            let map = self
                .category_milestone_defs
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let Some(list) = map.get(&category_id) else {
                return Vec::new();
            };
            if list.is_empty() {
                return Vec::new();
            }
            list.clone()
        };
        let mut reached_map = self
            .reached_category_milestones
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let entry = reached_map.entry((character_id, category_id)).or_default();
        let mut newly_reached = Vec::new();
        for def in defs {
            if cat_score >= def.target_score
                && !entry.iter().any(|r| r.milestone_id == def.id)
            {
                entry.push(ReachedCategoryMilestone {
                    milestone_id: def.id,
                    reached_date: now_unix,
                });
                newly_reached.push(def.id);
            }
        }
        newly_reached
    }

    fn prune_category_milestones_above(&self, character_id: u32, category_id: u32, new_score: u32) {
        if category_id == 0 {
            return;
        }
        let targets_by_id: HashMap<u32, u32> = {
            let map = self
                .category_milestone_defs
                .read()
                .unwrap_or_else(|e| e.into_inner());
            map.get(&category_id)
                .map(|list| list.iter().map(|d| (d.id, d.target_score)).collect())
                .unwrap_or_default()
        };
        let mut reached_map = self
            .reached_category_milestones
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let should_remove = match reached_map.get_mut(&(character_id, category_id)) {
            Some(list) => {
                if new_score == 0 {
                    list.clear();
                } else if !targets_by_id.is_empty() {
                    list.retain(|r| {
                        targets_by_id
                            .get(&r.milestone_id)
                            .is_some_and(|&target| new_score >= target)
                    });
                }
                list.is_empty()
            }
            None => false,
        };
        if should_remove {
            reached_map.remove(&(character_id, category_id));
        }
    }

    fn sync_category_score_and_milestones(
        &self,
        character_id: u32,
        category_id: u32,
        earned_score: u32,
        prior_cat_sum: u32,
        now_unix: u64,
    ) -> (u32, bool, Vec<u32>) {
        if category_id == 0 {
            return (0, false, Vec::new());
        }
        let (cat_score, cat_score_changed) = {
            let mut scores = self
                .category_scores
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = scores.get_mut(&(character_id, category_id)) {
                let old = *existing;
                if earned_score > 0 {
                    *existing = old.saturating_add(earned_score);
                }
                (*existing, *existing != old)
            } else {
                let updated = prior_cat_sum.saturating_add(earned_score);
                if earned_score > 0 {
                    scores.insert((character_id, category_id), updated);
                }
                (updated, updated != prior_cat_sum)
            }
        };
        let new_cat_milestones =
            self.evaluate_category_milestones(character_id, category_id, cat_score, now_unix);
        (cat_score, cat_score_changed, new_cat_milestones)
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        achievement_id: u32,
        delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterAchievement, bool, Vec<u32>)> {
        self.add_progress_with_outcomes(character_id, achievement_id, delta, now_unix)
            .map(|o| (o.achievement, o.just_completed, o.new_ach_milestones))
    }

    fn update_single_achievement_with_outcomes(
        &self,
        character_id: u32,
        achievement_id: u32,
        now_unix: u64,
        mut apply: impl FnMut(&mut CharacterAchievement),
    ) -> Option<AchievementUpdateOutcome> {
        let outcome = {
            let mut map = self
                .character_achievements
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let list = Self::init_list(&self.definitions, &mut map, character_id);
            let target_cat = list
                .iter()
                .find(|a| a.def.id == achievement_id)
                .map(|a| a.def.category_id);
            let mut found = None;
            if let Some(cat_id) = target_cat {
                let prior_cat_sum: u32 = if cat_id > 0 {
                    list.iter()
                        .filter(|a| a.def.category_id == cat_id && a.completed)
                        .map(|a| a.def.score)
                        .sum()
                } else {
                    0
                };
                for a in list.iter_mut() {
                    if a.def.id == achievement_id {
                        apply(a);
                        let (just_completed, new_milestones) =
                            advance_achievement_milestones_and_completion(a, now_unix);
                        found = Some((a.clone(), just_completed, new_milestones, prior_cat_sum));
                        break;
                    }
                }
            }
            found
        };
        let (ach, just_completed, new_ach_ms, prior_cat_sum) = outcome?;
        let earned_score = if just_completed { ach.def.score } else { 0 };
        let (cat_score, cat_score_changed, new_cat_ms) = self.sync_category_score_and_milestones(
            character_id,
            ach.def.category_id,
            earned_score,
            prior_cat_sum,
            now_unix,
        );
        Some(AchievementUpdateOutcome {
            achievement: ach,
            just_completed,
            new_ach_milestones: new_ach_ms,
            cat_score,
            cat_score_changed,
            new_cat_milestones: new_cat_ms,
        })
    }

    pub(crate) fn add_progress_with_outcomes(
        &self,
        character_id: u32,
        achievement_id: u32,
        delta: u64,
        now_unix: u64,
    ) -> Option<AchievementUpdateOutcome> {
        self.update_single_achievement_with_outcomes(character_id, achievement_id, now_unix, |a| {
            a.progress = a.progress.saturating_add(delta);
            let mut rem = a.progress;
            for cond in &a.def.conditions {
                let take = rem.min(cond.target_value);
                a.condition_progress.insert(cond.condition_id, take);
                rem = rem.saturating_sub(take);
            }
        })
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
        self.record_statistic_event_filtered_with_outcomes(
            character_id,
            statistic_event_id,
            delta,
            now_unix,
            false,
        )
        .into_iter()
        .map(|o| (o.achievement, o.just_completed, o.new_ach_milestones))
        .collect()
    }

    fn record_statistic_event_filtered_with_outcomes(
        &self,
        character_id: u32,
        statistic_event_id: u32,
        delta: u64,
        now_unix: u64,
        client_only: bool,
    ) -> Vec<AchievementUpdateOutcome> {
        let raw_updates = {
            let mut map = self
                .character_achievements
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let list = Self::init_list(&self.definitions, &mut map, character_id);
            let mut prior_cat_sums: HashMap<u32, u32> = HashMap::new();
            for item in list.iter() {
                if item.completed && item.def.category_id > 0 {
                    *prior_cat_sums.entry(item.def.category_id).or_insert(0) += item.def.score;
                }
            }
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
                    .map(|c| {
                        a.condition_progress
                            .get(&c.condition_id)
                            .copied()
                            .unwrap_or(0)
                            .min(c.target_value)
                    })
                    .sum();
                a.progress = a.progress.max(total_cond_progress);

                let (just_completed, new_milestones) =
                    advance_achievement_milestones_and_completion(a, now_unix);
                let prior_sum = prior_cat_sums
                    .get(&a.def.category_id)
                    .copied()
                    .unwrap_or(0);
                updates.push((a.clone(), just_completed, new_milestones, prior_sum));
            }
            updates
        };

        raw_updates
            .into_iter()
            .map(|(a, just_completed, new_ach_ms, prior_sum)| {
                let earned_score = if just_completed { a.def.score } else { 0 };
                let (cat_score, cat_score_changed, new_cat_ms) = self
                    .sync_category_score_and_milestones(
                        character_id,
                        a.def.category_id,
                        earned_score,
                        prior_sum,
                        now_unix,
                    );
                AchievementUpdateOutcome {
                    achievement: a,
                    just_completed,
                    new_ach_milestones: new_ach_ms,
                    cat_score,
                    cat_score_changed,
                    new_cat_milestones: new_cat_ms,
                }
            })
            .collect()
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
        crate::quasar_gateway::isolate_side_effect(
            "quasar_achievements.on_gameplay_event_by_name",
            || {
                if let Some(ref db) = self.db {
                    crate::services::quasar_goal_tale_notices::record_server_opportunity_event(
                        db,
                        &self.definitions,
                        character_id,
                        event_names,
                        delta,
                        now_unix,
                    );
                    if let Some(ref sde) = self.sde {
                        crate::services::operations_manager_helpers::advance_and_notify_server_operation_tasks(
                            db,
                            sde,
                            gateway,
                            i64::from(character_id),
                            event_names,
                            &[],
                            delta,
                            now_unix,
                        );
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
            },
        );
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
        crate::quasar_gateway::isolate_side_effect(
            "quasar_achievements.on_client_counter_event",
            || {
                let parsed_id = key.parse::<u32>().ok();
                let mut event_ids: Vec<u32> = self
                    .definitions
                    .iter()
                    .filter(|d| d.is_client_achievement)
                    .flat_map(|d| d.conditions.iter())
                    .filter(|c| {
                        (!c.event_name.is_empty() && c.event_name == key)
                            || parsed_id == Some(c.statistic_event_id)
                    })
                    .map(|c| c.statistic_event_id)
                    .collect();
                event_ids.sort_unstable();
                event_ids.dedup();
                self.apply_statistic_event_ids(character_id, &event_ids, delta, now_unix, true, gateway);
            },
        );
    }

    pub fn apply_client_task_completions_and_counters(
        &self,
        char_id: u32,
        client_ids: &[u32],
        counter_deltas: &[(String, u64)],
        now_unix: u64,
        gateway: &PublicGatewayRouter,
    ) {
        crate::quasar_gateway::isolate_side_effect(
            "quasar_achievements.apply_client_task_completions_and_counters",
            || {
                let mut any_marked = false;
                for &aid in client_ids {
                    if self.is_client_achievement_id(aid) {
                        if let Ok(o) =
                            self.mark_completed_with_outcomes(char_id, aid, now_unix)
                        {
                            any_marked = true;
                            emit_achievement_notices(
                                &gateway.queued_notices,
                                &gateway.descriptors,
                                char_id,
                                o.achievement.def.id,
                                o.achievement.def.category_id,
                                o.achievement.progress,
                                o.cat_score,
                                o.cat_score_changed,
                                o.just_completed,
                                &o.new_ach_milestones,
                                &o.new_cat_milestones,
                                now_unix,
                            );
                        }
                    }
                }
                if any_marked {
                    self.save_to_db();
                }
                for (k, delta) in counter_deltas {
                    self.on_client_counter_event(char_id, k, *delta, now_unix, gateway);
                }
                for (scope, payload, ts) in gateway.drain_queued_notices() {
                    let _ = gateway.publish_notice_isolated(
                        scope,
                        "quasar_achievements.apply_client_task_completions_and_counters.notice",
                        payload,
                        ts,
                    );
                }
            },
        );
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
            let updates = self.record_statistic_event_filtered_with_outcomes(
                character_id,
                eid,
                delta,
                now_unix,
                client_only,
            );
            for o in updates {
                any_changed = true;
                emit_achievement_notices(
                    &gateway.queued_notices,
                    &gateway.descriptors,
                    character_id,
                    o.achievement.def.id,
                    o.achievement.def.category_id,
                    o.achievement.progress,
                    o.cat_score,
                    o.cat_score_changed,
                    o.just_completed,
                    &o.new_ach_milestones,
                    &o.new_cat_milestones,
                    now_unix,
                );
            }
        }
        if any_changed {
            self.save_to_db();
        }
        for (scope, payload, ts) in gateway.drain_queued_notices() {
            let _ = gateway.publish_notice_isolated(
                scope,
                "quasar_achievements.apply_statistic_event_ids.notice",
                payload,
                ts,
            );
        }
    }

    pub fn mark_completed(&self, character_id: u32, achievement_id: u32, now_unix: u64) -> Result<CharacterAchievement, (u32, String)> {
        self.mark_completed_with_outcomes(character_id, achievement_id, now_unix)
            .map(|o| o.achievement)
    }

    pub(crate) fn mark_completed_with_outcomes(
        &self,
        character_id: u32,
        achievement_id: u32,
        now_unix: u64,
    ) -> Result<AchievementUpdateOutcome, (u32, String)> {
        self.update_single_achievement_with_outcomes(character_id, achievement_id, now_unix, |a| {
            a.progress = a.def.target;
            for cond in &a.def.conditions {
                a.condition_progress.insert(cond.condition_id, cond.target_value);
            }
        })
        .ok_or_else(|| (STATUS_NOT_FOUND, "Achievement not found".into()))
    }

    pub fn reset_achievement(&self, character_id: u32, achievement_id: u32) -> Result<CharacterAchievement, (u32, String)> {
        self.reset_achievement_with_category_score(character_id, achievement_id)
            .map(|(ach, _)| ach)
    }

    pub(crate) fn reset_achievement_with_category_score(
        &self,
        character_id: u32,
        achievement_id: u32,
    ) -> Result<(CharacterAchievement, Option<(u32, u32)>), (u32, String)> {
        let outcome = {
            let mut map = self.character_achievements.write().unwrap_or_else(|e| e.into_inner());
            let list = Self::init_list(&self.definitions, &mut map, character_id);
            let mut target_info = None;
            for a in list.iter_mut() {
                if a.def.id == achievement_id {
                    let was_completed = a.completed;
                    let cat_id = a.def.category_id;
                    let ach_score = a.def.score;
                    a.progress = 0;
                    a.condition_progress.clear();
                    a.completed = false;
                    a.completed_date = None;
                    a.reached_milestones.clear();
                    a.unclaimed_reward_count = 0;
                    target_info = Some((a.clone(), was_completed, cat_id, ach_score));
                    break;
                }
            }
            target_info.map(|(ach, was_completed, cat_id, ach_score)| {
                let recomputed_cat_sum: u32 = if cat_id > 0 {
                    list.iter()
                        .filter(|x| x.def.category_id == cat_id && x.completed)
                        .map(|x| x.def.score)
                        .sum()
                } else {
                    0
                };
                (ach, was_completed, cat_id, ach_score, recomputed_cat_sum)
            })
        };
        let (ach, was_completed, cat_id, ach_score, recomputed_cat_sum) =
            outcome.ok_or_else(|| (STATUS_NOT_FOUND, "Achievement not found".into()))?;
        let mut score_update = None;
        if cat_id > 0 {
            let (old_score, new_score) = {
                let mut scores = self.category_scores.write().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = scores.get_mut(&(character_id, cat_id)) {
                    let old = *entry;
                    let updated = if was_completed && ach_score > 0 {
                        old.saturating_sub(ach_score)
                    } else {
                        old
                    };
                    *entry = updated;
                    (old, updated)
                } else {
                    let prior_sum = if was_completed {
                        recomputed_cat_sum.saturating_add(ach_score)
                    } else {
                        recomputed_cat_sum
                    };
                    scores.insert((character_id, cat_id), recomputed_cat_sum);
                    (prior_sum, recomputed_cat_sum)
                }
            };
            self.prune_category_milestones_above(character_id, cat_id, new_score);
            if new_score != old_score {
                score_update = Some((cat_id, new_score));
            }
        }
        self.save_to_db();
        Ok((ach, score_update))
    }

    pub fn reset_category(&self, character_id: u32, category_id: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).remove(&(character_id, category_id));
        self.reached_category_milestones.write().unwrap_or_else(|e| e.into_inner()).remove(&(character_id, category_id));
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
        let _ = self.set_category_score_at(character_id, category_id, score, 1_700_000_000);
    }

    fn set_category_score_at(
        &self,
        character_id: u32,
        category_id: u32,
        score: u32,
        now_unix: u64,
    ) -> Vec<u32> {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).insert((character_id, category_id), score);
        self.prune_category_milestones_above(character_id, category_id, score);
        let new_cat_ms = self.evaluate_category_milestones(character_id, category_id, score, now_unix);
        self.save_to_db();
        new_cat_ms
    }

    pub fn reset_all(&self, character_id: u32) {
        self.category_scores.write().unwrap_or_else(|e| e.into_inner()).retain(|&(cid, _), _| cid != character_id);
        self.reached_category_milestones.write().unwrap_or_else(|e| e.into_inner()).retain(|&(cid, _), _| cid != character_id);
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
        self.reached_category_milestones.write().unwrap_or_else(|e| e.into_inner()).retain(|&(cid, _), _| cid != character_id);
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
                if a.unclaimed_reward_count == 0
                    || (a.def.reward_isk <= 0 && a.def.reward_sp <= 0)
                {
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

pub(crate) fn u32_to_uuid(id: u32) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    uuid
}

pub(crate) fn emit_achievement_notices(
    queued_notices: &RwLock<
        Vec<(
            crate::quasar_gateway::NoticeScope,
            eve_proto::quasar::GoogleAny,
            u64,
        )>,
    >,
    descriptors: &Arc<QuasarDescriptorRegistry>,
    cid: u32,
    aid: u32,
    cat_id: u32,
    progress: u64,
    cat_score: u32,
    cat_score_changed: bool,
    completed: bool,
    ach_milestones: &[u32],
    cat_milestones: &[u32],
    now: u64,
) {
    crate::quasar_gateway::queue_pb_notice(
        queued_notices,
        crate::quasar_gateway::NoticeScope::Character(cid),
        "quasar_achievements.emit_achievement_notices.progressed",
        pb::message_with_registry(Arc::clone(descriptors), "eve_public.achievement.api.ProgressedNotice")
            .set("achievement.uuid", u32_to_uuid(aid))
            .set("value", progress),
        now,
    );
    if completed {
        crate::quasar_gateway::queue_pb_notice(
            queued_notices,
            crate::quasar_gateway::NoticeScope::Character(cid),
            "quasar_achievements.emit_achievement_notices.completed",
            pb::message_with_registry(Arc::clone(descriptors), "eve_public.achievement.api.CompletedNotice")
                .set("achievement.uuid", u32_to_uuid(aid)),
            now,
        );
    }
    for m_id in ach_milestones {
        crate::quasar_gateway::queue_pb_notice(
            queued_notices,
            crate::quasar_gateway::NoticeScope::Character(cid),
            "quasar_achievements.emit_achievement_notices.milestone_reached",
            pb::message_with_registry(
                Arc::clone(descriptors),
                "eve_public.achievement.milestone.api.ReachedNotice",
            )
            .set("achievement.uuid", u32_to_uuid(aid))
            .set("milestone.uuid", u32_to_uuid(*m_id)),
            now,
        );
    }

    if cat_id > 0 {
        for cm_id in cat_milestones {
            crate::quasar_gateway::queue_pb_notice(
                queued_notices,
                crate::quasar_gateway::NoticeScope::Character(cid),
                "quasar_achievements.emit_achievement_notices.category_milestone_reached",
                pb::message_with_registry(
                    Arc::clone(descriptors),
                    "eve_public.achievement.category.milestone.api.ReachedNotice",
                )
                .set("category.uuid", u32_to_uuid(cat_id))
                .set("milestone.uuid", u32_to_uuid(*cm_id)),
                now,
            );
        }

        if cat_score_changed {
            crate::quasar_gateway::queue_pb_notice(
                queued_notices,
                crate::quasar_gateway::NoticeScope::Character(cid),
                "quasar_achievements.emit_achievement_notices.category_progressed",
                pb::message_with_registry(
                    Arc::clone(descriptors),
                    "eve_public.achievement.category.api.ProgressedNotice",
                )
                .set("category.uuid", u32_to_uuid(cat_id))
                .set("score", cat_score),
                now,
            );
        }
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

/// Minimal valid FlatBuffer root table with all field offsets `0` (kept for unit-test coverage).
#[cfg(test)]
pub(crate) fn empty_root_table_flatbuffer() -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(&12u32.to_le_bytes());
    for w in [4u16, 4, 0, 0] {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.extend_from_slice(&8i32.to_le_bytes());
    buf
}

/// Returns the isolated FlatBuffer staging directory for `content_id` ONLY when
/// `EVOMU_FLATBUFFER_STAGE_DIR` is explicitly set and non-empty (`#1120` / `#1089`).
///
/// `eve-server` never scans or mutates host EVE client directories (`%LOCALAPPDATA%\CCP\EVE` or
/// `~/Library/Application Support/CCP/EVE`); live clients receive FlatBuffers and checksums over
/// the in-process Quasar CDN endpoints.
fn isolated_flatbuffer_stage_content_dir(content_id: &str) -> Option<std::path::PathBuf> {
    let stage_dir = std::env::var_os("EVOMU_FLATBUFFER_STAGE_DIR")?;
    if stage_dir.is_empty() {
        return None;
    }
    let content_dir = std::path::PathBuf::from(stage_dir).join(content_id);
    if let Err(err) = std::fs::create_dir_all(&content_dir) {
        tracing::warn!(
            content_id,
            error = %err,
            "Failed to create isolated FlatBuffer staging directory"
        );
        return None;
    }
    Some(content_dir)
}

/// Seeds FlatBuffer payload files ONLY inside the isolated staging root configured via
/// `EVOMU_FLATBUFFER_STAGE_DIR` (`#1120` / `#1089`), leaving host EVE client directories untouched.
#[cfg(test)]
pub(crate) fn ensure_client_flatbuffer_storage_seeded(
    content_id: &str,
    version_str: &str,
    data_bytes: &[u8],
) {
    let Some(content_dir) = isolated_flatbuffer_stage_content_dir(content_id) else {
        return;
    };
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

/// Seeds the isolated `achievements/` staging directory with the full `AchievementData` FlatBuffer
/// and refreshes any stale empty/legacy-140-byte `data.bin` or mismatched `version.txt`
/// (e.g. `"24.1.60"`), ONLY when `EVOMU_FLATBUFFER_STAGE_DIR` is explicitly set and non-empty.
pub(crate) fn seed_or_upgrade_achievements_flatbuffer(version_str: &str, data_bytes: &[u8]) {
    let Some(content_dir) = isolated_flatbuffer_stage_content_dir("achievements") else {
        return;
    };
    let legacy_empty = empty_achievement_flatbuffer();
    let data_path = content_dir.join("data.bin");
    let ver_path = content_dir.join("version.txt");
    let existing_data = std::fs::read(&data_path).unwrap_or_default();
    let existing_ver = std::fs::read_to_string(&ver_path).unwrap_or_default();
    let needs_upgrade = existing_data.is_empty()
        || existing_data == legacy_empty
        || existing_ver.trim() != version_str;
    if needs_upgrade {
        let _ = std::fs::write(&data_path, data_bytes);
        let _ = std::fs::write(&ver_path, version_str.as_bytes());
    }
}

#[cfg(test)] #[path = "quasar_achievements_tests.rs"]
mod quasar_achievements_tests;

#[path = "quasar_achievements_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_achievements_cov_unit_tests;

