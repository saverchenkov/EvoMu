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

/// Season configuration structure supporting dynamic challenge limits (values below are SYNTHETIC).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SeasonConfig {
    pub season_id: i64,
    pub max_active_challenges: i64,
    pub season_name: String,
}

impl Default for SeasonConfig {
    fn default() -> Self {
        Self {
            season_id: 1,
            max_active_challenges: 4,
            season_name: "Yoiul Festival".into(),
        }
    }
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

// SYNTHETIC: V24 seasonal challenge definitions and reward crate type IDs used when live seasonal campaign FSD data is not active.
pub fn default_seasonal_challenge_definitions() -> Vec<SeasonalChallengeDef> {
    vec![
        SeasonalChallengeDef {
            challenge_id: 1001,
            season_id: 1,
            event_type: "totalMined".into(),
            title: "Mining".into(),
            description: "Mine ore in asteroid belts".into(),
            points: 25,
            max_progress: 1000,
            client_event_driven: false,
            reward_type_id: 43_682,
            reward_quantity: 1,
            is_dormant: false,
        },
        SeasonalChallengeDef {
            challenge_id: 1002,
            season_id: 1,
            event_type: "destroyHostileNPC".into(),
            title: "Combat".into(),
            description: "Eliminate hostile pirate vessels".into(),
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
            reward_type_id: 43_682,
            reward_quantity: 1,
            is_dormant: false,
        },
    ]
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
        "totalMined" => snap
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
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let max_ch = ctx.db.with_snapshot(|snap| {
                load_season_config(snap).max_active_challenges
            });
            Some(Ok(PyRep::Integer(max_ch)))
        }
        ("seasonManager", "get_season_end_time") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let end_ft = ctx.now_blueos_filetime_ticks() + 30 * 86_400 * 10_000_000;
            Some(Ok(PyRep::Integer(end_ft)))
        }
        ("seasonManager", "select_season") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let season_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            if !(1..=4).contains(&season_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSeasonId".into(),
                    message: format!("Unknown or inactive season ID {season_id}"),
                }));
            }
            let res = with_season_state(ctx, char_id, |st, _snap| {
                st.selected_season_id = season_id;
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_season_data_for_character") => {
            // V24 `seasons/client/seasonservice.pyj:270-280` (`_fetch_season_data`): `None` means no
            // active season (early return); a list is the season-selection list (shuffled); anything
            // else becomes `current_season`, whose attributes (season_id, title, start_time, end_time,
            // tracked_event_types, template_id, news, card picture paths, ...) come from the seasons FSD
            // data, which is not imported. Until it is, report "no active season" via the None path.
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::None))
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
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()));
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
            let type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(587);
            if type_id <= 0 || ctx.sde.types.get(&(type_id as u32)).is_none() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTypeId".into(),
                    message: format!("Invalid or unknown type ID {type_id}"),
                }));
            }
            let mut list = Vec::new();
            if let Some(mats) = ctx.sde.type_materials.get(&(type_id as u32)) {
                for mat in mats {
                    list.push(PyRep::keyval([
                        ("typeID", PyRep::Integer(mat.material_type_id as i64)),
                        ("quantity", PyRep::Integer(mat.quantity as i64)),
                        ("probability", PyRep::Real(1.0)),
                    ]));
                }
            }
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
