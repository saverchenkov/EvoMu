//! `air_npe_nodegraph_runner` — AIR New Player Experience (NPE) Server-Side Node-Graph Runner
//! & Tutorial Pocket Orchestrator (`plan:E-7`, `feat:pve.air-npe-nodegraph-runner`).
//!
//! Orchestrates the server-side AIR NPE tutorial flow:
//! - Client root & act node graphs: `AIR_NPE_NODE_GRAPH_ID = 716` (`[AIR] 0.0 - Client Master`),
//!   `AIR_NPE_ACT_1_NODE_GRAPH_ID = 707` (`[AIR] 1.0 - Opportunity`),
//!   `AIR_NPE_ACT_2_NODE_GRAPH_ID = 708` (`[AIR] 2.0 - Opportunity`),
//!   and scene sub-graphs `717..=726`.
//! - Server SDE NPE node graphs: `49` (`[AIR] 0.0 - Server Master`),
//!   `102` (`[AIR NPE] Void Space Pocket`), `103` (`[AIR NPE] Move New Character to Target Dungeon`),
//!   `733` (`[AIR] 1.1 - Setup`), and scene server graphs `685`, `686`, `695`, `709..=715`.
//! - Spawns the character's NPE dungeon pocket (`9103` `[AIR]Scene 1` / `10461` `AIR Solar Research Facility` /
//!   `9105` `[AIR NPE] Evacuation Point`), transitions the starter ship out of the station hangar into
//!   the tutorial pocket after the `"New Eden."` intro cinematic, pushes client node-graph notifications
//!   (`OnStartClientNodeGraph`, `OnClientNodeGraphUpdate`, `OnClientNodeGraphMessage`,
//!   `OnStopClientNodeGraph`, `OnStopClientNodeGraphById`, `OnAirNpeStateChanged`, `OnTutorialSkipped`),
//!   advances checkpoints/objectives (`12..=92`) on server-observed player actions, and restores
//!   normal station/space UI upon completion or skip.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::air_npe_nodegraph_pocket::{
    check_npe_live_combat_or_scramble, check_npe_snapshot_combat_timers,
    is_starter_or_school_solar_system, spawn_or_get_npe_pocket, spawn_or_move_ship_in_pocket,
    try_cleanup_npe_pocket_and_restore_ui, validate_npe_start_or_entry, NPE_POCKET_ANCHOR_POS,
};
use crate::services::notify_emitters_space_dungeons::emit_dungeon_progression_notifications;
use crate::services::notify_emitters_operations_nodegraph::{
    start_client_node_graph_instance, stop_client_node_graph_instances_by_id,
};
use crate::services::operations_manager::{
    can_character_play_tutorial, load_store, push_operation_event, save_store, sync_air_npe_skip,
};
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeMap;

/// V24 client root AIR NPE node graph (`[AIR] 0.0 - Client Master`).
pub const AIR_NPE_NODE_GRAPH_ID: i64 = 716;
/// V24 client Act 1 Opportunity node graph (`[AIR] 1.0 - Opportunity`).
pub const AIR_NPE_ACT_1_NODE_GRAPH_ID: i64 = 707;
/// V24 client Act 2 Opportunity node graph (`[AIR] 2.0 - Opportunity`).
pub const AIR_NPE_ACT_2_NODE_GRAPH_ID: i64 = 708;
/// Legacy/alternate client NPE root node graph ID referenced in `spec:04_pve_narrative_dungeons.md`.
pub const AIR_NPE_CLIENT_NODE_GRAPH_ID: i64 = 58_745;

/// SDE server master AIR NPE node graph (`[AIR] 0.0 - Server Master`).
pub const SERVER_NPE_MASTER_GRAPH_ID: u32 = 49;
/// SDE server NPE void-space pocket spawner graph (`[AIR NPE] Void Space Pocket`).
pub const SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID: u32 = 102;
/// SDE server NPE character-transfer graph (`[AIR NPE] Move New Character to Target Dungeon`).
pub const SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID: u32 = 103;
/// SDE server Scene 1.1 setup graph (`[AIR] 1.1 - Setup`).
pub const SERVER_NPE_SCENE_1_1_SETUP_GRAPH_ID: u32 = 733;

/// SDE NPE Scene 1 dungeon (`[AIR]Scene 1`).
pub const NPE_DUNGEON_SCENE_1_ID: u32 = 9_103;
/// SDE NPE Research Facility dungeon (`AIR Solar Research Facility`).
pub const NPE_DUNGEON_RESEARCH_FACILITY_ID: u32 = 10_461;
/// SDE NPE Evacuation Point dungeon (`[AIR NPE] Evacuation Point`).
pub const NPE_DUNGEON_EVACUATION_POINT_ID: u32 = 9_105;

/// `AirNpeState.DISABLED = 0`
pub const AIR_NPE_STATE_DISABLED: i64 = 0;
/// `AirNpeState.ACTIVE = 1`
pub const AIR_NPE_STATE_ACTIVE: i64 = 1;
/// `AirNpeState.COMPLETED = 2`
pub const AIR_NPE_STATE_COMPLETED: i64 = 2;
/// `AirNpeState.SKIPPED = 3`
pub const AIR_NPE_STATE_SKIPPED: i64 = 3;

#[derive(Debug, Clone, Copy)]
pub struct AirNpeCheckpointSpec {
    pub checkpoint_id: u32,
    pub act: u8,
    pub scene_index: u8,
    pub scene_code: &'static str,
    pub objective_key: &'static str,
    pub server_graph_id: u32,
    pub client_scene_graph_id: i64,
    pub dungeon_id: Option<u32>,
}

/// Ordered AIR NPE checkpoints across Act 1 (`SCENE_1_1`..`SCENE_1_5`) and Act 2 (`SCENE_2_1`..`SCENE_2_5`).
pub const AIR_NPE_CHECKPOINTS: &[AirNpeCheckpointSpec] = &[
    AirNpeCheckpointSpec {
        checkpoint_id: 12,
        act: 1,
        scene_index: 1,
        scene_code: "1.1",
        objective_key: "new_eden_intro",
        server_graph_id: 685,
        client_scene_graph_id: 725,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 13,
        act: 1,
        scene_index: 1,
        scene_code: "1.1",
        objective_key: "approach_station",
        server_graph_id: 685,
        client_scene_graph_id: 725,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 18,
        act: 1,
        scene_index: 2,
        scene_code: "1.2",
        objective_key: "lock_target",
        server_graph_id: 686,
        client_scene_graph_id: 726,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 20,
        act: 1,
        scene_index: 2,
        scene_code: "1.2",
        objective_key: "activate_weapons",
        server_graph_id: 686,
        client_scene_graph_id: 726,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 22,
        act: 1,
        scene_index: 2,
        scene_code: "1.2",
        objective_key: "destroy_enemies",
        server_graph_id: 686,
        client_scene_graph_id: 726,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 24,
        act: 1,
        scene_index: 3,
        scene_code: "1.3",
        objective_key: "activate_afterburner",
        server_graph_id: 695,
        client_scene_graph_id: 717,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 25,
        act: 1,
        scene_index: 3,
        scene_code: "1.3",
        objective_key: "activate_webifier",
        server_graph_id: 695,
        client_scene_graph_id: 717,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 26,
        act: 1,
        scene_index: 3,
        scene_code: "1.3",
        objective_key: "destroy_enemies_2",
        server_graph_id: 695,
        client_scene_graph_id: 717,
        dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 27,
        act: 1,
        scene_index: 4,
        scene_code: "1.4",
        objective_key: "warp_to_site",
        server_graph_id: 709,
        client_scene_graph_id: 718,
        dungeon_id: Some(NPE_DUNGEON_RESEARCH_FACILITY_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 28,
        act: 1,
        scene_index: 4,
        scene_code: "1.4",
        objective_key: "destroy_ships",
        server_graph_id: 709,
        client_scene_graph_id: 718,
        dungeon_id: Some(NPE_DUNGEON_RESEARCH_FACILITY_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 29,
        act: 1,
        scene_index: 4,
        scene_code: "1.4",
        objective_key: "dock_at_station",
        server_graph_id: 709,
        client_scene_graph_id: 718,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 30,
        act: 1,
        scene_index: 5,
        scene_code: "1.5",
        objective_key: "station_intro",
        server_graph_id: 710,
        client_scene_graph_id: 724,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 81,
        act: 1,
        scene_index: 5,
        scene_code: "1.5",
        objective_key: "board_ship",
        server_graph_id: 710,
        client_scene_graph_id: 724,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 83,
        act: 2,
        scene_index: 6,
        scene_code: "2.1",
        objective_key: "undock",
        server_graph_id: 711,
        client_scene_graph_id: 719,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 84,
        act: 2,
        scene_index: 6,
        scene_code: "2.1",
        objective_key: "start_skill",
        server_graph_id: 711,
        client_scene_graph_id: 719,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 85,
        act: 2,
        scene_index: 7,
        scene_code: "2.2",
        objective_key: "warp_to_gate",
        server_graph_id: 712,
        client_scene_graph_id: 720,
        dungeon_id: Some(NPE_DUNGEON_EVACUATION_POINT_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 86,
        act: 2,
        scene_index: 7,
        scene_code: "2.2",
        objective_key: "activate_gate",
        server_graph_id: 712,
        client_scene_graph_id: 720,
        dungeon_id: Some(NPE_DUNGEON_EVACUATION_POINT_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 87,
        act: 2,
        scene_index: 8,
        scene_code: "2.3",
        objective_key: "orbit_target",
        server_graph_id: 713,
        client_scene_graph_id: 721,
        dungeon_id: Some(NPE_DUNGEON_EVACUATION_POINT_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 88,
        act: 2,
        scene_index: 8,
        scene_code: "2.3",
        objective_key: "destroy_enemies_boss",
        server_graph_id: 713,
        client_scene_graph_id: 721,
        dungeon_id: Some(NPE_DUNGEON_EVACUATION_POINT_ID),
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 89,
        act: 2,
        scene_index: 9,
        scene_code: "2.4",
        objective_key: "warp_to_stargate",
        server_graph_id: 714,
        client_scene_graph_id: 722,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 90,
        act: 2,
        scene_index: 9,
        scene_code: "2.4",
        objective_key: "jump_through_stargate",
        server_graph_id: 714,
        client_scene_graph_id: 722,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 91,
        act: 2,
        scene_index: 10,
        scene_code: "2.5",
        objective_key: "warp_to_station",
        server_graph_id: 715,
        client_scene_graph_id: 723,
        dungeon_id: None,
    },
    AirNpeCheckpointSpec {
        checkpoint_id: 92,
        act: 2,
        scene_index: 10,
        scene_code: "2.5",
        objective_key: "dock_at_career_station",
        server_graph_id: 715,
        client_scene_graph_id: 723,
        dungeon_id: None,
    },
];

pub use eve_db::AirNpeRunnerState;

/// Returns `true` if `graph_id` belongs to the AIR NPE client or server node-graph family.
pub fn is_air_npe_graph_id(graph_id: i64) -> bool {
    matches!(
        graph_id,
        49 | 102
            | 103
            | 685
            | 686
            | 695
            | 707
            | 708
            | 709..=726
            | 733
            | 58_745
    )
}

pub fn load_runner_state(
    snap: &WorldStateSnapshot,
    char_id: i64,
) -> Option<AirNpeRunnerState> {
    snap.node_graph_state.air_npe_runners.get(&char_id).cloned()
}

pub(crate) fn save_runner_state(snap: &mut WorldStateSnapshot, state: &AirNpeRunnerState) {
    snap.node_graph_state
        .air_npe_runners
        .insert(state.char_id, state.clone());
}

fn bb_to_py_dict(bb: &BTreeMap<String, String>) -> PyRep {
    let items = bb
        .iter()
        .map(|(k, v)| {
            let val = if let Ok(num) = v.parse::<i64>() {
                PyRep::Integer(num)
            } else if v == "true" {
                PyRep::Bool(true)
            } else if v == "false" {
                PyRep::Bool(false)
            } else {
                PyRep::String(v.clone())
            };
            (PyRep::String(k.clone()), val)
        })
        .collect();
    PyRep::Dict(items)
}

/// Resolves a blackboard key from an active AIR NPE runner session if `graph_id` matches.
pub fn query_blackboard_value(
    snap: &WorldStateSnapshot,
    char_id: i64,
    graph_id: i64,
    key: &str,
) -> Option<PyRep> {
    let st = load_runner_state(snap, char_id)?;
    let matches_graph = graph_id == st.master_client_graph_id
        || st.act_client_graph_id == Some(graph_id)
        || st.scene_client_graph_id == Some(graph_id)
        || i64::from(st.active_server_graph_id) == graph_id;
    if !matches_graph {
        return None;
    }
    match key {
        "act" => Some(PyRep::Integer(i64::from(st.active_act))),
        "scene" => Some(PyRep::Integer(i64::from(st.scene_index))),
        "checkpoint" | "checkpoint_id" | "current_node_id" => {
            Some(PyRep::Integer(i64::from(st.checkpoint_id)))
        }
        "state_id" => Some(PyRep::Integer(st.state_id)),
        "completed" => Some(PyRep::Bool(st.completed)),
        "skipped" => Some(PyRep::Bool(st.skipped)),
        "moved_into_pocket" => Some(PyRep::Bool(st.moved_into_pocket)),
        "pocket_instance_id" => Some(
            st.pocket_instance_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        "dungeon_id" => Some(
            st.pocket_dungeon_id
                .map(|d| PyRep::Integer(i64::from(d)))
                .unwrap_or(PyRep::None),
        ),
        other => st.blackboard.get(other).map(|v| {
            if let Ok(num) = v.parse::<i64>() {
                PyRep::Integer(num)
            } else if v == "true" {
                PyRep::Bool(true)
            } else if v == "false" {
                PyRep::Bool(false)
            } else {
                PyRep::String(v.clone())
            }
        }),
    }
}

fn record_server_graph_executed(st: &mut AirNpeRunnerState, graph_id: u32) {
    st.active_server_graph_id = graph_id;
    if !st.executed_server_graphs.contains(&graph_id) {
        st.executed_server_graphs.push(graph_id);
    }
    st.blackboard
        .insert("server_graph_id".to_string(), graph_id.to_string());
}

fn emit_graph_update_and_message(
    ctx: &ServerContext,
    char_id: i64,
    instance_id: i64,
    node_id: i64,
    active_nodes: &[i64],
    bb: &BTreeMap<String, String>,
    msg_key: &str,
    msg_payload: PyRep,
) {
    let nodes_list = PyRep::List(active_nodes.iter().copied().map(PyRep::Integer).collect());
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnClientNodeGraphUpdate",
        "charid",
        vec![PyRep::Integer(instance_id), nodes_list, bb_to_py_dict(bb)],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnClientNodeGraphMessage",
        "charid",
        vec![
            PyRep::Integer(instance_id),
            PyRep::Integer(node_id),
            PyRep::String(msg_key.to_string()),
            msg_payload,
        ],
    );
}

/// Starts the AIR NPE server-side node-graph runner (`49` Master, `102` Void Space Pocket,
/// `716` Client Master, `707` Act 1 Opportunity, `725` Scene 1.1 Client) and spawns the NPE pocket.
pub fn on_start_tutorial(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    state_id: i64,
    career_path: Option<i64>,
) {
    let now = ctx.now_unix();
    let (sys_id, station_id, already_in_space) = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.tutorial_finished = Some(false);
        rec.tutorial_skipped = false;
        rec.tutorial_state = state_id;
        if let Some(p) = career_path {
            rec.career_path = p;
        }
        push_operation_event(rec, "start_tutorial".to_string(), state_id);
        save_store(snap, &store, now);

        let npe = snap
            .npe_progress
            .entry(char_id)
            .or_insert_with(|| eve_db::NpeProgressRecord {
                character_id: char_id,
                node_graph_id: AIR_NPE_NODE_GRAPH_ID as u32,
                current_node_id: 12,
                completed: false,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            });
        npe.node_graph_id = AIR_NPE_NODE_GRAPH_ID as u32;
        npe.current_node_id = 12;
        npe.completed = false;

        let ch_sys = snap
            .characters
            .get(&char_id)
            .map(|c| c.solar_system_id)
            .or(session.solar_system_id)
            .unwrap_or(30_000_141);
        let ch_station = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id)
            .or(session.station_id);
        let in_space = ch_station.is_none()
            && snap
                .characters
                .get(&char_id)
                .and_then(|c| c.structure_id)
                .is_none()
            && session.structure_id.is_none();
        (ch_sys, ch_station, in_space)
    });

    let (pocket_inst_id, _, _) =
        spawn_or_get_npe_pocket(ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);

    let mut bb = BTreeMap::new();
    bb.insert("act".to_string(), "1".to_string());
    bb.insert("scene".to_string(), "1".to_string());
    bb.insert("checkpoint".to_string(), "12".to_string());
    bb.insert(
        "server_graph_id".to_string(),
        SERVER_NPE_MASTER_GRAPH_ID.to_string(),
    );
    bb.insert(
        "dungeon_id".to_string(),
        NPE_DUNGEON_SCENE_1_ID.to_string(),
    );
    bb.insert("pocket_instance_id".to_string(), pocket_inst_id.to_string());

    let master_inst_id = start_client_node_graph_instance(
        ctx,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        0,
        0,
        bb.clone(),
    )
    .ok();
    let act1_inst_id = start_client_node_graph_instance(
        ctx,
        char_id,
        AIR_NPE_ACT_1_NODE_GRAPH_ID,
        0,
        0,
        bb.clone(),
    )
    .ok();
    let scene_1_1_inst_id =
        start_client_node_graph_instance(ctx, char_id, 725, 0, 0, bb.clone()).ok();

    let mut runner = AirNpeRunnerState {
        char_id,
        state_id,
        active_server_graph_id: SERVER_NPE_MASTER_GRAPH_ID,
        executed_server_graphs: vec![
            SERVER_NPE_MASTER_GRAPH_ID,
            SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID,
        ],
        active_act: 1,
        scene_index: 1,
        checkpoint_id: 12,
        completed_checkpoints: Vec::new(),
        master_client_graph_id: AIR_NPE_NODE_GRAPH_ID,
        master_client_instance_id: master_inst_id,
        act_client_graph_id: Some(AIR_NPE_ACT_1_NODE_GRAPH_ID),
        act_client_instance_id: act1_inst_id,
        scene_client_graph_id: Some(725),
        scene_client_instance_id: scene_1_1_inst_id,
        pocket_instance_id: Some(pocket_inst_id),
        pocket_dungeon_id: Some(NPE_DUNGEON_SCENE_1_ID),
        pocket_solar_system_id: Some(sys_id),
        origin_station_id: station_id,
        origin_solar_system_id: Some(sys_id),
        intro_cinematic_completed: already_in_space,
        moved_into_pocket: already_in_space,
        completed: false,
        skipped: false,
        blackboard: bb,
    };

    if already_in_space {
        record_server_graph_executed(&mut runner, SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID);
        record_server_graph_executed(&mut runner, 685);
    }

    ctx.db.with_mut_snapshot(|snap| {
        save_runner_state(snap, &runner);
    });

    crate::services::notify_emitters_operations_nodegraph::on_tutorial_started(ctx, char_id, state_id);
}

/// Executes SDE server graph `103` (`[AIR NPE] Move New Character to Target Dungeon`):
/// transitions/moves the starter ship out of the station hangar into the NPE tutorial pocket
/// after the `"New Eden."` intro cinematic.
pub fn transition_character_into_npe_pocket(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    let live_info = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| {
            (
                c.solar_system_id,
                c.active_ship_id,
                c.station_id.is_none() && c.structure_id.is_none(),
            )
        })
    });
    if let Some((sys_id, ship_id, in_space)) = live_info {
        check_npe_live_combat_or_scramble(ctx, char_id, sys_id, ship_id, in_space, now)?;
    }
    let (sys_id, old_station_id, ship_id, ship_type_id, sess_info) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return Err(BridgeRpcError::user_error("CharacterNotFound"));
            };
            if !can_character_play_tutorial(snap, char_id) {
                return Err(BridgeRpcError::user_error("TutorialNotActive"));
            }
            if !is_starter_or_school_solar_system(ctx, ch.solar_system_id) {
                return Err(BridgeRpcError::user_error("NpeStarterSystemRequired"));
            }
            let s_id = ch.solar_system_id;
            let prev_st = ch.station_id.or(ch.structure_id);
            let active_ship = ch.active_ship_id;
            check_npe_snapshot_combat_timers(snap, char_id, now)?;
            if active_ship <= 0 {
                return Err(BridgeRpcError::user_error("NoActiveShip"));
            }
            let _ = crate::router::apply_character_transition_in_snap(
                snap,
                &ctx.sde,
                char_id,
                crate::router::TransitionLocation::Space { solar_system_id: s_id },
                None,
                true,
                false,
            )
            .map_err(|e| BridgeRpcError {
                code: "SessionTransitionFailed".into(),
                message: e.to_string(),
                client_error: None,
            })?;
            let ch = snap.characters.get(&char_id).unwrap();
            snap.item_positions
                .insert(active_ship, NPE_POCKET_ANCHOR_POS);
            let stype = snap
                .items
                .get(&active_ship)
                .map(|i| i.type_id)
                .unwrap_or(596);
            let s_tuple = Some((
                ch.user_id,
                ch.corporation_id,
                ch.alliance_id,
                ch.constellation_id,
                ch.region_id,
            ));
            Ok((s_id, prev_st, active_ship, stype, s_tuple))
        })?;

    let (pocket_inst_id, _, _) =
        spawn_or_get_npe_pocket(ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    spawn_or_move_ship_in_pocket(ctx, char_id, sys_id, ship_id, ship_type_id);

    if let Some(sess) = session.as_deref_mut() {
        let _ = ctx.execute_session_transition_sync(
            Some(sess),
            crate::router::SessionTransitionSpec::for_kind(
                char_id,
                crate::router::TransitionKind::Teleport {
                    solar_system_id: sys_id,
                    dock_id: None,
                    ship_id: Some(ship_id),
                },
                "npe_pocket",
            )
            .with_apply_db(false)
            .with_sync_local_chat(false)
            .with_emit_on_session_changed(false),
        );
        let _ = ctx.bind_object(sess, "beyonce", i64::from(sys_id));
    }

    if let Some(st_id) = old_station_id {
        if let Some((uid, corp_id, alliance_id, const_id, reg_id)) = sess_info {
            let sess_rep = PyRep::keyval([
                ("userid", PyRep::Integer(uid as i64)),
                ("charid", PyRep::Integer(char_id)),
                ("corpid", PyRep::Integer(corp_id)),
                (
                    "allianceid",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("shipid", PyRep::Integer(ship_id)),
                ("stationid", PyRep::None),
                ("structureid", PyRep::None),
                ("solarsystemid2", PyRep::Integer(i64::from(sys_id))),
                ("solarsystemid", PyRep::Integer(i64::from(sys_id))),
                ("locationid", PyRep::Integer(i64::from(sys_id))),
                ("constellationid", PyRep::Integer(i64::from(const_id))),
                ("regionid", PyRep::Integer(i64::from(reg_id))),
            ]);
            let change_key = if st_id >= 1_000_000_000_000 {
                "structureid"
            } else {
                "stationid"
            };
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSessionChanged",
                "charid",
                vec![
                    PyRep::Bool(false),
                    sess_rep,
                    PyRep::Dict(vec![(
                        PyRep::String(change_key.into()),
                        PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::None]),
                    )]),
                ],
            );
        }
    }

    let expiry_ft = ctx.now_blueos_filetime_ticks() + 86_400_000_000_000;
    let room_id = i64::from(NPE_DUNGEON_SCENE_1_ID) * 100 + 1;
    emit_dungeon_progression_notifications(
        ctx,
        char_id,
        sys_id,
        "AirNpeTutorial",
        "[AIR]Scene 1",
        i64::from(NPE_DUNGEON_SCENE_1_ID),
        pocket_inst_id,
        room_id,
        NPE_POCKET_ANCHOR_POS,
        1,
        1,
        false,
        expiry_ft,
    );

    let update_payload = ctx.db.with_mut_snapshot(|snap| {
        let mut runner = load_runner_state(snap, char_id).unwrap_or_else(|| AirNpeRunnerState {
            char_id,
            state_id: AIR_NPE_STATE_ACTIVE,
            active_act: 1,
            scene_index: 1,
            checkpoint_id: 12,
            master_client_graph_id: AIR_NPE_NODE_GRAPH_ID,
            ..Default::default()
        });
        if old_station_id.is_some() && runner.origin_station_id.is_none() {
            runner.origin_station_id = old_station_id;
        }
        runner.origin_solar_system_id = Some(sys_id);
        runner.pocket_instance_id = Some(pocket_inst_id);
        runner.pocket_dungeon_id = Some(NPE_DUNGEON_SCENE_1_ID);
        runner.pocket_solar_system_id = Some(sys_id);
        runner.intro_cinematic_completed = true;
        runner.moved_into_pocket = true;
        record_server_graph_executed(&mut runner, SERVER_NPE_VOID_SPACE_POCKET_GRAPH_ID);
        record_server_graph_executed(&mut runner, SERVER_NPE_MOVE_CHAR_TO_DUNGEON_GRAPH_ID);
        record_server_graph_executed(&mut runner, 685);
        if runner.checkpoint_id <= 12 {
            if !runner.completed_checkpoints.contains(&12) {
                runner.completed_checkpoints.push(12);
            }
            runner.checkpoint_id = 13;
        }
        runner
            .blackboard
            .insert("intro_cinematic_completed".to_string(), "true".to_string());
        runner
            .blackboard
            .insert("moved_into_pocket".to_string(), "true".to_string());
        runner
            .blackboard
            .insert("checkpoint".to_string(), runner.checkpoint_id.to_string());
        runner
            .blackboard
            .insert("pocket_instance_id".to_string(), pocket_inst_id.to_string());

        if let Some(npe) = snap.npe_progress.get_mut(&char_id) {
            npe.node_graph_id = AIR_NPE_NODE_GRAPH_ID as u32;
            npe.current_node_id = runner.checkpoint_id;
        }

        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_dungeon_id = Some(NPE_DUNGEON_SCENE_1_ID);
        rec.active_site_id = Some(pocket_inst_id);
        rec.active_operation_system_id = Some(sys_id);
        push_operation_event(rec, "move_to_npe_pocket".to_string(), i64::from(NPE_DUNGEON_SCENE_1_ID));
        save_store(snap, &store, now);
        save_runner_state(snap, &runner);

        runner
            .master_client_instance_id
            .map(|inst| (inst, i64::from(runner.checkpoint_id), runner.blackboard.clone()))
    });

    if let Some((master_inst_id, cp_node_id, bb)) = update_payload {
        emit_graph_update_and_message(
            ctx,
            char_id,
            master_inst_id,
            cp_node_id,
            &[cp_node_id],
            &bb,
            "pocket_entered",
            PyRep::keyval([
                ("dungeonID", PyRep::Integer(i64::from(NPE_DUNGEON_SCENE_1_ID))),
                ("instanceID", PyRep::Integer(pocket_inst_id)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("checkpointID", PyRep::Integer(cp_node_id)),
            ]),
        );
    }

    Ok(())
}

fn find_checkpoint_idx(checkpoint_id: u32) -> usize {
    AIR_NPE_CHECKPOINTS
        .iter()
        .position(|c| c.checkpoint_id == checkpoint_id)
        .unwrap_or(0)
}

fn event_matches_checkpoint(
    spec: &AirNpeCheckpointSpec,
    event_name: &str,
    trusted_server: bool,
) -> bool {
    let ev = event_name.trim().to_ascii_lowercase();
    match spec.objective_key {
        "new_eden_intro" => matches!(
            ev.as_str(),
            "new_eden_intro"
                | "new_eden_cinematic_ended"
                | "intro_cinematic_finished"
                | "cinematic_finished"
                | "enter_tutorial_pocket"
                | "move_to_dungeon"
        ),
        "approach_station" => matches!(ev.as_str(), "approach_station" | "approach_complete"),
        _ if !trusted_server => false,
        _ => {
            if ev == spec.objective_key {
                return true;
            }
            match spec.objective_key {
                "lock_target" => matches!(ev.as_str(), "lock_target" | "target_locked"),
                "activate_weapons" => {
                    matches!(ev.as_str(), "activate_weapons" | "activatemodule")
                }
                "destroy_enemies" | "destroy_enemies_2" | "destroy_enemies_boss" => {
                    matches!(ev.as_str(), "destroy_enemies" | "npcdestroyed" | "destroynpc")
                }
                "activate_afterburner" => {
                    matches!(ev.as_str(), "activate_afterburner" | "activatemodule")
                }
                "activate_webifier" => {
                    matches!(ev.as_str(), "activate_webifier" | "activatemodule")
                }
                "warp_to_site" | "warp_to_gate" | "warp_to_stargate" | "warp_to_station" => {
                    ev == spec.objective_key || ev == "warpto"
                }
                "destroy_ships" => matches!(ev.as_str(), "destroy_ships" | "ship_destroyed"),
                "dock_at_station" | "dock_at_career_station" => {
                    ev == spec.objective_key || ev == "dockatstation"
                }
                "station_intro" => {
                    matches!(ev.as_str(), "station_intro" | "clone_rebirth_complete")
                }
                "board_ship" => {
                    matches!(ev.as_str(), "board_ship" | "boardship" | "assembleship")
                }
                "undock" => matches!(ev.as_str(), "undock" | "undockfromstation"),
                "start_skill" => matches!(ev.as_str(), "start_skill" | "trainskill"),
                "activate_gate" => matches!(ev.as_str(), "activate_gate" | "gateactivated"),
                "orbit_target" => matches!(ev.as_str(), "orbit_target" | "orbit_started"),
                "jump_through_stargate" => {
                    matches!(ev.as_str(), "jump_through_stargate" | "stargatejump")
                }
                _ => false,
            }
        }
    }
}

fn advance_runner_checkpoint(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    char_id: i64,
    trigger_event: &str,
    allow_generic_advance: bool,
) -> bool {
    let now = ctx.now_unix();

    enum TransitionPlan {
        None,
        Advance {
            prev_cp: u32,
            next_spec: AirNpeCheckpointSpec,
            master_inst_id: Option<i64>,
            old_scene_inst_id: Option<i64>,
            old_act_inst_id: Option<i64>,
            start_new_act: Option<i64>,
            start_new_scene: Option<i64>,
            bb: BTreeMap<String, String>,
        },
        Complete,
    }

    let plan = ctx.db.with_mut_snapshot(|snap| {
        let Some(mut runner) = load_runner_state(snap, char_id) else {
            return TransitionPlan::None;
        };
        if runner.state_id != AIR_NPE_STATE_ACTIVE || runner.completed || runner.skipped {
            return TransitionPlan::None;
        }
        let cur_idx = find_checkpoint_idx(runner.checkpoint_id);
        let cur_spec = AIR_NPE_CHECKPOINTS[cur_idx];
        let force_advance = allow_generic_advance
            && matches!(
                trigger_event,
                "checkpoint_reached" | "advance_checkpoint" | "objective_completed"
            );
        if !force_advance
            && !event_matches_checkpoint(&cur_spec, trigger_event, allow_generic_advance)
        {
            return TransitionPlan::None;
        }

        if !runner.completed_checkpoints.contains(&cur_spec.checkpoint_id) {
            runner.completed_checkpoints.push(cur_spec.checkpoint_id);
        }

        let Some(&next_spec) = AIR_NPE_CHECKPOINTS.get(cur_idx + 1) else {
            save_runner_state(snap, &runner);
            return TransitionPlan::Complete;
        };

        let old_scene_inst_id = if next_spec.client_scene_graph_id != cur_spec.client_scene_graph_id
        {
            runner.scene_client_instance_id.take()
        } else {
            None
        };
        let old_act_inst_id = if next_spec.act != cur_spec.act {
            runner.act_client_instance_id.take()
        } else {
            None
        };
        let start_new_act = (next_spec.act != cur_spec.act).then_some(if next_spec.act == 2 {
            AIR_NPE_ACT_2_NODE_GRAPH_ID
        } else {
            AIR_NPE_ACT_1_NODE_GRAPH_ID
        });
        let start_new_scene = (next_spec.client_scene_graph_id != cur_spec.client_scene_graph_id)
            .then_some(next_spec.client_scene_graph_id);

        runner.active_act = next_spec.act;
        runner.scene_index = next_spec.scene_index;
        runner.checkpoint_id = next_spec.checkpoint_id;
        record_server_graph_executed(&mut runner, next_spec.server_graph_id);
        runner
            .blackboard
            .insert("act".to_string(), next_spec.act.to_string());
        runner
            .blackboard
            .insert("scene".to_string(), next_spec.scene_index.to_string());
        runner.blackboard.insert(
            "checkpoint".to_string(),
            next_spec.checkpoint_id.to_string(),
        );
        runner.blackboard.insert(
            "objective".to_string(),
            next_spec.objective_key.to_string(),
        );

        if let Some(npe) = snap.npe_progress.get_mut(&char_id) {
            npe.current_node_id = next_spec.checkpoint_id;
        }

        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        push_operation_event(
            rec,
            format!("npe_checkpoint_{}", cur_spec.checkpoint_id),
            i64::from(next_spec.checkpoint_id),
        );
        save_store(snap, &store, now);
        save_runner_state(snap, &runner);

        TransitionPlan::Advance {
            prev_cp: cur_spec.checkpoint_id,
            next_spec,
            master_inst_id: runner.master_client_instance_id,
            old_scene_inst_id,
            old_act_inst_id,
            start_new_act,
            start_new_scene,
            bb: runner.blackboard,
        }
    });

    match plan {
        TransitionPlan::None => false,
        TransitionPlan::Complete => {
            complete_air_npe(ctx, session, char_id, now);
            true
        }
        TransitionPlan::Advance {
            prev_cp,
            next_spec,
            master_inst_id,
            old_scene_inst_id,
            old_act_inst_id,
            start_new_act,
            start_new_scene,
            bb,
        } => {
            if let Some(sid) = old_scene_inst_id {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnStopClientNodeGraph",
                    "charid",
                    vec![PyRep::Integer(sid)],
                );
            }
            if let Some(aid) = old_act_inst_id {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnStopClientNodeGraph",
                    "charid",
                    vec![PyRep::Integer(aid)],
                );
            }
            let new_act_inst = start_new_act
                .and_then(|gid| start_client_node_graph_instance(ctx, char_id, gid, 0, 0, bb.clone()).ok());
            let new_scene_inst = start_new_scene
                .and_then(|gid| start_client_node_graph_instance(ctx, char_id, gid, 0, 0, bb.clone()).ok());

            if new_act_inst.is_some() || new_scene_inst.is_some() {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(mut runner) = load_runner_state(snap, char_id) {
                        if let Some(aid) = new_act_inst {
                            runner.act_client_graph_id = start_new_act;
                            runner.act_client_instance_id = Some(aid);
                        }
                        if let Some(sid) = new_scene_inst {
                            runner.scene_client_graph_id = start_new_scene;
                            runner.scene_client_instance_id = Some(sid);
                        }
                        save_runner_state(snap, &runner);
                    }
                });
            }

            if let Some(inst_id) = master_inst_id {
                emit_graph_update_and_message(
                    ctx,
                    char_id,
                    inst_id,
                    i64::from(next_spec.checkpoint_id),
                    &[i64::from(next_spec.checkpoint_id)],
                    &bb,
                    "checkpoint_completed",
                    PyRep::keyval([
                        ("completedCheckpointID", PyRep::Integer(i64::from(prev_cp))),
                        ("nextCheckpointID", PyRep::Integer(i64::from(next_spec.checkpoint_id))),
                        ("act", PyRep::Integer(i64::from(next_spec.act))),
                        ("scene", PyRep::String(next_spec.scene_code.to_string())),
                        ("objective", PyRep::String(next_spec.objective_key.to_string())),
                    ]),
                );
            }
            true
        }
    }
}

/// Handles AIR NPE client events sent via `operationsManager.process_client_event`.
pub fn try_client_npe_event(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    event_name: &str,
    _extra_data: Option<&PyRep>,
) -> Result<Option<PyRep>, BridgeRpcError> {
    let ev = event_name.trim().to_ascii_lowercase();
    if matches!(
        ev.as_str(),
        "new_eden_cinematic_ended"
            | "intro_cinematic_finished"
            | "cinematic_finished"
            | "enter_tutorial_pocket"
            | "move_to_dungeon"
    ) {
        validate_npe_start_or_entry(ctx, char_id)?;
        let has_runner = ctx
            .db
            .with_snapshot(|snap| load_runner_state(snap, char_id).is_some());
        if !has_runner {
            on_start_tutorial(ctx, session, char_id, AIR_NPE_STATE_ACTIVE, None);
        }
        transition_character_into_npe_pocket(ctx, Some(session), char_id)?;
        return Ok(Some(PyRep::Bool(true)));
    }

    let is_gm = session.has_gm_role();
    if ev == "complete_air_npe" {
        if !is_gm {
            return Ok(None);
        }
        let is_active = ctx.db.with_snapshot(|snap| {
            load_runner_state(snap, char_id)
                .is_some_and(|r| r.state_id == AIR_NPE_STATE_ACTIVE && !r.completed && !r.skipped)
        });
        if !is_active {
            return Err(BridgeRpcError::user_error("TutorialNotActive"));
        }
        complete_air_npe_checked(ctx, Some(session), char_id, ctx.now_unix())?;
        return Ok(Some(PyRep::Bool(true)));
    }

    let is_active = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id).is_some_and(|r| r.state_id == AIR_NPE_STATE_ACTIVE)
    });
    if is_active && advance_runner_checkpoint(ctx, Some(session), char_id, &ev, is_gm) {
        return Ok(Some(PyRep::Bool(true)));
    }

    Ok(None)
}

pub fn on_client_npe_event(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    event_name: &str,
    extra_data: Option<&PyRep>,
) -> Option<PyRep> {
    try_client_npe_event(ctx, session, char_id, event_name, extra_data)
        .ok()
        .flatten()
}

/// Handles `node_graph.process_client_graph_message` when targeting an active AIR NPE graph.
/// Returns `true` if handled by an active AIR NPE runner session.
pub fn on_client_graph_message(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    graph_id: i64,
    node_id: i64,
    msg_name: &str,
    msg_data: &PyRep,
) -> bool {
    if !is_air_npe_graph_id(graph_id) {
        return false;
    }
    let has_runner = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id)
            .is_some_and(|r| r.state_id == AIR_NPE_STATE_ACTIVE && !r.completed && !r.skipped)
            && can_character_play_tutorial(snap, char_id)
    });
    if !has_runner {
        return false;
    }

    let is_gm = session.has_gm_role();
    let msg_lower = msg_name.trim().to_ascii_lowercase();
    if matches!(
        msg_lower.as_str(),
        "cinematic_finished"
            | "new_eden_cinematic_ended"
            | "intro_cinematic_finished"
            | "move_to_dungeon"
            | "enter_tutorial_pocket"
    ) {
        return transition_character_into_npe_pocket(ctx, Some(session), char_id).is_ok();
    }

    if msg_lower == "complete_air_npe" {
        if !is_gm {
            return false;
        }
        return complete_air_npe_checked(ctx, Some(session), char_id, ctx.now_unix()).is_ok();
    }

    if advance_runner_checkpoint(ctx, Some(session), char_id, &msg_lower, is_gm) {
        return true;
    }

    let payload_opt = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id).and_then(|r| {
            r.master_client_instance_id
                .map(|inst| (inst, r.blackboard.clone()))
        })
    });
    if let Some((master_inst_id, bb)) = payload_opt {
        emit_graph_update_and_message(
            ctx,
            char_id,
            master_inst_id,
            node_id,
            &[node_id],
            &bb,
            msg_name,
            msg_data.clone(),
        );
    }
    true
}

/// Hook invoked by `operations_manager_helpers::record_server_operation_event` when server
/// gameplay systems observe player actions (NPC kill, module cycle, warp, stargate jump, undock, dock, skill).
pub fn on_server_gameplay_event(
    ctx: &ServerContext,
    char_id: i64,
    event_name: &str,
    _params: &[(&str, String)],
) {
    let is_active = ctx.db.with_snapshot(|snap| {
        load_runner_state(snap, char_id).is_some_and(|r| r.state_id == AIR_NPE_STATE_ACTIVE)
    });
    if !is_active {
        return;
    }
    let _ = advance_runner_checkpoint(ctx, None, char_id, event_name, true);
}

/// Skips AIR NPE cleanly (`air_npe.skip_air_npe` or `operationsManager.process_client_event("skip_tutorial")`):
/// stops active AIR NPE client node graphs, despawns NPE pocket, restores normal station/space UI,
/// and emits `OnAirNpeStateChanged([char_id, 3])` and `OnTutorialSkipped([])`.
pub fn on_skip_air_npe(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    char_id: i64,
    now_unix: u64,
) -> Result<(u32, u32, bool), BridgeRpcError> {
    let (stopped_instances, stop_master_by_id) =
        try_cleanup_npe_pocket_and_restore_ui(ctx, session, char_id, false)?;

    for inst_id in stopped_instances {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnStopClientNodeGraph",
            "charid",
            vec![PyRep::Integer(inst_id)],
        );
    }
    if let Some(master_gid) = stop_master_by_id {
        let _ = stop_client_node_graph_instances_by_id(ctx, char_id, master_gid);
    }

    let (graph_id, curr_node, completed) = ctx.db.try_with_mut_snapshot(|snap| {
        let (gid, cnode, comp) = {
            let npe = snap
                .npe_progress
                .entry(char_id)
                .or_insert_with(|| eve_db::NpeProgressRecord {
                    character_id: char_id,
                    node_graph_id: 0,
                    current_node_id: 1,
                    completed: false,
                    career_goal_progress: Default::default(),
                    claimed_goals: Default::default(),
                });
            npe.completed = true;
            npe.current_node_id = 4;
            (npe.node_graph_id, npe.current_node_id, npe.completed)
        };
        if let Some(mut runner) = load_runner_state(snap, char_id) {
            runner.state_id = AIR_NPE_STATE_SKIPPED;
            runner.skipped = true;
            runner.completed = true;
            runner.active_act = 0;
            save_runner_state(snap, &runner);
        }
        sync_air_npe_skip(snap, char_id, now_unix);
        Ok::<_, BridgeRpcError>((gid, cnode, comp))
    })?;

    crate::services::notify_emitters_operations_nodegraph::on_tutorial_skipped(ctx, char_id);
    Ok((graph_id, curr_node, completed))
}

pub fn complete_air_npe_checked(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    char_id: i64,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let (stopped_instances, stop_master_by_id) =
        try_cleanup_npe_pocket_and_restore_ui(ctx, session, char_id, true)?;

    for inst_id in stopped_instances {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnStopClientNodeGraph",
            "charid",
            vec![PyRep::Integer(inst_id)],
        );
    }
    if let Some(master_gid) = stop_master_by_id {
        let _ = stop_client_node_graph_instances_by_id(ctx, char_id, master_gid);
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut runner) = load_runner_state(snap, char_id) {
            runner.state_id = AIR_NPE_STATE_COMPLETED;
            runner.completed = true;
            runner.skipped = false;
            runner.active_act = 0;
            runner.checkpoint_id = 92;
            save_runner_state(snap, &runner);
        }
        let npe = snap
            .npe_progress
            .entry(char_id)
            .or_insert_with(|| eve_db::NpeProgressRecord {
                character_id: char_id,
                node_graph_id: AIR_NPE_NODE_GRAPH_ID as u32,
                current_node_id: 92,
                completed: true,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            });
        npe.completed = true;
        npe.current_node_id = 92;

        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.tutorial_finished = Some(true);
        rec.tutorial_skipped = false;
        rec.tutorial_state = AIR_NPE_STATE_COMPLETED;
        rec.active_dungeon_id = None;
        rec.active_site_id = None;
        push_operation_event(rec, "complete_air_npe".to_string(), 92);
        save_store(snap, &store, now_unix);
    });

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAirNpeStateChanged",
        "charid",
        vec![
            PyRep::Integer(char_id),
            PyRep::Integer(AIR_NPE_STATE_COMPLETED),
        ],
    );
    Ok(())
}

/// Completes AIR NPE cleanly: stops active client node graphs, despawns NPE pocket,
/// marks NPE & tutorial finished (`AIR_NPE_STATE_COMPLETED = 2`), and emits `OnAirNpeStateChanged([char_id, 2])`.
pub fn complete_air_npe(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    char_id: i64,
    now_unix: u64,
) {
    let _ = complete_air_npe_checked(ctx, session, char_id, now_unix);
}

#[cfg(test)]
#[path = "air_npe_nodegraph_runner_tests.rs"]
mod air_npe_nodegraph_runner_tests;

#[cfg(test)]
#[path = "air_npe_nodegraph_runner_edges_tests.rs"]
mod air_npe_nodegraph_runner_edges_tests;

