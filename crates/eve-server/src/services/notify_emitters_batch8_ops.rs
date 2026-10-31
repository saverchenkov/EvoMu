//! `notify_emitters_batch8_ops` — Real Gameplay & Service Notification Emitters (Batch 8: Operations, AIR NPE & NodeGraph).
//!
//! Provides stateful service transition helpers and notification emitters for Group 3:
//! `OnOperationDeactivated`, `OnOperationReplayed`, `OnOperationSkipped`,
//! `OnOperationTaskSkipped`, `OnOperationsProgressReset`, `OnOperationsServerFSDReload`,
//! `OnAirNpeStateChanged`, `OnTutorialSkipped`, `OnUiRevealForced`,
//! `OnClientNodeGraphMessage`, `OnClientNodeGraphUpdate`, `OnStartClientNodeGraph`,
//! `OnStopClientNodeGraph`, `OnStopAllClientNodeGraphs`, `OnStopClientNodeGraphById`.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_batch8::broadcast_char_event;
use crate::services::operations_manager::{
    load_store, save_store, CharacterOperationsRecord,
};
use crate::services::operations_manager_helpers::{
    get_operation_defs, load_operation_definitions, OperationDefinitions,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const CLIENT_NODE_GRAPH_SEQ_KEY: &str = "client_node_graphs:seq";
const MAX_ACTIVE_CLIENT_GRAPHS: usize = 16;
const MAX_BLACKBOARD_KEYS: usize = 64;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveClientNodeGraph {
    pub instance_id: i64,
    pub node_graph_id: i64,
    pub category_id: i64,
    pub operation_id: i64,
    pub blackboard: BTreeMap<String, String>,
}

fn client_node_graphs_blob_key(char_id: i64) -> String {
    format!("client_node_graphs:{char_id}")
}

fn load_active_client_graphs(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> BTreeMap<i64, ActiveClientNodeGraph> {
    snap.service_state_blobs
        .get(&client_node_graphs_blob_key(char_id))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

fn save_active_client_graphs(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    graphs: &BTreeMap<i64, ActiveClientNodeGraph>,
) {
    if let Ok(json) = serde_json::to_string(graphs) {
        snap.service_state_blobs
            .insert(client_node_graphs_blob_key(char_id), json);
    }
}

fn blackboard_to_py_dict(bb: &BTreeMap<String, String>) -> PyRep {
    let items: Vec<(PyRep, PyRep)> = bb
        .iter()
        .map(|(k, v)| {
            let val_rep = if let Ok(num) = v.parse::<i64>() {
                PyRep::Integer(num)
            } else if v == "true" {
                PyRep::Bool(true)
            } else if v == "false" {
                PyRep::Bool(false)
            } else {
                PyRep::String(v.clone())
            };
            (PyRep::String(k.clone()), val_rep)
        })
        .collect();
    PyRep::Dict(items)
}

fn pyrep_to_bb_string(rep: &PyRep) -> String {
    match rep {
        PyRep::String(s) => s.clone(),
        PyRep::Integer(i) => i.to_string(),
        PyRep::Bool(b) => b.to_string(),
        PyRep::Real(r) => r.to_string(),
        PyRep::None => "None".to_string(),
        other => format!("{other:?}"),
    }
}

/// Starts a server-driven client node graph instance for `char_id` and emits `OnStartClientNodeGraph`.
pub fn start_client_node_graph_instance(
    ctx: &ServerContext,
    char_id: i64,
    graph_id: i64,
    category_id: i64,
    operation_id: i64,
    initial_bb: BTreeMap<String, String>,
) -> Result<i64, BridgeRpcError> {
    if graph_id <= 0 {
        return Err(rpc_err(
            "InvalidArgument",
            "node_graph_id must be a positive integer",
        ));
    }
    let valid_in_sde = ctx.sde.node_graphs.contains_key(&(graph_id as u32));
    if !valid_in_sde {
        return Err(rpc_err(
            "NodeGraphNotFound",
            format!("Node graph {graph_id} not found"),
        ));
    }
    let (instance_id, bb_dict) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut graphs = load_active_client_graphs(snap, char_id);
        if graphs.len() >= MAX_ACTIVE_CLIENT_GRAPHS {
            return Err(rpc_err(
                "TooManyNodeGraphs",
                "Maximum active client node graph instances reached",
            ));
        }
        let seq = snap
            .service_state_blobs
            .get(CLIENT_NODE_GRAPH_SEQ_KEY)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(100_000)
            + 1;
        snap.service_state_blobs
            .insert(CLIENT_NODE_GRAPH_SEQ_KEY.into(), seq.to_string());
        let mut bounded_bb = BTreeMap::new();
        for (k, v) in initial_bb.into_iter().take(MAX_BLACKBOARD_KEYS) {
            bounded_bb.insert(k, v);
        }
        let inst = ActiveClientNodeGraph {
            instance_id: seq,
            node_graph_id: graph_id,
            category_id,
            operation_id,
            blackboard: bounded_bb,
        };
        let dict = blackboard_to_py_dict(&inst.blackboard);
        graphs.insert(seq, inst);
        save_active_client_graphs(snap, char_id, &graphs);
        Ok((seq, dict))
    })?;

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStartClientNodeGraph",
        "charid",
        vec![
            PyRep::Integer(graph_id),
            PyRep::Integer(instance_id),
            bb_dict,
        ],
    );
    Ok(instance_id)
}

/// Starts a client node graph on `operationsManager.activate_operation_in_solar_system` ONLY when
/// an explicit `node_graph_id` was provided (preserving `notify_real_emitters_batch2` when omitted).
pub fn maybe_start_operation_node_graph(
    ctx: &ServerContext,
    char_id: i64,
    op_id: i64,
    cat_id: i64,
    explicit_graph_id: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let Some(gid) = explicit_graph_id else {
        return Ok(());
    };
    let mut bb = BTreeMap::new();
    bb.insert("operation_id".into(), op_id.to_string());
    bb.insert("category_id".into(), cat_id.to_string());
    let _ = start_client_node_graph_instance(ctx, char_id, gid, cat_id, op_id, bb)?;
    Ok(())
}

/// Records client node graph message on an active server-started instance without echoing client input back.
pub fn on_client_graph_message(
    ctx: &ServerContext,
    char_id: i64,
    graph_id: i64,
    node_id: i64,
    msg_name: &str,
    msg_data: &PyRep,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut graphs = load_active_client_graphs(snap, char_id);
        let Some(target_inst_id) = graphs
            .values()
            .find(|g| g.node_graph_id == graph_id)
            .map(|g| g.instance_id)
        else {
            return;
        };
        let Some(inst) = graphs.get_mut(&target_inst_id) else {
            return;
        };
        if inst.blackboard.len() < MAX_BLACKBOARD_KEYS || inst.blackboard.contains_key(msg_name) {
            inst.blackboard
                .insert(msg_name.to_string(), pyrep_to_bb_string(msg_data));
        }
        if inst.blackboard.len() < MAX_BLACKBOARD_KEYS || inst.blackboard.contains_key("node_id") {
            inst.blackboard
                .insert("node_id".to_string(), node_id.to_string());
        }
        save_active_client_graphs(snap, char_id, &graphs);
    });
}

/// Stops any active client node graph instance bound to `cat_id` (or `op_id`) and emits `OnStopClientNodeGraph`.
pub fn stop_active_operation_node_graph_for_category(ctx: &ServerContext, char_id: i64, cat_id: i64) {
    let stopped_ids: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let mut graphs = load_active_client_graphs(snap, char_id);
        let ids: Vec<i64> = graphs
            .values()
            .filter(|g| g.category_id == cat_id || g.operation_id == cat_id)
            .map(|g| g.instance_id)
            .collect();
        for id in &ids {
            graphs.remove(id);
        }
        if !ids.is_empty() {
            save_active_client_graphs(snap, char_id, &graphs);
        }
        ids
    });

    for inst_id in stopped_ids {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnStopClientNodeGraph",
            "charid",
            vec![PyRep::Integer(inst_id)],
        );
    }
}

/// Stops active client node graph instances matching `graph_id` and emits `OnStopClientNodeGraphById`.
pub fn stop_client_node_graph_instances_by_id(
    ctx: &ServerContext,
    char_id: i64,
    graph_id: i64,
) -> bool {
    if graph_id <= 0 {
        return false;
    }
    let removed_any = ctx.db.with_mut_snapshot(|snap| {
        let mut graphs = load_active_client_graphs(snap, char_id);
        let before = graphs.len();
        graphs.retain(|_, g| g.node_graph_id != graph_id);
        let changed = graphs.len() != before;
        if changed {
            save_active_client_graphs(snap, char_id, &graphs);
        }
        changed
    });
    if removed_any {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnStopClientNodeGraphById",
            "charid",
            vec![PyRep::Integer(graph_id)],
        );
    }
    removed_any
}

/// Stops all active client node graphs for `char_id` and emits `OnStopAllClientNodeGraphs`.
pub fn stop_all_client_node_graphs_for_char(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut graphs = load_active_client_graphs(snap, char_id);
        graphs.clear();
        save_active_client_graphs(snap, char_id, &graphs);
    });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStopAllClientNodeGraphs",
        "charid",
        vec![],
    );
}

/// Emits `OnAirNpeStateChanged` when `start_tutorial` runs.
pub fn on_tutorial_started(ctx: &ServerContext, char_id: i64, state_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAirNpeStateChanged",
        "charid",
        vec![PyRep::Integer(char_id), PyRep::Integer(state_id)],
    );
}

/// V24 client `airNpeController.pyj` `AirNpeState.SKIPPED = 3` enum constant.
pub const AIR_NPE_STATE_SKIPPED: i64 = 3;

/// Emits `OnAirNpeStateChanged([char_id, AIR_NPE_STATE_SKIPPED])` when `skip_tutorial` runs.
pub fn on_tutorial_skipped(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAirNpeStateChanged",
        "charid",
        vec![PyRep::Integer(char_id), PyRep::Integer(AIR_NPE_STATE_SKIPPED)],
    );
}

/// Emits `OnOperationTaskSkipped([task_list, includes_current])` in V24 tuple format.
pub fn emit_operation_task_skipped(
    ctx: &ServerContext,
    char_id: i64,
    op_defs: &OperationDefinitions,
    cat_id: i64,
    op_id: i64,
    task_id: i64,
) {
    let next_task_rep = op_defs
        .op_tasks
        .get(&op_id)
        .and_then(|tasks| {
            tasks
                .iter()
                .position(|&t| t == task_id)
                .and_then(|idx| tasks.get(idx + 1).copied())
        })
        .map(PyRep::Integer)
        .unwrap_or(PyRep::None);
    let task_list = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(cat_id),
        PyRep::Integer(op_id),
        PyRep::Integer(task_id),
        next_task_rep,
    ])]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationTaskSkipped",
        "charid",
        vec![task_list, PyRep::Bool(true)],
    );
}

fn with_character_operations_record<F, R>(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
    f: F,
) -> R
where
    F: FnOnce(&mut CharacterOperationsRecord) -> R,
{
    let mut store = load_store(snap);
    let rec = store.by_character.entry(char_id).or_default();
    let out = f(rec);
    save_store(snap, &store, now_unix);
    out
}

fn deactivate_operation_for_char(ctx: &ServerContext, char_id: i64) -> Result<PyRep, BridgeRpcError> {
    let now = ctx.now_unix();
    let prev_op = ctx.db.try_with_mut_snapshot(|snap| {
        with_character_operations_record(snap, char_id, now, |rec| {
            let Some(op_id) = rec.active_operation_id.take() else {
                return Err(BridgeRpcError::user_error("CustomInfo"));
            };
            Ok(op_id)
        })
    })?;
    stop_active_operation_node_graph_for_category(ctx, char_id, prev_op);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationDeactivated",
        "charid",
        vec![],
    );
    Ok(PyRep::Bool(true))
}

fn replay_operation_for_char(
    ctx: &ServerContext,
    char_id: i64,
    cat_id: i64,
    op_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let op_defs = get_operation_defs(&ctx.sde);
    if !(1..=100_000).contains(&cat_id)
        || !(1..=100_000).contains(&op_id)
        || !op_defs.op_tasks.contains_key(&op_id)
    {
        return Err(BridgeRpcError::user_error("CustomInfo"));
    }
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        with_character_operations_record(snap, char_id, now, |rec| {
            rec.active_category_id = Some(cat_id);
            rec.active_operation_id = Some(op_id);
            if let Some(tasks) = op_defs.op_tasks.get(&op_id) {
                for t in tasks {
                    rec.completed_tasks.remove(t);
                    rec.skipped_tasks.remove(t);
                }
            }
        });
    });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationReplayed",
        "charid",
        vec![PyRep::Integer(cat_id), PyRep::Integer(op_id)],
    );
    Ok(PyRep::Bool(true))
}

fn skip_operation_for_char(
    ctx: &ServerContext,
    char_id: i64,
    cat_id: i64,
    op_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let op_defs = get_operation_defs(&ctx.sde);
    if !(1..=100_000).contains(&cat_id)
        || !(1..=100_000).contains(&op_id)
        || !op_defs.op_tasks.contains_key(&op_id)
    {
        return Err(BridgeRpcError::user_error("CustomInfo"));
    }
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        with_character_operations_record(snap, char_id, now, |rec| {
            if let Some(tasks) = op_defs.op_tasks.get(&op_id) {
                for &t in tasks {
                    rec.skipped_tasks.insert(t);
                }
            }
            rec.completed_operations.insert(op_id);
            if rec.active_operation_id == Some(op_id) {
                rec.active_operation_id = None;
            }
        });
    });
    stop_active_operation_node_graph_for_category(ctx, char_id, op_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationSkipped",
        "charid",
        vec![PyRep::Integer(cat_id), PyRep::Integer(op_id)],
    );
    Ok(PyRep::Bool(true))
}

fn reset_operations_progress_for_char(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let op_defs = get_operation_defs(&ctx.sde);
    let first_op_id = op_defs.op_tasks.keys().copied().min().unwrap_or(1);
    let first_task_id = op_defs
        .op_tasks
        .get(&first_op_id)
        .and_then(|tasks| tasks.first().copied())
        .unwrap_or(10);
    let first_cat_id = ctx
        .sde
        .operations()
        .ok()
        .and_then(|ops| {
            ops.get(&first_op_id.to_string()).and_then(|v| {
                v.get("categoryID")
                    .or_else(|| v.get("category_id"))
                    .and_then(|c| c.as_i64())
            })
        })
        .unwrap_or(1);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        with_character_operations_record(snap, char_id, now, |rec: &mut CharacterOperationsRecord| {
            rec.completed_categories.clear();
            rec.completed_operations.clear();
            rec.completed_tasks.clear();
            rec.skipped_tasks.clear();
            rec.blocked_tasks.clear();
            rec.active_category_id = Some(first_cat_id);
            rec.active_operation_id = Some(first_op_id);
        });
    });
    stop_all_client_node_graphs_for_char(ctx, char_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOperationsProgressReset",
        "charid",
        vec![
            PyRep::List(Vec::new()),
            PyRep::List(Vec::new()),
            PyRep::Tuple(vec![
                PyRep::Integer(first_cat_id),
                PyRep::Integer(first_op_id),
                PyRep::Integer(first_task_id),
            ]),
        ],
    );
    Ok(PyRep::Bool(true))
}

fn reload_operations_fsd_data(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let allowed = service_roles::ROLE_GM_DEFAULT | service_roles::ROLE_CONTENT | service_roles::ROLE_PROGRAMMER;
    if (session.role & allowed) == 0 {
        return Err(rpc_err(
            "InsufficientRole",
            "reload_fsd_data requires GM/Content/Programmer role",
        ));
    }
    let _ = load_operation_definitions(&ctx.sde);
    broadcast_char_event(ctx, "OnOperationsServerFSDReload", vec![], Some(char_id));
    Ok(PyRep::Bool(true))
}

/// Handles GM `/operation` slash commands (`deactivate`, `replay`, `skip`, `reset_progress`, `reload_fsd`, `reveal_ui`).
pub fn handle_operation_slash(
    ctx: &ServerContext,
    session: &SessionState,
    caller_char_id: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let rest = cmd.strip_prefix("/operation")?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.first().copied() {
        Some("deactivate") => Some(deactivate_operation_for_char(ctx, caller_char_id)),
        Some("replay") => {
            let cat_id = parts.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let op_id = parts.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            Some(replay_operation_for_char(ctx, caller_char_id, cat_id, op_id))
        }
        Some("skip") => {
            let cat_id = parts.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let op_id = parts.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            Some(skip_operation_for_char(ctx, caller_char_id, cat_id, op_id))
        }
        Some("reset_progress" | "reset") => Some(reset_operations_progress_for_char(ctx, caller_char_id)),
        Some("reload_fsd") => Some(reload_operations_fsd_data(ctx, session, caller_char_id)),
        Some("reveal_ui") => {
            ctx.notify(
                NotificationTarget::Character(caller_char_id),
                "OnUiRevealForced",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::Bool(true)))
        }
        _ => Some(Err(rpc_err(
            "InvalidSlashArgument",
            "Usage: /operation <deactivate|replay|skip|reset_progress|reload_fsd|reveal_ui> ...",
        ))),
    }
}
