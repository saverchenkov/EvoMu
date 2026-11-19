//! Behavioral edge-case unit tests for `services::project_discovery_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::project_discovery::ProjectDiscoveryStore;
use eve_db::WorldRepository;
use eve_proto::PyRep;

#[test]
fn sample_uuid_and_target_center_are_deterministic_and_in_bounds() {
    let mut uuids = std::collections::HashSet::new();
    for idx in [-5, -1, 0, 1, 2, 5, 10, 100] {
        let u = sample_uuid_for_index(idx);
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(u, sample_uuid_for_index(idx));
        uuids.insert(u);

        let [nx, ny] = sample_target_center_norm(idx);
        assert!((0.22..0.72).contains(&nx), "nx out of bounds: {nx}");
        assert!((0.22..0.72).contains(&ny), "ny out of bounds: {ny}");
    }
    assert_eq!(sample_uuid_for_index(-5), sample_uuid_for_index(5));
    assert_eq!(sample_uuid_for_index(-1), sample_uuid_for_index(1));
    assert_eq!(uuids.len(), 6);
}

#[test]
fn build_sample_task_and_training_task_cover_control_standard_and_tutorial_slots() {
    let ctrl = build_sample_task(10, ACTIVE_PROJECT_ID, 25);
    assert_eq!(ctrl.task_id, "task_10");
    assert_eq!(ctrl.task_type.as_deref(), Some("control"));
    let ctrl_json: serde_json::Value =
        serde_json::from_str(ctrl.payload_json.as_deref().unwrap()).unwrap();
    assert!(ctrl_json.get("control_solution").is_some());
    assert_eq!(ctrl_json["clusters"].as_array().unwrap().len(), 2);

    let std_task = build_sample_task(11, "custom-proj", 12);
    assert_eq!(std_task.task_type.as_deref(), Some("standard"));
    let std_json: serde_json::Value =
        serde_json::from_str(std_task.payload_json.as_deref().unwrap()).unwrap();
    assert!(std_json.get("control_solution").is_none());

    // Authentic V24 tutorial UUIDs for slots 1..=5, generated fallback for 0 and 6+
    for slot in 1..=5 {
        let rep = build_training_task_response(slot, 2);
        match rep {
            PyRep::KeyVal(entries) => {
                let tid = entries
                    .iter()
                    .find(|(k, _)| *k == "task_id")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap();
                assert_eq!(tid, V24_TRAINING_SAMPLE_UUIDS[(slot - 1) as usize]);
                let level = entries
                    .iter()
                    .find(|(k, _)| *k == "level")
                    .and_then(|(_, v)| v.as_i64());
                assert_eq!(level, Some(2));
            }
            other => panic!("expected KeyVal, got {other:?}"),
        }
    }

    let fallback_rep = build_training_task_response(6, 1);
    if let PyRep::KeyVal(entries) = fallback_rep {
        let tid = entries
            .iter()
            .find(|(k, _)| *k == "task_id")
            .and_then(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(tid, sample_uuid_for_index(10_006));
    } else {
        panic!("expected KeyVal");
    }
}

#[test]
fn grade_classification_handles_consensus_overrides_explicit_flags_and_payload_shapes() {
    let repo = WorldRepository::with_demo_seed();

    repo.with_mut_snapshot(|snap| {
        let mut store = ProjectDiscoveryStore::default();

        // 1. Persisted task_consensus_by_index override short-circuits immediately
        store.task_consensus_by_index.insert(7, true);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 1, 7, &PyRep::None),
            (1.0, true)
        );
        store.task_consensus_by_index.insert(8, false);
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 1, 8, &PyRep::None),
            (0.0, false)
        );

        // 2. Coord-free, <3-vertex, degenerate, non-finite, and spoof-flag-only payloads return (0.0, false)
        for bad_arg in [
            PyRep::String("false".into()),
            PyRep::String("INVALID".into()),
            PyRep::String("accurate".into()),
            PyRep::Dict(vec![(PyRep::String("accurate".into()), PyRep::Bool(true))]),
            PyRep::keyval([("consensus", PyRep::Bool(true))]),
            PyRep::String(r#"{"is_accurate": true, "points": [[0.5, 0.5], [0.6, 0.6]]}"#.into()),
            PyRep::Object {
                type_name: "util.KeyVal".into(),
                args: Box::new(PyRep::keyval([("is_accurate", PyRep::Bool(true))])),
            },
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(0.5), PyRep::Real(0.5)]),
                PyRep::List(vec![PyRep::Real(0.6), PyRep::Real(0.5)]),
                PyRep::List(vec![PyRep::Real(0.7), PyRep::Real(0.5)]),
            ]),
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(f64::NAN), PyRep::Real(0.5)]),
                PyRep::List(vec![PyRep::Real(0.6), PyRep::Real(0.5)]),
                PyRep::List(vec![PyRep::Real(0.5), PyRep::Real(0.6)]),
            ]),
        ] {
            assert_eq!(
                grade_and_record_classification(snap, &mut store, 1, 10, &bad_arg),
                (0.0, false),
                "unexpected pass for {bad_arg:?}"
            );
        }

        // 3. Control task (idx % 5 == 0): exact polygon IoU, pixel scaling, concave L-shape, boundary thresholds, spoof resistance
        let [tx, ty] = sample_target_center_norm(10);
        let box_poly = |cx: f64, cy: f64, half: f64| {
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(cx - half), PyRep::Real(cy - half)]),
                PyRep::List(vec![PyRep::Real(cx + half), PyRep::Real(cy - half)]),
                PyRep::List(vec![PyRep::Real(cx + half), PyRep::Real(cy + half)]),
                PyRep::List(vec![PyRep::Real(cx - half), PyRep::Real(cy + half)]),
            ])
        };

        // Exact control square (with spoofed accurate=false key ignored) -> IoU 1.0
        let exact_spoof_false = PyRep::Dict(vec![
            (PyRep::String("accurate".into()), PyRep::Bool(false)),
            (PyRep::String("is_accurate".into()), PyRep::Bool(false)),
            (PyRep::String("consensus".into()), PyRep::Bool(false)),
            (PyRep::String("polygons".into()), PyRep::List(vec![box_poly(tx, ty, 0.12)])),
        ]);
        let (acc_exact, ok_exact) =
            grade_and_record_classification(snap, &mut store, 101, 10, &exact_spoof_false);
        assert!(ok_exact);
        assert!((acc_exact - 1.0).abs() < 1e-9, "expected IoU 1.0, got {acc_exact}");

        // Pixel-space (>1.5) polygon near control box -> IoU >= 0.90
        let px_poly = PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(((tx - 0.12) * 512.0).round() as i64),
                PyRep::Real((ty - 0.12) * 512.0),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(((tx + 0.12) * 512.0).round() as i64),
                PyRep::Real((ty - 0.12) * 512.0),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(((tx + 0.12) * 512.0).round() as i64),
                PyRep::Real((ty + 0.12) * 512.0),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(((tx - 0.12) * 512.0).round() as i64),
                PyRep::Real((ty + 0.12) * 512.0),
            ]),
        ]);
        let (acc_px, ok_px) =
            grade_and_record_classification(snap, &mut store, 102, 10, &px_poly);
        assert!(ok_px && acc_px >= 0.90, "expected pixel IoU >= 0.90, got {acc_px}");

        // Concave L-shape covering 3 quadrants of control_solution -> exact analytical IoU 0.75
        let concave_l = PyRep::List(vec![
            PyRep::List(vec![PyRep::Real(tx - 0.12), PyRep::Real(ty - 0.12)]),
            PyRep::List(vec![PyRep::Real(tx + 0.12), PyRep::Real(ty - 0.12)]),
            PyRep::List(vec![PyRep::Real(tx + 0.12), PyRep::Real(ty)]),
            PyRep::List(vec![PyRep::Real(tx), PyRep::Real(ty)]),
            PyRep::List(vec![PyRep::Real(tx), PyRep::Real(ty + 0.12)]),
            PyRep::List(vec![PyRep::Real(tx - 0.12), PyRep::Real(ty + 0.12)]),
        ]);
        let (acc_l, ok_l) =
            grade_and_record_classification(snap, &mut store, 103, 10, &concave_l);
        assert!(ok_l);
        assert!((acc_l - 0.75).abs() < 1e-9, "expected L-shape IoU 0.75, got {acc_l}");

        // Pass boundary: 0.15 x 0.15 centered square -> IoU = 0.0225 / 0.0576 = 0.390625 >= 0.35
        let (acc_pass_b, ok_pass_b) = grade_and_record_classification(
            snap,
            &mut store,
            104,
            10,
            &box_poly(tx, ty, 0.075),
        );
        assert!(ok_pass_b);
        assert!((acc_pass_b - 0.390625).abs() < 1e-6, "got {acc_pass_b}");

        // Fail boundary: 0.24 x 0.24 shifted by 0.15 -> IoU = 0.0216 / 0.0936 = 0.230769 < 0.35
        let (acc_fail_b, ok_fail_b) = grade_and_record_classification(
            snap,
            &mut store,
            105,
            10,
            &box_poly(tx + 0.15, ty, 0.12),
        );
        assert!(!ok_fail_b);
        assert!((acc_fail_b - (0.0216 / 0.0936)).abs() < 1e-6, "got {acc_fail_b}");

        // Valid inside-canvas disjoint polygon with spoofed accurate=true fails IoU == 0.0
        let far_spoof = PyRep::Dict(vec![
            (PyRep::String("accurate".into()), PyRep::Bool(true)),
            (PyRep::String("polygons".into()), PyRep::List(vec![box_poly(0.05, 0.05, 0.04)])),
        ]);
        let (acc_miss, ok_miss) =
            grade_and_record_classification(snap, &mut store, 106, 10, &far_spoof);
        assert!(!ok_miss && acc_miss == 0.0);

        // Out-of-canvas spoof polygon is rejected at extraction (`None`) and graded (0.0, false)
        let oob_spoof = PyRep::Dict(vec![
            (PyRep::String("accurate".into()), PyRep::Bool(true)),
            (PyRep::String("polygons".into()), PyRep::List(vec![box_poly(1.25, 1.25, 0.12)])),
        ]);
        assert!(extract_submission_polygons(&oob_spoof).is_none());
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 106_001, 10, &oob_spoof),
            (0.0, false)
        );

        // Full-canvas cheat box [0..1]^2 fails IoU = 0.0576 < 0.35
        let (acc_giant, ok_giant) = grade_and_record_classification(
            snap,
            &mut store,
            107,
            10,
            &box_poly(0.5, 0.5, 0.5),
        );
        assert!(!ok_giant && acc_giant < 0.1);

        // Empty list returns (0.0, false) — never (1.0, true)
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 108, 10, &PyRep::List(vec![])),
            (0.0, false)
        );

        // Overlapping duplicate gates use single-shared-grid fallback and remain <= 1.0
        let ctrl_poly = control_solution_polygon(10);
        let dup_iou = compute_polygons_iou(&[ctrl_poly.clone(), ctrl_poly.clone()], &[ctrl_poly.clone()]);
        assert!((0.95..=1.0).contains(&dup_iou), "got {dup_iou}");

        // F-01 regression: 3-polygon pixel bounding-box aliasing exploit rejected at extraction AND
        // single-shared-grid direct compute_polygons_iou_grid resists unbounded disparate boxes
        let f01_exploit = PyRep::List(vec![
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(25.6), PyRep::Real(25.6)]),
                PyRep::List(vec![PyRep::Real(460.8), PyRep::Real(25.6)]),
                PyRep::List(vec![PyRep::Real(460.8), PyRep::Real(460.8)]),
                PyRep::List(vec![PyRep::Real(25.6), PyRep::Real(460.8)]),
            ]),
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0)]),
                PyRep::List(vec![PyRep::Real(2359.296), PyRep::Real(0.0)]),
                PyRep::List(vec![PyRep::Real(256.0), PyRep::Real(256.0)]),
            ]),
            PyRep::List(vec![
                PyRep::List(vec![PyRep::Real(5.12), PyRep::Real(163_072.0)]),
                PyRep::List(vec![PyRep::Real(10.24), PyRep::Real(163_072.0)]),
                PyRep::List(vec![PyRep::Real(10.24), PyRep::Real(163_840.0)]),
                PyRep::List(vec![PyRep::Real(5.12), PyRep::Real(163_840.0)]),
            ]),
        ]);
        assert!(extract_submission_polygons(&f01_exploit).is_none());
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 109, 10, &f01_exploit),
            (0.0, false)
        );
        let grid_direct = compute_polygons_iou_grid(
            &[
                vec![[0.05, 0.05], [0.90, 0.05], [0.90, 0.90], [0.05, 0.90]],
                vec![[0.0, 0.0], [4.608, 0.0], [0.5, 0.5]],
                vec![[0.01, 318.5], [0.02, 318.5], [0.02, 320.0], [0.01, 320.0]],
            ],
            &[ctrl_poly.clone()],
        );
        assert!(grid_direct < 0.01, "single shared grid must prevent aliasing IoU=1.0, got {grid_direct}");

        // F-02 regression: 6-vertex CCW double-wound triangle cannot pass via ear-clipping double-count
        let ox = tx - 0.12;
        let oy = ty - 0.12;
        let double_wound_pts = vec![
            [ox + 0.04, oy + 0.04],
            [ox + 0.20, oy + 0.04],
            [ox + 0.04, oy + 0.22],
            [ox + 0.0405, oy + 0.039],
            [ox + 0.201, oy + 0.04],
            [ox + 0.039, oy + 0.222],
        ];
        assert!(polygon_has_self_intersection(&double_wound_pts));
        assert!(ear_clip_triangulate(&double_wound_pts).is_none());
        let double_wound_iou = compute_polygons_iou(&[double_wound_pts.clone()], &[ctrl_poly.clone()]);
        assert!(double_wound_iou < CONTROL_IOU_ACCURACY_THRESHOLD, "got {double_wound_iou}");
        let double_wound_rep = PyRep::List(vec![PyRep::List(
            double_wound_pts
                .iter()
                .map(|&[px, py]| PyRep::List(vec![PyRep::Real(px), PyRep::Real(py)]))
                .collect(),
        )]);
        let (dw_acc, dw_ok) =
            grade_and_record_classification(snap, &mut store, 110, 10, &double_wound_rep);
        assert!(!dw_ok && dw_acc < CONTROL_IOU_ACCURACY_THRESHOLD, "got ({dw_acc}, {dw_ok})");

        // R2 F-01 counterexamples: near-boundary crossing (v5.x = 0.612 - 4e-11) and 2e-6 x 2e-6 X-crossing
        // previously slipped through segments_intersect's dead zone; both must fail ear-clipping & IoU < 0.35.
        let r2_c1 = vec![
            [0.61, 0.45],
            [0.78, 0.45],
            [0.612, 0.62],
            [0.612, 0.452],
            [0.775, 0.452],
            [0.61199999996, 0.615],
        ];
        let r2_c2 = vec![
            [0.61, 0.45],
            [0.78, 0.45],
            [0.61, 0.62],
            [0.610999, 0.53],
            [0.611001, 0.53],
            [0.612, 0.452],
            [0.775, 0.452],
            [0.612, 0.615],
            [0.611, 0.530001],
            [0.611, 0.529999],
        ];
        for (uid, pts) in [(110_001_i64, r2_c1), (110_002_i64, r2_c2)] {
            assert!(polygon_has_self_intersection(&pts));
            assert!(ear_clip_triangulate(&pts).is_none());
            let iou = compute_polygons_iou(std::slice::from_ref(&pts), &[ctrl_poly.clone()]);
            assert!(iou < CONTROL_IOU_ACCURACY_THRESHOLD, "got {iou} for uid {uid}");
            let rep = polygons_to_pyrep(std::slice::from_ref(&pts));
            let (acc, ok) = grade_and_record_classification(snap, &mut store, uid, 10, &rep);
            assert!(!ok && acc < CONTROL_IOU_ACCURACY_THRESHOLD, "got ({acc}, {ok})");
        }

        // Collinear overlapping non-adjacent edge & adjacent 180-degree hairpin overlap both detected
        let collinear_overlap = vec![
            [0.61, 0.45],
            [0.78, 0.45],
            [0.78, 0.55],
            [0.70, 0.45],
            [0.64, 0.45],
            [0.61, 0.55],
        ];
        assert!(polygon_has_self_intersection(&collinear_overlap));
        assert!(ear_clip_triangulate(&collinear_overlap).is_none());
        let hairpin_overlap = vec![
            [0.61, 0.45],
            [0.75, 0.45],
            [0.65, 0.45],
            [0.65, 0.58],
            [0.61, 0.58],
        ];
        assert!(polygon_has_self_intersection(&hairpin_overlap));
        assert!(ear_clip_triangulate(&hairpin_overlap).is_none());

        // Symmetric zero-area bowtie is rejected as degenerate
        let zero_bowtie = PyRep::List(vec![PyRep::List(vec![
            PyRep::List(vec![PyRep::Real(0.2), PyRep::Real(0.2)]),
            PyRep::List(vec![PyRep::Real(0.4), PyRep::Real(0.4)]),
            PyRep::List(vec![PyRep::Real(0.4), PyRep::Real(0.2)]),
            PyRep::List(vec![PyRep::Real(0.2), PyRep::Real(0.4)]),
        ])]);
        assert!(extract_submission_polygons(&zero_bowtie).is_none());

        // R2 P2 #1: asymmetric self-intersecting bowtie cannot extrapolate centroid outside its bbox
        // onto sample_target_center_norm(3) = [0.60, 0.68] to pass is_plausible_first_responder
        let asym_bowtie = vec![
            [0.01, 0.01],
            [0.15, 0.01],
            [0.01, 0.148],
            [0.1499, 0.148],
        ];
        assert!(extract_submission_polygons(&polygons_to_pyrep(std::slice::from_ref(&asym_bowtie))).is_some());
        let asym_centroid = polygons_centroid(std::slice::from_ref(&asym_bowtie));
        assert!(
            (0.01..=0.15).contains(&asym_centroid[0]) && (0.01..=0.15).contains(&asym_centroid[1]),
            "bowtie centroid escaped bbox: {asym_centroid:?}"
        );
        assert!(!is_plausible_first_responder(&[asym_bowtie], 3));

        // R2 P2 #2: total vertex count across multi-polygon submission is bounded at <= MAX_TOTAL_VERTICES_PER_SUBMISSION
        let mk_200gon = |cx: f64, cy: f64| -> Vec<[f64; 2]> {
            (0..200)
                .map(|k| {
                    let a = (k as f64 / 200.0) * std::f64::consts::TAU;
                    [cx + 0.02 * a.cos(), cy + 0.02 * a.sin()]
                })
                .collect()
        };
        let too_many_verts = polygons_to_pyrep(&[
            mk_200gon(0.2, 0.2),
            mk_200gon(0.5, 0.5),
            mk_200gon(0.8, 0.8),
        ]);
        assert!(extract_submission_polygons(&too_many_verts).is_none());

        // P2 #1: direct Rust CW 3-vertex triangle matches CCW twin with IoU == 1.0
        let cw_tri = vec![[ox, oy], [ox, oy + 0.24], [ox + 0.24, oy]];
        let ccw_tri = vec![[ox, oy], [ox + 0.24, oy], [ox, oy + 0.24]];
        assert!((compute_polygons_iou(&[cw_tri], &[ccw_tri]) - 1.0).abs() < 1e-6);

        // 4. Standard task (idx % 5 != 0): crowd consensus via 2D polygon IoU
        let std_idx = 13;
        let [sx, sy] = sample_target_center_norm(std_idx);
        let json_poly = serde_json::json!({
            "gates": [[
                [sx - 0.12, sy - 0.12],
                [sx + 0.12, sy - 0.12],
                [sx + 0.12, sy + 0.12],
                [sx - 0.12, sy + 0.12]
            ]]
        })
        .to_string();
        let (a1, ok1) = grade_and_record_classification(
            snap,
            &mut store,
            201,
            std_idx,
            &PyRep::String(json_poly),
        );
        assert!(ok1 && a1 == 1.0);

        // Second responder inside-canvas disjoint -> disagrees -> (0.0, false)
        let (a2, ok2) = grade_and_record_classification(
            snap,
            &mut store,
            202,
            std_idx,
            &box_poly(0.05, 0.05, 0.04),
        );
        assert!(!ok2 && a2 == 0.0);

        // Third responder overlaps consensus polygon (0.20x0.20 -> IoU = 0.04/0.0576) -> locks task_consensus_by_index
        let (a3, ok3) = grade_and_record_classification(
            snap,
            &mut store,
            203,
            std_idx,
            &box_poly(sx, sy, 0.10),
        );
        assert!(ok3);
        assert!((a3 - (0.04 / 0.0576)).abs() < 1e-6, "got {a3}");
        assert_eq!(store.task_consensus_by_index.get(&std_idx), Some(&true));

        // Fourth responder discordant MUST still fail IoU grading (no cached consensus bypass)
        let (a4, ok4) = grade_and_record_classification(
            snap,
            &mut store,
            204,
            std_idx,
            &box_poly(0.05, 0.05, 0.04),
        );
        assert!(!ok4 && a4 == 0.0);

        // 5. First responder implausible (out of canvas bounds, full-canvas, or vertex-clustered cheat) -> (0.0, false)
        let (bad_first_acc, bad_first_ok) = grade_and_record_classification(
            snap,
            &mut store,
            301,
            14,
            &box_poly(1.3, 1.3, 0.1),
        );
        assert!(!bad_first_ok && bad_first_acc == 0.0);
        assert!(!grade_and_record_classification(
            snap,
            &mut store,
            301_001,
            14,
            &box_poly(0.5, 0.5, 0.5),
        )
        .1);
        assert!(snap
            .seasons_and_campaigns
            .project_discovery_votes
            .get(&14)
            .is_none_or(|b| b.consensus_centroid.is_none()));

        // P2 #2 regression: clustering 80 vertices near sample_target_center_norm(3) = [0.60, 0.68]
        // via a hairline needle cannot pull area-weighted polygons_centroid away from [0.01..0.25]^2
        let [t3x, t3y] = sample_target_center_norm(3);
        let mut cheat_pts = vec![
            [0.01, 0.01],
            [0.25, 0.01],
            [0.25, 0.25],
            [0.082, 0.25],
            [t3x + 0.002, t3y - 0.002],
        ];
        for k in 0..80 {
            let ang = -std::f64::consts::FRAC_PI_4 + (k as f64 / 79.0) * std::f64::consts::PI;
            cheat_pts.push([t3x + 0.003 * ang.cos(), t3y + 0.003 * ang.sin()]);
        }
        cheat_pts.push([t3x - 0.002, t3y + 0.002]);
        cheat_pts.push([0.080, 0.25]);
        cheat_pts.push([0.01, 0.25]);
        let cheat_centroid = polygons_centroid(std::slice::from_ref(&cheat_pts));
        assert!(cheat_centroid[0] < 0.20 && cheat_centroid[1] < 0.20, "got {cheat_centroid:?}");
        assert!(!is_plausible_first_responder(&[cheat_pts], 3));

        // P2 #3 regression: save_crowd_consensus_polygons bounds store.tasks at <= MAX_CACHED_TASKS
        for idx in 1000..1080 {
            save_crowd_consensus_polygons(&mut store, idx, std::slice::from_ref(&ctrl_poly));
        }
        assert!(store.tasks.len() <= MAX_CACHED_TASKS, "len={}", store.tasks.len());

        // 6. Standard task with None payload -> (0.0, false)
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 302, 16, &PyRep::None),
            (0.0, false)
        );
    });
}
