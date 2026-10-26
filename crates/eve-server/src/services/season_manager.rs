//! `season_manager` — Seasonal event and challenge tracking service (`seasonManager`).
//!
//! Tracks active seasons, character challenges, progress, rewards, and loot tables.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SEASON_STATE_KEY_PREFIX: &str = "season_state:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterChallenge {
    pub challenge_id: i64,
    pub season_id: i64,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub progress: i64,
    pub max_progress: i64,
    pub completed: bool,
    pub claimed: bool,
    pub is_dormant: bool,
    pub expiration_date: i64,
    #[serde(default)]
    pub client_event_driven: bool,
    #[serde(default)]
    pub reward_type_id: u32,
    #[serde(default)]
    pub reward_quantity: i32,
    #[serde(default)]
    pub baseline_progress: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterSeasonState {
    pub selected_season_id: i64,
    pub season_points: i64,
    pub challenges: HashMap<i64, CharacterChallenge>,
}

impl Default for CharacterSeasonState {
    fn default() -> Self {
        Self {
            selected_season_id: 1,
            season_points: 0,
            challenges: HashMap::new(),
        }
    }
}

pub fn load_season_state_from_blobs(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> CharacterSeasonState {
    let key = format!("{SEASON_STATE_KEY_PREFIX}{char_id}");
    if let Some(json) = snap.service_state_blobs.get(&key) {
        if let Ok(st) = serde_json::from_str::<CharacterSeasonState>(json) {
            return st;
        }
    }
    let alt_key = format!("season_manager:{char_id}");
    if let Some(json) = snap.service_state_blobs.get(&alt_key) {
        if let Ok(st) = serde_json::from_str::<CharacterSeasonState>(json) {
            return st;
        }
    }
    if let Some(json) = snap.service_state_blobs.get("season_manager") {
        if let Ok(map) = serde_json::from_str::<HashMap<i64, CharacterSeasonState>>(json) {
            if let Some(st) = map.get(&char_id) {
                return st.clone();
            }
        }
    }
    CharacterSeasonState::default()
}

pub fn save_season_state_to_blobs(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, state: &CharacterSeasonState) {
    let key = format!("{SEASON_STATE_KEY_PREFIX}{char_id}");
    if let Ok(json) = serde_json::to_string(state) {
        snap.service_state_blobs.insert(key, json);
    }
    let mut map: HashMap<i64, CharacterSeasonState> = snap
        .service_state_blobs
        .get("season_manager")
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    map.insert(char_id, state.clone());
    if let Ok(json) = serde_json::to_string(&map) {
        snap.service_state_blobs.insert("season_manager".into(), json);
    }
}

/// Challenge keyvals for `st`, sorted by challenge ID.
fn sorted_challenge_keyvals(st: &CharacterSeasonState) -> Vec<PyRep> {
    let mut list: Vec<_> = st.challenges.values().collect();
    list.sort_by_key(|c| c.challenge_id);
    list.into_iter().map(|c| challenge_to_keyval(c, st.season_points)).collect()
}

fn read_character_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&CharacterSeasonState, &eve_db::WorldStateSnapshot) -> R,
{
    ctx.db.with_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, char_id);
        let now_ft = ctx.now_blueos_filetime_ticks();
        let defs = load_challenge_definitions(snap);
        ensure_character_challenges(&mut st, &defs, snap, char_id, now_ft);
        sync_character_challenges(&mut st, snap, char_id);
        f(&st, snap)
    })
}

fn with_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> Result<R, BridgeRpcError>
where
    F: FnOnce(&mut CharacterSeasonState, &mut eve_db::WorldStateSnapshot) -> Result<R, BridgeRpcError>,
{
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut state = load_season_state_from_blobs(snap, char_id);
        let out = f(&mut state, snap)?;
        save_season_state_to_blobs(snap, char_id, &state);
        Ok(out)
    })
}

fn challenge_to_keyval(c: &CharacterChallenge, season_points: i64) -> PyRep {
    PyRep::keyval([
        ("challengeID", PyRep::Integer(c.challenge_id)),
        ("seasonID", PyRep::Integer(c.season_id)),
        ("seasonPoints", PyRep::Integer(season_points)),
        ("title", PyRep::String(c.title.clone())),
        ("description", PyRep::String(c.description.clone())),
        ("points", PyRep::Integer(c.points)),
        ("progress", PyRep::Integer(c.progress)),
        ("maxProgress", PyRep::Integer(c.max_progress)),
        ("completed", PyRep::Bool(c.completed)),
        ("claimed", PyRep::Bool(c.claimed)),
        ("isDormant", PyRep::Bool(c.is_dormant)),
        ("expirationDate", PyRep::Integer(c.expiration_date)),
    ])
}

/// Structured definition of a seasonal challenge template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl SeasonalChallengeDef {
    pub fn to_character_challenge(&self, season_id: i64, now_ft: i64) -> CharacterChallenge {
        let expiration_date = now_ft + 7 * 86_400 * 10_000_000;
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
}

pub const SEASON_CONFIG_KEY: &str = "season_manager:season_configuration";
pub const SEASONS_CATALOG_KEY: &str = "season_manager:seasons";
pub const SEASON_LOOT_TABLES_KEY: &str = "season_manager:loot_tables";

/// Season definition loaded from `res:/staticdata/seasonselection.fsdbinary`,
/// `res:/staticdata/community_track_templates.fsdbinary`, and
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary` (or persisted operator configuration).
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
///
/// DATA GAP: Seasonal challenge definitions and live season schedule are server-only and not present in V24 client FSD.
pub fn default_fsd_seasons(
    sde: &eve_sde::SdeRegistry,
    init_start_ft: i64,
) -> std::collections::BTreeMap<i64, SeasonConfig> {
    let mut out = std::collections::BTreeMap::new();
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

    let base_start_ft: i64 = init_start_ft;
    let base_end_ft: i64 = base_start_ft + 30 * 86_400 * 10_000_000;
    let max_ch = default_seasonal_challenge_definitions()
        .iter()
        .filter(|d| !d.is_dormant)
        .count()
        .max(1) as i64;

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
) -> std::collections::BTreeMap<i64, SeasonConfig> {
    let mut catalog = if let Some(json) = snap.service_state_blobs.get(SEASONS_CATALOG_KEY) {
        serde_json::from_str::<std::collections::BTreeMap<i64, SeasonConfig>>(json)
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

pub const SEASONAL_CHALLENGES_CONFIG_KEY: &str = "season_manager:challenge_definitions";

// DATA GAP: Seasonal challenge definitions and live season schedule are server-only and not present in V24 client FSD.
// SYNTHETIC: Fallback seasonal challenge definitions used when SEASONAL_CHALLENGES_CONFIG_KEY is not populated.
pub fn default_seasonal_challenge_definitions() -> Vec<SeasonalChallengeDef> {
    vec![
        SeasonalChallengeDef {
            challenge_id: 1001,
            season_id: 1,
            event_type: "totalMined".into(),
            title: "Mining".into(),
            description: "Player mined X volume of ore".into(),
            points: 25,
            max_progress: 1000,
            client_event_driven: false,
            reward_type_id: 47_952,
            reward_quantity: 1,
            is_dormant: false,
        },
        SeasonalChallengeDef {
            challenge_id: 1002,
            season_id: 1,
            event_type: "destroyHostileNPC".into(),
            title: "Combat".into(),
            description: "An enemy NPC ship just got killed".into(),
            points: 50,
            max_progress: 5,
            client_event_driven: false,
            reward_type_id: 0,
            reward_quantity: 0,
            is_dormant: false,
        },
        SeasonalChallengeDef {
            challenge_id: 1003,
            season_id: 1,
            event_type: "hackingMiniGameWon".into(),
            title: "Exploration".into(),
            description: "Complete exploration relic and data sites".into(),
            points: 30,
            max_progress: 3,
            client_event_driven: false,
            reward_type_id: 0,
            reward_quantity: 0,
            is_dormant: false,
        },
        SeasonalChallengeDef {
            challenge_id: 1004,
            season_id: 1,
            event_type: "industryJobsCompleted".into(),
            title: "Industry".into(),
            description: "Complete manufacturing jobs at industrial facilities".into(),
            points: 40,
            max_progress: 2,
            client_event_driven: false,
            reward_type_id: 47_952,
            reward_quantity: 1,
            is_dormant: false,
        },
    ]
}

/// Decodes authentic reward crate loot tables from `res:/staticdata/crates.fsdbinary`.
pub fn fsd_crate_loot_tables(
    sde: &eve_sde::SdeRegistry,
) -> &'static std::collections::BTreeMap<u32, Vec<(u32, i64, f64)>> {
    static LOOT_TABLES: std::sync::OnceLock<std::collections::BTreeMap<u32, Vec<(u32, i64, f64)>>> =
        std::sync::OnceLock::new();
    LOOT_TABLES.get_or_init(|| {
        let mut map: std::collections::BTreeMap<u32, Vec<(u32, i64, f64)>> =
            std::collections::BTreeMap::new();
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

pub fn default_challenge(ch_id: i64, season_id: i64, now_ft: i64) -> CharacterChallenge {
    if let Some(def) = default_seasonal_challenge_definitions().into_iter().find(|d| d.challenge_id == ch_id) {
        def.to_character_challenge(season_id, now_ft)
    } else {
        let expiration_date = now_ft + 7 * 86_400 * 10_000_000;
        CharacterChallenge {
            challenge_id: ch_id,
            season_id,
            title: format!("Challenge {ch_id}"),
            description: String::new(),
            points: 10,
            progress: 0,
            max_progress: 1,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date,
            client_event_driven: true,
            reward_type_id: 0,
            reward_quantity: 0,
            baseline_progress: 0,
        }
    }
}

pub fn get_current_activity_count(
    event_type: &str,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> i64 {
    match event_type {
        "totalMined" | "volumeMined" => snap
            .mining_ledger
            .iter()
            .filter(|e| e.character_id == char_id)
            .map(|e| e.quantity)
            .sum::<i64>(),
        "destroyHostileNPC" => snap
            .killmails
            .values()
            .filter(|k| {
                k.attacker_character_id == char_id
                    && k.victim_character_id != 0
                    && k.victim_character_id != char_id
                    && !snap.characters.contains_key(&k.victim_character_id)
                    && !snap.corporations.contains_key(&k.victim_character_id)
            })
            .count() as i64,
        "hackingMiniGameWon" => {
            let key = format!("hacking_wins:{char_id}");
            snap.service_state_blobs
                .get(&key)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
        }
        "industryJobsCompleted" | "typeManufacturedCount" => snap
            .industry_jobs
            .values()
            .filter(|j| {
                j.installer_id == char_id
                    && j.activity_id == 1
                    && (j.status.eq_ignore_ascii_case("delivered")
                        || j.status.eq_ignore_ascii_case("completed")
                        || j.status == "102")
            })
            .count() as i64,
        _ => 0,
    }
}

pub fn ensure_character_challenges(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
) {
    for def in defs {
        st.challenges.entry(def.challenge_id).or_insert_with(|| {
            let mut ch = def.to_character_challenge(st.selected_season_id, now_ft);
            ch.baseline_progress = get_current_activity_count(&def.event_type, snap, char_id);
            ch
        });
    }
}

pub fn ensure_challenges_with_defs(st: &mut CharacterSeasonState, defs: &[SeasonalChallengeDef], now_ft: i64) {
    for def in defs {
        st.challenges.entry(def.challenge_id).or_insert_with(|| {
            def.to_character_challenge(st.selected_season_id, now_ft)
        });
    }
}

pub fn ensure_challenges(st: &mut CharacterSeasonState, now_ft: i64) {
    let defs = default_seasonal_challenge_definitions();
    ensure_challenges_with_defs(st, &defs, now_ft);
}

pub fn sync_character_challenges(
    st: &mut CharacterSeasonState,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) {
    let defs = load_challenge_definitions(snap);
    let def_map: HashMap<i64, &SeasonalChallengeDef> = defs.iter().map(|d| (d.challenge_id, d)).collect();
    for ch in st.challenges.values_mut() {
        let Some(def) = def_map.get(&ch.challenge_id) else {
            continue;
        };
        let current = get_current_activity_count(&def.event_type, snap, char_id);
        let effective = current.saturating_sub(ch.baseline_progress);
        if effective > ch.progress {
            ch.progress = effective.min(ch.max_progress);
            if ch.progress >= ch.max_progress {
                ch.completed = true;
            }
        }
    }
}

pub fn get_character_challenges_for_quasar(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Vec<CharacterChallenge> {
    let mut state = load_season_state_from_blobs(snap, char_id);
    let now_ft = 133_000_000_000_000_000;
    let defs = load_challenge_definitions(snap);
    ensure_character_challenges(&mut state, &defs, snap, char_id, now_ft);
    sync_character_challenges(&mut state, snap, char_id);
    let mut list: Vec<_> = state.challenges.into_values().collect();
    list.sort_by_key(|c| c.challenge_id);
    list
}

fn season_config_to_keyval(cfg: &SeasonConfig, defs: &[SeasonalChallengeDef]) -> PyRep {
    let mut tracked: Vec<String> = defs.iter().map(|d| d.event_type.clone()).collect();
    tracked.sort();
    tracked.dedup();
    PyRep::keyval([
        ("season_id", PyRep::Integer(cfg.season_id)),
        ("seasonID", PyRep::Integer(cfg.season_id)),
        ("template_id", PyRep::Integer(cfg.template_id)),
        ("title", PyRep::Integer(cfg.title_message_id)),
        ("seasonName", PyRep::String(cfg.season_name.clone())),
        ("news", PyRep::Integer(cfg.description_message_id)),
        ("navigation_card_picture_path", PyRep::String(cfg.navigation_card_picture_path.clone())),
        ("content_card_picture_path", PyRep::String(cfg.navigation_card_picture_path.clone())),
        ("background_no_logo", PyRep::String(cfg.background_no_logo.clone())),
        ("agencyLargeSeasonSelectionTitleID", PyRep::Integer(cfg.large_selection_title_id)),
        ("seasonSelectionDescriptionID", PyRep::Integer(cfg.description_message_id)),
        ("seasonSelectionHintID", PyRep::Integer(cfg.hint_message_id)),
        ("seasonSelectionTitleID", PyRep::Integer(cfg.title_message_id)),
        ("start_time", PyRep::Integer(cfg.start_time_ft)),
        ("end_time", PyRep::Integer(cfg.end_time_ft)),
        ("multiple_season_running", PyRep::Bool(cfg.multiple_season_running)),
        ("max_active_challenges", PyRep::Integer(cfg.max_active_challenges)),
        ("tracked_event_types", PyRep::List(tracked.into_iter().map(PyRep::String).collect())),
    ])
}

/// Dispatches calls to `seasonManager`.
pub fn handle_season_manager(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "seasonManager" {
        return None;
    }

    match (service, method) {
        ("seasonManager", "get_max_active_challenges") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let max_ch = ctx.db.with_snapshot(|snap| {
                if let Some(json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
                    if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(json) {
                        return cfg.max_active_challenges;
                    }
                }
                let st = load_season_state_from_blobs(snap, char_id);
                let catalog = load_seasons_catalog(snap, &ctx.sde, ctx.now_blueos_filetime_ticks());
                if let Some(cfg) = catalog.get(&st.selected_season_id) {
                    return cfg.max_active_challenges;
                }
                let defs = load_challenge_definitions(snap);
                defs.iter().filter(|d| !d.is_dormant).count().max(1) as i64
            });
            Some(Ok(PyRep::Integer(max_ch)))
        }
        // DATA GAP: requires server-only season schedule and challenges (rpc:seasonManager.get_season_end_time)
        ("seasonManager", "get_season_end_time") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let end_ft = ctx.db.with_mut_snapshot(|snap| {
                let st = load_season_state_from_blobs(snap, char_id);
                let now_ft = ctx.now_blueos_filetime_ticks();
                let unseeded = !snap.service_state_blobs.contains_key(SEASONS_CATALOG_KEY);
                let mut catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let cfg = catalog.entry(st.selected_season_id).or_default();
                let out = cfg.end_time_ft;
                if unseeded {
                    if let Ok(json) = serde_json::to_string(&catalog) {
                        snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), json);
                    }
                }
                out
            });
            Some(Ok(PyRep::Integer(end_ft)))
        }
        // DATA GAP: requires server-only season schedule and challenges (rpc:seasonManager.select_season)
        ("seasonManager", "select_season") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let season_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = with_season_state(ctx, char_id, |st, snap| {
                let catalog = load_seasons_catalog(snap, &ctx.sde, ctx.now_blueos_filetime_ticks());
                if !catalog.contains_key(&season_id) {
                    return Err(BridgeRpcError {
                        code: "InvalidSeasonId".into(),
                        message: format!("Unknown or inactive season ID {season_id}"),
                    });
                }
                st.selected_season_id = season_id;
                for ch in st.challenges.values_mut() {
                    ch.season_id = season_id;
                }
                snap.service_state_blobs
                    .insert(format!("active_season:{char_id}"), season_id.to_string());
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }
        // DATA GAP: requires server-only season schedule and challenges (rpc:seasonManager.get_season_data_for_character)
        ("seasonManager", "get_season_data_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rep = ctx.db.with_mut_snapshot(|snap| {
                let st = load_season_state_from_blobs(snap, char_id);
                let now_ft = ctx.now_blueos_filetime_ticks();
                let unseeded = !snap.service_state_blobs.contains_key(SEASONS_CATALOG_KEY);
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                if unseeded {
                    if let Ok(json) = serde_json::to_string(&catalog) {
                        snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), json);
                    }
                }
                let active_cfg = catalog
                    .get(&st.selected_season_id)
                    .cloned()
                    .unwrap_or_default();
                let defs = load_challenge_definitions(snap);
                season_config_to_keyval(&active_cfg, &defs)
            });
            Some(Ok(rep))
        }
        ("seasonManager", "fill_up_and_return_challenges_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let defs = load_challenge_definitions(snap);
                ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                sync_character_challenges(st, snap, char_id);
                Ok(sorted_challenge_keyvals(st))
            });
            match res {
                Ok(challenges) => Some(Ok(PyRep::List(challenges))),
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_challenge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001);
            let res = read_character_season_state(ctx, char_id, |st, _snap| {
                if let Some(c) = st.challenges.get(&ch_id) {
                    Ok(challenge_to_keyval(c, st.season_points))
                } else {
                    Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: format!("Challenge {ch_id} not found"),
                    })
                }
            });
            Some(res)
        }
        ("seasonManager", "challenge_is_dormant") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id) {
                    let defs = load_challenge_definitions(snap);
                    let def_dormant = defs.iter().find(|d| d.challenge_id == ch_id).map(|d| d.is_dormant).unwrap_or(false);
                    let is_dormant = c.is_dormant || def_dormant || (c.completed && !c.claimed);
                    Ok(PyRep::Bool(is_dormant))
                } else {
                    Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: format!("Challenge {ch_id} not found"),
                    })
                }
            });
            Some(res)
        }
        ("seasonManager", "get_challenge_expiration_date") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = read_character_season_state(ctx, char_id, |st, _snap| {
                if let Some(c) = st.challenges.get(&ch_id) {
                    Ok(PyRep::Integer(c.expiration_date))
                } else {
                    Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: format!("Challenge {ch_id} not found"),
                    })
                }
            });
            Some(res)
        }
        ("seasonManager", "process_client_event") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0_int = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("challenge_id").and_then(|v| v.as_i64()));
            let arg0_str = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("event_name").and_then(|v| v.as_str()));
            if arg0_int.is_none() && arg0_str.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "Challenge not found".into(),
                }));
            }
            let amount_opt = kwargs
                .get("amount")
                .and_then(|v| v.as_i64());
            let amount = amount_opt.unwrap_or(1);
            if amount <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidEventProgress".into(),
                    message: "Event progress increment must be positive".into(),
                }));
            }
            let _extra_data = kwargs.get("extra_data");
            let res = with_season_state(ctx, char_id, |st, snap| {
                let defs = load_challenge_definitions(snap);
                ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                sync_character_challenges(st, snap, char_id);
                let ch_id = if let Some(id) = arg0_int {
                    id
                } else {
                    let s = arg0_str.unwrap();
                    if let Some(d) = defs.iter().find(|d| d.event_type == s || d.title == s) {
                        d.challenge_id
                    } else if let Some(c) = st.challenges.values().find(|c| c.title == s) {
                        c.challenge_id
                    } else {
                        return Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: "Challenge not found".into(),
                        });
                    }
                };
                let Some(ch) = st.challenges.get_mut(&ch_id) else {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                    });
                };
                let def = defs.iter().find(|d| d.challenge_id == ch_id);
                let is_client_driven = def.map(|d| d.client_event_driven).unwrap_or(ch.client_event_driven);

                if !is_client_driven {
                    return Err(BridgeRpcError {
                        code: "InvalidChallengeEvent".into(),
                        message: "Server-side activity challenge cannot be progressed via client events".into(),
                    });
                }
                ch.progress = ch.progress.saturating_add(amount).min(ch.max_progress);
                if ch.progress >= ch.max_progress {
                    ch.completed = true;
                }
                Ok(PyRep::None)
            });
            match res {
                Ok(rep) => Some(Ok(rep)),
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "claim_challenge_rewards") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let session_dock = session.station_id.or(session.structure_id);
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1002);
            let res = with_season_state(ctx, char_id, |st, snap| {
                let target_location = session_dock
                    .or_else(|| {
                        snap.characters
                            .get(&char_id)
                            .and_then(|c| c.station_id.or(c.structure_id))
                    })
                    .ok_or_else(|| BridgeRpcError::user_error("MustBeDocked"))?;
                let defs = load_challenge_definitions(snap);
                ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                sync_character_challenges(st, snap, char_id);
                let Some(ch) = st.challenges.get_mut(&ch_id) else {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                    });
                };
                if ch.claimed {
                    return Err(BridgeRpcError {
                        code: "ChallengeAlreadyClaimed".into(),
                        message: "Challenge rewards already claimed".into(),
                    });
                }
                if !ch.completed && ch.progress < ch.max_progress {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotCompleted".into(),
                        message: "Challenge is not yet completed".into(),
                    });
                }
                ch.completed = true;
                ch.claimed = true;
                st.season_points += ch.points;
                let pts = st.season_points;
                let reward_pts = ch.points;
                let delivered_rep = if !ch.client_event_driven && ch.reward_type_id > 0 && ch.reward_quantity > 0 {
                    snap.next_entity_id += 1;
                    let reward_item_id = snap.next_entity_id;
                    snap.items.insert(
                        reward_item_id,
                        eve_db::InventoryItemRecord {
                            item_id: reward_item_id,
                            type_id: ch.reward_type_id,
                            owner_id: char_id,
                            location_id: target_location,
                            flag: eve_db::InventoryFlag::Hangar,
                            quantity: ch.reward_quantity,
                            singleton: false,
                            ..Default::default()
                        },
                    );
                    PyRep::Integer(reward_item_id)
                } else {
                    PyRep::None
                };

                let rep = PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)),
                    ("claimed", PyRep::Bool(true)),
                    ("seasonPoints", PyRep::Integer(pts)),
                    ("rewardGranted", PyRep::Bool(true)),
                    ("rewardPoints", PyRep::Integer(reward_pts)),
                    ("deliveredItemID", delivered_rep),
                ]);
                Ok(rep)
            });

            match res {
                Ok(rep) => Some(Ok(rep)),
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_loot_table_by_typeID") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(type_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|n| u32::try_from(n).ok())
                .filter(|&tid| tid > 0 && ctx.sde.types.contains_key(&tid))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTypeId".into(),
                    message: "Invalid or unknown type ID".into(),
                }));
            };
            let custom_loot: Option<Vec<(u32, i64, f64)>> = ctx.db.with_snapshot(|snap| {
                snap.service_state_blobs
                    .get(SEASON_LOOT_TABLES_KEY)
                    .and_then(|s| serde_json::from_str::<HashMap<u32, Vec<(u32, i64, f64)>>>(s).ok())
                    .and_then(|m| m.get(&type_id).cloned())
            });
            let entries = custom_loot.unwrap_or_else(|| {
                fsd_crate_loot_tables(&ctx.sde)
                    .get(&type_id)
                    .cloned()
                    .unwrap_or_default()
            });
            let list: Vec<PyRep> = entries
                .into_iter()
                .map(|(tid, qty, prob)| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("quantity", PyRep::Integer(qty)),
                        ("probability", PyRep::Real(prob)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(list)))
        }
        ("seasonManager", "reload_fsd_data") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = ServerContext::require_gm_role(session) {
                return Some(Err(e));
            }
            let res = with_season_state(ctx, char_id, |st, snap| {
                let defs = default_seasonal_challenge_definitions();
                if let Ok(json) = serde_json::to_string(&defs) {
                    snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), json);
                }
                let seasons = default_fsd_seasons(&ctx.sde, ctx.now_blueos_filetime_ticks());
                if let Ok(json) = serde_json::to_string(&seasons) {
                    snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), json);
                }
                ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    }
}
