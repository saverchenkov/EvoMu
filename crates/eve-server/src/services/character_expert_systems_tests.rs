//! Unit tests for `character_expert_systems` (`expertSystemMgr` and `node_graph`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{InventoryFlag, InventoryItemRecord, NpeProgressRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{BTreeMap, HashMap, HashSet};

#[test]
fn expert_systems_lifecycle_and_active_skills() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();

    // Unauthenticated rejected
    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "expertSystemMgr", "GetMyExpertSystems", &[], &kw)
        .unwrap()
        .is_err());

    // Seed active and expired expert systems directly and verify max skill aggregation
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut map = BTreeMap::new();
        let mut s1 = BTreeMap::new();
        s1.insert(3300, 3);
        s1.insert(3301, 2);
        map.insert(
            1001,
            ExpertSystemRecord {
                type_id: 1001,
                inserted_at_ft: now_ft - 100,
                expires_at_ft: now_ft + 10_000,
                skills_granted: s1,
            },
        );
        let mut s2 = BTreeMap::new();
        s2.insert(3300, 5);
        map.insert(
            1002,
            ExpertSystemRecord {
                type_id: 1002,
                inserted_at_ft: now_ft - 200,
                expires_at_ft: now_ft - 1, // expired
                skills_granted: s2,
            },
        );
        save_expert_systems(snap, cid, &map);
        let active = get_active_expert_system_skills(snap, cid, now_ft);
        assert_eq!(active.get(&3300), Some(&3));
        assert_eq!(active.get(&3301), Some(&2));
    });

    let my_es = handle(&ctx, &mut sess, "expertSystemMgr", "GetMyExpertSystems", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(my_es.as_dict().map(|d| d.len()), Some(2));

    // ConsumeExpertSystem error branches
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(999_888_777)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // Find a valid expert system type ID from SDE
    let sde_es_type_id: i64 = ctx
        .sde
        .load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH)
        .ok()
        .and_then(|m| m.keys().find_map(|k| k.parse::<i64>().ok()))
        .unwrap_or(57452);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        // Unowned item
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: sde_es_type_id as u32,
                owner_id: 90_000_002,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Unreachable item (wrong station)
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: sde_es_type_id as u32,
                owner_id: cid,
                location_id: 60_009_999,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Non-expert-system item (Tritanium = 34)
        snap.items.insert(
            880_003,
            InventoryItemRecord {
                item_id: 880_003,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Valid expert system item with quantity 2
        snap.items.insert(
            880_004,
            InventoryItemRecord {
                item_id: 880_004,
                type_id: sde_es_type_id as u32,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(880_001)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotOwned"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(880_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotReachable"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(880_003)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidItemType"
    );

    // Consume twice: first decrements quantity to 1, second removes item
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(880_004)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&880_004).map(|i| i.quantity)),
        Some(1)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(880_004)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&880_004)));

    // RemoveMyExpertSystem
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "RemoveMyExpertSystem",
            &[PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "RemoveMyExpertSystem",
            &[PyRep::Integer(sde_es_type_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
}

#[test]
fn node_graph_blackboard_requests_messages_and_helpers() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let graph_id = 777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            cid,
            NpeProgressRecord {
                character_id: cid,
                node_graph_id: graph_id as u32,
                current_node_id: 10,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
    });

    // Invalid args & unknown graph
    let ng_svc = format!("node_{}", "graph");
    let err1 = handle(
        &ctx,
        &mut sess,
        &ng_svc,
        "process_client_blackboard_request",
        &[PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err1.code, "InvalidArgument");

    let err2 = handle(
        &ctx,
        &mut sess,
        &ng_svc,
        "process_client_blackboard_request",
        &[PyRep::Integer(999_999_999), PyRep::Integer(1), PyRep::String("k".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err2.code, "NodeGraphNotFound");

    let err3 = handle(
        &ctx,
        &mut sess,
        &ng_svc,
        "process_client_blackboard_request",
        &[PyRep::Integer(graph_id), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err3.code, "InvalidArgument");

    // Send messages of different types and verify blackboard round-trip
    for (key, val) in [
        ("client_step", PyRep::Integer(42)),
        ("flag_true", PyRep::Bool(true)),
        ("flag_false", PyRep::Bool(false)),
        ("text_val", PyRep::String("hello".into())),
    ] {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "node_graph",
                "process_client_graph_message",
                &[
                    PyRep::Integer(graph_id),
                    PyRep::Integer(25),
                    PyRep::String(key.into()),
                    val,
                ],
                &kw,
            )
            .unwrap()
            .unwrap(),
            PyRep::None
        );
    }

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("client_step".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(42)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("flag_true".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("flag_false".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("text_val".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("hello".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("current_node_id".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(26)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_blackboard_request",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(25),
                PyRep::String("missing_key".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    start_client_node_graph_for_operation(&ctx, cid, 1, 1, 30_000_142);
    stop_active_operation_node_graph(&ctx, cid, 1);
    let (_, _, completed) = skip_air_npe_and_stop_graph(&ctx, cid, 2_000).unwrap();
    assert!(completed);
}
