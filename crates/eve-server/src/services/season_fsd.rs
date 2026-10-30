//! V24 FSD static data loaders for `seasonManager` (`challenges.fsdbinary`,
//! `challengeevents.fsdbinary`, `challengeconditions.fsdbinary`, `seasonselection.fsdbinary`,
//! `community_track_templates.fsdbinary`, `seasonalcampaignsloginrewards.fsdbinary`, `crates.fsdbinary`).
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use super::season_manager::CharacterChallenge;

pub const SEASON_CONFIG_KEY: &str = "season_manager:season_configuration";
pub const SEASONS_CATALOG_KEY: &str = "season_manager:seasons";
pub const SEASON_LOOT_TABLES_KEY: &str = "season_manager:loot_tables";
pub const SEASONAL_CHALLENGES_CONFIG_KEY: &str = "season_manager:challenge_definitions";

/// Structured definition of a seasonal challenge template from `res:/staticdata/challenges.fsdbinary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonalChallengeDef {
    pub challenge_id: i64,
    pub season_id: i64,
    pub event_type: String,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub max_progress: i64,
    pub client_event_driven: bool,
    pub reward_type_id: u32,
    pub reward_quantity: i32,
    #[serde(default)]
    pub is_dormant: bool,
    #[serde(default)]
    pub expiration_time_minutes: i64,
    #[serde(default)]
    pub isk_awarded: i64,
    #[serde(default)]
    pub next_challenge: Option<i64>,
    #[serde(default)]
    pub should_be_active_on_season_start: bool,
}

impl SeasonalChallengeDef {
    pub fn to_character_challenge_with_end(&self, season_id: i64, now_ft: i64, season_end_ft: i64) -> CharacterChallenge {
        let expiration_date = if self.expiration_time_minutes > 0 {
            now_ft + self.expiration_time_minutes * 60 * 10_000_000
        } else if season_end_ft > now_ft {
            season_end_ft
        } else {
            now_ft + 30 * 86_400 * 10_000_000
        };
        CharacterChallenge {
            challenge_id: self.challenge_id,
            season_id,
            title: self.title.clone(),
            description: self.description.clone(),
            points: self.points,
            progress: 0,
            max_progress: self.max_progress,
            completed: false,
            claimed: false,
            is_dormant: self.is_dormant,
            expiration_date,
            client_event_driven: self.client_event_driven,
            reward_type_id: self.reward_type_id,
            reward_quantity: self.reward_quantity,
            baseline_progress: 0,
        }
    }

    pub fn to_character_challenge(&self, season_id: i64, now_ft: i64) -> CharacterChallenge {
        self.to_character_challenge_with_end(season_id, now_ft, now_ft + 30 * 86_400 * 10_000_000)
    }
}

/// Season definition loaded from `res:/staticdata/seasonselection.fsdbinary`,
/// `res:/staticdata/community_track_templates.fsdbinary`, and
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary` (or persisted operator configuration).
///
/// DATA GAP: Live season schedule (start/end timestamps) and `max_active_challenges` cap are
/// server-only and not present in V24 client FSD.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SeasonConfig {
    pub season_id: i64,
    pub template_id: i64,
    pub max_active_challenges: i64,
    pub max_season_points: i64,
    pub season_name: String,
    pub title_message_id: i64,
    pub description_message_id: i64,
    pub hint_message_id: i64,
    pub large_selection_title_id: i64,
    pub navigation_card_picture_path: String,
    pub background_no_logo: String,
    pub start_time_ft: i64,
    pub end_time_ft: i64,
    pub multiple_season_running: bool,
}

impl Default for SeasonConfig {
    fn default() -> Self {
        Self {
            season_id: 1,
            template_id: 1,
            max_active_challenges: 4,
            max_season_points: 100,
            season_name: "Crimson Harvest".into(),
            title_message_id: 583_333,
            description_message_id: 583_334,
            hint_message_id: 583_335,
            large_selection_title_id: 583_380,
            navigation_card_picture_path: "res:/UI/Texture/classes/agency/navigationCards/CrimsonHarvest2026_Agency_Navicons_400x666.png".into(),
            background_no_logo: "res:/UI/Texture/classes/NewFeatureNotify/triglavianInvasion.png".into(),
            start_time_ft: 133_700_000_000_000_000,
            end_time_ft: 133_730_240_000_000_000,
            multiple_season_running: false,
        }
    }
}

/// Builds the default season catalog from `res:/staticdata/seasonselection.fsdbinary`,
/// `res:/staticdata/community_track_templates.fsdbinary`, and
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary` using the server's `SdeRegistry`.
pub fn default_fsd_seasons(
    sde: &eve_sde::SdeRegistry,
    init_start_ft: i64,
) -> BTreeMap<i64, SeasonConfig> {
    let mut out = BTreeMap::new();
    let sel = sde
        .load_cfsd_nested("res:/staticdata/seasonselection.fsdbinary")
        .unwrap_or_default();
    let nav_path = sel
        .get("navigation_card_picture_path")
        .and_then(|v| v.as_str())
        .unwrap_or("res:/UI/Texture/classes/agency/navigationCards/CrimsonHarvest2026_Agency_Navicons_400x666.png")
        .to_string();
    let title_id = sel.get("seasonSelectionTitleID").and_then(|v| v.as_i64()).unwrap_or(583_333);
    let desc_id = sel.get("seasonSelectionDescriptionID").and_then(|v| v.as_i64()).unwrap_or(583_334);
    let hint_id = sel.get("seasonSelectionHintID").and_then(|v| v.as_i64()).unwrap_or(583_335);
    let large_title_id = sel
        .get("agencyLargeSeasonSelectionTitleID")
        .and_then(|v| v.as_i64())
        .unwrap_or(583_380);

    // DATA GAP: V24 FSD ships no live season start/end schedule or active-challenge cap;
    // synthesize a 30-day window from init_start_ft and a default cap of 4.
    let base_start_ft: i64 = init_start_ft;
    let base_end_ft: i64 = base_start_ft + 30 * 86_400 * 10_000_000;
    let max_ch = 4_i64;

    if let Ok(tracks) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
        for (id_str, val) in tracks {
            if let Ok(sid) = id_str.parse::<i64>() {
                let sname = val.get("name").and_then(|v| v.as_str()).unwrap_or("Crimson Harvest").to_string();
                let goal_pts = val.get("goal").and_then(|v| v.as_i64()).unwrap_or(100);
                out.insert(
                    sid,
                    SeasonConfig {
                        season_id: sid,
                        template_id: sid,
                        max_active_challenges: max_ch,
                        max_season_points: goal_pts,
                        season_name: sname,
                        title_message_id: title_id,
                        description_message_id: desc_id,
                        hint_message_id: hint_id,
                        large_selection_title_id: large_title_id,
                        navigation_card_picture_path: nav_path.clone(),
                        background_no_logo: nav_path.clone(),
                        start_time_ft: base_start_ft,
                        end_time_ft: base_end_ft,
                        multiple_season_running: false,
                    },
                );
            }
        }
    }

    if let Ok(campaigns) = sde.load_cfsd_nested("res:/staticdata/seasonalcampaignsloginrewards.fsdbinary") {
        let mut camp_list: Vec<(i64, serde_json::Value)> = campaigns
            .into_iter()
            .filter_map(|(k, v)| k.parse::<i64>().ok().map(|id| (id, v)))
            .collect();
        camp_list.sort_by_key(|(id, _)| *id);
        for (idx, (cid, val)) in camp_list.into_iter().enumerate() {
            let cname_id = val.get("campaignName").and_then(|v| v.as_i64()).unwrap_or(title_id);
            let bg = val
                .get("windowHeaderBackground")
                .and_then(|v| v.as_str())
                .unwrap_or(nav_path.as_str())
                .to_string();
            // DERIVED: seasonalcampaignsloginrewards.fsdbinary has no point-track goal or plain-text name;
            // derive a 1,000-point cap and label from the campaign ID.
            let cfg = SeasonConfig {
                season_id: cid,
                template_id: cid,
                max_active_challenges: max_ch,
                max_season_points: 1_000,
                season_name: format!("Seasonal Campaign {cid}"),
                title_message_id: cname_id,
                description_message_id: desc_id,
                hint_message_id: hint_id,
                large_selection_title_id: large_title_id,
                navigation_card_picture_path: nav_path.clone(),
                background_no_logo: bg,
                start_time_ft: base_start_ft,
                end_time_ft: base_end_ft,
                multiple_season_running: true,
            };
            out.insert(cid, cfg.clone());
            // DERIVED: Map ordinal season IDs 2..=4 when not present in community_track_templates.
            let ordinal_sid = (idx as i64) + 2;
            if ordinal_sid <= 4 && !out.contains_key(&ordinal_sid) {
                let mut alias_cfg = cfg;
                alias_cfg.season_id = ordinal_sid;
                out.insert(ordinal_sid, alias_cfg);
            }
        }
    }
    out
}

pub fn load_seasons_catalog(
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    init_start_ft: i64,
) -> BTreeMap<i64, SeasonConfig> {
    let mut catalog = if let Some(json) = snap.service_state_blobs.get(SEASONS_CATALOG_KEY) {
        serde_json::from_str::<BTreeMap<i64, SeasonConfig>>(json)
            .unwrap_or_else(|_| default_fsd_seasons(sde, init_start_ft))
    } else {
        default_fsd_seasons(sde, init_start_ft)
    };
    if let Some(json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
        if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(json) {
            let mut merged = catalog.get(&cfg.season_id).cloned().unwrap_or_default();
            merged.season_id = cfg.season_id;
            merged.max_active_challenges = cfg.max_active_challenges;
            if cfg.max_season_points > 0 {
                merged.max_season_points = cfg.max_season_points;
            }
            if !cfg.season_name.is_empty() {
                merged.season_name = cfg.season_name;
            }
            if cfg.end_time_ft > merged.end_time_ft {
                merged.start_time_ft = cfg.start_time_ft;
                merged.end_time_ft = cfg.end_time_ft;
            }
            catalog.insert(merged.season_id, merged.clone());
            catalog.insert(1, merged);
        }
    }
    catalog
}

pub fn load_season_config(snap: &eve_db::WorldStateSnapshot) -> SeasonConfig {
    if let Some(json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
        if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(json) {
            return cfg;
        }
    }
    SeasonConfig::default()
}

/// Decodes all 1,210 V24 seasonal challenges from `res:/staticdata/challenges.fsdbinary`.
pub fn fsd_challenge_definitions(sde: &eve_sde::SdeRegistry) -> &'static [SeasonalChallengeDef] {
    static FSD_CHALLENGES: OnceLock<Vec<SeasonalChallengeDef>> = OnceLock::new();
    FSD_CHALLENGES.get_or_init(|| {
        let Ok(raw) = sde.challenges() else {
            return Vec::new();
        };
        let mut parsed: Vec<(i64, serde_json::Value)> = raw
            .into_iter()
            .filter_map(|(k, v)| k.parse::<i64>().ok().map(|id| (id, v)))
            .collect();
        parsed.sort_by_key(|(id, _)| *id);
        let next_targets: HashSet<i64> = parsed
            .iter()
            .filter_map(|(_, v)| v.get("next_challenge").and_then(|n| n.as_i64()).filter(|&n| n > 0))
            .collect();
        parsed
            .into_iter()
            .map(|(cid, v)| {
                let event_type = v
                    .get("achievement_event_type")
                    .and_then(|s| s.as_str())
                    .unwrap_or("destroyHostileNPC")
                    .to_string();
                let category = v
                    .get("challenge_category")
                    .and_then(|s| s.as_str())
                    .unwrap_or("PvE");
                let disp_id = v.get("display_name_id").and_then(|n| n.as_i64()).unwrap_or(0);
                let msg_id = v.get("message_text_id").and_then(|n| n.as_i64()).unwrap_or(0);
                let points = v.get("points_awarded").and_then(|n| n.as_i64()).unwrap_or(0);
                let max_progress = v.get("max_progress").and_then(|n| n.as_i64()).unwrap_or(1).max(1);
                let exp_mins = v
                    .get("expiration_time_minutes")
                    .and_then(|n| n.as_i64())
                    .unwrap_or(0);
                let isk_awarded = v.get("isk_awarded").and_then(|n| n.as_i64()).unwrap_or(0);
                let next_challenge = v
                    .get("next_challenge")
                    .and_then(|n| n.as_i64())
                    .filter(|&n| n > 0);
                let active_start = v
                    .get("should_be_active_on_season_start")
                    .and_then(|n| n.as_i64())
                    .unwrap_or(0)
                    != 0;
                let reward_type_id = v
                    .get("reward")
                    .and_then(|r| r.get("type_id"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0) as u32;
                let reward_quantity = v
                    .get("reward")
                    .and_then(|r| r.get("amount"))
                    .and_then(|n| n.as_i64())
                    .unwrap_or(0) as i32;
                let client_event_driven = event_type.starts_with("client_");
                let is_dormant = !active_start || next_targets.contains(&cid);
                // DERIVED: challenges.fsdbinary has no per-season binding or localized text table in FSD;
                // default season_id to 1 and format title from category, challenge ID, and message ID.
                SeasonalChallengeDef {
                    challenge_id: cid,
                    season_id: 1,
                    event_type,
                    title: format!("{category} #{cid} (msg:{disp_id})"),
                    description: format!("msg:{msg_id}"),
                    points,
                    max_progress,
                    client_event_driven,
                    reward_type_id,
                    reward_quantity,
                    is_dormant,
                    expiration_time_minutes: exp_mins,
                    isk_awarded,
                    next_challenge,
                    should_be_active_on_season_start: active_start,
                }
            })
            .collect()
    })
}

/// Returns the starter active challenges from `res:/staticdata/challenges.fsdbinary`.
pub fn default_seasonal_challenge_definitions() -> Vec<SeasonalChallengeDef> {
    if let Ok(sde) = eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        let defs: Vec<SeasonalChallengeDef> = fsd_challenge_definitions(&sde)
            .iter()
            .filter(|d| !d.is_dormant)
            .take(4)
            .cloned()
            .collect();
        if !defs.is_empty() {
            return defs;
        }
    }
    Vec::new()
}

/// Decodes authentic reward crate loot tables from `res:/staticdata/crates.fsdbinary`.
pub fn fsd_crate_loot_tables(
    sde: &eve_sde::SdeRegistry,
) -> &'static BTreeMap<u32, Vec<(u32, i64, f64)>> {
    static LOOT_TABLES: OnceLock<BTreeMap<u32, Vec<(u32, i64, f64)>>> =
        OnceLock::new();
    LOOT_TABLES.get_or_init(|| {
        let mut map: BTreeMap<u32, Vec<(u32, i64, f64)>> = BTreeMap::new();
        if let Ok(cfsd) = sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
            for (_k, v) in cfsd {
                let Some(type_id) = v.get("typeID").and_then(|t| t.as_u64()).map(|t| t as u32) else {
                    continue;
                };
                let mut entries = Vec::new();
                if let Some(list) = v.get("fixedLootItems").and_then(|l| l.as_array()) {
                    for item_val in list {
                        if let Some(tid) = item_val.as_u64().map(|n| n as u32) {
                            if sde.types.contains_key(&tid) {
                                entries.push((tid, 1_i64, 1.0_f64));
                            }
                        }
                    }
                }
                if entries.is_empty() {
                    if let Some(fits) = v.get("fittings").and_then(|f| f.as_array()) {
                        if !fits.is_empty() {
                            if let Some(crate_type) = sde.types.get(&type_id) {
                                let name_lower = crate_type.name.to_lowercase();
                                if let Some(ship) = sde
                                    .types
                                    .values()
                                    .filter(|t| t.category_id == 6 && t.published && name_lower.contains(&t.name.to_lowercase()))
                                    .max_by_key(|t| t.name.len())
                                {
                                    entries.push((ship.type_id, 1_i64, 1.0_f64));
                                }
                            }
                        }
                    }
                }
                if !entries.is_empty() {
                    map.insert(type_id, entries);
                }
            }
        }
        map
    })
}

pub fn load_challenge_definitions_with_sde(
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
) -> Vec<SeasonalChallengeDef> {
    if let Some(json) = snap.service_state_blobs.get(SEASONAL_CHALLENGES_CONFIG_KEY) {
        if let Ok(defs) = serde_json::from_str::<Vec<SeasonalChallengeDef>>(json) {
            if !defs.is_empty() {
                return defs;
            }
        }
    }
    let fsd = fsd_challenge_definitions(sde);
    if !fsd.is_empty() {
        return fsd.to_vec();
    }
    default_seasonal_challenge_definitions()
}

pub fn load_challenge_definitions(snap: &eve_db::WorldStateSnapshot) -> Vec<SeasonalChallengeDef> {
    if let Some(json) = snap.service_state_blobs.get(SEASONAL_CHALLENGES_CONFIG_KEY) {
        if let Ok(defs) = serde_json::from_str::<Vec<SeasonalChallengeDef>>(json) {
            if !defs.is_empty() {
                return defs;
            }
        }
    }
    default_seasonal_challenge_definitions()
}
