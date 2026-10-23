//! `project_discovery` — Citizen science Project Discovery service (`ProjectDiscovery`).
//!
//! Handles tasks, player state, tutorials, classifications, and reward distributions.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const PROJECT_DISCOVERY_NOTE_BASE: i64 = -8_700_000_000_000;
const PROJECT_DISCOVERY_NOTE_OWNER_ID: i64 = -87;

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
}

impl Default for PlayerProjectState {
    fn default() -> Self {
        Self {
            rank: 1,
            experience: 0.0,
            accuracy: 0.95,
            score: 100.0,
            tasks_submitted: 0,
            analysis_kredits: 250,
            tutorial_level: 1,
            tutorial_complete: false,
            current_task_index: 0,
            bonus_samples_left: 5,
        }
    }
}

fn with_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&mut PlayerProjectState) -> R,
{
    let note_id = PROJECT_DISCOVERY_NOTE_BASE - char_id.abs();
    let mut state = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .get(&note_id)
            .and_then(|n| serde_json::from_str::<PlayerProjectState>(&n.note).ok())
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
                    character_id: PROJECT_DISCOVERY_NOTE_OWNER_ID,
                    item_id: char_id,
                    label: format!("S:PROJECT_DISCOVERY_STATE:{char_id}"),
                    note: json,
                    timestamp_unix: now_unix,
                },
            );
        });
    }
    out
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
            let complete = with_player_state(ctx, char_id, |st| st.tutorial_complete);
            return Some(Ok(PyRep::Bool(complete)));
        }
        _ => {}
    }

    match method {
        "get_player_state" => {
            let res = with_player_state(ctx, char_id, |st| {
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
            let res = with_player_state(ctx, char_id, |st| {
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
            let complete = with_player_state(ctx, char_id, |st| st.tutorial_complete);
            Some(Ok(PyRep::Bool(complete)))
        }

        "get_tutorial_level" => {
            let lvl = with_player_state(ctx, char_id, |st| st.tutorial_level);
            Some(Ok(PyRep::Integer(lvl)))
        }

        "increase_tutorial_level" => {
            let new_lvl = with_player_state(ctx, char_id, |st| {
                st.tutorial_level += 1;
                st.tutorial_level
            });
            Some(Ok(PyRep::Integer(new_lvl)))
        }

        "reset_tutorial" => {
            with_player_state(ctx, char_id, |st| {
                st.tutorial_level = 1;
                st.tutorial_complete = false;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "skip_tutorial" => {
            with_player_state(ctx, char_id, |st| {
                st.tutorial_level = 5;
                st.tutorial_complete = true;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "get_is_player_entitled_to_tutorial_reward" => {
            let entitled = with_player_state(ctx, char_id, |st| !st.tutorial_complete);
            Some(Ok(PyRep::Bool(entitled)))
        }

        "give_tutorial_rewards" => {
            let rep = with_player_state(ctx, char_id, |st| {
                st.experience += 500.0;
                st.analysis_kredits += 100;
                st.tutorial_complete = true;
                PyRep::keyval([
                    ("experience", PyRep::Real(500.0)),
                    ("analysis_kredits", PyRep::Integer(100)),
                    ("score", PyRep::Real(st.score)),
                ])
            });
            Some(Ok(rep))
        }

        "get_tutorial_xp_reward" => {
            let lvl = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let reward = (lvl * 250) as f64;
            Some(Ok(PyRep::Real(reward)))
        }

        "get_new_task" => {
            let task_idx = with_player_state(ctx, char_id, |st| {
                st.current_task_index += 1;
                st.current_task_index
            });
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(format!("task_{task_idx}"))),
                ("task_number", PyRep::Integer(task_idx)),
                (
                    "url",
                    PyRep::String(format!("https://projectdiscovery.eveonline.com/tasks/{task_idx}")),
                ),
            ])))
        }

        "new_training_task" => {
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String("training_task_01".into())),
                ("taskId", PyRep::String("training_task_01".into())),
                ("level", PyRep::Integer(1)),
            ])))
        }

        "get_remaining_bonus_samples" => {
            let rem = with_player_state(ctx, char_id, |st| st.bonus_samples_left);
            Some(Ok(PyRep::Integer(rem)))
        }

        "get_maximum_bonus_samples" => {
            Some(Ok(PyRep::Integer(10)))
        }

        "get_player_analysis_kredits" => {
            let ak = with_player_state(ctx, char_id, |st| st.analysis_kredits);
            Some(Ok(PyRep::Integer(ak)))
        }

        "get_project_id" => {
            Some(Ok(PyRep::String("covid".into())))
        }

        "post_classification" => {
            let rep = with_player_state(ctx, char_id, |st| {
                st.tasks_submitted += 1;
                st.experience += 150.0;
                st.analysis_kredits += 25;
                if st.experience >= (st.rank * 1_000 + 500) as f64 {
                    st.rank += 1;
                }
                PyRep::keyval([
                    ("score", PyRep::Real(st.score)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("experience_reward", PyRep::Real(150.0)),
                    ("kredits_reward", PyRep::Integer(25)),
                    ("rank", PyRep::Integer(st.rank)),
                ])
            });
            Some(Ok(rep))
        }

        "fix_task_id" => {
            let tid = args.first().cloned().unwrap_or(PyRep::String("1".into()));
            Some(Ok(tid))
        }

        "get_current_task_index" => {
            let idx = with_player_state(ctx, char_id, |st| st.current_task_index);
            Some(Ok(PyRep::Integer(idx)))
        }

        "get_data_from_url" => {
            Some(Ok(PyRep::String("data_payload".into())))
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
