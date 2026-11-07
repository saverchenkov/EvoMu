//! V24 Season Manager localization, FSD schedule resolution, expiration timing,
//! and challenge lifecycle helpers.
#![forbid(unsafe_code)]

use crate::services::season_fsd::{
    fsd_challenge_definitions, SeasonalChallengeDef,
};
use crate::services::season_manager::{
    ensure_character_challenges_with_limit, get_current_activity_count, CharacterChallenge,
    CharacterSeasonState, SeasonConfig, SEASONAL_CHALLENGES_CONFIG_KEY, SEASONS_CATALOG_KEY,
    SEASON_CONFIG_KEY,
};
use eve_db::WorldStateSnapshot;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;
const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
const DEFAULT_SEASON_BASE_FT: i64 = 133_500_000_000_000_000;
const DEFAULT_SEASON_DURATION_FT: i64 = 30 * 86_400 * FILETIME_TICKS_PER_SEC;

/// Cached map of `challenge_id -> (localized_title, localized_description, display_name_id, message_text_id)`
/// resolved directly from `challenges.fsdbinary` and `localization_fsd_en-us.pickle`.
fn fsd_localized_challenge_texts(
    sde: &SdeRegistry,
) -> &'static HashMap<i64, (String, String, i64, i64)> {
    static TEXT_MAP: OnceLock<HashMap<i64, (String, String, i64, i64)>> = OnceLock::new();
    TEXT_MAP.get_or_init(|| {
        let mut out = HashMap::new();
        let msg_table = sde.load_all_localized_messages_en_us().unwrap_or_default();
        if let Ok(raw) = sde.challenges() {
            for (k, v) in raw {
                let Ok(cid) = k.parse::<i64>() else {
                    continue;
                };
                let disp_id = v.get("display_name_id").and_then(|n| n.as_i64()).unwrap_or(0);
                let msg_id = v.get("message_text_id").and_then(|n| n.as_i64()).unwrap_or(0);
                let title = u32::try_from(disp_id)
                    .ok()
                    .and_then(|id| msg_table.get(&id).cloned())
                    .unwrap_or_default();
                let desc = u32::try_from(msg_id)
                    .ok()
                    .and_then(|id| msg_table.get(&id).cloned())
                    .unwrap_or_default();
                out.insert(cid, (title, desc, disp_id, msg_id));
            }
        }
        out
    })
}

/// Apply authentic English localization strings from `localization_fsd_en-us.pickle` to a
/// `SeasonalChallengeDef`, replacing any unresolved `Category #id (msg:...)` placeholders.
pub fn localize_challenge_def(def: &mut SeasonalChallengeDef, sde: &SdeRegistry) {
    if let Some((loc_title, loc_desc, disp_id, msg_id)) =
        fsd_localized_challenge_texts(sde).get(&def.challenge_id)
    {
        if !loc_title.is_empty()
            && (def.title.is_empty() || def.title.contains("(msg:"))
        {
            def.title = loc_title.clone();
        } else if (def.title.is_empty() || def.title.contains("(msg:")) && *disp_id > 0 {
            if let Some(t) = u32::try_from(*disp_id)
                .ok()
                .and_then(|id| sde.localized_message_en_us(id))
            {
                if !t.is_empty() {
                    def.title = t;
                }
            }
        }
        if !loc_desc.is_empty()
            && (def.description.is_empty() || def.description.starts_with("msg:"))
        {
            def.description = loc_desc.clone();
        } else if (def.description.is_empty() || def.description.starts_with("msg:")) && *msg_id > 0
        {
            if let Some(d) = u32::try_from(*msg_id)
                .ok()
                .and_then(|id| sde.localized_message_en_us(id))
            {
                if !d.is_empty() {
                    def.description = d;
                }
            }
        }
    }
}

/// Load challenge definitions for `snap`, enriching FSD challenges with localized titles/descriptions.
pub fn load_localized_challenge_definitions(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> Vec<SeasonalChallengeDef> {
    if let Some(json) = snap.service_state_blobs.get(SEASONAL_CHALLENGES_CONFIG_KEY) {
        if let Ok(mut defs) = serde_json::from_str::<Vec<SeasonalChallengeDef>>(json) {
            if !defs.is_empty() {
                for def in &mut defs {
                    localize_challenge_def(def, sde);
                }
                return defs;
            }
        }
    }
    let mut defs = fsd_challenge_definitions(sde).to_vec();
    for def in &mut defs {
        localize_challenge_def(def, sde);
    }
    defs
}

/// Compute the next EVE daily downtime (`11:00 UTC`) in Windows FILETIME ticks at or after `now_ft`.
pub fn next_downtime_filetime_ticks(now_ft: i64) -> i64 {
    let now_unix = (now_ft / FILETIME_TICKS_PER_SEC).saturating_sub(EPOCH_DIFF_SECS).max(0);
    let dt_offset = 11 * 3_600_i64;
    let shifted = now_unix.saturating_sub(dt_offset);
    let next_day_unix = (shifted / 86_400 + 1) * 86_400 + dt_offset;
    (next_day_unix + EPOCH_DIFF_SECS) * FILETIME_TICKS_PER_SEC
}

/// Build an FSD season catalog from `seasonselection.fsdbinary`, `community_track_templates.fsdbinary`,
/// and `seasonalcampaignsloginrewards.fsdbinary` without inventing ordinal `2..=4` aliases.
pub fn load_fsd_seasons_catalog_clean(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    anchor_ft: i64,
) -> BTreeMap<i64, SeasonConfig> {
    if let Some(json) = snap.service_state_blobs.get(SEASONS_CATALOG_KEY) {
        if let Ok(mut parsed) = serde_json::from_str::<BTreeMap<i64, SeasonConfig>>(json) {
            if let Some(cfg_json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
                if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(cfg_json) {
                    apply_season_override(&mut parsed, &cfg);
                }
            }
            return parsed;
        }
    }

    let sel = sde
        .load_cfsd_nested("res:/staticdata/seasonselection.fsdbinary")
        .unwrap_or_default();
    let nav_path = sel
        .get("navigationCardPicturePath")
        .and_then(|v| v.as_str())
        .unwrap_or("res:/UI/Texture/classes/seasons/agency/seasonSelectionCard.png")
        .to_string();
    let title_id = sel
        .get("seasonSelectionTitleID")
        .and_then(|v| v.as_i64())
        .unwrap_or(583_333);
    let desc_id = sel
        .get("seasonSelectionDescriptionID")
        .and_then(|v| v.as_i64())
        .unwrap_or(583_334);
    let hint_id = sel
        .get("seasonSelectionHintID")
        .and_then(|v| v.as_i64())
        .unwrap_or(583_335);
    let large_title_id = sel
        .get("agencyLargeSeasonSelectionTitleID")
        .and_then(|v| v.as_i64())
        .unwrap_or(583_380);

    let (base_start_ft, base_end_ft) = active_fsd_epoch_window_ft(anchor_ft);
    let default_max_ch = 4_i64;
    let mut out = BTreeMap::new();

    if let Ok(tracks) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
        let msg_table = sde.load_all_localized_messages_en_us().unwrap_or_default();
        for (id_str, val) in tracks {
            let Ok(sid) = id_str.parse::<i64>() else {
                continue;
            };
            let sname = val
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    u32::try_from(title_id)
                        .ok()
                        .and_then(|id| msg_table.get(&id).cloned())
                })
                .unwrap_or_else(|| format!("Season {sid}"));
            let goal_pts = val.get("goal").and_then(|v| v.as_i64()).unwrap_or(100);
            out.insert(
                sid,
                SeasonConfig {
                    season_id: sid,
                    template_id: sid,
                    max_active_challenges: default_max_ch,
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

    if let Ok(campaigns) =
        sde.load_cfsd_nested("res:/staticdata/seasonalcampaignsloginrewards.fsdbinary")
    {
        let msg_table = sde.load_all_localized_messages_en_us().unwrap_or_default();
        for (k, val) in campaigns {
            let Ok(cid) = k.parse::<i64>() else {
                continue;
            };
            let cname_id = val
                .get("campaignName")
                .and_then(|v| v.as_i64())
                .unwrap_or(title_id);
            let loc_name = u32::try_from(cname_id)
                .ok()
                .and_then(|id| msg_table.get(&id).cloned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Seasonal Campaign {cid}"));
            let bg = val
                .get("windowHeaderBackground")
                .and_then(|v| v.as_str())
                .unwrap_or(nav_path.as_str())
                .to_string();
            out.insert(
                cid,
                SeasonConfig {
                    season_id: cid,
                    template_id: cid,
                    max_active_challenges: default_max_ch,
                    max_season_points: 1_000,
                    season_name: loc_name,
                    title_message_id: cname_id,
                    description_message_id: desc_id,
                    hint_message_id: hint_id,
                    large_selection_title_id: large_title_id,
                    navigation_card_picture_path: nav_path.clone(),
                    background_no_logo: bg,
                    start_time_ft: base_start_ft,
                    end_time_ft: base_end_ft,
                    multiple_season_running: true,
                },
            );
        }
    }

    if let Some(cfg_json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
        if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(cfg_json) {
            apply_season_override(&mut out, &cfg);
        }
    }
    out
}

fn apply_season_override(catalog: &mut BTreeMap<i64, SeasonConfig>, cfg: &SeasonConfig) {
    let mut merged = catalog.get(&cfg.season_id).cloned().unwrap_or_default();
    merged.season_id = cfg.season_id;
    merged.max_active_challenges = cfg.max_active_challenges;
    if cfg.max_season_points > 0 {
        merged.max_season_points = cfg.max_season_points;
    }
    if !cfg.season_name.is_empty() {
        merged.season_name = cfg.season_name.clone();
    }
    if cfg.end_time_ft > merged.end_time_ft {
        merged.start_time_ft = cfg.start_time_ft;
        merged.end_time_ft = cfg.end_time_ft;
    }
    catalog.insert(merged.season_id, merged);
}

/// Deterministic FSD season cadence epoch window `(start_ft, end_ft)` covering `now_ft`.
pub fn active_fsd_epoch_window_ft(now_ft: i64) -> (i64, i64) {
    let idx = (now_ft - DEFAULT_SEASON_BASE_FT).div_euclid(DEFAULT_SEASON_DURATION_FT);
    let start_ft = DEFAULT_SEASON_BASE_FT + idx * DEFAULT_SEASON_DURATION_FT;
    let end_ft = start_ft + DEFAULT_SEASON_DURATION_FT;
    (start_ft, end_ft)
}

/// Resolve active season schedule end FILETIME ticks without mutating snapshot state.
pub fn resolve_season_end_time_readonly(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    selected_season_id: i64,
    now_ft: i64,
) -> Option<i64> {
    if let Some(json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(json) {
            if let Some(explicit_end_ft) = raw.get("end_time_ft").and_then(|v| v.as_i64()) {
                if explicit_end_ft > 0 {
                    return (explicit_end_ft > now_ft).then_some(explicit_end_ft);
                }
            }
        }
    }
    let catalog = load_fsd_seasons_catalog_clean(snap, sde, now_ft);
    let cfg = if selected_season_id > 0 {
        catalog.get(&selected_season_id)
    } else {
        catalog.values().find(|c| c.end_time_ft > now_ft)
    };
    cfg.and_then(|c| (c.end_time_ft > now_ft).then_some(c.end_time_ft))
}

/// Resolve active `(season_end_ft, max_active_challenges)` for challenge generation and progress tracking.
pub fn resolve_season_bounds_clean(
    st: &CharacterSeasonState,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    now_ft: i64,
) -> (i64, usize) {
    let max_override = snap
        .service_state_blobs
        .get(SEASON_CONFIG_KEY)
        .and_then(|json| serde_json::from_str::<SeasonConfig>(json).ok())
        .map(|cfg| cfg.max_active_challenges.max(1));
    let catalog = load_fsd_seasons_catalog_clean(snap, sde, now_ft);
    let cfg = catalog
        .get(&st.selected_season_id)
        .or_else(|| catalog.values().find(|c| c.end_time_ft > now_ft))
        .or_else(|| catalog.values().next());
    let (_, default_end_ft) = active_fsd_epoch_window_ft(now_ft);
    let end_ft = cfg
        .map(|c| c.end_time_ft)
        .filter(|&ft| ft > now_ft)
        .unwrap_or(default_end_ft);
    let max_ch = max_override
        .or_else(|| cfg.map(|c| c.max_active_challenges.max(1)))
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(4);
    (end_ft, max_ch)
}

/// Compute effective expiration FILETIME for a challenge based on its FSD `expiration_time_minutes`,
/// `should_not_expire_at_downtime`, and active season `season_end_ft`.
pub fn compute_challenge_expiration_ft(
    def: &SeasonalChallengeDef,
    now_ft: i64,
    season_end_ft: i64,
) -> i64 {
    if def.expiration_time_minutes > 0 {
        let dur_ft = def.expiration_time_minutes * 60 * FILETIME_TICKS_PER_SEC;
        let exp = now_ft.saturating_add(dur_ft);
        if season_end_ft > now_ft {
            return exp.min(season_end_ft);
        }
        return exp;
    }
    if !def.should_not_expire_at_downtime {
        let dt_ft = next_downtime_filetime_ticks(now_ft);
        if season_end_ft > now_ft {
            return dt_ft.min(season_end_ft);
        }
        return dt_ft;
    }
    if season_end_ft > now_ft {
        season_end_ft
    } else {
        now_ft.saturating_add(86_400 * FILETIME_TICKS_PER_SEC)
    }
}

/// Instantiate a localized `CharacterChallenge` with FSD-derived expiration and activity baseline.
pub fn build_character_challenge(
    def: &SeasonalChallengeDef,
    season_id: i64,
    snap: &WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
    season_end_ft: i64,
    is_dormant: bool,
) -> CharacterChallenge {
    let mut ch = def.to_character_challenge_with_end(season_id, now_ft, season_end_ft);
    ch.is_dormant = is_dormant;
    ch.expiration_date = compute_challenge_expiration_ft(def, now_ft, season_end_ft);
    ch.baseline_progress = get_current_activity_count(&def.event_type, snap, char_id);
    ch
}

/// Fill character challenges up to `max_active` and normalize expiration dates & localized metadata.
pub fn fill_and_normalize_character_challenges(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
    season_end_ft: i64,
    max_active: usize,
) -> Vec<(i64, CharacterChallenge)> {
    let mut expired = ensure_character_challenges_with_limit(
        st,
        defs,
        snap,
        char_id,
        now_ft,
        season_end_ft,
        max_active,
    );
    let def_map: HashMap<i64, &SeasonalChallengeDef> =
        defs.iter().map(|d| (d.challenge_id, d)).collect();
    for ch in st.challenges.values_mut() {
        if let Some(def) = def_map.get(&ch.challenge_id) {
            if !def.title.is_empty() && (ch.title.is_empty() || ch.title.contains("(msg:")) {
                ch.title = def.title.clone();
            }
            if !def.description.is_empty()
                && (ch.description.is_empty() || ch.description.starts_with("msg:"))
            {
                ch.description = def.description.clone();
            }
        }
    }
    for (_, new_ch) in &mut expired {
        if let Some(def) = def_map.get(&new_ch.challenge_id) {
            new_ch.expiration_date = compute_challenge_expiration_ft(def, now_ft, season_end_ft);
            if let Some(live) = st.challenges.get_mut(&new_ch.challenge_id) {
                live.expiration_date = new_ch.expiration_date;
            }
        }
    }
    expired
}
