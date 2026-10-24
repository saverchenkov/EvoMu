//! `project_discovery` — Citizen science Project Discovery service (`ProjectDiscovery`).
//!
//! Handles tasks, player state, tutorials, classifications, and reward distributions.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const PROJECT_DISCOVERY_KEY_PREFIX: &str = "project_discovery:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerProjectState {
    pub rank: i64,
    pub experience: f64,
    pub accuracy: f64,
    pub score: f64,
    pub tasks_submitted: i64,
    pub analysis_kredits: i64,
    pub tutorial_level: i64,
    pub tutorial_complete: bool,
    pub current_task_index: i64,
    pub bonus_samples_left: i64,
    #[serde(default)]
    pub tutorial_reward_claimed: bool,
    #[serde(default)]
    pub tutorial_skipped: bool,
    #[serde(default = "default_max_bonus_samples")]
    pub max_bonus_samples: i64,
    #[serde(default = "default_project_id")]
    pub project_id: String,
    #[serde(default)]
    pub last_submitted_task_index: i64,
}

// SYNTHETIC_PROJECT_DISCOVERY_CONFIG: default Project Discovery sample and task constants
fn default_max_bonus_samples() -> i64 {
    10
}

fn default_project_id() -> String {
    "flow_cytometry".into()
}

impl Default for PlayerProjectState {
    fn default() -> Self {
        Self {
            rank: 1,
            experience: 0.0,
            accuracy: 0.95,
            score: 100.0,
            tasks_submitted: 0,
            analysis_kredits: 0,
            tutorial_level: 1,
            tutorial_complete: false,
            tutorial_skipped: false,
            current_task_index: 0,
            bonus_samples_left: 10,
            tutorial_reward_claimed: false,
            max_bonus_samples: 10,
            project_id: "flow_cytometry".into(),
            last_submitted_task_index: 0,
        }
    }
}

fn read_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&PlayerProjectState) -> R,
{
    let key = format!("{PROJECT_DISCOVERY_KEY_PREFIX}{char_id}");
    ctx.db.with_snapshot(|snap| {
        let state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<PlayerProjectState>(json).ok())
            .unwrap_or_default();
        f(&state)
    })
}

fn with_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&mut PlayerProjectState, &mut eve_db::WorldStateSnapshot) -> R,
{
    let key = format!("{PROJECT_DISCOVERY_KEY_PREFIX}{char_id}");
    ctx.db.with_mut_snapshot(|snap| {
        let mut state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<PlayerProjectState>(json).ok())
            .unwrap_or_default();
        let out = f(&mut state, snap);
        if let Ok(json) = serde_json::to_string(&state) {
            snap.service_state_blobs.insert(key, json);
        }
        out
    })
}

fn try_with_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> Result<R, BridgeRpcError>
where
    F: FnOnce(&mut PlayerProjectState, &mut eve_db::WorldStateSnapshot) -> Result<R, BridgeRpcError>,
{
    let key = format!("{PROJECT_DISCOVERY_KEY_PREFIX}{char_id}");
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut state = snap
            .service_state_blobs
            .get(&key)
            .and_then(|json| serde_json::from_str::<PlayerProjectState>(json).ok())
            .unwrap_or_default();
        let out = f(&mut state, snap)?;
        if let Ok(json) = serde_json::to_string(&state) {
            snap.service_state_blobs.insert(key, json);
        }
        Ok(out)
    })
}

/// Dispatches calls to `ProjectDiscovery`.
pub fn handle_project_discovery(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "ProjectDiscovery" {
        return None;
    }

    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    match (service, method) {
        ("ProjectDiscovery", "is_enabled") => {
            // const-ok: `CommonProjectDiscoveryClientService.is_enabled` caches this boolean
            // (`projectdiscovery/client/baseservice.pyj:32-35`); Project Discovery is enabled in EvoMu.
            return Some(Ok(PyRep::Bool(true)));
        }
        ("ProjectDiscovery", "initialize_tutorial_status") => {
            let complete = read_player_state(ctx, char_id, |st| st.tutorial_complete || st.tutorial_skipped);
            return Some(Ok(PyRep::Bool(complete)));
        }
        _ => {}
    }

    match method {
        "get_player_state" => {
            let res = read_player_state(ctx, char_id, |st| {
                PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("score", PyRep::Real(st.score)),
                ])
            });
            Some(Ok(res))
        }

        "get_player_statistics" => {
            let res = read_player_state(ctx, char_id, |st| {
                PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("score", PyRep::Real(st.score)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("tasks_submitted", PyRep::Integer(st.tasks_submitted)),
                ])
            });
            Some(Ok(res))
        }

        "get_total_needed_xp" => {
            let rank = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let needed = (rank * 1_000 + 500) as f64;
            Some(Ok(PyRep::Real(needed)))
        }

        "get_tutorial_completion_status" => {
            let complete = read_player_state(ctx, char_id, |st| st.tutorial_complete || st.tutorial_skipped);
            Some(Ok(PyRep::Bool(complete)))
        }

        "get_tutorial_level" => {
            let lvl = read_player_state(ctx, char_id, |st| st.tutorial_level);
            Some(Ok(PyRep::Integer(lvl)))
        }

        "increase_tutorial_level" => {
            let new_lvl = with_player_state(ctx, char_id, |st, _snap| {
                st.tutorial_level += 1;
                if !st.tutorial_skipped && st.tutorial_level >= 5 {
                    st.tutorial_complete = true;
                }
                st.tutorial_level
            });
            Some(Ok(PyRep::Integer(new_lvl)))
        }

        "reset_tutorial" => {
            with_player_state(ctx, char_id, |st, _snap| {
                st.tutorial_level = 1;
                st.tutorial_skipped = false;
                st.tutorial_complete = false;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "skip_tutorial" => {
            with_player_state(ctx, char_id, |st, _snap| {
                st.tutorial_level = 5;
                st.tutorial_skipped = true;
                st.tutorial_complete = false;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "get_is_player_entitled_to_tutorial_reward" => {
            let entitled = read_player_state(ctx, char_id, |st| !st.tutorial_reward_claimed && !st.tutorial_skipped);
            Some(Ok(PyRep::Bool(entitled)))
        }

        "give_tutorial_rewards" => {
            let granted = with_player_state(ctx, char_id, |st, _snap| {
                if !st.tutorial_reward_claimed && !st.tutorial_skipped {
                    st.tutorial_complete = true;
                    st.tutorial_reward_claimed = true;
                    st.analysis_kredits = st.analysis_kredits.saturating_add(50);
                    st.experience += 250.0;
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(granted)))
        }

        "get_tutorial_xp_reward" => {
            // SYNTHETIC_PROJECT_DISCOVERY_CONFIG: default tutorial XP progression
            Some(Ok(PyRep::Real(250.0)))
        }

        "get_new_task" => {
            let (task_idx, pid) = with_player_state(ctx, char_id, |st, _snap| {
                if st.current_task_index < st.last_submitted_task_index + 1 {
                    st.current_task_index = st.last_submitted_task_index + 1;
                } else {
                    st.current_task_index += 1;
                }
                (st.current_task_index, st.project_id.clone())
            });
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(format!("task_{task_idx}"))),
                ("task_number", PyRep::Integer(task_idx)),
                (
                    "url",
                    PyRep::String(format!("https://projectdiscovery.eveonline.com/{pid}/tasks/{task_idx}")),
                ),
            ])))
        }

        "new_training_task" => {
            let lvl = read_player_state(ctx, char_id, |st| st.tutorial_level);
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(format!("training_task_{lvl:02}"))),
                ("taskId", PyRep::String(format!("training_task_{lvl:02}"))),
                ("level", PyRep::Integer(lvl)),
            ])))
        }

        "get_remaining_bonus_samples" => {
            let rem = read_player_state(ctx, char_id, |st| st.bonus_samples_left);
            Some(Ok(PyRep::Integer(rem)))
        }

        "get_maximum_bonus_samples" => {
            let m = read_player_state(ctx, char_id, |st| st.max_bonus_samples);
            Some(Ok(PyRep::Integer(m)))
        }

        "get_player_analysis_kredits" => {
            let ak = read_player_state(ctx, char_id, |st| st.analysis_kredits);
            Some(Ok(PyRep::Integer(ak)))
        }

        "get_project_id" => {
            let pid = read_player_state(ctx, char_id, |st| st.project_id.clone());
            Some(Ok(PyRep::String(pid)))
        }

        "post_classification" => {
            if args.len() < 2 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidClassification".into(),
                    message: "Both task ID and classification payload are required".into(),
                }));
            }
            let task_arg = &args[0];
            let classification_arg = &args[1];

            let is_empty_classification = match classification_arg {
                PyRep::None => true,
                PyRep::String(s) if s.trim().is_empty() => true,
                PyRep::List(l) if l.is_empty() => true,
                PyRep::Tuple(t) if t.is_empty() => true,
                PyRep::Dict(d) if d.is_empty() => true,
                _ => false,
            };
            if is_empty_classification {
                return Some(Err(BridgeRpcError {
                    code: "InvalidClassification".into(),
                    message: "Classification payload cannot be empty".into(),
                }));
            }

            let task_index = match task_arg {
                PyRep::Integer(i) => Some(*i),
                PyRep::String(s) => {
                    let s = s.trim();
                    let clean = s.strip_prefix("task_").unwrap_or(s);
                    clean.parse::<i64>().ok()
                }
                _ => None,
            };

            let Some(req_task_index) = task_index else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTaskId".into(),
                    message: "Invalid task_id format".into(),
                }));
            };

            let now_unix = ctx.now_unix();
            let res = try_with_player_state(ctx, char_id, |st, snap| {
                if req_task_index != st.current_task_index {
                    return Err(BridgeRpcError {
                        code: "InvalidTaskId".into(),
                        message: format!("task_id {} does not match current active task {}", req_task_index, st.current_task_index),
                    });
                }
                if st.last_submitted_task_index >= st.current_task_index {
                    return Err(BridgeRpcError {
                        code: "TaskAlreadySubmitted".into(),
                        message: "Active task has already been submitted".into(),
                    });
                }
                st.last_submitted_task_index = st.current_task_index;
                st.tasks_submitted += 1;
                st.experience += 150.0;
                st.analysis_kredits += 50;

                let had_bonus = st.bonus_samples_left > 0;
                st.bonus_samples_left = st.bonus_samples_left.saturating_sub(1);

                // SYNTHETIC_PROJECT_DISCOVERY_CONFIG: sample classification rewards
                let (_reward, new_bal) = if had_bonus {
                    let r = eve_db::IskCents::from_isk(25_000);
                    let bal = if let Some(c) = snap.characters.get_mut(&char_id) {
                        c.balance = c.balance.checked_add(r).unwrap_or(c.balance);
                        c.balance
                    } else {
                        r
                    };
                    snap.next_entity_id += 1;
                    let jid = snap.next_entity_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id: jid,
                        character_id: char_id,
                        entry_type: "project_discovery_reward".to_string(),
                        amount: r,
                        balance_after: bal,
                        reason: "Project Discovery classification reward".to_string(),
                    });
                    snap.wallet_journal_timestamps.insert(jid, now_unix);
                    (r, Some(bal))
                } else {
                    (eve_db::IskCents(0), None)
                };

                if st.experience >= (st.rank * 1_000 + 500) as f64 {
                    st.rank += 1;
                }
                let rep = PyRep::keyval([
                    ("score", PyRep::Real(st.score)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("experience_reward", PyRep::Real(150.0)),
                    ("kredits_reward", PyRep::Integer(50)),
                    ("rank", PyRep::Integer(st.rank)),
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

        "fix_task_id" => {
            if let Err(e) = ServerContext::require_gm_role(session) {
                return Some(Err(e));
            }
            let (tid_str, new_idx) = match args.first() {
                Some(PyRep::Integer(i)) => {
                    if *i <= 0 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidTaskId".into(),
                            message: "Task identifier must contain a positive numeric task index".into(),
                        }));
                    }
                    (format!("task_{i}"), *i)
                }
                Some(PyRep::String(s)) => {
                    let trimmed = s.trim();
                    let parsed_idx = trimmed
                        .rsplit('_')
                        .next()
                        .unwrap_or(trimmed)
                        .parse::<i64>()
                        .ok();
                    match parsed_idx {
                        Some(idx) if idx > 0 => (trimmed.to_string(), idx),
                        _ => {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidTaskId".into(),
                                message: "Task identifier must contain a positive numeric task index".into(),
                            }));
                        }
                    }
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTaskId".into(),
                        message: "Task identifier must contain a positive numeric task index".into(),
                    }));
                }
            };
            let tid = with_player_state(ctx, char_id, |st, _snap| {
                st.current_task_index = new_idx;
                tid_str
            });
            Some(Ok(PyRep::keyval([
                ("fixed", PyRep::Bool(true)),
                ("task_id", PyRep::String(tid)),
                ("current_task_index", PyRep::Integer(new_idx)),
            ])))
        }

        "get_current_task_index" => {
            let idx = read_player_state(ctx, char_id, |st| st.current_task_index);
            Some(Ok(PyRep::Integer(idx)))
        }

        "get_data_from_url" => {
            let url = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let task_seg = url.split('/').last().unwrap_or("1");
            let json_str = format!("{{\"url\":{:?},\"task_id\":{:?},\"samples\":10}}", url, task_seg);
            Some(Ok(PyRep::String(json_str)))
        }

        "append" => {
            Some(Ok(PyRep::None))
        }

        "remove" => {
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

/// Convenience alias accepting `&SessionState`.
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let mut sess = session.clone();
    handle_project_discovery(ctx, &mut sess, service, method, args, kwargs)
}
