//! V24 Project Discovery sample catalog, geometric gate grading,
//! and multi-player consensus persistence helpers.
#![forbid(unsafe_code)]

use crate::services::project_discovery::{
    ProjectDiscoveryStore, ProjectDiscoveryTaskData, ACTIVE_PROJECT_ID,
};
use eve_db::WorldStateSnapshot;
use eve_proto::PyRep;
use std::collections::HashMap;

/// Authentic V24 tutorial sample UUIDs from `projectdiscovery/client/projects/covid/gameplaymodes/tutorial/{twogates,tryuntilpass}.pyj`.
const V24_TRAINING_SAMPLE_UUIDS: [&str; 5] = [
    "ed134308-e080-49bc-be56-169a5f839c96",
    "be362d8b-4ff4-448b-877f-23b8c2cb4fe0",
    "eb4c6403-e878-43cf-a009-9c8c566ae90f",
    "21cbef0c-684e-4a4c-b1fe-be6e24d894c2",
    "ff9f9a21-954b-4c1c-9a54-16b21cf17670",
];

pub use eve_db::TaskVoteBucket;

fn load_vote_map(snap: &WorldStateSnapshot) -> HashMap<i64, TaskVoteBucket> {
    snap.seasons_and_campaigns.project_discovery_votes.clone()
}

fn save_vote_map(snap: &mut WorldStateSnapshot, votes: &HashMap<i64, TaskVoteBucket>) {
    snap.seasons_and_campaigns.project_discovery_votes = votes.clone();
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

#[cfg(test)]
#[path = "project_discovery_ops_edges_tests.rs"]
mod project_discovery_ops_edges_tests;
