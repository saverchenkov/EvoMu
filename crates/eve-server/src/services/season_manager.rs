//! `season_manager` — Seasonal event and challenge tracking service (`seasonManager`).
//!
//! Tracks active seasons, character challenges, progress, rewards, and loot tables.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SEASON_STATE_NOTE_BASE: i64 = -8_600_000_000_000;
const SEASON_STATE_NOTE_OWNER_ID: i64 = -86;

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

fn with_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&mut CharacterSeasonState) -> R,
{
    let note_id = SEASON_STATE_NOTE_BASE - char_id.abs();
    let mut state = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .get(&note_id)
            .and_then(|n| serde_json::from_str::<CharacterSeasonState>(&n.note).ok())
            .unwrap_or_default()
    });
    let out = f(&mut state);
    if let Ok(json) = serde_json::to_string(&state) {
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.owner_notes.insert(
                note_id,
                eve_db::OwnerNoteRecord {
                    note_id,
                    character_id: SEASON_STATE_NOTE_OWNER_ID,
                    item_id: char_id,
                    label: format!("S:SEASON_STATE:{char_id}"),
                    note: json,
                    timestamp_unix: now_unix,
                },
            );
        });
    }
    out
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
        },
        1002 => CharacterChallenge {
            challenge_id: 1002,
            season_id,
            title: "Combat Patrol".into(),
            description: "Eliminate 5 pirate ships in combat anomalies".into(),
            points: 50,
            progress: 5,
            max_progress: 5,
            completed: true,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
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
            with_season_state(ctx, char_id, |st| {
                st.selected_season_id = season_id;
            });
            Some(Ok(PyRep::Bool(true)))
        }
        ("seasonManager", "fill_up_and_return_challenges_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let challenges = with_season_state(ctx, char_id, |st| {
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
            let kv = with_season_state(ctx, char_id, |st| {
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
                let pts = st.season_points;
                st.challenges
                    .get(&ch_id)
                    .cloned()
                    .map(|c| challenge_to_keyval(&c, pts))
                    .unwrap_or_else(|| {
                        let c = default_challenge(ch_id, st.selected_season_id, ctx.now_blueos_filetime_ticks());
                        challenge_to_keyval(&c, pts)
                    })
            });
            Some(Ok(kv))
        }
        ("seasonManager", "challenge_is_dormant") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let dormant = with_season_state(ctx, char_id, |st| {
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
            let exp = with_season_state(ctx, char_id, |st| {
                st.challenges
                    .get(&ch_id)
                    .map(|c| c.expiration_date)
                    .unwrap_or_else(|| ctx.now_blueos_filetime_ticks() + 7 * 86_400 * 10_000_000)
            });
            Some(Ok(PyRep::Integer(exp)))
        }
        ("seasonManager", "claim_challenge_rewards") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1002);
            let res = with_season_state(ctx, char_id, |st| {
                ensure_challenges(st, ctx.now_blueos_filetime_ticks());
                if let Some(ch) = st.challenges.get_mut(&ch_id) {
                    ch.completed = true;
                    ch.claimed = true;
                    st.season_points += ch.points;
                } else {
                    st.season_points += 50;
                }
                PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)),
                    ("claimed", PyRep::Bool(true)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                    ("rewardGranted", PyRep::Bool(true)),
                    ("iskReward", PyRep::Integer(100_000)),
                ])
            });
            Some(Ok(res))
        }
        ("seasonManager", "get_loot_table_by_typeID") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(34);
            let table = PyRep::List(vec![
                PyRep::keyval([
                    ("typeID", PyRep::Integer(type_id)),
                    ("quantity", PyRep::Integer(500)),
                    ("probability", PyRep::Real(1.0)),
                ]),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(35)),
                    ("quantity", PyRep::Integer(200)),
                    ("probability", PyRep::Real(0.75)),
                ]),
            ]);
            Some(Ok(table))
        }
        ("seasonManager", "reload_fsd_data") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}
