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

        // Far polygon with spoofed accurate=true fails IoU == 0.0
        let far_spoof = PyRep::Dict(vec![
            (PyRep::String("accurate".into()), PyRep::Bool(true)),
            (PyRep::String("polygons".into()), PyRep::List(vec![box_poly(1.25, 1.25, 0.12)])),
        ]);
        let (acc_miss, ok_miss) =
            grade_and_record_classification(snap, &mut store, 106, 10, &far_spoof);
        assert!(!ok_miss && acc_miss == 0.0);

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

        // Overlapping duplicate gates use grid fallback and remain <= 1.0
        let ctrl_poly = control_solution_polygon(10);
        let dup_iou = compute_polygons_iou(&[ctrl_poly.clone(), ctrl_poly.clone()], &[ctrl_poly]);
        assert!((0.95..=1.0).contains(&dup_iou), "got {dup_iou}");

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

        // Second responder far away -> disagrees -> (0.0, false)
        let (a2, ok2) = grade_and_record_classification(
            snap,
            &mut store,
            202,
            std_idx,
            &box_poly(1.3, 1.3, 0.1),
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
            &box_poly(1.3, 1.3, 0.1),
        );
        assert!(!ok4 && a4 == 0.0);

        // 5. First responder implausible (out of canvas bounds or full-canvas) -> (0.0, false), no consensus stored
        let (bad_first_acc, bad_first_ok) = grade_and_record_classification(
            snap,
            &mut store,
            301,
            14,
            &box_poly(1.3, 1.3, 0.1),
        );
        assert!(!bad_first_ok && bad_first_acc == 0.0);
        assert!(snap
            .seasons_and_campaigns
            .project_discovery_votes
            .get(&14)
            .is_none_or(|b| b.consensus_centroid.is_none()));

        // 6. Standard task with None payload -> (0.0, false)
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 302, 16, &PyRep::None),
            (0.0, false)
        );
    });
}
