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

/// V24 `projectdiscovery/common/const.pyj` constants.
pub const ACTIVE_PROJECT_ID: &str = "mcgill-polyseg";
pub const INITIAL_PLAYER_SCORE: f64 = 0.0;
pub const ISK_MULTIPLIERS: [i64; 3] = [500, 800, 1_000];
pub const LOOT_CRATE_TYPE_ID: u32 = 55358;
pub const SUPERIOR_LOOT_CRATE_TYPE_ID: u32 = 55359;
pub const NEEDED_LEVEL_FOR_SUPERIOR_CRATES: i64 = 25;

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
    /// Persisted MMOS rank -> cumulative XP threshold overrides (`get_total_needed_xp`).
    #[serde(default)]
    pub xp_curve_by_rank: HashMap<i64, f64>,
    /// Persisted MMOS per-rank XP step when `xp_curve_by_rank` does not specify an override.
    #[serde(default)]
    pub xp_per_rank: f64,
    /// Persisted MMOS tutorial completion XP reward (`get_tutorial_xp_reward`, `give_tutorial_rewards`).
    #[serde(default)]
    pub tutorial_xp_reward: f64,
    /// Persisted MMOS per-task consensus classification (`task_number -> accurate`).
    #[serde(default)]
    pub task_consensus_by_index: HashMap<i64, bool>,
    /// Persisted MMOS base XP reward for accurate classification (`post_classification`).
    #[serde(default)]
    pub classification_xp_accurate: f64,
    /// Persisted MMOS base XP reward for inaccurate classification (`post_classification`).
    #[serde(default)]
    pub classification_xp_inaccurate: f64,
    /// Persisted MMOS Analysis Kredits reward per accurate classification (`post_classification`).
    #[serde(default)]
    pub classification_ak_reward: i64,
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

/// Configure persisted MMOS rank XP curve, tutorial XP reward, and classification rewards in `ProjectDiscoveryStore`.
pub fn configure_project_discovery_mmos(
    snap: &mut eve_db::WorldStateSnapshot,
    xp_per_rank: f64,
    tutorial_xp_reward: f64,
    classification_xp_accurate: f64,
    classification_xp_inaccurate: f64,
    classification_ak_reward: i64,
) {
    let mut store = load_discovery_store(snap);
    store.xp_per_rank = xp_per_rank.max(0.0);
    store.tutorial_xp_reward = tutorial_xp_reward.max(0.0);
    store.classification_xp_accurate = classification_xp_accurate.max(0.0);
    store.classification_xp_inaccurate = classification_xp_inaccurate.max(0.0);
    store.classification_ak_reward = classification_ak_reward.max(0);
    save_discovery_store(snap, &store);
}

/// Register persisted MMOS consensus result for a specific Project Discovery task index.
pub fn set_project_discovery_task_consensus(
    snap: &mut eve_db::WorldStateSnapshot,
    task_index: i64,
    is_accurate: bool,
) {
    let mut store = load_discovery_store(snap);
    store.task_consensus_by_index.insert(task_index, is_accurate);
    save_discovery_store(snap, &store);
}

/// Resolve cumulative XP threshold for `rank` from persisted `ProjectDiscoveryStore` MMOS configuration.
///
/// DATA GAP: Project Discovery XP curve, tutorial reward, and MMOS classification grading live on the
/// external MMOS backend and are not in V24 client static data.
pub fn project_discovery_total_needed_xp_from_store(
    store: &ProjectDiscoveryStore,
    rank: i64,
) -> f64 {
    if rank <= 0 {
        return 0.0;
    }
    if let Some(&override_xp) = store.xp_curve_by_rank.get(&rank) {
        return override_xp.max(0.0);
    }
    if store.xp_per_rank > 0.0 {
        (rank as f64) * store.xp_per_rank
    } else {
        0.0
    }
}

/// Resolve the player's rank for a given cumulative `experience` using `ProjectDiscoveryStore`.
pub fn rank_for_xp(store: &ProjectDiscoveryStore, experience: f64, current_rank: i64) -> i64 {
    let mut rank = current_rank.max(1);
    for _ in 0..10_000 {
        let needed = project_discovery_total_needed_xp_from_store(store, rank);
        if needed > 0.0 && experience >= needed {
            rank += 1;
        } else {
            break;
        }
    }
    rank
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

fn default_max_bonus_samples() -> i64 {
    10
}

fn default_project_id() -> String {
    ACTIVE_PROJECT_ID.into()
}

impl Default for PlayerProjectState {
    fn default() -> Self {
        Self {
            rank: 1,
            experience: 0.0,
            accuracy: INITIAL_PLAYER_SCORE,
            score: INITIAL_PLAYER_SCORE,
            tasks_submitted: 0,
            analysis_kredits: 0,
            tutorial_level: 1,
            tutorial_complete: false,
            tutorial_skipped: false,
            current_task_index: 0,
            bonus_samples_left: 10,
            tutorial_reward_claimed: false,
            max_bonus_samples: 10,
            project_id: ACTIVE_PROJECT_ID.into(),
            last_submitted_task_index: 0,
        }
    }
}

/// Global config key (`machoNet.SetGlobalConfigValue`) controlling Project Discovery enablement.
pub const PROJECT_DISCOVERY_ENABLED_CONFIG_KEY: &str = "projectDiscoveryEnabled";

/// Return whether Project Discovery is enabled on `ctx` (`machoNet` global config overriding `ServerConfig`).
pub fn is_project_discovery_enabled(ctx: &ServerContext) -> bool {
    if let Ok(guard) = ctx.global_config_values.read() {
        if let Some(val) = guard.get(PROJECT_DISCOVERY_ENABLED_CONFIG_KEY) {
            return match val {
                PyRep::Bool(b) => *b,
                PyRep::Integer(i) => *i != 0,
                PyRep::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
                _ => ctx.feature_flags.enable_project_discovery,
            };
        }
    }
    ctx.feature_flags.enable_project_discovery
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
            let enabled = is_project_discovery_enabled(ctx);
            return Some(Ok(PyRep::Bool(enabled)));
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
                    ("totalXP", PyRep::Real(st.experience)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("score", PyRep::Real(st.score)),
                    ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
                    ("finishedTutorial", PyRep::Bool(st.tutorial_complete || st.tutorial_skipped)),
                ])
            });
            Some(Ok(res))
        }

        "get_player_statistics" => {
            let res = read_player_state(ctx, char_id, |st| {
                let proj = PyRep::Dict(vec![
                    (PyRep::String("score".into()), PyRep::Real(st.accuracy)),
                    (PyRep::String("accuracy".into()), PyRep::Real(st.accuracy)),
                    (PyRep::String("rank".into()), PyRep::Integer(st.rank)),
                    (PyRep::String("experience".into()), PyRep::Real(st.experience)),
                    (
                        PyRep::String("tasks_submitted".into()),
                        PyRep::Integer(st.tasks_submitted),
                    ),
                ]);
                PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("score", PyRep::Real(st.score)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("tasks_submitted", PyRep::Integer(st.tasks_submitted)),
                    ("project", proj.clone()),
                    ("projects", PyRep::List(vec![proj])),
                ])
            });
            Some(Ok(res))
        }

        // DERIVED (synthetic): DATA GAP — MMOS rank XP progression curve is external server data not shipped in V24 client FSD.
        "get_total_needed_xp" => {
            let rank = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let needed = ctx.db.with_snapshot(|snap| {
                let store = load_discovery_store(snap);
                project_discovery_total_needed_xp_from_store(&store, rank)
            });
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

        // DERIVED (synthetic): DATA GAP — MMOS tutorial XP reward is external server data not shipped in V24 client FSD.
        "give_tutorial_rewards" | "give_tutorial_complete_experience" => {
            // V24 `projectdiscovery/client/projects/subcellular/tutorial.pyj:601`:
            // `give_tutorial_rewards()` grants `get_tutorial_xp_reward()` XP (`AK_Reward = 0`, `ISK_Reward = 0`, `LP_Reward = 0`).
            let (granted, _xp_gained, new_xp, old_rank, new_rank, xp_for_new_rank, xp_for_next_rank) = with_player_state(ctx, char_id, |st, snap| {
                let store = load_discovery_store(snap);
                if st.tutorial_complete
                    && st.tutorial_level >= 5
                    && !st.tutorial_reward_claimed
                    && !st.tutorial_skipped
                {
                    st.tutorial_reward_claimed = true;
                    let xp_reward = store.tutorial_xp_reward.max(0.0);
                    let prev_rank = st.rank;
                    st.experience += xp_reward;
                    st.rank = rank_for_xp(&store, st.experience, st.rank);
                    let xp_cur = project_discovery_total_needed_xp_from_store(&store, st.rank);
                    let xp_next = project_discovery_total_needed_xp_from_store(&store, st.rank + 1);
                    (true, xp_reward, st.experience, prev_rank, st.rank, xp_cur, xp_next)
                } else {
                    let xp_cur = project_discovery_total_needed_xp_from_store(&store, st.rank);
                    let xp_next = project_discovery_total_needed_xp_from_store(&store, st.rank + 1);
                    (false, 0.0, st.experience, st.rank, st.rank, xp_cur, xp_next)
                }
            });
            if granted {
                let xp_args = vec![PyRep::Real(new_xp)];
                session.push_notification("OnProjectDiscoveryExperience", xp_args.clone());
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnProjectDiscoveryExperience",
                    "charid",
                    xp_args,
                );
                if new_rank > old_rank {
                    let lvl_args = vec![
                        PyRep::Integer(new_rank),
                        PyRep::Real(xp_for_new_rank),
                        PyRep::Real(xp_for_next_rank),
                    ];
                    session.push_notification("OnProjectDiscoveryLevelUp", lvl_args.clone());
                    let _ = ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnProjectDiscoveryLevelUp",
                        "charid",
                        lvl_args,
                    );
                }
            }
            Some(Ok(PyRep::Bool(granted)))
        }

        // DERIVED (synthetic): DATA GAP — MMOS tutorial XP reward is external server data not shipped in V24 client FSD.
        "get_tutorial_xp_reward" => {
            let reward = ctx.db.with_snapshot(|snap| {
                let store = load_discovery_store(snap);
                store.tutorial_xp_reward.max(0.0)
            });
            Some(Ok(PyRep::Real(reward)))
        }

        // DERIVED (synthetic): DATA GAP — MMOS citizen science tasks and URLs are external backend data not in V24 client FSD.
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
            let tid_str = format!("task_{task_idx}");
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(tid_str.clone())),
                ("taskId", PyRep::String(tid_str.clone())),
                ("taskID", PyRep::String(tid_str)),
                ("task_number", PyRep::Integer(task_idx)),
                ("url", PyRep::String(url)),
            ])))
        }

        // DERIVED (synthetic): DATA GAP — MMOS training task solutions are external backend data not in V24 client FSD.
        "new_training_task" => {
            let lvl = read_player_state(ctx, char_id, |st| st.tutorial_level);
            let task_num = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&n| n > 0)
                .unwrap_or(lvl);
            let tid_str = format!("training_task_{task_num:02}");
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(tid_str.clone())),
                ("taskId", PyRep::String(tid_str.clone())),
                ("taskID", PyRep::String(tid_str)),
                ("level", PyRep::Integer(lvl)),
                ("task_number", PyRep::Integer(task_num)),
                ("solution", PyRep::List(vec![PyRep::Integer(1)])),
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

        // DERIVED (synthetic): DATA GAP — MMOS classification consensus grading and XP/AK rewards are external backend data not in V24 client FSD.
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
                let prev_tasks = st.tasks_submitted;
                st.last_submitted_task_index = st.current_task_index;
                st.tasks_submitted += 1;
                let store = load_discovery_store(snap);
                let is_accurate = store
                    .task_consensus_by_index
                    .get(&req_task_index)
                    .copied()
                    .unwrap_or(false);
                let sample_acc = if is_accurate { 1.0 } else { 0.0 };
                st.accuracy = if prev_tasks <= 0 {
                    sample_acc
                } else {
                    (((st.accuracy * 9.0) + sample_acc) / 10.0).clamp(0.0, 1.0)
                };
                st.score = (st.accuracy * 100.0).clamp(0.0, 1000.0);
                let xp_gain = if is_accurate {
                    store.classification_xp_accurate
                } else {
                    store.classification_xp_inaccurate
                };
                let ak_gain = if is_accurate {
                    store.classification_ak_reward
                } else {
                    0
                };
                let prev_rank = st.rank;
                st.experience += xp_gain;
                st.analysis_kredits = st.analysis_kredits.saturating_add(ak_gain);

                let had_bonus = st.bonus_samples_left > 0;
                st.bonus_samples_left = st.bonus_samples_left.saturating_sub(1);

                // V24 `projectdiscovery/common/projects/covid/iskmultipliers.pyj`: `ISK_MULTIPLIERS = [500, 800, 1000]`
                let tier_idx = if st.accuracy >= 0.9 {
                    2
                } else if st.accuracy >= 0.5 {
                    1
                } else {
                    0
                };
                let isk_amount = if had_bonus && is_accurate {
                    ISK_MULTIPLIERS[tier_idx] * st.rank.max(1)
                } else {
                    0
                };
                let (_reward, new_bal) = if isk_amount > 0 {
                    let r = eve_db::IskCents::from_isk(isk_amount);
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

                let new_rank = rank_for_xp(&store, st.experience, st.rank);
                let mut crates_granted = Vec::new();
                if new_rank > st.rank {
                    st.rank = new_rank;
                    let crate_type_id = if st.rank >= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
                        SUPERIOR_LOOT_CRATE_TYPE_ID
                    } else {
                        LOOT_CRATE_TYPE_ID
                    };
                    crates_granted.push(PyRep::Integer(crate_type_id as i64));
                }
                let xp_for_new_rank = project_discovery_total_needed_xp_from_store(&store, st.rank);
                let xp_for_next_rank = project_discovery_total_needed_xp_from_store(&store, st.rank + 1);
                let player_state_rep = PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("totalXP", PyRep::Real(st.experience)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("score", PyRep::Real(st.score)),
                    ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
                    ("finishedTutorial", PyRep::Bool(st.tutorial_complete || st.tutorial_skipped)),
                ]);
                let rep = PyRep::keyval([
                    ("score", PyRep::Real(st.score)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("experience_reward", PyRep::Real(xp_gain)),
                    ("kredits_reward", PyRep::Integer(ak_gain)),
                    ("XP_Reward", PyRep::Real(xp_gain)),
                    ("ISK_Reward", PyRep::Real(isk_amount as f64)),
                    ("AK_Reward", PyRep::Integer(ak_gain)),
                    ("gotBonusXP", PyRep::Bool(had_bonus)),
                    ("bonusSamplesAfterClassification", PyRep::Integer(st.bonus_samples_left)),
                    ("loot_crates", PyRep::List(crates_granted)),
                    ("playerState", player_state_rep),
                    ("player", PyRep::keyval([("score", PyRep::Real(st.accuracy))])),
                    ("rank", PyRep::Integer(st.rank)),
                ]);
                Ok((
                    rep,
                    new_bal,
                    xp_gain,
                    st.experience,
                    prev_rank,
                    st.rank,
                    xp_for_new_rank,
                    xp_for_next_rank,
                ))
            });

            match res {
                Ok((
                    rep,
                    new_bal,
                    _xp_gain,
                    total_xp,
                    old_rank,
                    new_rank,
                    xp_for_new_rank,
                    xp_for_next_rank,
                )) => {
                    if let Some(bal) = new_bal {
                        let _ = ctx.notify_account_change(char_id, "cash", char_id, bal.to_wire_f64());
                    }
                    let xp_args = vec![PyRep::Real(total_xp)];
                    session.push_notification("OnProjectDiscoveryExperience", xp_args.clone());
                    let _ = ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnProjectDiscoveryExperience",
                        "charid",
                        xp_args,
                    );
                    if new_rank > old_rank {
                        let lvl_args = vec![
                            PyRep::Integer(new_rank),
                            PyRep::Real(xp_for_new_rank),
                            PyRep::Real(xp_for_next_rank),
                        ];
                        session.push_notification("OnProjectDiscoveryLevelUp", lvl_args.clone());
                        let _ = ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnProjectDiscoveryLevelUp",
                            "charid",
                            lvl_args,
                        );
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

        // DERIVED (synthetic): DATA GAP — MMOS task payload URLs are external backend data not in V24 client FSD.
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
