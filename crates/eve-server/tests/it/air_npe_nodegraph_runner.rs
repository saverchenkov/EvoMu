//! Integration tests for `plan:E-7` (`feat:pve.air-npe-nodegraph-runner`) —
//! AIR New Player Experience (NPE) server-side node-graph runner & tutorial pocket orchestrator.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::air_npe_nodegraph_pocket::NPE_POCKET_ANCHOR_POS;
use eve_server::services::air_npe_nodegraph_runner::{
    load_runner_state, AIR_NPE_ACT_1_NODE_GRAPH_ID, AIR_NPE_ACT_2_NODE_GRAPH_ID,
    AIR_NPE_NODE_GRAPH_ID, AIR_NPE_STATE_ACTIVE,
    AIR_NPE_STATE_COMPLETED, AIR_NPE_STATE_SKIPPED, NPE_DUNGEON_EVACUATION_POINT_ID,
    NPE_DUNGEON_RESEARCH_FACILITY_ID, NPE_DUNGEON_SCENE_1_ID,
    SERVER_NPE_MASTER_GRAPH_ID, SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID,
    SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID,
};
use eve_server::services::operations_manager_helpers::record_server_operation_event;
use eve_server::services::pve_dungeon_runtime::with_dungeon_runtime_store;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn create_new_tutorial_pilot(ctx: &ServerContext, sess: &mut SessionState, name: &str) -> i64 {
    let doll = PyRep::Dict(vec![(PyRep::String("genderID".into()), PyRep::Integer(1))]);
    let portrait = PyRep::Dict(vec![(PyRep::String("lighting".into()), PyRep::Integer(1))]);
    let char_id_rep = rpc(
        ctx,
        sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(1),  // bloodlineID
            PyRep::Integer(1),  // genderID
            PyRep::Integer(11), // ancestryID
            doll,
            portrait,
            PyRep::Integer(17), // schoolID
        ],
    )
    .await
    .expect("CreateCharacterWithDoll");
    let char_id = char_id_rep.as_i64().expect("created character_id");
    rpc(
        ctx,
        sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    char_id
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn keyval_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(kv) => kv.get(key),
        PyRep::Object { args, .. } => {
            if let PyRep::Dict(entries) = args.as_ref() {
                entries
                    .iter()
                    .find(|(k, _)| k.as_str() == Some(key))
                    .map(|(_, v)| v)
            } else {
                None
            }
        }
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

// parity: feat:pve.air-npe-nodegraph-runner, plan:E-7
#[tokio::test]
async fn air_npe_server_nodegraph_runner_full_tutorial_to_completion() {
    eve_testkit::attest(&["feat:pve.air-npe-nodegraph-runner", "plan:E-7"]);

    let ctx = new_ctx();

    // 1. Verify SDE AIR NPE node graphs and tutorial dungeons are present.
    for gid in [
        SERVER_NPE_MASTER_GRAPH_ID,
        SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID,
        SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID,
        685,
        686,
        695,
        707,
        708,
        709,
        710,
        711,
        712,
        713,
        714,
        715,
        716,
        717,
        718,
        719,
        720,
        721,
        722,
        723,
        724,
        725,
        726,
        733,
    ] {
        assert!(
            ctx.sde.node_graphs.contains_key(&gid),
            "SDE missing AIR NPE node graph {gid}"
        );
    }
    for did in [
        NPE_DUNGEON_SCENE_1_ID,
        NPE_DUNGEON_RESEARCH_FACILITY_ID,
        NPE_DUNGEON_EVACUATION_POINT_ID,
    ] {
        assert!(
            ctx.sde.dungeons.contains_key(&did),
            "SDE missing AIR NPE dungeon {did}"
        );
    }

    // 2. Authenticate and create a brand-new docked pilot.
    let mut sess = SessionState::new(9_701);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = create_new_tutorial_pilot(&ctx, &mut sess, "New Tutorial Pilot").await;

    let (origin_station_id, sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("new pilot");
        (
            ch.station_id.expect("new pilot starts docked"),
            ch.solar_system_id,
            ch.active_ship_id,
        )
    });
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "can_character_play_the_tutorial",
            &[],
        )
        .await
        .unwrap()
        .as_bool()
        == Some(true)
    );
    drain_all(&ctx, &mut sess, char_id);

    // 3. Start AIR NPE tutorial -> executes server master (49) & pocket spawner (102),
    // spawns NPE dungeon pocket (9103), starts client graphs (716, 707, 725).
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");

    let start_notifs = drain_all(&ctx, &mut sess, char_id);
    let started_graphs: Vec<i64> = start_notifs
        .iter()
        .filter(|(e, _)| e == "OnStartClientNodeGraph")
        .filter_map(|(_, args)| args.first().and_then(|a| a.as_i64()))
        .collect();
    assert!(
        started_graphs.contains(&AIR_NPE_NODE_GRAPH_ID)
            && started_graphs.contains(&AIR_NPE_ACT_1_NODE_GRAPH_ID)
            && started_graphs.contains(&725),
        "expected OnStartClientNodeGraph for 716, 707, 725, got {started_graphs:?}"
    );
    assert!(start_notifs.iter().any(|(e, args)| {
        e == "OnAirNpeStateChanged"
            && args == &[PyRep::Integer(char_id), PyRep::Integer(AIR_NPE_STATE_ACTIVE)]
    }));

    let runner_after_start = ctx
        .db
        .with_snapshot(|snap| load_runner_state(snap, char_id))
        .expect("runner state initialized");
    assert_eq!(runner_after_start.checkpoint_id, 12);
    assert!(runner_after_start
        .executed_server_graphs
        .contains(&SERVER_NPE_MASTER_GRAPH_ID));
    assert!(runner_after_start
        .executed_server_graphs
        .contains(&SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID));
    let pocket_inst_id = runner_after_start
        .pocket_instance_id
        .expect("pocket instance spawned");
    assert!(with_dungeon_runtime_store(&ctx, |s| {
        s.instances
            .get(&pocket_inst_id)
            .is_some_and(|i| !i.despawned && i.dungeon_id == NPE_DUNGEON_SCENE_1_ID)
    }));

    // 4. Complete "New Eden." intro cinematic -> executes server graph 103,
    // undocks starter ship out of station hangar into NPE tutorial pocket.
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("new_eden_cinematic_ended".into())],
    )
    .await
    .expect("new_eden_cinematic_ended");

    let cin_notifs = drain_all(&ctx, &mut sess, char_id);
    assert!(cin_notifs.iter().any(|(e, _)| e == "OnSessionChanged"));
    assert!(cin_notifs.iter().any(|(e, _)| e == "OnEnteringDungeonRoom"));
    assert!(cin_notifs.iter().any(|(e, _)| e == "OnDungeonValues"));
    assert!(cin_notifs.iter().any(|(e, _)| e == "OnClientNodeGraphUpdate"));
    assert!(cin_notifs.iter().any(|(e, args)| {
        e == "OnClientNodeGraphMessage"
            && args.get(2).and_then(|a| a.as_str()) == Some("pocket_entered")
    }));

    assert_eq!(sess.station_id, None);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("pilot 1");
        assert_eq!(ch.station_id, None);
        let ship = snap.items.get(&ship_id).expect("starter ship");
        assert_eq!(ship.location_id, i64::from(sys_id));
        assert_eq!(ship.flag, InventoryFlag::None);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle
        .get_ball_position_sync(ship_id)
        .expect("starter ship ball in pocket");
    assert_eq!(ship_pos, DVec3::from_array(NPE_POCKET_ANCHOR_POS));

    // Query blackboard via node_graph.process_client_blackboard_request.
    let bb_moved = rpc(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_blackboard_request",
        &[
            PyRep::Integer(AIR_NPE_NODE_GRAPH_ID),
            PyRep::Integer(13),
            PyRep::String("moved_into_pocket".into()),
        ],
    )
    .await
    .expect("blackboard moved_into_pocket");
    assert_eq!(bb_moved, PyRep::Bool(true));

    // 5. Advance checkpoints across Act 1 (13 -> 81) and Act 2 (83 -> 92) via server-observed gameplay events.
    let gameplay_steps = [
        ("approach_station", 18_u32),
        ("lock_target", 20),
        ("activateModule", 22),
        ("npcDestroyed", 24),
        ("activateModule", 25),
        ("activateModule", 26),
        ("npcDestroyed", 27),
        ("warpTo", 28),
        ("destroy_ships", 29),
        ("dockAtStation", 30),
        ("station_intro", 81),
        ("boardship", 83), // transitions Act 1 (707) -> Act 2 (708)
        ("undockFromStation", 84),
        ("trainSkill", 85),
        ("warpTo", 86),
        ("gateActivated", 87),
        ("orbit_target", 88),
        ("npcDestroyed", 89),
        ("warpTo", 90),
        ("stargateJump", 91),
        ("warpTo", 92),
    ];

    for (ev, expected_cp) in gameplay_steps {
        record_server_operation_event(&ctx, char_id, ev, &[], 1);
        let st = ctx
            .db
            .with_snapshot(|snap| load_runner_state(snap, char_id))
            .expect("runner active");
        assert_eq!(
            st.checkpoint_id, expected_cp,
            "expected checkpoint {expected_cp} after event {ev}"
        );
        if expected_cp == 83 {
            assert_eq!(st.active_act, 2);
            assert_eq!(st.act_client_graph_id, Some(AIR_NPE_ACT_2_NODE_GRAPH_ID));
        }
    }

    let mid_notifs = drain_all(&ctx, &mut sess, char_id);
    assert!(mid_notifs.iter().any(|(e, args)| {
        e == "OnStartClientNodeGraph"
            && args.first().and_then(|a| a.as_i64()) == Some(AIR_NPE_ACT_2_NODE_GRAPH_ID)
    }));

    // 6. Complete final checkpoint (92) via server dockAtStation operation on 716 -> completes AIR NPE!
    eve_server::services::operations_manager_helpers::record_server_operation_event(
        &ctx,
        char_id,
        "dockAtStation",
        &[],
        1,
    );

    let end_notifs = drain_all(&ctx, &mut sess, char_id);
    assert!(end_notifs.iter().any(|(e, args)| {
        e == "OnAirNpeStateChanged"
            && args
                == &[
                    PyRep::Integer(char_id),
                    PyRep::Integer(AIR_NPE_STATE_COMPLETED),
                ]
    }));
    assert!(end_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraph"));
    assert!(end_notifs.iter().any(|(e, args)| {
        e == "OnStopClientNodeGraphById" && args == &[PyRep::Integer(AIR_NPE_NODE_GRAPH_ID)]
    }));
    assert!(end_notifs.iter().any(|(e, _)| e == "OnDungeonExited"));

    let npe_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state");
    assert_eq!(
        keyval_get(&npe_state, "stateID").and_then(|v| v.as_i64()),
        Some(AIR_NPE_STATE_COMPLETED)
    );
    assert_eq!(
        keyval_get(&npe_state, "completed").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "is_main_tutorial_finished",
            &[],
        )
        .await
        .unwrap()
        .as_bool(),
        Some(true)
    );
    assert!(origin_station_id > 0);
}

#[tokio::test]
async fn air_npe_server_nodegraph_runner_skip_mid_tutorial_restores_hangar() {
    eve_testkit::attest(&["feat:pve.air-npe-nodegraph-runner", "plan:E-7"]);

    let ctx = new_ctx();
    let mut sess = SessionState::new(9_702);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = create_new_tutorial_pilot(&ctx, &mut sess, "New Skip Pilot").await;

    let (origin_station_id, sys_id, ship_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("new pilot");
        (
            ch.station_id.expect("new pilot starts docked"),
            ch.solar_system_id,
            ch.active_ship_id,
        )
    });

    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("new_eden_cinematic_ended".into())],
    )
    .await
    .expect("new_eden_cinematic_ended");
    drain_all(&ctx, &mut sess, char_id);

    // Confirm pilot is in space in the NPE pocket before skipping.
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.station_id)),
        None
    );

    let skip_res = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[])
        .await
        .expect("skip_air_npe");
    assert_eq!(
        keyval_get(&skip_res, "completed").and_then(|v| v.as_bool()),
        Some(true)
    );

    let skip_notifs = drain_all(&ctx, &mut sess, char_id);
    assert!(skip_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraph"));
    assert!(skip_notifs.iter().any(|(e, args)| {
        e == "OnStopClientNodeGraphById" && args == &[PyRep::Integer(AIR_NPE_NODE_GRAPH_ID)]
    }));
    assert!(skip_notifs.iter().any(|(e, _)| e == "OnDungeonExited"));
    assert!(skip_notifs.iter().any(|(e, _)| e == "OnSessionChanged"));
    assert!(skip_notifs.iter().any(|(e, args)| {
        e == "OnAirNpeStateChanged"
            && args == &[PyRep::Integer(char_id), PyRep::Integer(AIR_NPE_STATE_SKIPPED)]
    }));
    assert!(skip_notifs.iter().any(|(e, _)| e == "OnTutorialSkipped"));

    // Ship and character are restored to origin station hangar; pocket is despawned.
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("pilot");
        assert_eq!(ch.station_id, Some(origin_station_id));
        let ship = snap.items.get(&ship_id).expect("starter ship");
        assert_eq!(ship.location_id, origin_station_id);
        assert_eq!(ship.flag, InventoryFlag::Hangar);
    });
    assert!(ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_position_sync(ship_id)
        .is_none());
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "can_character_play_the_tutorial",
            &[],
        )
        .await
        .unwrap()
        .as_bool(),
        Some(false)
    );
}
