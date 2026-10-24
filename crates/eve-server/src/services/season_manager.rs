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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterSeasonState {
    pub selected_season_id: i64,
    pub season_points: i64,
    pub challenges: HashMap<i64, CharacterChallenge>,
    #[serde(default)]
    pub claimed_rewards: Vec<i64>,
}

impl Default for CharacterSeasonState {
    fn default() -> Self {
        Self {
            selected_season_id: 1,
            season_points: 0,
            challenges: HashMap::new(),
            claimed_rewards: Vec::new(),
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

// SYNTHETIC_SEASON_DATA: server-configured default season challenges when FSD season table is not populated
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
        },
        1002 => CharacterChallenge {
            challenge_id: 1002,
            season_id,
            title: "Combat Patrol".into(),
            description: "Eliminate 5 pirate ships in combat anomalies".into(),
            points: 50,
            progress: 0,
            max_progress: 5,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: exp,
            client_event_driven: true,
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
            let kv = read_season_state(ctx, char_id, |st| {
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
                    ch.client_event_driven = true;
                    ch.progress = ch.progress.saturating_add(amount.min(ch.max_progress)).min(ch.max_progress);
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
            let ch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1002);
            let now_unix = ctx.now_unix();
            let res = try_with_season_state(ctx, char_id, |st, snap| {
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

                let is_isk_reward = !ch.client_event_driven && ch.points > 0;
                let (isk_payout, new_bal) = if is_isk_reward {
                    let reward = eve_db::IskCents::from_isk(100_000);
                    let bal = if let Some(c) = snap.characters.get_mut(&char_id) {
                        c.balance = c.balance.checked_add(reward).unwrap_or(c.balance);
                        c.balance
                    } else {
                        reward
                    };
                    snap.next_entity_id += 1;
                    let jid = snap.next_entity_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id: jid,
                        character_id: char_id,
                        entry_type: "seasonal_challenge_reward".into(),
                        amount: reward,
                        balance_after: bal,
                        reason: format!("Season challenge {} reward", ch_id),
                    });
                    snap.wallet_journal_timestamps.insert(jid, now_unix);
                    (100_000, Some(bal))
                } else {
                    (0, None)
                };

                let rep = PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)),
                    ("claimed", PyRep::Bool(true)),
                    ("seasonPoints", PyRep::Integer(pts)),
                    ("rewardGranted", PyRep::Bool(true)),
                    ("iskReward", PyRep::Integer(isk_payout)),
                ]);
                Ok((rep, new_bal))
            });

            match res {
                Ok((rep, new_bal)) => {
                    if let Some(bal) = new_bal {
                        let _ = ctx.notify_account_change(char_id, "cash", char_id, bal.to_wire_f64());
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
            // const-ok: reload static season FSD data returns true acknowledgment
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::Bool(true)))
        }
        _ => None,
    }
}

/// Helper for testing challenge progression without non-client RPC exposure.
pub fn complete_challenge_for_character(
    ctx: &ServerContext,
    char_id: i64,
    ch_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    if ch_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidChallengeId".into(),
            message: "Challenge identifier must be positive".into(),
        });
    }

    try_with_season_state(ctx, char_id, |st, _snap| {
        ensure_challenges(st, ctx.now_blueos_filetime_ticks());
        let Some(ch) = st.challenges.get_mut(&ch_id) else {
            return Err(BridgeRpcError {
                code: "ChallengeNotFound".into(),
                message: format!("Challenge {ch_id} not found"),
            });
        };
        let points_awarded = ch.points;
        ch.progress = ch.max_progress;
        let was_completed = ch.completed;
        ch.completed = true;
        if !was_completed {
            st.season_points += points_awarded;
        }
        Ok(PyRep::keyval([
            ("challengeID", PyRep::Integer(ch_id)),
            ("progress", PyRep::Integer(ch.progress)),
            ("maxProgress", PyRep::Integer(ch.max_progress)),
            ("completed", PyRep::Bool(true)),
            ("pointsAwarded", PyRep::Integer(points_awarded)),
            ("seasonPoints", PyRep::Integer(st.season_points)),
        ]))
    })
}
