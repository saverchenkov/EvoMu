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
const PROJECT_DISCOVERY_STORE_KEY: &str = "project_discovery";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDiscoveryTaskData {
    pub task_id: String,
    pub task_number: i64,
    pub project_id: String,
    pub url: String,
    pub samples: i64,
    #[serde(default)]
    pub fetch_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectDiscoveryStore {
    #[serde(default)]
    pub tasks: HashMap<i64, ProjectDiscoveryTaskData>,
    #[serde(default)]
    pub char_tasks: HashMap<String, ProjectDiscoveryTaskData>,
}

fn char_task_key(char_id: i64, task_idx: i64) -> String {
    format!("{char_id}:{task_idx}")
}

fn insert_bounded_char_task(
    store: &mut ProjectDiscoveryStore,
    char_id: i64,
    task_idx: i64,
    task: ProjectDiscoveryTaskData,
) {
    let prefix = format!("{char_id}:");
    let char_keys: Vec<(i64, String)> = store
        .char_tasks
        .iter()
        .filter_map(|(k, v)| k.starts_with(&prefix).then_some((v.task_number, k.clone())))
        .collect();
    if char_keys.len() >= 64 {
        if let Some((_, oldest_key)) = char_keys.into_iter().min_by_key(|(n, _)| *n) {
            store.char_tasks.remove(&oldest_key);
        }
    }
    if !store.tasks.contains_key(&task_idx) && store.tasks.len() >= 64 {
        if let Some(&oldest_idx) = store.tasks.keys().min() {
            store.tasks.remove(&oldest_idx);
        }
    }
    store.char_tasks.insert(char_task_key(char_id, task_idx), task.clone());
    store.tasks.insert(task_idx, task);
}

/// Server-side Analysis Kredits (AK) debit helper for AK-priced rewards and store offers.
pub fn spend_analysis_kredits_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    amount: i64,
) -> Result<i64, BridgeRpcError> {
    if amount <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidAmount".into(),
            message: "Analysis Kredits spend amount must be positive".into(),
        });
    }
    let key = format!("{PROJECT_DISCOVERY_KEY_PREFIX}{char_id}");
    let mut state = snap
        .service_state_blobs
        .get(&key)
        .and_then(|json| serde_json::from_str::<PlayerProjectState>(json).ok())
        .unwrap_or_default();
    if state.analysis_kredits < amount {
        return Err(BridgeRpcError {
            code: "InsufficientAnalysisKredits".into(),
            message: format!(
                "Insufficient Analysis Kredits (required {amount}, available {})",
                state.analysis_kredits
            ),
        });
    }
    state.analysis_kredits -= amount;
    if let Ok(json) = serde_json::to_string(&state) {
        snap.service_state_blobs.insert(key, json);
    }
    Ok(state.analysis_kredits)
}

/// Server-side Analysis Kredits (AK) debit helper for AK-priced rewards and store offers.
pub fn spend_analysis_kredits(
    ctx: &ServerContext,
    char_id: i64,
    amount: i64,
) -> Result<i64, BridgeRpcError> {
    ctx.db
        .try_with_mut_snapshot(|snap| spend_analysis_kredits_in_snap(snap, char_id, amount))
}

fn load_discovery_store(snap: &eve_db::WorldStateSnapshot) -> ProjectDiscoveryStore {
    snap.service_state_blobs
        .get(PROJECT_DISCOVERY_STORE_KEY)
        .and_then(|json| serde_json::from_str::<ProjectDiscoveryStore>(json).ok())
        .unwrap_or_default()
}

fn save_discovery_store(snap: &mut eve_db::WorldStateSnapshot, store: &ProjectDiscoveryStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.service_state_blobs
            .insert(PROJECT_DISCOVERY_STORE_KEY.to_string(), json);
    }
}

/// SYNTHETIC_PROJECT_DISCOVERY_CONFIG: placeholder XP curve (2,000 XP per rank), NOT imported from
/// client or live data. The real per-rank XP thresholds are served by the live MMOS-backed service and
/// are not present in the V24 client static data, so `rpc:ProjectDiscovery.get_total_needed_xp` stays open.
pub fn project_discovery_total_needed_xp(rank: i64) -> f64 {
    if rank <= 0 {
        0.0
    } else {
        (rank as f64) * 2_000.0
    }
}

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
            let needed = project_discovery_total_needed_xp(rank);
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
            let entitled = read_player_state(ctx, char_id, |st| {
                st.tutorial_complete
                    && st.tutorial_level >= 5
                    && !st.tutorial_reward_claimed
                    && !st.tutorial_skipped
            });
            Some(Ok(PyRep::Bool(entitled)))
        }

        "give_tutorial_rewards" => {
            let granted = with_player_state(ctx, char_id, |st, _snap| {
                if st.tutorial_complete
                    && st.tutorial_level >= 5
                    && !st.tutorial_reward_claimed
                    && !st.tutorial_skipped
                {
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
            let (task_idx, pid, url) = with_player_state(ctx, char_id, |st, snap| {
                if st.current_task_index < st.last_submitted_task_index + 1 {
                    st.current_task_index = st.last_submitted_task_index + 1;
                } else {
                    st.current_task_index += 1;
                }
                let idx = st.current_task_index;
                let pid = st.project_id.clone();
                let url = format!("https://projectdiscovery.eveonline.com/{pid}/tasks/{idx}");
                let mut store = load_discovery_store(snap);
                let key = char_task_key(char_id, idx);
                if !store.char_tasks.contains_key(&key) {
                    insert_bounded_char_task(
                        &mut store,
                        char_id,
                        idx,
                        ProjectDiscoveryTaskData {
                            task_id: format!("task_{idx}"),
                            task_number: idx,
                            project_id: pid.clone(),
                            url: url.clone(),
                            samples: st.max_bonus_samples.max(1),
                            fetch_count: 0,
                        },
                    );
                }
                save_discovery_store(snap, &store);
                (idx, pid, url)
            });
            let _ = pid;
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(format!("task_{task_idx}"))),
                ("task_number", PyRep::Integer(task_idx)),
                ("url", PyRep::String(url)),
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
                // SYNTHETIC: Offline citizen-science consensus heuristic; live Project Discovery delegates consensus grading to external MMOS service.
                let is_accurate = match classification_arg {
                    PyRep::List(items) | PyRep::Tuple(items) => items.iter().all(|v| {
                        v.as_f64().map(|f| f.is_finite() && f >= 0.0).unwrap_or(true)
                    }),
                    _ => true,
                };
                st.accuracy = (((st.accuracy * 9.0) + if is_accurate { 1.0 } else { 0.0 }) / 10.0)
                    .clamp(0.0, 1.0);
                st.score = (st.score + if is_accurate { 2.5 } else { -5.0 }).clamp(0.0, 1000.0);
                let xp_gain = if is_accurate { 150.0 } else { 75.0 };
                st.experience += xp_gain;
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

                if st.experience >= project_discovery_total_needed_xp(st.rank) {
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
            let tid = with_player_state(ctx, char_id, |st, snap| {
                st.current_task_index = new_idx;
                let pid = st.project_id.clone();
                let mut store = load_discovery_store(snap);
                insert_bounded_char_task(
                    &mut store,
                    char_id,
                    new_idx,
                    ProjectDiscoveryTaskData {
                        task_id: tid_str.clone(),
                        task_number: new_idx,
                        project_id: pid.clone(),
                        url: format!("https://projectdiscovery.eveonline.com/{pid}/tasks/{new_idx}"),
                        samples: st.max_bonus_samples.max(1),
                        fetch_count: 0,
                    },
                );
                save_discovery_store(snap, &store);
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
            let url = args
                .first()
                .and_then(|a| a.as_str())
                .map(str::trim)
                .unwrap_or("");
            if !url.starts_with("https://projectdiscovery.eveonline.com/") {
                return Some(Err(BridgeRpcError::user_error("InvalidTaskUrl")));
            }
            let task_seg = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            let parsed_num = task_seg
                .strip_prefix("task_")
                .unwrap_or(task_seg)
                .parse::<i64>()
                .ok()
                .filter(|&n| n > 0);
            let Some(task_num) = parsed_num else {
                return Some(Err(BridgeRpcError::user_error("InvalidTaskUrl")));
            };
            let res = try_with_player_state(ctx, char_id, |st, snap| {
                let mut store = load_discovery_store(snap);
                let key = char_task_key(char_id, task_num);
                if !store.char_tasks.contains_key(&key) {
                    if st.current_task_index == task_num {
                        let pid = st.project_id.clone();
                        insert_bounded_char_task(
                            &mut store,
                            char_id,
                            task_num,
                            ProjectDiscoveryTaskData {
                                task_id: format!("task_{task_num}"),
                                task_number: task_num,
                                project_id: pid.clone(),
                                url: format!("https://projectdiscovery.eveonline.com/{pid}/tasks/{task_num}"),
                                samples: st.max_bonus_samples.max(1),
                                fetch_count: 0,
                            },
                        );
                    } else {
                        return Err(BridgeRpcError::user_error("TaskNotFound"));
                    }
                }
                let entry = store.char_tasks.get_mut(&key).expect("task exists");
                entry.fetch_count += 1;
                let out = serde_json::json!({
                    "url": entry.url,
                    "task_id": entry.task_id,
                    "task_number": entry.task_number,
                    "project_id": entry.project_id,
                    "samples": entry.samples,
                    "fetch_count": entry.fetch_count,
                })
                .to_string();
                save_discovery_store(snap, &store);
                Ok(out)
            });
            Some(res.map(PyRep::String))
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
