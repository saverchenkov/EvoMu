//! `season_manager` — Seasonal event and challenge tracking service (`seasonManager`).
//!
//! Tracks active seasons, character challenges, progress, rewards, and loot tables.
#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
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
            selected_season_id: 0,
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

pub use super::season_fsd::*;

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
        let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
        let season_end_ft = catalog
            .get(&st.selected_season_id)
            .or_else(|| catalog.values().next())
            .map(|c| c.end_time_ft)
            .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
        let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
        let max_active = catalog
            .get(&st.selected_season_id)
            .map(|c| c.max_active_challenges as usize)
            .unwrap_or(4);
        ensure_character_challenges_with_limit(&mut st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
        sync_character_challenges_with_defs(&mut st, &defs, snap, char_id);
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

pub fn ensure_character_challenges_with_limit(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
    season_end_ft: i64,
    max_active: usize,
) {
    let def_map: HashMap<i64, &SeasonalChallengeDef> = defs.iter().map(|d| (d.challenge_id, d)).collect();
    for (cid, existing) in st.challenges.iter_mut() {
        if existing.expiration_date > 0 && now_ft >= existing.expiration_date {
            if let Some(def) = def_map.get(cid) {
                let mut fresh = def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
                fresh.baseline_progress = get_current_activity_count(&def.event_type, snap, char_id);
                *existing = fresh;
            }
        }
    }
    let mut active_count = st
        .challenges
        .values()
        .filter(|c| !c.is_dormant && !c.claimed)
        .count();
    for def in defs {
        if active_count >= max_active {
            break;
        }
        if def.is_dormant || st.challenges.contains_key(&def.challenge_id) {
            continue;
        }
        let mut ch = def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
        ch.baseline_progress = get_current_activity_count(&def.event_type, snap, char_id);
        st.challenges.insert(def.challenge_id, ch);
        active_count += 1;
    }
}

pub fn ensure_character_challenges(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
) {
    ensure_character_challenges_with_limit(
        st,
        defs,
        snap,
        char_id,
        now_ft,
        now_ft + 30 * 86_400 * 10_000_000,
        4,
    );
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

pub fn sync_character_challenges_with_defs(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) {
    let def_map: HashMap<i64, &SeasonalChallengeDef> = defs.iter().map(|d| (d.challenge_id, d)).collect();
    for ch in st.challenges.values_mut() {
        if ch.is_dormant {
            continue;
        }
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

pub fn sync_character_challenges(
    st: &mut CharacterSeasonState,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) {
    let defs = load_challenge_definitions(snap);
    sync_character_challenges_with_defs(st, &defs, snap, char_id);
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
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                defs.iter().filter(|d| !d.is_dormant).count().min(4).max(1) as i64
            });
            Some(Ok(PyRep::Integer(max_ch)))
        }
        ("seasonManager", "get_season_end_time") => {
            // DATA GAP: live season schedule timestamps are server-only; catalog seeds a 30-day
            // window from init_start_ft when unconfigured (rpc:seasonManager.get_season_end_time)
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let end_ft_opt = ctx.db.with_mut_snapshot(|snap| {
                let st = load_season_state_from_blobs(snap, char_id);
                let now_ft = ctx.now_blueos_filetime_ticks();
                let unseeded = !snap.service_state_blobs.contains_key(SEASONS_CATALOG_KEY);
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let cfg_opt = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().find(|c| c.end_time_ft > now_ft))
                    .cloned();
                if unseeded {
                    if let Ok(json) = serde_json::to_string(&catalog) {
                        snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), json);
                    }
                }
                cfg_opt.and_then(|c| (c.end_time_ft > now_ft).then_some(c.end_time_ft))
            });
            Some(Ok(end_ft_opt.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("seasonManager", "select_season") => {
            // DATA GAP: live season schedule and ordinal season aliases 2..=4 are server-derived
            // when not configured in service_state_blobs (rpc:seasonManager.select_season)
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(season_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSeasonId".into(),
                    message: "select_season requires an integer season ID".into(),
                }));
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let Some(active_cfg) = catalog.get(&season_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "InvalidSeasonId".into(),
                        message: format!("Unknown or inactive season ID {season_id}"),
                    });
                };
                if active_cfg.end_time_ft <= now_ft {
                    return Err(BridgeRpcError {
                        code: "InvalidSeasonId".into(),
                        message: format!("Season ID {season_id} has already ended"),
                    });
                }
                st.selected_season_id = season_id;
                for ch in st.challenges.values_mut() {
                    ch.season_id = season_id;
                }
                snap.service_state_blobs
                    .insert(format!("active_season:{char_id}"), season_id.to_string());

                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                let season_data_kv = season_config_to_keyval(&active_cfg, &defs);
                let character_progress_kv = PyRep::keyval([
                    ("character_id", PyRep::Integer(char_id)),
                    ("characterID", PyRep::Integer(char_id)),
                    ("season_id", PyRep::Integer(season_id)),
                    ("seasonID", PyRep::Integer(season_id)),
                    ("season_points", PyRep::Integer(st.season_points)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                    (
                        "challenges",
                        PyRep::List(
                            st.challenges
                                .values()
                                .map(|c| challenge_to_keyval(c, st.season_points))
                                .collect(),
                        ),
                    ),
                ]);
                Ok((season_data_kv, character_progress_kv))
            });
            match res {
                Ok((season_data_kv, character_progress_kv)) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSeasonSelected",
                        "charid",
                        vec![
                            PyRep::Integer(char_id),
                            season_data_kv,
                            character_progress_kv,
                        ],
                    );
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_season_data_for_character") => {
            // DATA GAP: live season schedule, max_active_challenges=4, and campaign
            // max_season_points=1000 are server-derived when unconfigured (rpc:seasonManager.get_season_data_for_character)
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
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                let active: Vec<&SeasonConfig> = catalog
                    .values()
                    .filter(|cfg| cfg.end_time_ft > now_ft)
                    .collect();
                if active.is_empty() {
                    PyRep::None
                } else if st.selected_season_id == 0 && active.len() > 1 {
                    let list: Vec<PyRep> = active
                        .into_iter()
                        .map(|cfg| season_config_to_keyval(cfg, &defs))
                        .collect();
                    PyRep::List(list)
                } else if let Some(cfg) = catalog
                    .get(&st.selected_season_id)
                    .filter(|c| c.end_time_ft > now_ft)
                    .or_else(|| active.first().copied())
                {
                    season_config_to_keyval(cfg, &defs)
                } else {
                    PyRep::None
                }
            });
            Some(Ok(rep))
        }
        ("seasonManager", "fill_up_and_return_challenges_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let season_end_ft = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().next())
                    .map(|c| c.end_time_ft)
                    .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                let max_active = catalog
                    .get(&st.selected_season_id)
                    .map(|c| c.max_active_challenges as usize)
                    .unwrap_or(4);
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                ensure_character_challenges_with_limit(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
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
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "get_challenge requires a challenge ID".into(),
                }));
            };
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id) {
                    Ok(challenge_to_keyval(c, st.season_points))
                } else {
                    let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                        let season_end_ft = catalog
                            .get(&st.selected_season_id)
                            .or_else(|| catalog.values().next())
                            .map(|c| c.end_time_ft)
                            .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                        let ch = def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
                        Ok(challenge_to_keyval(&ch, st.season_points))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                        })
                    }
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
                    let is_dormant = c.is_dormant || (c.completed && !c.claimed);
                    Ok(PyRep::Bool(is_dormant))
                } else {
                    let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        Ok(PyRep::Bool(def.is_dormant))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                        })
                    }
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
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id) {
                    Ok(PyRep::Integer(c.expiration_date))
                } else {
                    let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                        let season_end_ft = catalog
                            .get(&st.selected_season_id)
                            .or_else(|| catalog.values().next())
                            .map(|c| c.end_time_ft)
                            .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                        let ch = def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
                        Ok(PyRep::Integer(ch.expiration_date))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                        })
                    }
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
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(1).and_then(|v| v.as_i64()));
            let amount = amount_opt.unwrap_or(1);
            if amount <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidEventProgress".into(),
                    message: "Event progress increment must be positive".into(),
                }));
            }
            let _extra_data = kwargs.get("extra_data");
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let season_end_ft = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().next())
                    .map(|c| c.end_time_ft)
                    .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                let max_active = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().next())
                    .map(|c| c.max_active_challenges.max(1) as usize)
                    .unwrap_or(4);
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                ensure_character_challenges_with_limit(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                let ch_id = if let Some(id) = arg0_int {
                    id
                } else {
                    let s = arg0_str.unwrap();
                    if let Some(c) = st.challenges.values().find(|c| !c.is_dormant && c.title == s) {
                        c.challenge_id
                    } else if let Some(d) = defs.iter().find(|d| d.event_type == s && st.challenges.get(&d.challenge_id).is_some_and(|c| !c.is_dormant)) {
                        d.challenge_id
                    } else if let Some(d) = defs.iter().find(|d| d.event_type == s || d.title == s) {
                        d.challenge_id
                    } else {
                        return Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: "Challenge not found".into(),
                        });
                    }
                };
                let def_opt = defs.iter().find(|d| d.challenge_id == ch_id);
                let Some(ch) = st.challenges.get_mut(&ch_id) else {
                    if def_opt.is_some_and(|d| !d.client_event_driven) {
                        return Err(BridgeRpcError {
                            code: "InvalidChallengeEvent".into(),
                            message: "Server-side activity challenge cannot be progressed via client events".into(),
                        });
                    }
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                    });
                };
                if ch.is_dormant {
                    return Err(BridgeRpcError {
                        code: "ChallengeDormant".into(),
                        message: "Dormant challenge cannot be progressed".into(),
                    });
                }
                let is_client_driven = def_opt.map(|d| d.client_event_driven).unwrap_or(ch.client_event_driven);

                if !is_client_driven {
                    return Err(BridgeRpcError {
                        code: "InvalidChallengeEvent".into(),
                        message: "Server-side activity challenge cannot be progressed via client events".into(),
                    });
                }
                let was_completed = ch.completed;
                let step = amount.min(1);
                ch.progress = ch.progress.saturating_add(step).min(ch.max_progress);
                if ch.progress >= ch.max_progress {
                    ch.completed = true;
                }
                Ok((ch_id, ch.progress, !was_completed && ch.completed))
            });
            match res {
                Ok((cid, new_prog, newly_completed)) => {
                    ctx.notify(NotificationTarget::Character(char_id), "OnChallengeProgressUpdate", "charid", vec![PyRep::Integer(cid), PyRep::Integer(new_prog)]);
                    if newly_completed {
                        ctx.notify(NotificationTarget::Character(char_id), "OnChallengeCompleted", "charid", vec![PyRep::Integer(cid)]);
                    }
                    Some(Ok(PyRep::None))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "claim_challenge_rewards") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let session_dock = session.station_id.or(session.structure_id);
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "claim_challenge_rewards requires a challenge ID".into(),
                }));
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let target_location = session_dock
                    .or_else(|| {
                        snap.characters
                            .get(&char_id)
                            .and_then(|c| c.station_id.or(c.structure_id))
                    })
                    .ok_or_else(|| BridgeRpcError::user_error("MustBeDocked"))?;
                let now_ft = ctx.now_blueos_filetime_ticks();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let season_end_ft = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().next())
                    .map(|c| c.end_time_ft)
                    .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                let max_active = catalog
                    .get(&st.selected_season_id)
                    .or_else(|| catalog.values().next())
                    .map(|c| c.max_active_challenges.max(1) as usize)
                    .unwrap_or(4);
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                ensure_character_challenges_with_limit(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                let def_opt = defs.iter().find(|d| d.challenge_id == ch_id).cloned();
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                let Some(ch) = st.challenges.get_mut(&ch_id) else {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                    });
                };
                if ch.is_dormant {
                    return Err(BridgeRpcError {
                        code: "ChallengeDormant".into(),
                        message: "Dormant challenge cannot be claimed".into(),
                    });
                }
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
                let (prev_pts, reward_pts, season_id) = (st.season_points, ch.points, st.selected_season_id);
                let (r_tid, r_qty, client_driven) = (ch.reward_type_id, ch.reward_quantity, ch.client_event_driven);
                st.season_points += reward_pts;
                let pts = st.season_points;
                if let Some(ref def) = def_opt {
                    if def.isk_awarded > 0 {
                        if let Some(char_rec) = snap.characters.get_mut(&char_id) {
                            char_rec.balance = eve_db::IskCents(
                                char_rec
                                    .balance
                                    .0
                                    .saturating_add(eve_db::IskCents::from_isk(def.isk_awarded).0),
                            );
                        }
                    }
                    if let Some(next_id) = def.next_challenge {
                        if let Some(next_def) = defs.iter().find(|d| d.challenge_id == next_id) {
                            let mut next_ch = next_def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
                            next_ch.is_dormant = false;
                            next_ch.baseline_progress = get_current_activity_count(&next_def.event_type, snap, char_id);
                            st.challenges.insert(next_id, next_ch);
                        }
                    }
                }
                let is_omega = snap.characters.get(&char_id).map(|c| c.clone_grade >= 2).unwrap_or(false);
                let delivered_rep = if !client_driven && r_tid > 0 && r_qty > 0 {
                    snap.next_entity_id += 1;
                    let reward_item_id = snap.next_entity_id;
                    snap.items.insert(reward_item_id, eve_db::InventoryItemRecord {
                        item_id: reward_item_id, type_id: r_tid, owner_id: char_id,
                        location_id: target_location, flag: eve_db::InventoryFlag::Hangar,
                        quantity: r_qty, singleton: false, ..Default::default()
                    });
                    PyRep::Integer(reward_item_id)
                } else {
                    PyRep::None
                };
                let rep = PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)), ("claimed", PyRep::Bool(true)),
                    ("seasonPoints", PyRep::Integer(pts)), ("rewardGranted", PyRep::Bool(true)),
                    ("rewardPoints", PyRep::Integer(reward_pts)), ("deliveredItemID", delivered_rep),
                ]);
                Ok((rep, season_id, prev_pts, pts, is_omega))
            });
            match res {
                Ok((rep, season_id, prev_pts, pts, is_omega)) => {
                    ctx.notify(NotificationTarget::Character(char_id), "OnChallengeRewardsGranted", "charid", vec![PyRep::Integer(ch_id)]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnSeasonalPointsUpdated", "charid", vec![PyRep::Integer(season_id), PyRep::Integer(pts)]);
                    // DATA GAP: V24 seasonselection.fsdbinary ships no static reward-track point thresholds or clone-grade requirements
                    let (prev_goal, new_goal) = (prev_pts / 10, pts / 10);
                    if new_goal > prev_goal {
                        for g_idx in prev_goal..new_goal {
                            let goal_data = PyRep::Dict(vec![
                                (PyRep::String("goal_index".into()), PyRep::Integer(g_idx)),
                                (PyRep::String("points_required".into()), PyRep::Integer((g_idx + 1) * 10)),
                                (PyRep::String("completed".into()), PyRep::Bool(true)),
                            ]);
                            ctx.notify(NotificationTarget::Character(char_id), "OnSeasonalGoalCompleted", "charid", vec![PyRep::Integer(g_idx), goal_data]);
                        }
                        if is_omega {
                            let claimed_idxs: Vec<PyRep> = (prev_goal..new_goal).map(PyRep::Integer).collect();
                            ctx.notify(NotificationTarget::Character(char_id), "OnSeasonOmegaRewardsClaimed", "charid", vec![PyRep::List(claimed_idxs)]);
                        }
                    }
                    Some(Ok(rep))
                }
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
        ("seasonManager", "get_character_progress_by_character_id") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(raw_goal_id) = args.first().and_then(|a| a.as_i64()) {
                let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidGoalId".into(),
                        message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                    }));
                };
                let milestone = crate::services::quasar_career_goals::default_career_milestones()
                    .into_iter()
                    .find(|m| m.milestone_id == goal_id);
                let sde_goal = ctx.sde.career_goals.get(&goal_id);
                if milestone.is_none() && sde_goal.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: format!("Goal or challenge {goal_id} not found"),
                    }));
                }
                let is_mining_career = milestone
                    .as_ref()
                    .is_some_and(|m| m.career_id == Some(2))
                    || sde_goal.is_some_and(|g| {
                        g.career_path.eq_ignore_ascii_case("Industrialist")
                            && g.name.to_lowercase().contains("mine")
                    });
                let res = with_season_state(ctx, char_id, |st, snap| {
                    let defs = load_challenge_definitions(snap);
                    ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                    sync_character_challenges(st, snap, char_id);
                    let prog = if is_mining_career {
                        snap.mining_ledger
                            .iter()
                            .filter(|r| r.character_id == char_id)
                            .map(|r| r.quantity.max(0) as u64)
                            .sum()
                    } else {
                        snap.npe_progress
                            .get(&char_id)
                            .and_then(|n| n.career_goal_progress.get(&goal_id).copied())
                            .unwrap_or(0)
                    };
                    let challenges = sorted_challenge_keyvals(st);
                    let claimed_cnt = st.challenges.values().filter(|c| c.claimed).count() as i64;
                    Ok(PyRep::keyval([
                        ("goalID", PyRep::Integer(goal_id as i64)),
                        ("currentProgress", PyRep::Integer(prog as i64)),
                        ("verifiedFromLedger", PyRep::Bool(is_mining_career)),
                        ("season_id", PyRep::Integer(st.selected_season_id)),
                        ("seasonID", PyRep::Integer(st.selected_season_id)),
                        ("total_points", PyRep::Integer(st.season_points)),
                        ("season_points", PyRep::Integer(st.season_points)),
                        ("seasonPoints", PyRep::Integer(st.season_points)),
                        ("current_reward_index", PyRep::Integer(claimed_cnt)),
                        ("challenges", PyRep::List(challenges)),
                    ]))
                });
                return Some(res);
            }

            let res = with_season_state(ctx, char_id, |st, snap| {
                let defs = load_challenge_definitions(snap);
                ensure_character_challenges(st, &defs, snap, char_id, ctx.now_blueos_filetime_ticks());
                sync_character_challenges(st, snap, char_id);
                let challenges = sorted_challenge_keyvals(st);
                let claimed_cnt = st.challenges.values().filter(|c| c.claimed).count() as i64;
                Ok(PyRep::keyval([
                    ("season_id", PyRep::Integer(st.selected_season_id)),
                    ("seasonID", PyRep::Integer(st.selected_season_id)),
                    ("total_points", PyRep::Integer(st.season_points)),
                    ("season_points", PyRep::Integer(st.season_points)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                    ("current_reward_index", PyRep::Integer(claimed_cnt)),
                    ("challenges", PyRep::List(challenges)),
                ]))
            });
            Some(res)
        }
        ("seasonManager", "claim_goal_reward") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGoalId".into(),
                    message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                }));
            };
            let milestones = crate::services::quasar_career_goals::default_career_milestones();
            let base_sp = milestones
                .iter()
                .find(|m| m.career_id.is_none())
                .map(|m| m.sp_reward)
                .unwrap_or(50_000.0);
            let resolved = if let Some(g) = ctx.sde.career_goals.get(&goal_id) {
                let is_mining = g.career_path.eq_ignore_ascii_case("Industrialist")
                    && g.name.to_lowercase().contains("mine");
                Some((
                    g.target_progress,
                    g.sp_reward as f64,
                    is_mining,
                ))
            } else {
                milestones
                    .iter()
                    .find(|m| m.milestone_id == goal_id)
                    .map(|m| {
                        let is_mining = m.career_id == Some(2);
                        let target = ((m.required_points as u64) * 2) / 3;
                        let sp = if is_mining { base_sp } else { m.sp_reward };
                        (target.max(1), sp, is_mining)
                    })
            };
            let Some((target_prog, sp_reward, is_mining_career)) = resolved else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: format!("Goal or challenge {goal_id} not found"),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                let cur_prog = if is_mining_career {
                    let total_mined: u64 = snap
                        .mining_ledger
                        .iter()
                        .filter(|r| r.character_id == char_id)
                        .map(|r| r.quantity.max(0) as u64)
                        .sum();
                    npe.career_goal_progress.insert(goal_id, total_mined);
                    total_mined
                } else {
                    npe.career_goal_progress.get(&goal_id).copied().unwrap_or(0)
                };
                if cur_prog < target_prog {
                    return Err(BridgeRpcError {
                        code: "GoalProgressIncomplete".into(),
                        message: format!(
                            "Career goal {goal_id} requires progress {target_prog}, current is {cur_prog}"
                        ),
                    });
                }
                if !npe.claimed_goals.insert(goal_id) {
                    return Err(BridgeRpcError {
                        code: "GoalAlreadyClaimed".into(),
                        message: format!("Career goal {goal_id} has already been claimed"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points += sp_reward;
                    Ok(ch.free_skill_points)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });
            match res {
                Ok(new_free_sp) => Some(Ok(PyRep::keyval([
                    ("goalID", PyRep::Integer(goal_id as i64)),
                    ("spReward", PyRep::Real(sp_reward)),
                    ("freeSkillPoints", PyRep::Real(new_free_sp)),
                ]))),
                Err(e) => Some(Err(e)),
            }
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
                let now_ft = ctx.now_blueos_filetime_ticks();
                let defs = fsd_challenge_definitions(&ctx.sde).to_vec();
                if let Ok(json) = serde_json::to_string(&defs) {
                    snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), json);
                }
                let seasons = default_fsd_seasons(&ctx.sde, now_ft);
                let season_end_ft = seasons
                    .get(&st.selected_season_id)
                    .or_else(|| seasons.values().next())
                    .map(|c| c.end_time_ft)
                    .unwrap_or(now_ft + 30 * 86_400 * 10_000_000);
                let max_active = seasons
                    .get(&st.selected_season_id)
                    .or_else(|| seasons.values().next())
                    .map(|c| c.max_active_challenges.max(1) as usize)
                    .unwrap_or(4);
                if let Ok(json) = serde_json::to_string(&seasons) {
                    snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), json);
                }
                ensure_character_challenges_with_limit(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                Ok(())
            });
            match res {
                Ok(()) => {
                    let sent = ctx.notify(NotificationTarget::All, "OnSeasonsFsdDataChanged", "charid", vec![]);
                    if !sent.iter().any(|n| n.recipient_char_id == char_id) {
                        ctx.notify(NotificationTarget::Character(char_id), "OnSeasonsFsdDataChanged", "charid", vec![]);
                    }
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    }
}
