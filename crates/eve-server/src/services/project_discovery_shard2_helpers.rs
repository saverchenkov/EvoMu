//! V24 Project Discovery sample catalog, analyst rank XP curve, geometric gate grading,
//! and multi-player consensus persistence helpers.
#![forbid(unsafe_code)]

use crate::services::project_discovery::{
    load_discovery_store, save_discovery_store, PlayerProjectState, ProjectDiscoveryStore,
    ProjectDiscoveryTaskData, ACTIVE_PROJECT_ID, ISK_MULTIPLIERS, LOOT_CRATE_TYPE_ID,
    MIN_SUBMISSION_INTERVAL_SECS, NEEDED_LEVEL_FOR_SUPERIOR_CRATES, SUPERIOR_LOOT_CRATE_TYPE_ID,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default tutorial completion XP reward shared identically across `get_tutorial_xp_reward`
/// and `give_tutorial_rewards`.
pub const DEFAULT_TUTORIAL_XP_REWARD: f64 = 500.0;

/// Daily bonus sample allowance for Alpha vs Omega clones when no store override is active.
pub const ALPHA_BONUS_SAMPLES: i64 = 10;
pub const OMEGA_BONUS_SAMPLES: i64 = 20;

/// Authentic V24 tutorial sample UUIDs from `projectdiscovery/client/projects/covid/gameplaymodes/tutorial/{twogates,tryuntilpass}.pyj`.
const V24_TRAINING_SAMPLE_UUIDS: [&str; 5] = [
    "ed134308-e080-49bc-be56-169a5f839c96",
    "be362d8b-4ff4-448b-877f-23b8c2cb4fe0",
    "eb4c6403-e878-43cf-a009-9c8c566ae90f",
    "21cbef0c-684e-4a4c-b1fe-be6e24d894c2",
    "ff9f9a21-954b-4c1c-9a54-16b21cf17670",
];

/// Persistent per-task multi-player consensus and vote log key in `service_state_blobs`.
const PROJECT_DISCOVERY_VOTES_KEY: &str = "project_discovery_votes";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskVoteBucket {
    pub submissions: Vec<(i64, [f64; 2], bool)>,
    pub consensus_centroid: Option<[f64; 2]>,
}

fn load_vote_map(snap: &WorldStateSnapshot) -> HashMap<i64, TaskVoteBucket> {
    snap.service_state_blobs
        .get(PROJECT_DISCOVERY_VOTES_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_vote_map(snap: &mut WorldStateSnapshot, votes: &HashMap<i64, TaskVoteBucket>) {
    if let Ok(json) = serde_json::to_string(votes) {
        snap.service_state_blobs
            .insert(PROJECT_DISCOVERY_VOTES_KEY.to_string(), json);
    }
}

/// Effective maximum daily bonus samples for a character (Omega doubles Alpha allowance unless overridden).
pub fn effective_max_bonus_samples(store: &ProjectDiscoveryStore, clone_grade: u32) -> i64 {
    if let Some(m) = store.max_bonus_samples.filter(|&v| v > 0) {
        return m;
    }
    if clone_grade >= 2 {
        OMEGA_BONUS_SAMPLES
    } else {
        ALPHA_BONUS_SAMPLES
    }
}

/// Sync bonus samples respecting clone grade (Alpha vs Omega) and 24-hour daily refill.
pub fn sync_player_bonus_samples(
    st: &mut PlayerProjectState,
    store: &ProjectDiscoveryStore,
    clone_grade: u32,
    now_unix: u64,
) {
    let target_max = effective_max_bonus_samples(store, clone_grade);
    if st.max_bonus_samples != target_max {
        let diff = target_max.saturating_sub(st.max_bonus_samples);
        st.max_bonus_samples = target_max;
        st.bonus_samples_left = (st.bonus_samples_left + diff).clamp(0, target_max);
    }
    if st.last_bonus_refill_unix == 0 {
        st.last_bonus_refill_unix = now_unix;
        st.bonus_samples_left = st.max_bonus_samples;
    } else if now_unix >= st.last_bonus_refill_unix.saturating_add(86_400) {
        st.bonus_samples_left = st.max_bonus_samples;
        st.last_bonus_refill_unix = now_unix;
    }
}

/// Cumulative XP required to reach `rank + 1` in V24 Project Discovery.
///
/// Honors explicit MMOS curve overrides when configured; otherwise evaluates the progressive
/// analyst rank curve through Rank 25 (`NEEDED_LEVEL_FOR_SUPERIOR_CRATES`) and higher mastery tiers.
pub fn total_needed_xp_for_rank(store: &ProjectDiscoveryStore, rank: i64) -> f64 {
    if rank <= 0 {
        return 0.0;
    }
    if let Some(&override_xp) = store.xp_curve_by_rank.get(&rank) {
        return override_xp.max(0.0);
    }
    if store.xp_per_rank > 0.0 {
        return (rank as f64) * store.xp_per_rank;
    }
    // Progressive analyst rank thresholds: Rank 1 -> 500, Rank 2 -> 1,250, Rank 3 -> 2,250, ...
    let mut cumulative = 0.0;
    for r in 1..=rank {
        let tier_step = if r <= 5 {
            250.0 + 250.0 * (r as f64)
        } else if r <= 15 {
            1_500.0 + 350.0 * ((r - 5) as f64)
        } else if r <= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
            5_000.0 + 500.0 * ((r - 15) as f64)
        } else {
            10_000.0 + 750.0 * ((r - NEEDED_LEVEL_FOR_SUPERIOR_CRATES) as f64)
        };
        cumulative += tier_step;
    }
    cumulative
}

/// Compute analyst rank for `experience` using `total_needed_xp_for_rank`.
pub fn compute_rank_for_xp(store: &ProjectDiscoveryStore, experience: f64, current_rank: i64) -> i64 {
    let mut rank = current_rank.max(1);
    for _ in 0..10_000 {
        let needed = total_needed_xp_for_rank(store, rank);
        if needed > 0.0 && experience >= needed {
            rank += 1;
        } else {
            break;
        }
    }
    rank
}

/// Deterministic UUID-v4 style identifier for flow cytometry sample indices.
pub fn sample_uuid_for_index(task_idx: i64) -> String {
    let u = task_idx.unsigned_abs().wrapping_add(1);
    let p1 = (u.wrapping_mul(0x9E37_79B9_7F4A_7C15)) as u32;
    let p2 = (u.wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 16) as u16;
    let p3 = (0x4000u16) | (((u.wrapping_mul(0x94D0_49BB_1331_11EB) >> 20) as u16) & 0x0FFF);
    let p4 = (0x8000u16) | (((u.wrapping_mul(0xD6E8_FEB8_6659_FD93) >> 24) as u16) & 0x3FFF);
    let p5 = u.wrapping_mul(0xA5A3_5625_E514_57A9) & 0xFFFF_FFFF_FFFF;
    format!("{p1:08x}-{p2:04x}-{p3:04x}-{p4:04x}-{p5:012x}")
}

/// Normalized `[0.0..=1.0]` primary Lymphocyte cluster center for sample `task_idx`.
pub fn sample_target_center_norm(task_idx: i64) -> [f64; 2] {
    let ux = ((task_idx.unsigned_abs().wrapping_mul(73)).wrapping_add(19) % 50) as f64 / 100.0;
    let uy = ((task_idx.unsigned_abs().wrapping_mul(41)).wrapping_add(23) % 50) as f64 / 100.0;
    [0.22 + ux, 0.22 + uy]
}

/// Build an authentic flow cytometry sample task with normalized & pixel-space PBMC gate geometry.
pub fn build_sample_task(
    task_idx: i64,
    project_id: &str,
    samples: i64,
) -> ProjectDiscoveryTaskData {
    let task_type = if task_idx % 5 == 0 { "control" } else { "standard" };
    let sample_uuid = sample_uuid_for_index(task_idx);
    let [nx, ny] = sample_target_center_norm(task_idx);
    let c1_x = (nx * 512.0).round();
    let c1_y = (ny * 512.0).round();
    let c2_x = (((nx + 0.28).min(0.88)) * 512.0).round();
    let c2_y = (((ny + 0.24).min(0.88)) * 512.0).round();
    let url = format!("https://projectdiscovery.eveonline.com/{project_id}/tasks/{task_idx}");

    let mut payload = serde_json::json!({
        "task_id": format!("task_{task_idx}"),
        "task_uuid": sample_uuid,
        "task_number": task_idx,
        "project_id": project_id,
        "url": url,
        "samples": samples,
        "fetch_count": 0,
        "task_type": task_type,
        "experiment": {
            "cell_type": "Peripheral Blood Mononuclear Cells (PBMC)",
            "fluorophores": ["FITC", "PE", "PerCP-Cy5.5", "APC"],
            "markers": ["CD3", "CD4", "CD8", "CD19"],
            "panel_id": format!("PANEL-{project_id}-{task_idx:04}")
        },
        "assets": {
            "image_url": format!("https://projectdiscovery.eveonline.com/assets/{project_id}/{sample_uuid}.png"),
            "thumbnail_url": format!("https://projectdiscovery.eveonline.com/assets/{project_id}/{sample_uuid}_thumb.png")
        },
        "gate_template": {
            "channels": ["FSC-A", "SSC-A"],
            "min_vertices": 3,
            "max_vertices": 12
        },
        "clusters": [
            {"id": 1, "name": "Lymphocytes", "center_norm": [nx, ny], "center": [c1_x, c1_y], "radius": 48.0, "events": 4500},
            {"id": 2, "name": "Monocytes", "center": [c2_x, c2_y], "radius": 40.0, "events": 1800}
        ]
    });
    if task_type == "control" {
        payload["control_solution"] = serde_json::json!([
            [nx - 0.12, ny - 0.12],
            [nx + 0.12, ny - 0.12],
            [nx + 0.12, ny + 0.12],
            [nx - 0.12, ny + 0.12]
        ]);
    }

    ProjectDiscoveryTaskData {
        task_id: format!("task_{task_idx}"),
        task_number: task_idx,
        project_id: project_id.to_string(),
        url,
        samples,
        fetch_count: 0,
        task_type: Some(task_type.to_string()),
        payload_json: Some(payload.to_string()),
    }
}

/// Build an authentic V24 tutorial training task using real `twogates.pyj` / `tryuntilpass.pyj` sample UUIDs
/// and multi-vertex PBMC polygon ground truth.
pub fn build_training_task_response(task_num: i64, current_level: i64) -> PyRep {
    let idx = usize::try_from(task_num.saturating_sub(1)).unwrap_or(0);
    let sample_uuid = V24_TRAINING_SAMPLE_UUIDS
        .get(idx)
        .map(|s| (*s).to_string())
        .unwrap_or_else(|| sample_uuid_for_index(task_num.saturating_add(10_000)));
    let [nx, ny] = sample_target_center_norm(task_num);
    let poly_pts = vec![
        PyRep::List(vec![PyRep::Real(nx - 0.10), PyRep::Real(ny - 0.10)]),
        PyRep::List(vec![PyRep::Real(nx + 0.10), PyRep::Real(ny - 0.10)]),
        PyRep::List(vec![PyRep::Real(nx + 0.10), PyRep::Real(ny + 0.10)]),
        PyRep::List(vec![PyRep::Real(nx - 0.10), PyRep::Real(ny + 0.10)]),
    ];
    let solution = PyRep::List(vec![PyRep::List(poly_pts)]);
    PyRep::keyval([
        ("task_id", PyRep::String(sample_uuid.clone())),
        ("taskId", PyRep::String(sample_uuid.clone())),
        ("taskID", PyRep::String(sample_uuid.clone())),
        ("level", PyRep::Integer(current_level)),
        ("task_number", PyRep::Integer(task_num)),
        ("project_id", PyRep::String(ACTIVE_PROJECT_ID.into())),
        ("solution", solution),
        (
            "url",
            PyRep::String(format!(
                "https://projectdiscovery.eveonline.com/{ACTIVE_PROJECT_ID}/training/{sample_uuid}"
            )),
        ),
    ])
}

/// Extract normalized `[0.0..=1.0]` centroid coordinates and any explicit consensus override flag from
/// a client classification payload (lists of points/polygons, JSON strings, dicts, or KeyVals).
fn extract_submission_centroid(rep: &PyRep) -> (Option<[f64; 2]>, Option<bool>) {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut explicit_acc: Option<bool> = None;

    fn collect_json(
        val: &serde_json::Value,
        xs: &mut Vec<f64>,
        ys: &mut Vec<f64>,
        explicit_acc: &mut Option<bool>,
    ) {
        match val {
            serde_json::Value::Array(arr) => {
                if arr.len() == 2 && arr[0].is_f64() && arr[1].is_f64() {
                    if let (Some(x), Some(y)) = (arr[0].as_f64(), arr[1].as_f64()) {
                        xs.push(x);
                        ys.push(y);
                        return;
                    }
                }
                for item in arr {
                    collect_json(item, xs, ys, explicit_acc);
                }
            }
            serde_json::Value::Object(map) => {
                for key in ["accurate", "is_accurate", "consensus"] {
                    if let Some(b) = map.get(key).and_then(|v| v.as_bool()) {
                        *explicit_acc = Some(b);
                    }
                }
                for v in map.values() {
                    collect_json(v, xs, ys, explicit_acc);
                }
            }
            _ => {}
        }
    }

    fn collect_pyrep(
        rep: &PyRep,
        xs: &mut Vec<f64>,
        ys: &mut Vec<f64>,
        explicit_acc: &mut Option<bool>,
    ) {
        match rep {
            PyRep::List(items) | PyRep::Tuple(items) => {
                let num_pair = if items.len() == 2 {
                    match (&items[0], &items[1]) {
                        (PyRep::Real(x), PyRep::Real(y)) => Some((*x, *y)),
                        (PyRep::Integer(x), PyRep::Integer(y)) => Some((*x as f64, *y as f64)),
                        (PyRep::Real(x), PyRep::Integer(y)) => Some((*x, *y as f64)),
                        (PyRep::Integer(x), PyRep::Real(y)) => Some((*x as f64, *y)),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((x, y)) = num_pair {
                    xs.push(x);
                    ys.push(y);
                } else {
                    for it in items {
                        collect_pyrep(it, xs, ys, explicit_acc);
                    }
                }
            }
            PyRep::String(s) => {
                let t = s.trim();
                if t.eq_ignore_ascii_case("false") || t.eq_ignore_ascii_case("invalid") {
                    *explicit_acc = Some(false);
                } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
                    collect_json(&v, xs, ys, explicit_acc);
                }
            }
            PyRep::Dict(entries) => {
                for (k, v) in entries {
                    if let Some(ks) = k.as_str() {
                        if matches!(ks, "accurate" | "is_accurate" | "consensus") {
                            if let Some(b) = v.as_bool() {
                                *explicit_acc = Some(b);
                            }
                        }
                    }
                    collect_pyrep(v, xs, ys, explicit_acc);
                }
            }
            PyRep::KeyVal(entries) => {
                for (k, v) in entries {
                    if matches!(k.as_str(), "accurate" | "is_accurate" | "consensus") {
                        if let Some(b) = v.as_bool() {
                            *explicit_acc = Some(b);
                        }
                    }
                    collect_pyrep(v, xs, ys, explicit_acc);
                }
            }
            PyRep::Object { args, .. } => collect_pyrep(args, xs, ys, explicit_acc),
            _ => {}
        }
    }

    collect_pyrep(rep, &mut xs, &mut ys, &mut explicit_acc);
    if xs.is_empty() || ys.is_empty() {
        return (None, explicit_acc);
    }
    let avg_x = xs.iter().sum::<f64>() / (xs.len() as f64);
    let avg_y = ys.iter().sum::<f64>() / (ys.len() as f64);
    let max_coord = xs
        .iter()
        .chain(ys.iter())
        .fold(0.0f64, |acc, v| acc.max(v.abs()));
    let norm = if max_coord > 1.5 {
        [avg_x / 512.0, avg_y / 512.0]
    } else {
        [avg_x, avg_y]
    };
    (Some(norm), explicit_acc)
}

/// Evaluate accuracy of a player's gate submission against control ground truth or multi-player
/// crowd consensus, updating persisted consensus state across characters.
pub fn grade_and_record_classification(
    snap: &mut WorldStateSnapshot,
    store: &mut ProjectDiscoveryStore,
    char_id: i64,
    req_task_index: i64,
    classification_arg: &PyRep,
) -> (f64, bool) {
    let (submitted_centroid, explicit_flag) = extract_submission_centroid(classification_arg);
    let [tx, ty] = sample_target_center_norm(req_task_index);

    if let Some(&consensus) = store.task_consensus_by_index.get(&req_task_index) {
        let sample_acc = if consensus { 1.0 } else { 0.0 };
        return (sample_acc, consensus);
    }

    if let Some(false) = explicit_flag {
        return (0.0, false);
    }

    let is_control = req_task_index % 5 == 0;
    let mut votes = load_vote_map(snap);

    let (sample_acc, is_accurate, already_recorded) = if is_control {
        match submitted_centroid {
            Some([sx, sy]) => {
                let dist = ((sx - tx).powi(2) + (sy - ty).powi(2)).sqrt();
                if dist <= 0.35 {
                    let q = (1.0 - (dist / 0.35) * 0.25).clamp(0.75, 1.0);
                    (q, true, false)
                } else {
                    ((1.0 - dist).clamp(0.0, 0.35), false, false)
                }
            }
            None => (1.0, true, false),
        }
    } else {
        // Multi-player crowd consensus: aggregate votes across players when multiple analysts classify
        // the same sample; single first-responder or concordant multi-player votes establish consensus.
        match submitted_centroid {
            Some([sx, sy]) => {
                let bucket = votes.entry(req_task_index).or_default();
                if let Some([cx, cy]) = bucket.consensus_centroid {
                    let dist = ((sx - cx).powi(2) + (sy - cy).powi(2)).sqrt();
                    if dist <= 0.35 {
                        bucket.submissions.push((char_id, [sx, sy], true));
                        let n = bucket.submissions.len() as f64;
                        let new_cx = (cx * (n - 1.0) + sx) / n;
                        let new_cy = (cy * (n - 1.0) + sy) / n;
                        bucket.consensus_centroid = Some([new_cx, new_cy]);
                        store.task_consensus_by_index.insert(req_task_index, true);
                        (1.0, true, true)
                    } else {
                        bucket.submissions.push((char_id, [sx, sy], false));
                        (0.2, false, true)
                    }
                } else {
                    let dist_target = ((sx - tx).powi(2) + (sy - ty).powi(2)).sqrt();
                    let plausible = dist_target <= 0.65;
                    bucket.consensus_centroid = Some([sx, sy]);
                    bucket.submissions.push((char_id, [sx, sy], plausible));
                    if plausible {
                        (1.0, true, true)
                    } else {
                        (0.2, false, true)
                    }
                }
            }
            None => (1.0, true, false),
        }
    };

    if !already_recorded {
        if let Some(c) = submitted_centroid {
            let bucket = votes.entry(req_task_index).or_default();
            bucket.submissions.push((char_id, c, is_accurate));
        }
    }
    save_vote_map(snap, &votes);
    (sample_acc, is_accurate)
}

/// Spawn rank-up reward crate (`LOOT_CRATE_TYPE_ID = 55358` or `SUPERIOR_LOOT_CRATE_TYPE_ID = 55359`)
/// into the character's current or home station hangar on rank advancement.
pub fn spawn_rank_reward_crate(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    crate_type_id: u32,
) {
    let station_id = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.station_id.or(Some(c.home_station_id)))
        .filter(|&id| id > 0);
    let Some(loc_id) = station_id else {
        return;
    };
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id: crate_type_id,
            owner_id: char_id,
            location_id: loc_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
}

/// Format `get_player_statistics` reply expected by `rewardview.pyj` and `subcellularatlas.pyj`.
pub fn format_player_statistics_rep(st: &PlayerProjectState) -> PyRep {
    let proj = PyRep::Dict(vec![
        (PyRep::String("score".into()), PyRep::Real(st.score)),
        (PyRep::String("accuracy".into()), PyRep::Real(st.accuracy)),
        (PyRep::String("rank".into()), PyRep::Integer(st.rank)),
        (PyRep::String("experience".into()), PyRep::Real(st.experience)),
        (
            PyRep::String("tasks_submitted".into()),
            PyRep::Integer(st.tasks_submitted),
        ),
    ]);
    let projects_map = PyRep::Dict(vec![
        (PyRep::String(st.project_id.clone()), proj.clone()),
        (PyRep::String(ACTIVE_PROJECT_ID.into()), proj.clone()),
    ]);
    PyRep::keyval([
        ("rank", PyRep::Integer(st.rank)),
        ("experience", PyRep::Real(st.experience)),
        ("score", PyRep::Real(st.score)),
        ("accuracy", PyRep::Real(st.accuracy)),
        ("tasks_submitted", PyRep::Integer(st.tasks_submitted)),
        ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
        ("project", proj.clone()),
        ("projects", projects_map),
        ("project_list", PyRep::List(vec![proj])),
    ])
}

/// Process a citizen-science classification submission end-to-end inside a snapshot transaction.
#[allow(clippy::type_complexity)]
pub fn execute_post_classification(
    snap: &mut WorldStateSnapshot,
    st: &mut PlayerProjectState,
    char_id: i64,
    req_task_index: i64,
    classification_arg: &PyRep,
    now_unix: u64,
) -> Result<
    (
        PyRep,
        PyRep,
        Option<eve_db::IskCents>,
        f64,
        f64,
        i64,
        i64,
        f64,
        f64,
        i64,
    ),
    BridgeRpcError,
> {
    if st.last_submission_unix > 0
        && (now_unix <= st.last_submission_unix
            || now_unix.saturating_sub(st.last_submission_unix) < MIN_SUBMISSION_INTERVAL_SECS)
    {
        return Err(BridgeRpcError::user_error("ProjectDiscoverySubmissionTooFast"));
    }
    if req_task_index != st.current_task_index {
        return Err(BridgeRpcError {
            code: "InvalidTaskId".into(),
            message: format!(
                "task_id {} does not match current active task {}",
                req_task_index, st.current_task_index
            ),
        });
    }
    if st.last_submitted_task_index >= st.current_task_index {
        return Err(BridgeRpcError {
            code: "TaskAlreadySubmitted".into(),
            message: "Active task has already been submitted".into(),
        });
    }

    let clone_grade = snap
        .characters
        .get(&char_id)
        .map(|c| c.clone_grade)
        .unwrap_or(1);
    let mut store = load_discovery_store(snap);
    sync_player_bonus_samples(st, &store, clone_grade, now_unix);

    let prev_tasks = st.tasks_submitted;
    st.last_submitted_task_index = st.current_task_index;
    st.last_submission_unix = now_unix;
    st.tasks_submitted += 1;

    let (sample_acc, is_accurate) =
        grade_and_record_classification(snap, &mut store, char_id, req_task_index, classification_arg);

    let base_xp_acc = if store.classification_xp_accurate > 0.0 {
        store.classification_xp_accurate
    } else {
        150.0
    };
    let base_xp_inacc = if store.mmos_configured {
        store.classification_xp_inaccurate.max(0.0)
    } else {
        25.0
    };
    let base_ak = if store.classification_ak_reward > 0 {
        store.classification_ak_reward
    } else {
        20
    };
    save_discovery_store(snap, &store);

    st.accuracy = if prev_tasks <= 0 {
        sample_acc
    } else {
        (((st.accuracy * 9.0) + sample_acc) / 10.0).clamp(0.0, 1.0)
    };
    st.score = (st.accuracy * 100.0).clamp(0.0, 1000.0);

    let had_bonus = st.bonus_samples_left > 0;
    st.bonus_samples_left = st.bonus_samples_left.saturating_sub(1);
    let bonus_mult = if had_bonus { 2.0 } else { 1.0 };
    let xp_gain = (if is_accurate { base_xp_acc } else { base_xp_inacc }) * bonus_mult;
    let ak_gain = if is_accurate { base_ak } else { 0 };

    let prev_rank = st.rank;
    st.experience += xp_gain;
    st.analysis_kredits = st.analysis_kredits.saturating_add(ak_gain);

    // V24 `projectdiscovery/common/projects/covid/iskmultipliers.pyj`: `ISK_MULTIPLIERS = [500, 800, 1000]`
    let tier_idx = if st.accuracy >= 0.9 {
        2
    } else if st.accuracy >= 0.5 {
        1
    } else {
        0
    };
    let isk_amount = if is_accurate {
        let base_isk = ISK_MULTIPLIERS[tier_idx] * st.rank.max(1);
        if had_bonus {
            base_isk * 2
        } else {
            base_isk
        }
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

    let new_rank = compute_rank_for_xp(&store, st.experience, st.rank);
    let mut crates_granted = Vec::new();
    for reached_rank in (prev_rank + 1)..=new_rank {
        let crate_type_id = if reached_rank >= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
            SUPERIOR_LOOT_CRATE_TYPE_ID
        } else {
            LOOT_CRATE_TYPE_ID
        };
        spawn_rank_reward_crate(snap, char_id, crate_type_id);
        crates_granted.push(PyRep::Integer(crate_type_id as i64));
    }
    st.rank = new_rank;

    let xp_for_new_rank = total_needed_xp_for_rank(&store, st.rank);
    let xp_for_next_rank = total_needed_xp_for_rank(&store, st.rank + 1);
    let player_state_rep = PyRep::keyval([
        ("rank", PyRep::Integer(st.rank)),
        ("experience", PyRep::Real(st.experience)),
        ("totalXP", PyRep::Real(st.experience)),
        ("accuracy", PyRep::Real(st.accuracy)),
        ("score", PyRep::Real(st.score)),
        ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
        (
            "finishedTutorial",
            PyRep::Bool(st.tutorial_complete || st.tutorial_skipped),
        ),
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
        (
            "bonusSamplesAfterClassification",
            PyRep::Integer(st.bonus_samples_left),
        ),
        ("loot_crates", PyRep::List(crates_granted)),
        ("playerState", player_state_rep.clone()),
        ("player", PyRep::keyval([("score", PyRep::Real(st.accuracy))])),
        ("rank", PyRep::Integer(st.rank)),
    ]);
    Ok((
        rep,
        player_state_rep,
        new_bal,
        xp_gain,
        st.experience,
        prev_rank,
        st.rank,
        xp_for_new_rank,
        xp_for_next_rank,
        ak_gain,
    ))
}
