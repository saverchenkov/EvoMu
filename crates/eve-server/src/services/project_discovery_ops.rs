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

/// Minimum 2D polygon IoU required to consider a classification accurate.
pub const CONTROL_IOU_ACCURACY_THRESHOLD: f64 = 0.35;
const PIXEL_CANVAS_EXTENT: f64 = 512.0;
const CONTROL_HALF_EXTENT: f64 = 0.12;
const MIN_POLYGON_AREA_NORM: f64 = 1e-7;
const MAX_SUBMITTED_COORD_ABS: f64 = 1e6;
const MAX_POLYGONS_PER_SUBMISSION: usize = 32;
const MAX_VERTICES_PER_POLYGON: usize = 256;
const MAX_TOTAL_VERTICES_PER_SUBMISSION: usize = 512;
const MAX_JSON_CLASSIFICATION_BYTES: usize = 65_536;
const MAX_PAYLOAD_DEPTH: usize = 8;
pub(crate) const MAX_CACHED_TASKS: usize = 64;
const CENTROID_GRID_SAMPLES: usize = 64;
const FIRST_RESPONDER_MIN_AREA_NORM: f64 = 5e-4;
const FIRST_RESPONDER_MAX_AREA_NORM: f64 = 0.50;
const FIRST_RESPONDER_MAX_CENTER_DIST: f64 = 0.65;
const CANVAS_BOUNDS_MIN: f64 = -0.05;
const CANVAS_BOUNDS_MAX: f64 = 1.05;
const PIP_GRID_SAMPLES: usize = 160;
const POLYGON_CONTAINER_KEYS: [&str; 7] = [
    "polygons",
    "gates",
    "classification",
    "points",
    "vertices",
    "shapes",
    "selections",
];
const CROWD_CONSENSUS_POLYGONS_KEY: &str = "crowd_consensus_polygons";

/// Canonical normalized `[0.0..=1.0]` ground-truth gate square for sample `task_idx`.
pub fn control_solution_polygon(task_idx: i64) -> Vec<[f64; 2]> {
    let [nx, ny] = sample_target_center_norm(task_idx);
    square_polygon(nx, ny, CONTROL_HALF_EXTENT)
}

fn square_polygon(cx: f64, cy: f64, half: f64) -> Vec<[f64; 2]> {
    vec![
        [cx - half, cy - half],
        [cx + half, cy - half],
        [cx + half, cy + half],
        [cx - half, cy + half],
    ]
}

fn dist_sq(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

fn cross_2d(origin: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - origin[0]) * (b[1] - origin[1]) - (a[1] - origin[1]) * (b[0] - origin[0])
}

fn signed_polygon_area(poly: &[[f64; 2]]) -> f64 {
    if poly.len() < 3 {
        return 0.0;
    }
    let origin = poly[0];
    let mut twice_area = 0.0_f64;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        twice_area += cross_2d(origin, a, b);
    }
    0.5 * twice_area
}

fn pyrep_finite_f64(rep: &PyRep) -> Option<f64> {
    let val = match rep {
        PyRep::Real(r) => *r,
        PyRep::Integer(i) => *i as f64,
        _ => return None,
    };
    if val.is_finite() && val.abs() <= MAX_SUBMITTED_COORD_ABS {
        Some(val)
    } else {
        None
    }
}

fn json_value_to_pyrep(val: &serde_json::Value, depth: usize) -> Option<PyRep> {
    if depth > MAX_PAYLOAD_DEPTH {
        return None;
    }
    match val {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PyRep::Integer(i))
            } else {
                let f = n.as_f64().filter(|v| v.is_finite())?;
                Some(PyRep::Real(f))
            }
        }
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(json_value_to_pyrep(item, depth + 1)?);
            }
            Some(PyRep::List(out))
        }
        serde_json::Value::Object(map) => {
            let mut entries = Vec::with_capacity(map.len());
            for (k, v) in map {
                entries.push((PyRep::String(k.clone()), json_value_to_pyrep(v, depth + 1)?));
            }
            Some(PyRep::Dict(entries))
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Some(PyRep::None)
        }
    }
}

fn lookup_dict_field<'a>(entries: &'a [(PyRep, PyRep)], key: &str) -> Option<&'a PyRep> {
    entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v))
}

fn parse_pyrep_point(rep: &PyRep) -> Option<[f64; 2]> {
    match rep {
        PyRep::List(items) | PyRep::Tuple(items) if items.len() == 2 => {
            let x = pyrep_finite_f64(&items[0])?;
            let y = pyrep_finite_f64(&items[1])?;
            Some([x, y])
        }
        PyRep::Dict(entries) => {
            let x = pyrep_finite_f64(lookup_dict_field(entries, "x")?)?;
            let y = pyrep_finite_f64(lookup_dict_field(entries, "y")?)?;
            Some([x, y])
        }
        PyRep::KeyVal(entries) => {
            let x = pyrep_finite_f64(entries.get("x")?)?;
            let y = pyrep_finite_f64(entries.get("y")?)?;
            Some([x, y])
        }
        _ => None,
    }
}

fn parse_pyrep_point_list(items: &[PyRep]) -> Option<Vec<[f64; 2]>> {
    if items.len() < 3 || items.len() > MAX_VERTICES_PER_POLYGON {
        return None;
    }
    let mut pts = Vec::with_capacity(items.len());
    for item in items {
        pts.push(parse_pyrep_point(item)?);
    }
    Some(pts)
}

fn parse_single_polygon_item(rep: &PyRep, depth: usize) -> Option<Vec<[f64; 2]>> {
    if depth > MAX_PAYLOAD_DEPTH {
        return None;
    }
    match rep {
        PyRep::List(items) | PyRep::Tuple(items) => parse_pyrep_point_list(items),
        PyRep::Dict(entries) => {
            for key in ["points", "vertices"] {
                if let Some(sub) = lookup_dict_field(entries, key) {
                    return parse_single_polygon_item(sub, depth + 1);
                }
            }
            None
        }
        PyRep::KeyVal(entries) => {
            for key in ["points", "vertices"] {
                if let Some(sub) = entries.get(key) {
                    return parse_single_polygon_item(sub, depth + 1);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_raw_polygons_from_pyrep(rep: &PyRep, depth: usize) -> Option<Vec<Vec<[f64; 2]>>> {
    if depth > MAX_PAYLOAD_DEPTH {
        return None;
    }
    match rep {
        PyRep::Object { args, .. } => parse_raw_polygons_from_pyrep(args, depth + 1),
        PyRep::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty()
                || trimmed.len() > MAX_JSON_CLASSIFICATION_BYTES
                || !(trimmed.starts_with('{') || trimmed.starts_with('['))
            {
                return None;
            }
            let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
            let py = json_value_to_pyrep(&val, 0)?;
            parse_raw_polygons_from_pyrep(&py, depth + 1)
        }
        PyRep::Dict(entries) => {
            for key in POLYGON_CONTAINER_KEYS {
                if let Some(sub) = lookup_dict_field(entries, key) {
                    return parse_raw_polygons_from_pyrep(sub, depth + 1);
                }
            }
            None
        }
        PyRep::KeyVal(entries) => {
            for key in POLYGON_CONTAINER_KEYS {
                if let Some(sub) = entries.get(key) {
                    return parse_raw_polygons_from_pyrep(sub, depth + 1);
                }
            }
            None
        }
        PyRep::List(items) | PyRep::Tuple(items) => {
            if let Some(pts) = parse_pyrep_point_list(items) {
                return Some(vec![pts]);
            }
            if items.is_empty() || items.len() > MAX_POLYGONS_PER_SUBMISSION {
                return None;
            }
            let mut polys = Vec::with_capacity(items.len());
            let mut total_verts = 0usize;
            for item in items {
                let poly = parse_single_polygon_item(item, depth + 1)?;
                total_verts += poly.len();
                if total_verts > MAX_TOTAL_VERTICES_PER_SUBMISSION {
                    return None;
                }
                polys.push(poly);
            }
            Some(polys)
        }
        _ => None,
    }
}

fn clean_single_polygon(pts: &[[f64; 2]], scale: f64) -> Option<Vec<[f64; 2]>> {
    if pts.len() < 3 || pts.len() > MAX_VERTICES_PER_POLYGON {
        return None;
    }
    let mut dedup: Vec<[f64; 2]> = Vec::with_capacity(pts.len());
    for &[rx, ry] in pts {
        let p = [rx * scale, ry * scale];
        if !p[0].is_finite()
            || !p[1].is_finite()
            || !(CANVAS_BOUNDS_MIN..=CANVAS_BOUNDS_MAX).contains(&p[0])
            || !(CANVAS_BOUNDS_MIN..=CANVAS_BOUNDS_MAX).contains(&p[1])
        {
            return None;
        }
        if dedup.last().is_none_or(|&prev| dist_sq(prev, p) > 1e-18) {
            dedup.push(p);
        }
    }
    if dedup.len() > 1 && dist_sq(dedup[0], dedup[dedup.len() - 1]) <= 1e-18 {
        dedup.pop();
    }
    if dedup.len() < 3 {
        return None;
    }
    // Strip intermediate collinear vertices lying strictly along a straight edge.
    let n = dedup.len();
    let mut cleaned = Vec::with_capacity(n);
    for i in 0..n {
        let prev = dedup[(i + n - 1) % n];
        let curr = dedup[i];
        let next = dedup[(i + 1) % n];
        let ab = [curr[0] - prev[0], curr[1] - prev[1]];
        let bc = [next[0] - curr[0], next[1] - curr[1]];
        let cross = ab[0] * bc[1] - ab[1] * bc[0];
        let dot = ab[0] * bc[0] + ab[1] * bc[1];
        let tol = 1e-12 * dist_sq(prev, curr).max(dist_sq(curr, next)).max(1e-18);
        if cross.abs() <= tol && dot > 0.0 {
            continue;
        }
        cleaned.push(curr);
    }
    if cleaned.len() < 3 {
        return None;
    }
    let area = signed_polygon_area(&cleaned);
    if !area.is_finite() || area.abs() <= MIN_POLYGON_AREA_NORM {
        return None;
    }
    if area < 0.0 {
        cleaned.reverse();
    }
    Some(cleaned)
}

/// Extract and normalize (`[0.0..=1.0]` canvas or `512px`-scaled) non-degenerate 2D gate polygons
/// (`>= 3` finite vertices per polygon) from a client classification payload.
/// Never reads or honors client-supplied accuracy override keys (`accurate`, `is_accurate`, `consensus`).
pub(crate) fn extract_submission_polygons(rep: &PyRep) -> Option<Vec<Vec<[f64; 2]>>> {
    let raw_polys = parse_raw_polygons_from_pyrep(rep, 0)?;
    if raw_polys.is_empty() {
        return None;
    }
    let max_coord = raw_polys
        .iter()
        .flat_map(|poly| poly.iter())
        .fold(0.0_f64, |acc, &[x, y]| acc.max(x.abs()).max(y.abs()));
    if !max_coord.is_finite() {
        return None;
    }
    let scale = if max_coord > 1.5 {
        1.0 / PIXEL_CANVAS_EXTENT
    } else {
        1.0
    };
    let mut out = Vec::with_capacity(raw_polys.len());
    for poly in &raw_polys {
        out.push(clean_single_polygon(poly, scale)?);
    }
    Some(out)
}

pub(crate) fn polygons_to_pyrep(polys: &[Vec<[f64; 2]>]) -> PyRep {
    PyRep::List(
        polys
            .iter()
            .map(|poly| {
                PyRep::List(
                    poly.iter()
                        .map(|&[x, y]| PyRep::Tuple(vec![PyRep::Real(x), PyRep::Real(y)]))
                        .collect(),
                )
            })
            .collect(),
    )
}

fn point_in_or_on_ccw_triangle(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    cross_2d(a, b, p) >= -1e-12
        && cross_2d(b, c, p) >= -1e-12
        && cross_2d(c, a, p) >= -1e-12
}

fn triangle_area(tri: [[f64; 2]; 3]) -> f64 {
    0.5 * cross_2d(tri[0], tri[1], tri[2]).abs()
}

fn point_on_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2], eps: f64) -> bool {
    p[0] >= a[0].min(b[0]) - eps
        && p[0] <= a[0].max(b[0]) + eps
        && p[1] >= a[1].min(b[1]) - eps
        && p[1] <= a[1].max(b[1]) + eps
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let eps = 1e-11;
    if a[0].max(b[0]) + eps < c[0].min(d[0])
        || c[0].max(d[0]) + eps < a[0].min(b[0])
        || a[1].max(b[1]) + eps < c[1].min(d[1])
        || c[1].max(d[1]) + eps < a[1].min(b[1])
    {
        return false;
    }
    let o1 = cross_2d(a, b, c);
    let o2 = cross_2d(a, b, d);
    let o3 = cross_2d(c, d, a);
    let o4 = cross_2d(c, d, b);

    // Exact sign checks prevent edge-length tolerance floors from masking real transverse crossings.
    let ab_straddles = (o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0);
    let cd_straddles = (o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0);
    if ab_straddles && cd_straddles {
        return true;
    }

    let tol_ab = eps * dist_sq(a, b).sqrt();
    let tol_cd = eps * dist_sq(c, d).sqrt();
    (o1.abs() <= tol_ab && point_on_segment(c, a, b, eps))
        || (o2.abs() <= tol_ab && point_on_segment(d, a, b, eps))
        || (o3.abs() <= tol_cd && point_on_segment(a, c, d, eps))
        || (o4.abs() <= tol_cd && point_on_segment(b, c, d, eps))
}

fn adjacent_edges_overlap(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let bc = [c[0] - b[0], c[1] - b[1]];
    let len_ab = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt();
    let len_bc = (bc[0] * bc[0] + bc[1] * bc[1]).sqrt();
    if len_ab == 0.0 || len_bc == 0.0 {
        return true;
    }
    let cross = ab[0] * bc[1] - ab[1] * bc[0];
    let dot = ab[0] * bc[0] + ab[1] * bc[1];
    cross.abs() <= 1e-11 * len_ab * len_bc && dot < 0.0
}

fn polygon_has_self_intersection(poly: &[[f64; 2]]) -> bool {
    let n = poly.len();
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if adjacent_edges_overlap(prev, a, b) {
            return true;
        }
        for j in (i + 2)..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            if segments_intersect(a, b, poly[j], poly[(j + 1) % n]) {
                return true;
            }
        }
    }
    false
}

fn ear_clip_triangulate(poly: &[[f64; 2]]) -> Option<Vec<[[f64; 2]; 3]>> {
    if poly.len() < 3 {
        return None;
    }
    let signed_area = signed_polygon_area(poly);
    if !signed_area.is_finite() || signed_area.abs() <= MIN_POLYGON_AREA_NORM {
        return None;
    }
    let ccw_storage: Vec<[f64; 2]>;
    let poly_ccw: &[[f64; 2]] = if signed_area < 0.0 {
        ccw_storage = poly.iter().copied().rev().collect();
        &ccw_storage
    } else {
        poly
    };
    if poly_ccw.len() == 3 {
        return Some(vec![[poly_ccw[0], poly_ccw[1], poly_ccw[2]]]);
    }
    if polygon_has_self_intersection(poly_ccw) {
        return None;
    }
    let poly_area = signed_area.abs();
    let mut rem: Vec<usize> = (0..poly_ccw.len()).collect();
    let mut tris = Vec::with_capacity(poly_ccw.len() - 2);
    while rem.len() > 3 {
        let len = rem.len();
        let mut clipped = false;
        for i in 0..len {
            let prev_idx = (i + len - 1) % len;
            let next_idx = (i + 1) % len;
            let a = poly_ccw[rem[prev_idx]];
            let b = poly_ccw[rem[i]];
            let c = poly_ccw[rem[next_idx]];
            if cross_2d(a, b, c) <= 1e-12 {
                continue;
            }
            let any_inside = (0..len).any(|j| {
                j != prev_idx
                    && j != i
                    && j != next_idx
                    && point_in_or_on_ccw_triangle(poly_ccw[rem[j]], a, b, c)
            });
            if !any_inside {
                tris.push([a, b, c]);
                rem.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            return None;
        }
    }
    let a = poly_ccw[rem[0]];
    let b = poly_ccw[rem[1]];
    let c = poly_ccw[rem[2]];
    if cross_2d(a, b, c) <= 1e-12 {
        return None;
    }
    tris.push([a, b, c]);
    let tri_sum: f64 = tris.iter().map(|&t| triangle_area(t)).sum();
    if (tri_sum - poly_area).abs() > 1e-5 * poly_area.max(1e-6) {
        return None;
    }
    Some(tris)
}

fn bbox_of_points(pts: &[[f64; 2]]) -> [f64; 4] {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &[x, y] in pts {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    [min_x, min_y, max_x, max_y]
}

fn sutherland_hodgman_triangle_inter_area(subj: [[f64; 2]; 3], clip_ccw: [[f64; 2]; 3]) -> f64 {
    let sb = bbox_of_points(&subj);
    let cb = bbox_of_points(&clip_ccw);
    if sb[2] <= cb[0] || cb[2] <= sb[0] || sb[3] <= cb[1] || cb[3] <= sb[1] {
        return 0.0;
    }
    let mut output: Vec<[f64; 2]> = subj.to_vec();
    for e in 0..3 {
        let u = clip_ccw[e];
        let v = clip_ccw[(e + 1) % 3];
        if output.len() < 3 {
            return 0.0;
        }
        let input = output;
        output = Vec::with_capacity(input.len() + 2);
        let mut s = input[input.len() - 1];
        let mut d_s = cross_2d(u, v, s);
        for &curr in &input {
            let d_e = cross_2d(u, v, curr);
            let s_in = d_s >= -1e-12;
            let e_in = d_e >= -1e-12;
            if e_in {
                if !s_in {
                    let denom = d_s - d_e;
                    if denom.abs() > 1e-18 {
                        let t = (d_s / denom).clamp(0.0, 1.0);
                        output.push([s[0] + t * (curr[0] - s[0]), s[1] + t * (curr[1] - s[1])]);
                    }
                }
                output.push(curr);
            } else if s_in {
                let denom = d_s - d_e;
                if denom.abs() > 1e-18 {
                    let t = (d_s / denom).clamp(0.0, 1.0);
                    output.push([s[0] + t * (curr[0] - s[0]), s[1] + t * (curr[1] - s[1])]);
                }
            }
            s = curr;
            d_s = d_e;
        }
    }
    if output.len() < 3 {
        return 0.0;
    }
    signed_polygon_area(&output).abs()
}

fn triangulate_disjoint_set(polys: &[Vec<[f64; 2]>]) -> Option<(Vec<[[f64; 2]; 3]>, f64)> {
    let mut per_poly = Vec::with_capacity(polys.len());
    let mut all_tris = Vec::new();
    let mut total_area = 0.0;
    for poly in polys {
        let tris = ear_clip_triangulate(poly)?;
        total_area += tris.iter().map(|&t| triangle_area(t)).sum::<f64>();
        per_poly.push(tris);
    }
    for i in 0..per_poly.len() {
        for j in (i + 1)..per_poly.len() {
            for &ta in &per_poly[i] {
                for &tb in &per_poly[j] {
                    if sutherland_hodgman_triangle_inter_area(ta, tb) > 1e-9 {
                        return None;
                    }
                }
            }
        }
    }
    for tris in per_poly {
        all_tris.extend(tris);
    }
    Some((all_tris, total_area))
}

fn point_in_polygon_even_odd(p: [f64; 2], poly: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x_inter = a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
            if p[0] < x_inter {
                inside = !inside;
            }
        }
    }
    inside
}

fn polygons_bbox(polys: &[Vec<[f64; 2]>]) -> Option<[f64; 4]> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for poly in polys {
        for &[x, y] in poly {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x < max_x && min_y < max_y {
        Some([min_x, min_y, max_x, max_y])
    } else {
        None
    }
}

fn grid_union_area(polys: &[Vec<[f64; 2]>], bbox: [f64; 4]) -> f64 {
    let w = bbox[2] - bbox[0];
    let h = bbox[3] - bbox[1];
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let n = PIP_GRID_SAMPLES;
    let mut hits = 0usize;
    for iy in 0..n {
        let y = bbox[1] + ((iy as f64 + 0.5) / n as f64) * h;
        for ix in 0..n {
            let x = bbox[0] + ((ix as f64 + 0.5) / n as f64) * w;
            if polys.iter().any(|poly| point_in_polygon_even_odd([x, y], poly)) {
                hits += 1;
            }
        }
    }
    (hits as f64 / ((n * n) as f64)) * (w * h)
}

fn compute_polygons_iou_grid(sub_polys: &[Vec<[f64; 2]>], ref_polys: &[Vec<[f64; 2]>]) -> f64 {
    let (Some(sb), Some(rb)) = (polygons_bbox(sub_polys), polygons_bbox(ref_polys)) else {
        return 0.0;
    };
    let ub = [
        sb[0].min(rb[0]),
        sb[1].min(rb[1]),
        sb[2].max(rb[2]),
        sb[3].max(rb[3]),
    ];
    let uw = ub[2] - ub[0];
    let uh = ub[3] - ub[1];
    if uw <= 0.0 || uh <= 0.0 {
        return 0.0;
    }
    let n = PIP_GRID_SAMPLES;
    let mut inter_hits = 0usize;
    let mut union_hits = 0usize;
    for iy in 0..n {
        let y = ub[1] + ((iy as f64 + 0.5) / n as f64) * uh;
        for ix in 0..n {
            let x = ub[0] + ((ix as f64 + 0.5) / n as f64) * uw;
            let p = [x, y];
            let in_sub = sub_polys.iter().any(|poly| point_in_polygon_even_odd(p, poly));
            let in_ref = ref_polys.iter().any(|poly| point_in_polygon_even_odd(p, poly));
            if in_sub || in_ref {
                union_hits += 1;
                if in_sub && in_ref {
                    inter_hits += 1;
                }
            }
        }
    }
    if union_hits == 0 {
        0.0
    } else {
        (inter_hits as f64 / union_hits as f64).clamp(0.0, 1.0)
    }
}

/// Compute exact 2D Intersection-over-Union (`[0.0..=1.0]`) between submitted gate polygons and
/// reference gate polygons using ear-clipping triangulation + Sutherland-Hodgman clipping +
/// shoelace area, falling back to even-odd point-in-polygon grid integration for overlapping or
/// self-intersecting gates.
pub fn compute_polygons_iou(sub_polys: &[Vec<[f64; 2]>], ref_polys: &[Vec<[f64; 2]>]) -> f64 {
    if sub_polys.is_empty() || ref_polys.is_empty() {
        return 0.0;
    }
    if let (Some((tris_a, area_a)), Some((tris_b, area_b))) = (
        triangulate_disjoint_set(sub_polys),
        triangulate_disjoint_set(ref_polys),
    ) {
        let mut inter_area = 0.0;
        for &ta in &tris_a {
            for &tb in &tris_b {
                inter_area += sutherland_hodgman_triangle_inter_area(ta, tb);
            }
        }
        let inter_clamped = inter_area.min(area_a).min(area_b);
        let union = (area_a + area_b - inter_clamped).max(0.0);
        return if union <= 1e-12 {
            0.0
        } else {
            (inter_clamped / union).clamp(0.0, 1.0)
        };
    }
    compute_polygons_iou_grid(sub_polys, ref_polys)
}

fn single_polygon_area_centroid(poly: &[[f64; 2]]) -> Option<(f64, [f64; 2])> {
    if poly.len() < 3 || polygon_has_self_intersection(poly) {
        return None;
    }
    let mut twice_area = 0.0;
    let mut cx_sum = 0.0;
    let mut cy_sum = 0.0;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let cross = a[0] * b[1] - b[0] * a[1];
        twice_area += cross;
        cx_sum += (a[0] + b[0]) * cross;
        cy_sum += (a[1] + b[1]) * cross;
    }
    if !twice_area.is_finite() || twice_area.abs() <= 1e-12 {
        return None;
    }
    let centroid = [cx_sum / (3.0 * twice_area), cy_sum / (3.0 * twice_area)];
    let [min_x, min_y, max_x, max_y] = bbox_of_points(poly);
    if !centroid[0].is_finite()
        || !centroid[1].is_finite()
        || centroid[0] < min_x - 1e-6
        || centroid[0] > max_x + 1e-6
        || centroid[1] < min_y - 1e-6
        || centroid[1] > max_y + 1e-6
    {
        return None;
    }
    Some((0.5 * twice_area.abs(), centroid))
}

fn grid_even_odd_centroid(polys: &[Vec<[f64; 2]>]) -> Option<[f64; 2]> {
    let [min_x, min_y, max_x, max_y] = polygons_bbox(polys)?;
    let width = max_x - min_x;
    let height = max_y - min_y;
    if !width.is_finite() || !height.is_finite() || width <= 1e-12 || height <= 1e-12 {
        return None;
    }
    let dx = width / (CENTROID_GRID_SAMPLES as f64);
    let dy = height / (CENTROID_GRID_SAMPLES as f64);
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut hits = 0usize;
    for iy in 0..CENTROID_GRID_SAMPLES {
        let y = min_y + ((iy as f64) + 0.5) * dy;
        for ix in 0..CENTROID_GRID_SAMPLES {
            let x = min_x + ((ix as f64) + 0.5) * dx;
            if polys.iter().any(|poly| point_in_polygon_even_odd([x, y], poly)) {
                sum_x += x;
                sum_y += y;
                hits += 1;
            }
        }
    }
    if hits > 0 {
        Some([sum_x / (hits as f64), sum_y / (hits as f64)])
    } else {
        None
    }
}

fn polygons_centroid(polys: &[Vec<[f64; 2]>]) -> [f64; 2] {
    if !polys.iter().any(|poly| polygon_has_self_intersection(poly)) {
        let mut weight_sum = 0.0;
        let mut wx = 0.0;
        let mut wy = 0.0;
        for poly in polys {
            if let Some((area, [cx, cy])) = single_polygon_area_centroid(poly) {
                weight_sum += area;
                wx += area * cx;
                wy += area * cy;
            }
        }
        if weight_sum > 1e-12 {
            return [wx / weight_sum, wy / weight_sum];
        }
    }
    if let Some(centroid) = grid_even_odd_centroid(polys) {
        return centroid;
    }
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut count = 0usize;
    for poly in polys {
        for &[x, y] in poly {
            sum_x += x;
            sum_y += y;
            count += 1;
        }
    }
    if count == 0 {
        [0.0, 0.0]
    } else {
        [sum_x / count as f64, sum_y / count as f64]
    }
}

fn total_submission_area(polys: &[Vec<[f64; 2]>]) -> f64 {
    if let Some((_, a)) = triangulate_disjoint_set(polys) {
        a
    } else if let Some(bb) = polygons_bbox(polys) {
        grid_union_area(polys, bb)
    } else {
        0.0
    }
}

fn is_plausible_first_responder(polys: &[Vec<[f64; 2]>], task_idx: i64) -> bool {
    let all_in_canvas = polys.iter().flatten().all(|&[x, y]| {
        (CANVAS_BOUNDS_MIN..=CANVAS_BOUNDS_MAX).contains(&x)
            && (CANVAS_BOUNDS_MIN..=CANVAS_BOUNDS_MAX).contains(&y)
    });
    if !all_in_canvas {
        return false;
    }
    if polys.iter().any(|poly| polygon_has_self_intersection(poly)) {
        return false;
    }
    let area = total_submission_area(polys);
    if !(FIRST_RESPONDER_MIN_AREA_NORM..=FIRST_RESPONDER_MAX_AREA_NORM).contains(&area) {
        return false;
    }
    let [sx, sy] = polygons_centroid(polys);
    let [tx, ty] = sample_target_center_norm(task_idx);
    let dist_target = ((sx - tx).powi(2) + (sy - ty).powi(2)).sqrt();
    dist_target <= FIRST_RESPONDER_MAX_CENTER_DIST
}

fn load_crowd_consensus_polygons(
    store: &ProjectDiscoveryStore,
    bucket: &TaskVoteBucket,
    req_task_index: i64,
) -> Option<Vec<Vec<[f64; 2]>>> {
    if let Some(task) = store.tasks.get(&req_task_index) {
        if let Some(ref raw_json) = task.payload_json {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_json) {
                if let Some(poly_val) = val.get(CROWD_CONSENSUS_POLYGONS_KEY) {
                    if let Some(py) = json_value_to_pyrep(poly_val, 0) {
                        if let Some(polys) = extract_submission_polygons(&py) {
                            return Some(polys);
                        }
                    }
                }
            }
        }
    }
    bucket
        .consensus_centroid
        .map(|[cx, cy]| vec![square_polygon(cx, cy, CONTROL_HALF_EXTENT)])
}

fn task_has_crowd_consensus(task: &ProjectDiscoveryTaskData) -> bool {
    task.payload_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .is_some_and(|v| v.get(CROWD_CONSENSUS_POLYGONS_KEY).is_some())
}

pub(crate) fn ensure_store_task_capacity(store: &mut ProjectDiscoveryStore, keep_idx: i64) {
    while store.tasks.len() >= MAX_CACHED_TASKS && !store.tasks.contains_key(&keep_idx) {
        let evict_idx = store
            .tasks
            .iter()
            .filter(|(&idx, task)| idx != keep_idx && !task_has_crowd_consensus(task))
            .map(|(&idx, _)| idx)
            .min()
            .or_else(|| store.tasks.keys().copied().filter(|&idx| idx != keep_idx).min());
        match evict_idx {
            Some(idx) => {
                store.tasks.remove(&idx);
            }
            None => break,
        }
    }
}

pub(crate) fn insert_bounded_store_task_if_absent(
    store: &mut ProjectDiscoveryStore,
    task_idx: i64,
    task: ProjectDiscoveryTaskData,
) {
    if store.tasks.contains_key(&task_idx) {
        return;
    }
    ensure_store_task_capacity(store, task_idx);
    store.tasks.insert(task_idx, task);
}

fn save_crowd_consensus_polygons(
    store: &mut ProjectDiscoveryStore,
    req_task_index: i64,
    polys: &[Vec<[f64; 2]>],
) {
    ensure_store_task_capacity(store, req_task_index);
    let task = store
        .tasks
        .entry(req_task_index)
        .or_insert_with(|| build_sample_task(req_task_index, ACTIVE_PROJECT_ID, 10));
    let mut val = task
        .payload_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = val.as_object_mut() {
        obj.insert(CROWD_CONSENSUS_POLYGONS_KEY.into(), serde_json::json!(polys));
        task.payload_json = Some(val.to_string());
    }
}

/// Evaluate accuracy of a player's gate polygon submission via 2D polygon IoU against
/// `control_solution` (for control tasks) or persisted multi-player crowd consensus polygons
/// (for standard tasks). Coordinate-free/malformed inputs return `(0.0, false)` unless an
/// explicit admin `/pd consensus` override was seeded without prior votes.
pub fn grade_and_record_classification(
    snap: &mut WorldStateSnapshot,
    store: &mut ProjectDiscoveryStore,
    char_id: i64,
    req_task_index: i64,
    classification_arg: &PyRep,
) -> (f64, bool) {
    let parsed_polys = extract_submission_polygons(classification_arg);
    let mut votes = load_vote_map(snap);

    if let Some(&consensus) = store.task_consensus_by_index.get(&req_task_index) {
        let explicit_override = !consensus
            || parsed_polys.is_none()
            || !votes.contains_key(&req_task_index);
        if explicit_override {
            let sample_acc = if consensus { 1.0 } else { 0.0 };
            return (sample_acc, consensus);
        }
    }

    let Some(submitted_polys) = parsed_polys else {
        return (0.0, false);
    };
    let centroid = polygons_centroid(&submitted_polys);
    let is_control = req_task_index % 5 == 0;

    let (sample_acc, is_accurate) = if is_control {
        let ref_polys = vec![control_solution_polygon(req_task_index)];
        let iou = compute_polygons_iou(&submitted_polys, &ref_polys);
        let accurate = iou >= CONTROL_IOU_ACCURACY_THRESHOLD;
        let bucket = votes.entry(req_task_index).or_default();
        bucket.submissions.push((char_id, centroid, accurate));
        (iou, accurate)
    } else {
        let existing_consensus = votes
            .get(&req_task_index)
            .and_then(|b| load_crowd_consensus_polygons(store, b, req_task_index));
        let bucket = votes.entry(req_task_index).or_default();
        if let Some(ref_polys) = existing_consensus {
            let iou = compute_polygons_iou(&submitted_polys, &ref_polys);
            let accurate = iou >= CONTROL_IOU_ACCURACY_THRESHOLD;
            bucket.submissions.push((char_id, centroid, accurate));
            if accurate {
                let acc_count = bucket.submissions.iter().filter(|s| s.2).count().max(1) as f64;
                if let Some([cx, cy]) = bucket.consensus_centroid {
                    bucket.consensus_centroid = Some([
                        (cx * (acc_count - 1.0) + centroid[0]) / acc_count,
                        (cy * (acc_count - 1.0) + centroid[1]) / acc_count,
                    ]);
                } else {
                    bucket.consensus_centroid = Some(centroid);
                }
                if acc_count >= 2.0 {
                    store.task_consensus_by_index.insert(req_task_index, true);
                }
            }
            (iou, accurate)
        } else {
            let plausible = is_plausible_first_responder(&submitted_polys, req_task_index);
            bucket.submissions.push((char_id, centroid, plausible));
            if plausible {
                bucket.consensus_centroid = Some(centroid);
                save_crowd_consensus_polygons(store, req_task_index, &submitted_polys);
                (1.0, true)
            } else {
                (0.0, false)
            }
        }
    };

    save_vote_map(snap, &votes);
    (sample_acc, is_accurate)
}

#[cfg(test)]
#[path = "project_discovery_ops_edges_tests.rs"]
mod project_discovery_ops_edges_tests;
