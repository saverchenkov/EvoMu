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

        // 2. Explicit false flags via String / Dict / KeyVal / JSON String / Object wrapper
        for bad_arg in [
            PyRep::String("false".into()),
            PyRep::String("INVALID".into()),
            PyRep::Dict(vec![(
                PyRep::String("accurate".into()),
                PyRep::Bool(false),
            )]),
            PyRep::keyval([("consensus", PyRep::Bool(false))]),
            PyRep::String(r#"{"is_accurate": false, "points": [[0.5, 0.5]]}"#.into()),
            PyRep::Object {
                type_name: "util.KeyVal".into(),
                args: Box::new(PyRep::keyval([("is_accurate", PyRep::Bool(false))])),
            },
        ] {
            assert_eq!(
                grade_and_record_classification(snap, &mut store, 1, 10, &bad_arg),
                (0.0, false)
            );
        }

        // 3. Control task (idx % 5 == 0): exact hit, pixel-space (>1.5) hit, far miss, empty centroid
        let [tx, ty] = sample_target_center_norm(10);
        let (acc_exact, ok_exact) = grade_and_record_classification(
            snap,
            &mut store,
            101,
            10,
            &PyRep::List(vec![PyRep::Real(tx), PyRep::Real(ty)]),
        );
        assert!(ok_exact);
        assert!((acc_exact - 1.0).abs() < 1e-9);

        let (acc_px, ok_px) = grade_and_record_classification(
            snap,
            &mut store,
            102,
            10,
            &PyRep::Tuple(vec![
                PyRep::Integer((tx * 512.0).round() as i64),
                PyRep::Real(ty * 512.0),
            ]),
        );
        assert!(ok_px);
        assert!(acc_px >= 0.75);

        // Mixed Integer/Real pair far from target -> inaccurate
        let (acc_miss, ok_miss) = grade_and_record_classification(
            snap,
            &mut store,
            103,
            10,
            &PyRep::List(vec![PyRep::Integer(1), PyRep::Real(1.4)]),
        );
        assert!(!ok_miss);
        assert!(acc_miss <= 0.35);

        // Empty payload -> (1.0, true)
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 104, 10, &PyRep::List(vec![])),
            (1.0, true)
        );

        // 4. Standard task (idx % 5 != 0): crowd consensus workflow
        let std_idx = 13;
        let [sx, sy] = sample_target_center_norm(std_idx);
        // First responder via JSON string polygon -> plausible -> establishes consensus_centroid
        let json_poly = serde_json::json!({
            "gates": [[[sx - 0.02, sy], [sx + 0.02, sy]]]
        })
        .to_string();
        let (a1, ok1) = grade_and_record_classification(
            snap,
            &mut store,
            201,
            std_idx,
            &PyRep::String(json_poly),
        );
        assert!(ok1);
        assert_eq!(a1, 1.0);

        // Second responder far away -> disagrees -> (0.2, false)
        let (a2, ok2) = grade_and_record_classification(
            snap,
            &mut store,
            202,
            std_idx,
            &PyRep::List(vec![PyRep::Real(1.4), PyRep::Integer(1)]),
        );
        assert!(!ok2);
        assert_eq!(a2, 0.2);

        // Third responder close to consensus -> updates running centroid & locks task_consensus_by_index
        let (a3, ok3) = grade_and_record_classification(
            snap,
            &mut store,
            203,
            std_idx,
            &PyRep::List(vec![PyRep::Real(sx), PyRep::Real(sy)]),
        );
        assert!(ok3);
        assert_eq!(a3, 1.0);
        assert_eq!(store.task_consensus_by_index.get(&std_idx), Some(&true));

        // 5. First responder implausible (> 0.65 from target) -> (0.2, false)
        let (bad_first_acc, bad_first_ok) = grade_and_record_classification(
            snap,
            &mut store,
            301,
            14,
            &PyRep::List(vec![PyRep::Real(1.4), PyRep::Real(1.4)]),
        );
        assert!(!bad_first_ok);
        assert_eq!(bad_first_acc, 0.2);

        // 6. Standard task with None centroid -> (1.0, true)
        assert_eq!(
            grade_and_record_classification(snap, &mut store, 302, 16, &PyRep::None),
            (1.0, true)
        );
    });
}
