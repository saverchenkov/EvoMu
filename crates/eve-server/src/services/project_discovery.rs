//! `project_discovery` — Citizen science Project Discovery service (`ProjectDiscovery`).
//!
//! Handles tasks, player state, tutorials, classifications, and reward distributions.
#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[path = "project_discovery_ops.rs"]
mod ops;
#[path = "project_discovery_rewards.rs"]
mod rewards;

/// V24 `projectdiscovery/common/const.pyj` constants.
pub const ACTIVE_PROJECT_ID: &str = "mcgill-polyseg";
pub const INITIAL_PLAYER_SCORE: f64 = 0.0;
pub const ISK_MULTIPLIERS: [i64; 3] = [500, 800, 1_000];
pub const LOOT_CRATE_TYPE_ID: u32 = 55358;
pub const SUPERIOR_LOOT_CRATE_TYPE_ID: u32 = 55359;
pub const NEEDED_LEVEL_FOR_SUPERIOR_CRATES: i64 = 25;
pub const MIN_SUBMISSION_INTERVAL_SECS: u64 = 1;

pub use eve_db::{ProjectDiscoveryStore, ProjectDiscoveryTaskData};

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
    let state = snap
        .seasons_and_campaigns
        .project_discovery_players
        .entry(char_id)
        .or_default();
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
    Ok(state.analysis_kredits)
}

/// Server-side Analysis Kredits (AK) debit helper for AK-priced rewards and store offers.
pub fn spend_analysis_kredits(
    ctx: &ServerContext,
    char_id: i64,
    amount: i64,
) -> Result<i64, BridgeRpcError> {
    let rem = ctx
        .db
        .try_with_mut_snapshot(|snap| spend_analysis_kredits_in_snap(snap, char_id, amount))?;
    if amount > 0 {
        crate::services::notify_emitters_tournaments_anomalies::emit_analysis_kredits_change(ctx, char_id);
    }
    Ok(rem)
}

fn load_discovery_store(snap: &eve_db::WorldStateSnapshot) -> ProjectDiscoveryStore {
    snap.seasons_and_campaigns.project_discovery_store.clone()
}

fn save_discovery_store(snap: &mut eve_db::WorldStateSnapshot, store: &ProjectDiscoveryStore) {
    snap.seasons_and_campaigns.project_discovery_store = store.clone();
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
    store.mmos_configured = true;
    save_discovery_store(snap, &store);
}

/// Helper to configure or update MMOS bonus samples config in `ProjectDiscoveryStore`.
pub fn configure_project_discovery_bonus_samples(
    snap: &mut eve_db::WorldStateSnapshot,
    max_bonus_samples: i64,
) {
    let mut store = load_discovery_store(snap);
    store.max_bonus_samples = Some(max_bonus_samples.max(1));
    store.mmos_configured = true;
    save_discovery_store(snap, &store);
}

/// Sync player bonus samples with store configuration and daily refill timer.
pub fn sync_bonus_samples(
    st: &mut PlayerProjectState,
    store: &ProjectDiscoveryStore,
    now_unix: u64,
) {
    rewards::sync_player_bonus_samples(st, store, 1, now_unix);
}

/// Generates authentic citizen science task data matching authentic V24 MMOS flow cytometry payloads.
pub fn generate_authentic_task_data(
    task_idx: i64,
    project_id: &str,
    samples: i64,
) -> ProjectDiscoveryTaskData {
    ops::build_sample_task(task_idx, project_id, samples)
}

/// Register persisted MMOS consensus result for a specific Project Discovery task index.
pub fn set_project_discovery_task_consensus(
    snap: &mut eve_db::WorldStateSnapshot,
    task_index: i64,
    is_accurate: bool,
) {
    let mut store = load_discovery_store(snap);
    store.task_consensus_by_index.insert(task_index, is_accurate);
    store.mmos_configured = true;
    save_discovery_store(snap, &store);
}

/// Resolve cumulative XP threshold for `rank` using `ProjectDiscoveryStore` or V24 analyst progression curve.
pub fn project_discovery_total_needed_xp_from_store(
    store: &ProjectDiscoveryStore,
    rank: i64,
) -> f64 {
    rewards::total_needed_xp_for_rank(store, rank)
}

/// Resolve the player's rank for a given cumulative `experience` using `ProjectDiscoveryStore`.
pub fn rank_for_xp(store: &ProjectDiscoveryStore, experience: f64, current_rank: i64) -> i64 {
    rewards::compute_rank_for_xp(store, experience, current_rank)
}

pub use eve_db::PlayerProjectState;

fn json_has_non_empty_classification_content(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Null | serde_json::Value::Bool(false) => false,
        serde_json::Value::Bool(true) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(s) => !s.trim().is_empty(),
        serde_json::Value::Array(arr) => {
            !arr.is_empty() && arr.iter().any(json_has_non_empty_classification_content)
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return false;
            }
            for key in ["polygons", "gates"] {
                if let Some(poly_val) = map.get(key) {
                    return matches!(poly_val, serde_json::Value::Array(arr) if !arr.is_empty() && arr.iter().any(json_has_non_empty_classification_content));
                }
            }
            map.values().any(json_has_non_empty_classification_content)
        }
    }
}

fn pyrep_has_non_empty_classification_content(rep: &PyRep, top_level: bool) -> bool {
    match rep {
        PyRep::None | PyRep::Bool(false) => false,
        PyRep::Bool(true) | PyRep::Integer(_) | PyRep::Real(_) => !top_level,
        PyRep::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty()
                || trimmed == "[]"
                || trimmed == "{}"
                || trimmed.eq_ignore_ascii_case("null")
                || trimmed.eq_ignore_ascii_case("none")
            {
                return false;
            }
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    return json_has_non_empty_classification_content(&json_val);
                }
            }
            true
        }
        PyRep::Bytes(b) | PyRep::Buffer(b) => !b.is_empty(),
        PyRep::List(items) | PyRep::Tuple(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .any(|item| pyrep_has_non_empty_classification_content(item, false))
        }
        PyRep::Dict(entries) => {
            if entries.is_empty() {
                return false;
            }
            for (k, v) in entries {
                if matches!(k.as_str(), Some("polygons" | "gates")) {
                    return match v {
                        PyRep::List(items) | PyRep::Tuple(items) => {
                            !items.is_empty()
                                && items
                                    .iter()
                                    .any(|item| pyrep_has_non_empty_classification_content(item, false))
                        }
                        _ => false,
                    };
                }
            }
            entries
                .iter()
                .any(|(_, v)| pyrep_has_non_empty_classification_content(v, false))
        }
        PyRep::KeyVal(entries) => {
            if entries.is_empty() {
                return false;
            }
            for (k, v) in entries {
                if k == "polygons" || k == "gates" {
                    return match v {
                        PyRep::List(items) | PyRep::Tuple(items) => {
                            !items.is_empty()
                                && items
                                    .iter()
                                    .any(|item| pyrep_has_non_empty_classification_content(item, false))
                        }
                        _ => false,
                    };
                }
            }
            entries
                .iter()
                .any(|(_, v)| pyrep_has_non_empty_classification_content(v, false))
        }
        PyRep::Object { args, .. } => pyrep_has_non_empty_classification_content(args, top_level),
        PyRep::Rowset { rows, .. } => !rows.is_empty(),
        _ => false,
    }
}

fn has_valid_classification_payload(rep: &PyRep) -> bool {
    pyrep_has_non_empty_classification_content(rep, true)
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
    ctx.db.with_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .get(&char_id)
            .cloned()
            .unwrap_or_default();
        f(&state)
    })
}

fn with_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&mut PlayerProjectState, &mut eve_db::WorldStateSnapshot) -> R,
{
    ctx.db.with_mut_snapshot(|snap| {
        let mut state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .get(&char_id)
            .cloned()
            .unwrap_or_default();
        let out = f(&mut state, snap);
        snap.seasons_and_campaigns
            .project_discovery_players
            .insert(char_id, state);
        out
    })
}

fn try_with_player_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> Result<R, BridgeRpcError>
where
    F: FnOnce(&mut PlayerProjectState, &mut eve_db::WorldStateSnapshot) -> Result<R, BridgeRpcError>,
{
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .get(&char_id)
            .cloned()
            .unwrap_or_default();
        let out = f(&mut state, snap)?;
        snap.seasons_and_campaigns
            .project_discovery_players
            .insert(char_id, state);
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
                    ("tutorialLevel", PyRep::Integer(st.tutorial_level)),
                    ("finishedTutorial", PyRep::Bool(st.tutorial_complete || st.tutorial_skipped)),
                ])
            });
            Some(Ok(res))
        }

        "get_player_statistics" => {
            let res = read_player_state(ctx, char_id, rewards::format_player_statistics_rep);
            Some(Ok(res))
        }

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
            let res = with_player_state(ctx, char_id, |st, _snap| {
                st.tutorial_level += 1;
                if !st.tutorial_skipped && st.tutorial_level >= 5 {
                    st.tutorial_complete = true;
                }
                PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("totalXP", PyRep::Real(st.experience)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("score", PyRep::Real(st.score)),
                    ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
                    ("tutorialLevel", PyRep::Integer(st.tutorial_level)),
                    ("finishedTutorial", PyRep::Bool(st.tutorial_complete || st.tutorial_skipped)),
                ])
            });
            Some(Ok(res))
        }

        "reset_tutorial" => {
            let res = with_player_state(ctx, char_id, |st, _snap| {
                st.tutorial_level = 1;
                st.tutorial_skipped = false;
                st.tutorial_complete = false;
                PyRep::keyval([
                    ("rank", PyRep::Integer(st.rank)),
                    ("experience", PyRep::Real(st.experience)),
                    ("totalXP", PyRep::Real(st.experience)),
                    ("accuracy", PyRep::Real(st.accuracy)),
                    ("score", PyRep::Real(st.score)),
                    ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
                    ("tutorialLevel", PyRep::Integer(st.tutorial_level)),
                    ("finishedTutorial", PyRep::Bool(st.tutorial_complete || st.tutorial_skipped)),
                ])
            });
            Some(Ok(res))
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
                    let xp_reward = if store.mmos_configured || store.tutorial_xp_reward > 0.0 {
                        store.tutorial_xp_reward.max(0.0)
                    } else {
                        rewards::DEFAULT_TUTORIAL_XP_REWARD
                    };
                    let prev_rank = st.rank;
                    st.experience += xp_reward;
                    let next_rank = rank_for_xp(&store, st.experience, st.rank);
                    for reached_rank in (prev_rank + 1)..=next_rank {
                        let crate_type_id = if reached_rank >= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
                            SUPERIOR_LOOT_CRATE_TYPE_ID
                        } else {
                            LOOT_CRATE_TYPE_ID
                        };
                        rewards::spawn_rank_reward_crate(snap, char_id, crate_type_id);
                    }
                    st.rank = next_rank;
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

        "get_tutorial_xp_reward" => {
            let reward = ctx.db.with_snapshot(|snap| {
                let store = load_discovery_store(snap);
                if store.mmos_configured || store.tutorial_xp_reward > 0.0 {
                    store.tutorial_xp_reward.max(0.0)
                } else {
                    rewards::DEFAULT_TUTORIAL_XP_REWARD
                }
            });
            Some(Ok(PyRep::Real(reward)))
        }

        "get_new_task" => {
            let (task_idx, tid_str, task_uuid, url) = with_player_state(ctx, char_id, |st, snap| {
                let mut store = load_discovery_store(snap);
                let clone_grade = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.clone_grade)
                    .unwrap_or(1);
                rewards::sync_player_bonus_samples(st, &store, clone_grade, ctx.now_unix());
                if st.current_task_index < st.last_submitted_task_index + 1 {
                    st.current_task_index = st.last_submitted_task_index + 1;
                } else {
                    st.current_task_index += 1;
                }
                let idx = st.current_task_index;
                let key = char_task_key(char_id, idx);
                let task = if let Some(existing) = store.char_tasks.get(&key).cloned() {
                    existing
                } else if let Some(seeded) = store.tasks.get(&idx).cloned() {
                    insert_bounded_char_task(&mut store, char_id, idx, seeded.clone());
                    seeded
                } else {
                    let pid = st.project_id.clone();
                    let created = generate_authentic_task_data(idx, &pid, st.max_bonus_samples.max(1));
                    insert_bounded_char_task(&mut store, char_id, idx, created.clone());
                    created
                };
                save_discovery_store(snap, &store);
                (
                    idx,
                    task.task_id,
                    ops::sample_uuid_for_index(idx),
                    task.url,
                )
            });
            Some(Ok(PyRep::keyval([
                ("task_id", PyRep::String(tid_str.clone())),
                ("taskId", PyRep::String(tid_str.clone())),
                ("taskID", PyRep::String(tid_str)),
                ("task_uuid", PyRep::String(task_uuid)),
                ("task_number", PyRep::Integer(task_idx)),
                ("url", PyRep::String(url)),
            ])))
        }

        "new_training_task" => {
            let lvl = read_player_state(ctx, char_id, |st| st.tutorial_level);
            let task_num = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&n| n > 0)
                .unwrap_or(lvl);
            Some(Ok(ops::build_training_task_response(task_num, lvl)))
        }

        "get_remaining_bonus_samples" => {
            let rem = with_player_state(ctx, char_id, |st, snap| {
                let store = load_discovery_store(snap);
                let clone_grade = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.clone_grade)
                    .unwrap_or(1);
                rewards::sync_player_bonus_samples(st, &store, clone_grade, ctx.now_unix());
                st.bonus_samples_left
            });
            Some(Ok(PyRep::Integer(rem)))
        }

        "get_maximum_bonus_samples" => {
            let m = with_player_state(ctx, char_id, |st, snap| {
                let store = load_discovery_store(snap);
                let clone_grade = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.clone_grade)
                    .unwrap_or(1);
                rewards::sync_player_bonus_samples(st, &store, clone_grade, ctx.now_unix());
                st.max_bonus_samples
            });
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

        "post_classification" | "submit" | "submit_classification" => {
            if args.len() < 2 {
                return Some(Err(BridgeRpcError::user_error("InvalidClassification")));
            }
            let task_arg = &args[0];
            let classification_arg = &args[1];

            if !has_valid_classification_payload(classification_arg) {
                return Some(Err(BridgeRpcError::user_error("InvalidClassification")));
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
                rewards::execute_post_classification(
                    snap,
                    st,
                    char_id,
                    req_task_index,
                    classification_arg,
                    now_unix,
                )
            });

            match res {
                Ok(out) => {
                    if let Some(bal) = out.new_bal {
                        let _ = ctx.notify_account_change(char_id, "cash", char_id, bal.to_wire_f64());
                    }
                    crate::services::notify_emitters_skills_trade_godma::emit_player_state_updated(
                        ctx,
                        char_id,
                        out.player_state_rep,
                    );
                    if out.ak_gain > 0 {
                        session.push_notification("OnAnalysisKreditsChange", vec![]);
                        crate::services::notify_emitters_tournaments_anomalies::emit_analysis_kredits_change(ctx, char_id);
                    }
                    let xp_args = vec![PyRep::Real(out.total_xp)];
                    session.push_notification("OnProjectDiscoveryExperience", xp_args.clone());
                    let _ = ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnProjectDiscoveryExperience",
                        "charid",
                        xp_args,
                    );
                    if out.new_rank > out.prev_rank {
                        let lvl_args = vec![
                            PyRep::Integer(out.new_rank),
                            PyRep::Real(out.xp_for_new_rank),
                            PyRep::Real(out.xp_for_next_rank),
                        ];
                        session.push_notification("OnProjectDiscoveryLevelUp", lvl_args.clone());
                        let _ = ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnProjectDiscoveryLevelUp",
                            "charid",
                            lvl_args,
                        );
                    }
                    Some(Ok(out.rep))
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
                let mut created = generate_authentic_task_data(new_idx, &pid, st.max_bonus_samples.max(1));
                created.task_id = tid_str.clone();
                insert_bounded_char_task(
                    &mut store,
                    char_id,
                    new_idx,
                    created,
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
            let res = try_with_player_state(ctx, char_id, |st, snap| {
                let mut store = load_discovery_store(snap);
                let clone_grade = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.clone_grade)
                    .unwrap_or(1);
                rewards::sync_player_bonus_samples(st, &store, clone_grade, ctx.now_unix());
                let resolved_num = parsed_num.or_else(|| {
                    let prefix = format!("{char_id}:");
                    store
                        .char_tasks
                        .iter()
                        .find(|(k, v)| {
                            k.starts_with(&prefix)
                                && (v.url == url
                                    || ops::sample_uuid_for_index(v.task_number) == task_seg)
                        })
                        .map(|(_, v)| v.task_number)
                });
                let Some(task_num) = resolved_num else {
                    return Err(BridgeRpcError::user_error(if parsed_num.is_some() {
                        "TaskNotFound"
                    } else {
                        "InvalidTaskUrl"
                    }));
                };
                let key = char_task_key(char_id, task_num);
                if !store.char_tasks.contains_key(&key) {
                    if st.current_task_index == task_num {
                        let pid = st.project_id.clone();
                        let created = generate_authentic_task_data(task_num, &pid, st.max_bonus_samples.max(1));
                        insert_bounded_char_task(&mut store, char_id, task_num, created);
                    } else {
                        return Err(BridgeRpcError::user_error("TaskNotFound"));
                    }
                }
                let Some(entry) = store.char_tasks.get_mut(&key) else {
                    return Err(BridgeRpcError::user_error("TaskNotFound"));
                };
                entry.fetch_count += 1;
                let fetch_cnt = entry.fetch_count;
                let out = if let Some(ref pl) = entry.payload_json {
                    let mut val: serde_json::Value = serde_json::from_str(pl).unwrap_or_else(|_| {
                        serde_json::json!({
                            "task_id": entry.task_id,
                            "task_number": entry.task_number,
                            "project_id": entry.project_id,
                            "url": entry.url,
                            "samples": entry.samples,
                        })
                    });
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert("fetch_count".into(), serde_json::json!(fetch_cnt));
                        obj.insert("url".into(), serde_json::json!(entry.url));
                        obj.insert("task_id".into(), serde_json::json!(entry.task_id));
                        obj.insert("task_number".into(), serde_json::json!(entry.task_number));
                        obj.insert("project_id".into(), serde_json::json!(entry.project_id));
                        obj.insert("samples".into(), serde_json::json!(entry.samples));
                    }
                    val.to_string()
                } else {
                    serde_json::json!({
                        "url": entry.url,
                        "task_id": entry.task_id,
                        "task_number": entry.task_number,
                        "project_id": entry.project_id,
                        "samples": entry.samples,
                        "fetch_count": fetch_cnt,
                    })
                    .to_string()
                };
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

/// Handles GM `/pd` slash commands (`config`, `consensus`, `bonus`).
pub fn handle_project_discovery_slash(
    ctx: &ServerContext,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.first().copied() != Some("/pd") {
        return None;
    }
    match parts.get(1).copied() {
        Some("config") => {
            let xp_per_rank = parts.get(2).and_then(|s| s.parse::<f64>().ok());
            let tut_xp = parts.get(3).and_then(|s| s.parse::<f64>().ok());
            let acc_xp = parts.get(4).and_then(|s| s.parse::<f64>().ok());
            let inacc_xp = parts.get(5).and_then(|s| s.parse::<f64>().ok());
            let ak = parts.get(6).and_then(|s| s.parse::<i64>().ok());
            let max_bonus = parts.get(7).and_then(|s| s.parse::<i64>().ok());
            let (Some(xp), Some(txp), Some(axp), Some(ixp), Some(ak_val)) =
                (xp_per_rank, tut_xp, acc_xp, inacc_xp, ak)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "Usage: /pd config <xp_per_rank> <tutorial_xp> <acc_xp> <inacc_xp> <ak> [max_bonus]".into(),
                }));
            };
            ctx.db.with_mut_snapshot(|snap| {
                configure_project_discovery_mmos(snap, xp, txp, axp, ixp, ak_val);
                if let Some(mb) = max_bonus {
                    configure_project_discovery_bonus_samples(snap, mb);
                }
            });
            Some(Ok(PyRep::keyval([
                ("configured", PyRep::Bool(true)),
                ("xp_per_rank", PyRep::Real(xp)),
                ("tutorial_xp_reward", PyRep::Real(txp)),
            ])))
        }
        Some("consensus") => {
            let task_idx = parts.get(2).and_then(|s| s.parse::<i64>().ok()).filter(|&i| i > 0);
            let accurate = parts.get(3).and_then(|s| s.parse::<bool>().ok());
            let (Some(idx), Some(acc)) = (task_idx, accurate) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "Usage: /pd consensus <task_index> <true|false>".into(),
                }));
            };
            ctx.db.with_mut_snapshot(|snap| {
                set_project_discovery_task_consensus(snap, idx, acc);
            });
            Some(Ok(PyRep::keyval([
                ("task_index", PyRep::Integer(idx)),
                ("is_accurate", PyRep::Bool(acc)),
            ])))
        }
        Some("bonus") => {
            let Some(mb) = parts.get(2).and_then(|s| s.parse::<i64>().ok()).filter(|&m| m > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "Usage: /pd bonus <max_bonus>".into(),
                }));
            };
            ctx.db.with_mut_snapshot(|snap| {
                configure_project_discovery_bonus_samples(snap, mb);
            });
            Some(Ok(PyRep::keyval([("max_bonus_samples", PyRep::Integer(mb))])))
        }
        _ => Some(Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "Usage: /pd <config|consensus|bonus> ...".into(),
        })),
    }
}

#[cfg(test)] #[path = "project_discovery_tests.rs"] mod project_discovery_tests;
#[cfg(test)]
#[path = "project_discovery_edges_tests.rs"]
mod project_discovery_edges_tests;

