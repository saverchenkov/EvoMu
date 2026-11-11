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
                current_node_id: 25,
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

#[test]
fn expert_systems_exact_expiry_boundary_multi_max_notifications_and_rebase() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let cid = 90_000_101_i64;
    let station_id = 60_003_760_i64;
    let kw = HashMap::new();

    // 1. Exact boundary: expires_at_ft == now_ft MUST be treated as expired; multiple active systems take max level
    let now_ft = ctx.now_blueos_filetime_ticks();
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
            ch.user_id
        } else {
            let mut ch = snap.characters.values().next().cloned().expect("seed char");
            let uid = ch.user_id;
            ch.character_id = cid;
            ch.station_id = Some(station_id);
            snap.characters.insert(cid, ch);
            uid
        };

        let mut map = BTreeMap::new();
        map.insert(
            2001,
            ExpertSystemRecord {
                type_id: 2001,
                inserted_at_ft: now_ft - 500,
                expires_at_ft: now_ft, // exact equality boundary => expired (`expires_at_ft > now_ft` false)
                skills_granted: BTreeMap::from([(3300, 5), (3399, 5)]),
            },
        );
        map.insert(
            2002,
            ExpertSystemRecord {
                type_id: 2002,
                inserted_at_ft: now_ft - 100,
                expires_at_ft: now_ft + 1, // minimally active
                skills_granted: BTreeMap::from([(3300, 2), (3302, 3)]),
            },
        );
        map.insert(
            2003,
            ExpertSystemRecord {
                type_id: 2003,
                inserted_at_ft: now_ft - 100,
                expires_at_ft: now_ft + 100,
                skills_granted: BTreeMap::from([(3300, 4)]),
            },
        );
        save_expert_systems(snap, cid, &map);

        let skills = get_active_expert_system_skills(snap, cid, now_ft);
        assert_eq!(skills.get(&3300), Some(&4), "max across active systems; ignore exact boundary=now_ft");
        assert_eq!(skills.get(&3302), Some(&3));
        assert!(!skills.contains_key(&3399));

        // Clear synthetic entries before testing ConsumeExpertSystem
        save_expert_systems(snap, cid, &BTreeMap::new());
        assert!(!snap.node_graph_state.expert_systems.contains_key(&cid));
        uid
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 90_101, Some(cid), true, ctx.now_unix_secs())
        .unwrap();

    // Load a real expert system from SDE to verify ConsumeExpertSystem rebase, extension, station fallback, and notifications
    let sde_map = ctx.sde.load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH).unwrap();
    let (es_tid_str, es_rec) = sde_map.iter().next().expect("seed expert system");
    let es_tid: i64 = es_tid_str.parse().unwrap();
    let duration_days = es_rec.get("durationDays").and_then(|v| v.as_u64()).unwrap_or(7);
    let duration_ticks = (duration_days as i64) * TICKS_PER_DAY;
    let item_id = 881_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Seed expired record to verify ConsumeExpertSystem rebases from `now_ft` instead of `old_expiry + duration`
        let mut stale = BTreeMap::new();
        stale.insert(
            es_tid,
            ExpertSystemRecord {
                type_id: es_tid,
                inserted_at_ft: now_ft - 100_000,
                expires_at_ft: now_ft - 50_000,
                skills_granted: BTreeMap::new(),
            },
        );
        save_expert_systems(snap, cid, &stale);
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: es_tid as u32,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Session intentionally omits station_id so ConsumeExpertSystem exercises char.station_id fallback
    let mut sess = auth_session(uid, cid, 98_000_101, 30_000_142);
    sess.station_id = None;

    let _ = ctx.drain_notifications_for_char(cid);
    let res1 = handle(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "ConsumeExpertSystem",
        &[PyRep::Integer(item_id)],
        &kw,
    )
    .unwrap();
    assert!(res1.is_ok());

    let notes1 = ctx.drain_notifications_for_char(cid);
    assert!(notes1.iter().any(|n| {
        n.event == "OnExpertSystemsUpdated"
            && n.args.get(1) == Some(&PyRep::Bool(true))
            && n.args.get(2) == Some(&PyRep::Integer(es_tid))
    }));
    let exp_after_first = ctx.db.with_snapshot(|snap| load_expert_systems(snap, cid)[&es_tid].expires_at_ft);
    assert_eq!(exp_after_first, now_ft + duration_ticks);

    // Second consume on already-active record extends from `exp_after_first` rather than `now_ft`
    let res2 = handle(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "ConsumeExpertSystem",
        &[PyRep::Integer(item_id)],
        &kw,
    )
    .unwrap();
    assert!(res2.is_ok());
    let exp_after_second = ctx.db.with_snapshot(|snap| load_expert_systems(snap, cid)[&es_tid].expires_at_ft);
    assert_eq!(exp_after_second, now_ft + 2 * duration_ticks);

    // Removing missing type_id is a silent no-op (no notification emitted)
    let _ = ctx.drain_notifications_for_char(cid);
    let rem_miss = handle(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "RemoveMyExpertSystem",
        &[PyRep::Integer(9_999_999)],
        &kw,
    )
    .unwrap();
    assert!(rem_miss.is_ok());
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // Removing existing type emits OnExpertSystemsUpdated(..., false, tid) and removes empty cid map
    let rem_ok = handle(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "RemoveMyExpertSystem",
        &[PyRep::Integer(es_tid)],
        &kw,
    )
    .unwrap();
    assert!(rem_ok.is_ok());
    let notes_rem = ctx.drain_notifications_for_char(cid);
    assert!(notes_rem.iter().any(|n| {
        n.event == "OnExpertSystemsUpdated"
            && n.args.get(1) == Some(&PyRep::Bool(false))
            && n.args.get(2) == Some(&PyRep::Integer(es_tid))
    }));
    assert!(ctx.db.with_snapshot(|snap| !snap.node_graph_state.expert_systems.contains_key(&cid)));
}

#[test]
fn node_graph_guards_gm_role_utf8_truncation_limits_ordering_and_cleanup() {
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    sde.node_graphs.insert(
        777,
        eve_sde::NodeGraphTemplate {
            node_graph_id: 777,
            name: "UnitTestGraph".into(),
            parent_graph_id: None,
            node_count: 5,
            tags: Vec::new(),
        },
    );
    let ctx = crate::router::ServerContext::with_sde_and_repository(
        std::sync::Arc::new(sde),
        std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));

    let cid = 90_000_202_i64;
    let kw = HashMap::new();
    let mut sess = auth_session(202, cid, 98_000_202, 30_000_142);

    // Unhandled service returns None
    assert!(handle(&ctx, &mut sess, "otherService", "foo", &[], &kw).is_none());

    // Without npe_progress, graph 777 is still valid via ctx.sde.node_graphs and "completed" falls back to None
    let bb_unstarted = handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_blackboard_request",
        &[PyRep::Integer(777), PyRep::Integer(0), PyRep::String("completed".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bb_unstarted, PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&cid) {
            let mut ch = snap.characters.values().next().cloned().expect("seed char");
            ch.character_id = cid;
            snap.characters.insert(cid, ch);
        }
        snap.npe_progress.insert(
            cid,
            NpeProgressRecord {
                character_id: cid,
                node_graph_id: 777,
                current_node_id: 2,
                completed: true,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
    });
    let bb_completed = handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_blackboard_request",
        &[PyRep::Integer(777), PyRep::Integer(0), PyRep::String("completed".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bb_completed, PyRep::Bool(true));

    // Switch to active NPE for ordering, bounds, key length, role, UTF-8 truncation, and limit checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.get_mut(&cid).unwrap().completed = false;
    });
    let bb_incomplete = handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_blackboard_request",
        &[PyRep::Integer(777), PyRep::Integer(0), PyRep::String("completed".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bb_incomplete, PyRep::Bool(false));

    // Unauth, invalid graph_id, unknown graph_id, negative node_id
    let mut unauth = SessionState::default();
    assert!(handle(
        &ctx,
        &mut unauth,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(1), PyRep::String("ok".into())],
        &kw,
    )
    .unwrap()
    .is_err());
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(0), PyRep::Integer(1), PyRep::String("ok".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument" // rpc-lint: allow test fixture
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(999_999), PyRep::Integer(1), PyRep::String("ok".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NodeGraphNotFound" // rpc-lint: allow test fixture
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(777), PyRep::Integer(-1), PyRep::String("ok".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument" // rpc-lint: allow test fixture
    );

    // Non-GM node_id > graph.node_count (6 > 5) rejected vs == 5 accepted
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(777), PyRep::Integer(6), PyRep::String("ok".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument" // rpc-lint: allow test fixture
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(5), PyRep::String("at_bound".into())],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Empty and 129-byte keys rejected; 128-byte key accepted
    for bad_key in [String::new(), "k".repeat(129)] {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "node_graph",
                "process_client_graph_message",
                &[PyRep::Integer(777), PyRep::Integer(2), PyRep::String(bad_key)],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument" // rpc-lint: allow test fixture
        );
    }
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(2), PyRep::String("k".repeat(128))],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Restricted NPE control keys rejected for non-GM
    for restricted in [
        "Complete_AIR_NPE",
        " checkpoint_reached ",
        "ADVANCE_CHECKPOINT",
        "objective_completed",
    ] {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "node_graph",
                "process_client_graph_message",
                &[
                    PyRep::Integer(777),
                    PyRep::Integer(2),
                    PyRep::String(restricted.into()),
                    PyRep::None,
                ],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InsufficientRole"
        );
    }

    // Non-GM "client_*" ordering: out-of-order rejected, idempotent (node_u32 + 1 == current) keeps 2, match (2 == 2) increments to 3
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(777), PyRep::Integer(4), PyRep::String("client_step".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument" // rpc-lint: allow test fixture
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(1), PyRep::String("client_step".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.npe_progress[&cid].current_node_id), 2);
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(2), PyRep::String("client_step".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.npe_progress[&cid].current_node_id), 3);

    // UTF-8 multi-byte truncation + Real/None/List formatting
    let utf8_long = "é".repeat(600); // 1200 bytes
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(2),
            PyRep::String("utf8_val".into()),
            PyRep::String(utf8_long),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(2),
            PyRep::String("real_val".into()),
            PyRep::Real(3.5),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(2),
            PyRep::String("none_val".into()),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(2),
            PyRep::String("list_val".into()),
            PyRep::List(vec![PyRep::Integer(42)]),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        let bb = &snap.node_graph_state.node_graph_blackboards[&(cid, 777)];
        assert_eq!(bb["utf8_val"].len(), 1024);
        assert_eq!(bb["real_val"], "3.5");
        assert_eq!(bb["none_val"], "None");
        assert!(bb["list_val"].contains("42"));
    });

    // GM session: bypasses node_count, restricted keys, and jumps current_node_id
    let mut gm_sess = sess.clone();
    gm_sess.role |= eve_machonet::service_roles::ROLE_GML;
    assert!(handle(
        &ctx,
        &mut gm_sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(10),
            PyRep::String("complete_air_npe".into()),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut gm_sess,
        "node_graph",
        "process_client_graph_message",
        &[PyRep::Integer(777), PyRep::Integer(9), PyRep::String("client_jump".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.npe_progress[&cid].current_node_id), 10);

    // Key limit = 64: new 65th key fails, updating existing key succeeds
    ctx.db.with_mut_snapshot(|snap| {
        let bb = snap.node_graph_state.node_graph_blackboards.entry((cid, 777)).or_default();
        bb.clear();
        for i in 0..64 {
            bb.insert(format!("k_{i}"), "v".into());
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[PyRep::Integer(777), PyRep::Integer(2), PyRep::String("new_65th".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument" // rpc-lint: allow test fixture
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(777),
            PyRep::Integer(2),
            PyRep::String("k_0".into()),
            PyRep::String("updated".into()),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Blob byte limit = 8192: pushing total bytes over 8192 rejects
    ctx.db.with_mut_snapshot(|snap| {
        let bb = snap.node_graph_state.node_graph_blackboards.entry((cid, 777)).or_default();
        bb.clear();
        for i in 0..8 {
            bb.insert(format!("big_{i}"), "x".repeat(1000));
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[
                PyRep::Integer(777),
                PyRep::Integer(2),
                PyRep::String("overflow".into()),
                PyRep::String("z".repeat(400)),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // skip_air_npe_and_stop_graph removes node_graph_blackboards for cid
    let (_, _, completed) = skip_air_npe_and_stop_graph(&ctx, cid, 2_000).unwrap();
    assert!(completed);
    assert!(ctx.db.with_snapshot(|s| !s.node_graph_state.node_graph_blackboards.contains_key(&(cid, 777))));

    // Direct check for `sweep_expired_expert_systems_all`: removes expired record & notifies pilot
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut map = BTreeMap::new();
        map.insert(
            2001,
            ExpertSystemRecord {
                type_id: 2001,
                inserted_at_ft: now_ft - 500,
                expires_at_ft: now_ft,
                skills_granted: BTreeMap::from([(3300, 4)]),
            },
        );
        save_expert_systems(snap, cid, &map);
    });
    sweep_expired_expert_systems_all(&ctx, now_ft);
    assert!(ctx
        .db
        .with_snapshot(|s| load_expert_systems(s, cid).is_empty()));
}

#[test]
fn expert_systems_load_save_sweep_guards_ship_cargo_and_bb_exact_limit() {
    let ctx = test_ctx();
    let cid_a = 90_720_001_i64;
    let cid_b = 90_720_002_i64;
    let ship_a = 720_001_001_i64;
    let kw = HashMap::new();

    let sde_es_type_id: i64 = ctx
        .sde
        .load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH)
        .ok()
        .and_then(|m| m.keys().find_map(|k| k.parse::<i64>().ok()))
        .unwrap_or(57452);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(cid_a, ship_a), (cid_b, 720_002_001)] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            ship_a,
            InventoryItemRecord {
                item_id: ship_a,
                type_id: 587,
                owner_id: cid_a,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Expert system item placed in ACTIVE SHIP Cargo (kills active_ship_id StructField mutant)
        snap.items.insert(
            990_010,
            InventoryItemRecord {
                item_id: 990_010,
                type_id: sde_es_type_id as u32,
                owner_id: cid_a,
                location_id: ship_a,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // 1. load_expert_systems empty on unseeded + save_expert_systems roundtrip + remove on empty
    ctx.db
        .with_snapshot(|s| assert!(load_expert_systems(s, cid_a).is_empty()));
    let now_ft = ctx.now_blueos_filetime_ticks();
    let sample_rec = ExpertSystemRecord {
        type_id: 3003,
        inserted_at_ft: now_ft - 10,
        expires_at_ft: now_ft + 1_000,
        skills_granted: BTreeMap::from([(3302, 4)]),
    };
    ctx.db.with_mut_snapshot(|s| {
        save_expert_systems(s, cid_a, &BTreeMap::from([(3003, sample_rec.clone())]));
    });
    ctx.db.with_snapshot(|s| {
        let loaded = load_expert_systems(s, cid_a);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&3003).map(|r| r.expires_at_ft), Some(now_ft + 1_000));
    });
    ctx.db.with_mut_snapshot(|s| save_expert_systems(s, cid_a, &BTreeMap::new()));
    ctx.db.with_snapshot(|s| {
        assert!(!s.node_graph_state.expert_systems.contains_key(&cid_a));
        assert!(load_expert_systems(s, cid_a).is_empty());
    });

    // 2. sweep_expired_expert_systems_all:
    //    (a) strictly future only -> no-op, no notifications
    ctx.db.with_mut_snapshot(|s| {
        save_expert_systems(
            s,
            cid_b,
            &BTreeMap::from([(
                3004,
                ExpertSystemRecord {
                    type_id: 3004,
                    inserted_at_ft: now_ft - 10,
                    expires_at_ft: now_ft + 1_000,
                    skills_granted: BTreeMap::from([(3303, 2)]),
                },
            )]),
        );
    });
    let _ = ctx.drain_notifications_for_char(cid_b);
    sweep_expired_expert_systems_all(&ctx, now_ft);
    ctx.db
        .with_snapshot(|s| assert!(load_expert_systems(s, cid_b).contains_key(&3004)));
    assert!(ctx.drain_notifications_for_char(cid_b).is_empty());

    //    (b) cid_a has expired (<= now_ft) with overlapping skill 3300 + future (> now_ft), while cid_b has only future
    ctx.db.with_mut_snapshot(|s| {
        save_expert_systems(
            s,
            cid_a,
            &BTreeMap::from([
                (
                    3001,
                    ExpertSystemRecord {
                        type_id: 3001,
                        inserted_at_ft: now_ft - 200,
                        expires_at_ft: now_ft,
                        skills_granted: BTreeMap::from([(3300, 3), (3301, 2)]),
                    },
                ),
                (
                    3002,
                    ExpertSystemRecord {
                        type_id: 3002,
                        inserted_at_ft: now_ft - 300,
                        expires_at_ft: now_ft - 1,
                        skills_granted: BTreeMap::from([(3300, 5)]),
                    },
                ),
                (
                    3003,
                    ExpertSystemRecord {
                        type_id: 3003,
                        inserted_at_ft: now_ft - 10,
                        expires_at_ft: now_ft + 500,
                        skills_granted: BTreeMap::from([(3302, 4)]),
                    },
                ),
            ]),
        );
    });
    let _ = ctx.drain_notifications_for_char(cid_a);
    let _ = ctx.drain_notifications_for_char(cid_b);
    let uid_a = ctx.db.with_snapshot(|s| s.characters[&cid_a].user_id);
    ctx.db
        .acquire_online_lock(uid_a, 701, Some(cid_a), true, ctx.now_unix())
        .expect("online lock cid_a");
    sweep_expired_expert_systems_all(&ctx, now_ft);
    ctx.db.with_snapshot(|s| {
        let a_recs = load_expert_systems(s, cid_a);
        assert_eq!(a_recs.keys().copied().collect::<Vec<_>>(), vec![3003]);
        assert!(load_expert_systems(s, cid_b).contains_key(&3004));
    });
    let notes_a = ctx.drain_notifications_for_char(cid_a);
    assert_eq!(
        notes_a.iter().filter(|n| n.event == "OnExpertSystemsUpdated").count(),
        2
    );
    assert!(notes_a.iter().any(|n| n.event != "OnExpertSystemsUpdated"));
    assert!(ctx.drain_notifications_for_char(cid_b).is_empty());

    // 3. ConsumeExpertSystem from active ship Cargo when undocked + GetMyExpertSystems verifies persisted state
    let mut sess = auth_session(1, cid_a, 98_000_001, 30_000_142);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_a);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "expertSystemMgr",
            "ConsumeExpertSystem",
            &[PyRep::Integer(990_010)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&990_010)));
    let my_es = handle(&ctx, &mut sess, "expertSystemMgr", "GetMyExpertSystems", &[], &kw)
        .unwrap()
        .unwrap();
    let es_dict = my_es.as_dict().expect("GetMyExpertSystems dict");
    assert!(es_dict.iter().any(|(k, _)| k == &PyRep::Integer(sde_es_type_id)));

    // 4. node_graph blackboard exact 8192-byte limit accepted vs 8193 rejected, plus blackboard request value check
    let graph_id = 777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            cid_a,
            NpeProgressRecord {
                character_id: cid_a,
                node_graph_id: graph_id as u32,
                current_node_id: 2,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
        let bb = snap
            .node_graph_state
            .node_graph_blackboards
            .entry((cid_a, graph_id))
            .or_default();
        bb.clear();
        for i in 0..8 {
            bb.insert(format!("k{i}"), "a".repeat(998)); // 8 * (2 + 998) = 8000 bytes
        }
    });
    // m1 (2+155=157) + last_message (12+2=14) + node.2.last_message (19+2=21) = 192 -> total 8192 bytes exact!
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(2),
                PyRep::String("m1".into()),
                PyRep::String("v".repeat(155)),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let bb_val = handle(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_blackboard_request",
        &[PyRep::Integer(graph_id), PyRep::Integer(2), PyRep::String("m1".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bb_val, PyRep::String("v".repeat(155)));

    // 8193 bytes rejects with InvalidArgument
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "node_graph",
            "process_client_graph_message",
            &[
                PyRep::Integer(graph_id),
                PyRep::Integer(2),
                PyRep::String("m1".into()),
                PyRep::String("v".repeat(156)),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // 5. skip_air_npe_and_stop_graph exact tuple (graph_id, curr_node, completed) kills all 8 FnValue mutants
    assert_eq!(
        skip_air_npe_and_stop_graph(&ctx, cid_a, 2_000).unwrap(),
        (777_001, 4, true)
    );
    ctx.db.with_snapshot(|s| {
        assert!(!s.node_graph_state.node_graph_blackboards.contains_key(&(cid_a, graph_id)));
    });
}
