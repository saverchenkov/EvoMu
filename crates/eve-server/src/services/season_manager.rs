//! `season_manager` — Seasonal event and challenge tracking service (`seasonManager`).
//!
//! Tracks active seasons, character challenges, progress, rewards, and loot tables.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SEASON_STATE_KEY_PREFIX: &str = "season_state:";

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

fn read_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&CharacterSeasonState) -> R,
{
    let key = format!("{SEASON_STATE_KEY_PREFIX}{char_id}");
    ctx.db.with_snapshot(|snap| {
        let state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<CharacterSeasonState>(json).ok())
            .unwrap_or_default();
        f(&state)
    })
}

fn with_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&mut CharacterSeasonState, &mut eve_db::WorldStateSnapshot) -> R,
{
    let key = format!("{SEASON_STATE_KEY_PREFIX}{char_id}");
    ctx.db.with_mut_snapshot(|snap| {
        let mut state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<CharacterSeasonState>(json).ok())
            .unwrap_or_default();
        let out = f(&mut state, snap);
        if let Ok(json) = serde_json::to_string(&state) {
            snap.service_state_blobs.insert(key, json);
        }
        out
    })
}

fn try_with_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> Result<R, BridgeRpcError>
where
    F: FnOnce(&mut CharacterSeasonState, &mut eve_db::WorldStateSnapshot) -> Result<R, BridgeRpcError>,
{
    let key = format!("{SEASON_STATE_KEY_PREFIX}{char_id}");
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<CharacterSeasonState>(json).ok())
            .unwrap_or_default();
        let out = f(&mut state, snap)?;
        if let Ok(json) = serde_json::to_string(&state) {
            snap.service_state_blobs.insert(key, json);
        }
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

// SYNTHETIC: V24 seasonal challenge definitions and reward crate type IDs used when live seasonal campaign FSD data is not active.
fn default_challenge(ch_id: i64, season_id: i64, now_ft: i64) -> CharacterChallenge {
    let exp = now_ft + 7 * 86_400 * 10_000_000;
    match ch_id {
        1001 => CharacterChallenge {
            challenge_id: 1001,
            season_id,
            title: "Mining Expedition".into(),
            description: "Mine 1,000 units of ore in asteroid belts".into(),
            points: 25,
            progress: 250,
            max_progress: 1000,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
            client_event_driven: false,
            reward_type_id: 43_682,
            reward_quantity: 1,
        },
        1002 => CharacterChallenge {
            challenge_id: 1002,
            season_id,
            title: "Combat Patrol".into(),
            description: "Eliminate 5 pirate ships in combat anomalies".into(),
            points: 50,
            progress: 4,
            max_progress: 5,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
            client_event_driven: true,
            reward_type_id: 0,
            reward_quantity: 0,
        },
        1003 => CharacterChallenge {
            challenge_id: 1003,
            season_id,
            title: "Exploration Survey".into(),
            description: "Scan down 3 cosmic signatures in star systems".into(),
            points: 30,
            progress: 1,
            max_progress: 3,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
            client_event_driven: true,
            reward_type_id: 0,
            reward_quantity: 0,
        },
        _ => CharacterChallenge {
            challenge_id: ch_id,
            season_id,
            title: "Industry Assembly".into(),
            description: "Manufacture 2 items at an industrial facility".into(),
            points: 40,
            progress: 0,
            max_progress: 2,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
            client_event_driven: false,
            reward_type_id: 43_682,
            reward_quantity: 1,
        },
    }
}

fn ensure_challenges(st: &mut CharacterSeasonState, now_ft: i64) {
    if st.challenges.is_empty() {
        for id in [1001, 1002, 1003, 1004] {
            st.challenges.insert(id, default_challenge(id, st.selected_season_id, now_ft));
        }
    }
}

/// Dispatches calls to `seasonManager`.
pub fn handle_season_manager(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "seasonManager" {
        return None;
    }

    match (service, method) {
        ("seasonManager", "get_max_active_challenges") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::Integer(4)))
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
            with_season_state(ctx, char_id, |st, _snap| {
                st.selected_season_id = season_id;
            });
            Some(Ok(PyRep::Bool(true)))
        }
        ("seasonManager", "get_season_data_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rep = read_season_state(ctx, char_id, |st| {
                PyRep::keyval([
                    ("seasonID", PyRep::Integer(st.selected_season_id)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                ])
            });
            Some(Ok(rep))
        }
        ("seasonManager", "fill_up_and_return_challenges_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let challenges = with_season_state(ctx, char_id, |st, _snap| {
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
                let pts = st.season_points;
                let mut list: Vec<_> = st.challenges.values().cloned().collect();
                list.sort_by_key(|c| c.challenge_id);
                list.into_iter().map(|c| challenge_to_keyval(&c, pts)).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(challenges)))
        }
        ("seasonManager", "get_challenge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let res = read_season_state(ctx, char_id, |st| {
                let pts = st.season_points;
                if let Some(c) = st.challenges.get(&ch_id) {
                    Ok(challenge_to_keyval(c, pts))
                } else if (1001..=1004).contains(&ch_id) {
                    let c = default_challenge(ch_id, st.selected_season_id, now_ft);
                    Ok(challenge_to_keyval(&c, pts))
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
            let dormant = read_season_state(ctx, char_id, |st| {
                st.challenges.get(&ch_id).map(|c| c.is_dormant).unwrap_or(false)
            });
            Some(Ok(PyRep::Bool(dormant)))
        }
        ("seasonManager", "get_challenge_expiration_date") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let exp = read_season_state(ctx, char_id, |st| {
                st.challenges
                    .get(&ch_id)
                    .map(|c| c.expiration_date)
                    .unwrap_or_else(|| ctx.now_blueos_filetime_ticks() + 7 * 86_400 * 10_000_000)
            });
            Some(Ok(PyRep::Integer(exp)))
        }
        ("seasonManager", "process_client_event") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1002);
            let amount = if let Some(a) = args.get(1) {
                let Some(val) = a.as_i64() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidEventProgress".into(),
                        message: "Event progress increment must be positive".into(),
                    }));
                };
                if val <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidEventProgress".into(),
                        message: "Event progress increment must be positive".into(),
                    }));
                }
                val
            } else {
                5
            };
            let res = try_with_season_state(ctx, char_id, |st, snap| {
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
                let Some(ch) = st.challenges.get_mut(&ch_id) else {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                    });
                };
                if ch.challenge_id == 1001 {
                    let mined = snap
                        .mining_ledger
                        .iter()
                        .filter(|e| e.character_id == char_id)
                        .map(|e| e.quantity)
                        .sum::<i64>();
                    ch.progress = mined.min(ch.max_progress);
                } else {
                    if !ch.client_event_driven {
                        return Err(BridgeRpcError {
                            code: "InvalidChallengeEvent".into(),
                            message: "Server-side activity challenge cannot be progressed via client events".into(),
                        });
                    }
                    let _ = amount;
                    let inc = 1i64;
                    ch.progress = ch.progress.saturating_add(inc).min(ch.max_progress);
                }
                if ch.progress >= ch.max_progress {
                    ch.completed = true;
                }
                Ok(PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)),
                    ("progress", PyRep::Integer(ch.progress)),
                    ("maxProgress", PyRep::Integer(ch.max_progress)),
                    ("completed", PyRep::Bool(ch.completed)),
                    ("nodeGraphID", PyRep::Integer(473)),
                    ("currentNodeID", PyRep::Integer(2)),
                ]))
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
            let res = try_with_season_state(ctx, char_id, |st, snap| {
                let target_location = session_dock
                    .or_else(|| {
                        snap.characters
                            .get(&char_id)
                            .and_then(|c| c.station_id.or(c.structure_id))
                    })
                    .ok_or_else(|| BridgeRpcError::user_error("MustBeDocked"))?;
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
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
            with_season_state(ctx, char_id, |st, _snap| {
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
            });
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}
