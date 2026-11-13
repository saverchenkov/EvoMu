//! `character_expert_systems` — Expert System Subscriptions and Node Graph Client Handlers.
//!
//! Handles:
//! - `expertSystemMgr`: `GetMyExpertSystems`, `ConsumeExpertSystem`, `RemoveMyExpertSystem`
//!   (`expertSystems/client/service.pyj`).
//! - `node_graph`: `process_client_blackboard_request`, `process_client_graph_message`
//!   (`nodegraph/client/graph.pyj`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};

pub use eve_db::ExpertSystemRecord;

/// SDE path to Expert Systems binary definitions.
const SDE_EXPERT_SYSTEMS_PATH: &str = "res:/staticdata/expertsystems.fsdbinary";

/// 1 day in 100ns BlueOS FileTime ticks: 24 * 3600 * 10_000_000.
const TICKS_PER_DAY: i64 = 864_000_000_000;

fn load_expert_systems(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> BTreeMap<i64, ExpertSystemRecord> {
    snap.node_graph_state
        .expert_systems
        .get(&char_id)
        .cloned()
        .unwrap_or_default()
}

fn save_expert_systems(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    records: &BTreeMap<i64, ExpertSystemRecord>,
) {
    if records.is_empty() {
        snap.node_graph_state.expert_systems.remove(&char_id);
    } else {
        snap.node_graph_state
            .expert_systems
            .insert(char_id, records.clone());
    }
}

/// Returns active, non-expired expert system skills for a character.
pub fn get_active_expert_system_skills(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
) -> HashMap<u32, u8> {
    let records = load_expert_systems(snap, char_id);
    let mut skills = HashMap::new();
    for rec in records.values() {
        if rec.expires_at_ft > now_ft {
            for (&skill_id, &level) in &rec.skills_granted {
                let entry = skills.entry(skill_id as u32).or_insert(0u8);
                *entry = (*entry).max(level as u8);
            }
        }
    }
    skills
}

/// Removes expired Expert Systems across all characters, notifies affected pilots via
/// `OnExpertSystemsUpdated`, and pushes updated ship kinematics / `CombatState` to active
/// `SystemActor` `Ballpark`s (`#736`).
pub fn sweep_expired_expert_systems_all(ctx: &ServerContext, now_ft: i64) {
    let any_expired = ctx.db.with_snapshot(|snap| {
        snap.node_graph_state
            .expert_systems
            .values()
            .any(|recs| recs.values().any(|r| r.expires_at_ft <= now_ft))
    });
    if !any_expired {
        return;
    }
    let expired_events = ctx.db.with_mut_snapshot(|snap| {
        let char_ids: Vec<i64> = snap.node_graph_state.expert_systems.keys().copied().collect();
        let mut events: Vec<(i64, Vec<i64>, Vec<u32>, PyRep, i64)> = Vec::new();
        for char_id in char_ids {
            let mut records = load_expert_systems(snap, char_id);
            let mut removed_tids = Vec::new();
            let mut removed_skill_ids = Vec::new();
            records.retain(|&tid, rec| {
                if rec.expires_at_ft <= now_ft {
                    removed_tids.push(tid);
                    for &skill_tid in rec.skills_granted.keys() {
                        if let Ok(u) = u32::try_from(skill_tid) {
                            if !removed_skill_ids.contains(&u) {
                                removed_skill_ids.push(u);
                            }
                        }
                    }
                    false
                } else {
                    true
                }
            });
            if !removed_tids.is_empty() {
                save_expert_systems(snap, char_id, &records);
                let ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                events.push((char_id, removed_tids, removed_skill_ids, expert_systems_to_py_dict(&records), ship_id));
            }
        }
        events
    });
    for (char_id, removed_tids, removed_skill_ids, es_dict, ship_id) in expired_events {
        for tid in removed_tids {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnExpertSystemsUpdated",
                "charid",
                vec![es_dict.clone(), PyRep::Bool(false), PyRep::Integer(tid)],
            );
        }
        crate::services::jump_clone_names::emit_server_skills_changed(
            ctx,
            char_id,
            &removed_skill_ids,
            Some("ExpertSystemExpired"),
        );
        ctx.sync_ship_ballpark_stats(char_id, ship_id);
    }
}

fn expert_systems_to_py_dict(records: &BTreeMap<i64, ExpertSystemRecord>) -> PyRep {
    let mut dict = Vec::with_capacity(records.len());
    for (&tid, rec) in records {
        dict.push((
            PyRep::Integer(tid),
            PyRep::keyval([
                ("typeID", PyRep::Integer(rec.type_id)),
                ("insertedAt", PyRep::Integer(rec.inserted_at_ft)),
                ("expiresAt", PyRep::Integer(rec.expires_at_ft)),
            ]),
        ));
    }
    PyRep::Dict(dict)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: rpc:expertSystemMgr.GetMyExpertSystems
        ("expertSystemMgr", "GetMyExpertSystems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let records = ctx.db.with_snapshot(|snap| load_expert_systems(snap, char_id));
            Some(Ok(expert_systems_to_py_dict(&records)))
        }

        // parity: rpc:expertSystemMgr.ConsumeExpertSystem
        ("expertSystemMgr", "ConsumeExpertSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "ConsumeExpertSystem requires an itemID".into(),
                        client_error: None,
                    }));
                }
            };

            let now_ft = ctx.now_blueos_filetime_ticks();
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;

            let consume_res = ctx.db.try_with_mut_snapshot(|snap| {
                let item = match snap.items.get(&item_id) {
                    Some(it) => it,
                    None => {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Inventory item {item_id} not found"),
                            client_error: None,
                        });
                    }
                };
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotOwned".into(),
                        message: format!("Item {item_id} is not owned by caller"),
                        client_error: None,
                    });
                }
                let effective_dock = current_dock.or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .and_then(|c| c.station_id.or(c.structure_id))
                });
                let effective_ship = current_ship.or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.active_ship_id)
                        .filter(|&sid| sid > 0)
                });
                if !snap.is_item_consumable_by(item, char_id, effective_dock, effective_ship) {
                    return Err(BridgeRpcError {
                        code: "ItemNotReachable".into(),
                        message: format!(
                            "Item {item_id} is not reachable in your hangar/cargo or is locked in a contract"
                        ),
                        client_error: None,
                    });
                }
                let resolved_type_id = item.type_id as i64;

                let sde_map = ctx
                    .sde
                    .load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH)
                    .map_err(|e| BridgeRpcError {
                        code: "SdeError".into(),
                        message: e.to_string(),
                        client_error: None,
                    })?;
                let sde_rec = match sde_map.get(&resolved_type_id.to_string()) {
                    Some(r) => r,
                    None => {
                        return Err(BridgeRpcError {
                            code: "InvalidItemType".into(),
                            message: format!("Item type {resolved_type_id} is not an expert system"),
                            client_error: None,
                        });
                    }
                };

                let duration_days = sde_rec
                    .get("durationDays")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(7);
                let duration_ticks = (duration_days as i64) * TICKS_PER_DAY;

                let mut skills_granted = BTreeMap::new();
                if let Some(skills_obj) = sde_rec.get("skillsGranted").and_then(|v| v.as_object()) {
                    for (k, v) in skills_obj {
                        if let (Ok(skill_id), Some(lvl)) = (k.parse::<i32>(), v.as_i64()) {
                            skills_granted.insert(skill_id, lvl as i32);
                        }
                    }
                }

                snap.consume_exact_item_quantity(
                    item_id,
                    1,
                    eve_db::ConsumeExactOptions {
                        active_ship_id: session.ship_id,
                        ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id)
                    },
                )
                .map_err(|e| BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: e.to_string(),
                    client_error: None,
                })?;

                let mut records = load_expert_systems(snap, char_id);
                let entry = records.entry(resolved_type_id).or_insert(ExpertSystemRecord {
                    type_id: resolved_type_id,
                    inserted_at_ft: now_ft,
                    expires_at_ft: now_ft,
                    skills_granted: skills_granted.clone(),
                });
                entry.skills_granted = skills_granted;
                entry.expires_at_ft = entry.expires_at_ft.max(now_ft) + duration_ticks;
                save_expert_systems(snap, char_id, &records);
                Ok((expert_systems_to_py_dict(&records), resolved_type_id))
            });

            match consume_res {
                Ok((es_dict, added_type_id)) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnExpertSystemsUpdated",
                        "charid",
                        vec![es_dict, PyRep::Bool(true), PyRep::Integer(added_type_id)],
                    );
                    Some(Ok(PyRep::None))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // parity: rpc:expertSystemMgr.RemoveMyExpertSystem
        ("expertSystemMgr", "RemoveMyExpertSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let type_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "RemoveMyExpertSystem requires an expertSystemTypeID".into(),
                        client_error: None,
                    }));
                }
            };
            let (es_dict, was_removed) = ctx.db.with_mut_snapshot(|snap| {
                let mut records = load_expert_systems(snap, char_id);
                let removed = records.remove(&type_id).is_some();
                save_expert_systems(snap, char_id, &records);
                (expert_systems_to_py_dict(&records), removed)
            });
            if was_removed {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnExpertSystemsUpdated",
                    "charid",
                    vec![es_dict, PyRep::Bool(false), PyRep::Integer(type_id)],
                );
            }
            Some(Ok(PyRep::None))
        }

        // parity: rpc:node_graph.process_client_blackboard_request
        ("node_graph", "process_client_blackboard_request") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let graph_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "process_client_blackboard_request requires a valid graph_id".into(),
                        client_error: None,
                    }));
                }
            };
            let is_valid_graph = ctx.sde.node_graphs.contains_key(&(graph_id as u32))
                || ctx.db.with_snapshot(|snap| {
                    snap.npe_progress
                        .get(&char_id)
                        .is_some_and(|n| n.node_graph_id == graph_id as u32)
                });
            if !is_valid_graph {
                return Some(Err(BridgeRpcError {
                    code: "NodeGraphNotFound".into(),
                    message: format!("Node graph {graph_id} not found"),
                    client_error: None,
                }));
            }
            let _node_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let bb_key = match args.get(2).and_then(|a| a.as_str()) {
                Some(k) => k,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "process_client_blackboard_request requires a blackboard key".into(),
                        client_error: None,
                    }));
                }
            };

            let res = ctx.db.with_snapshot(|snap| {
                if let Some(bb_val) =
                    crate::services::air_npe_nodegraph_runner::query_blackboard_value(
                        snap, char_id, graph_id, bb_key,
                    )
                {
                    return bb_val;
                }

                if let Some(npe) = snap.npe_progress.get(&char_id) {
                    if npe.node_graph_id == graph_id as u32 {
                        if bb_key == "current_node_id" {
                            return PyRep::Integer(npe.current_node_id as i64);
                        }
                        if bb_key == "completed" {
                            return PyRep::Bool(npe.completed);
                        }
                    }
                }

                let val_str = snap
                    .node_graph_state
                    .node_graph_blackboards
                    .get(&(char_id, graph_id))
                    .and_then(|bb| bb.get(bb_key));

                if let Some(val_str) = val_str {
                    if let Ok(num) = val_str.parse::<i64>() {
                        PyRep::Integer(num)
                    } else if val_str == "true" {
                        PyRep::Bool(true)
                    } else if val_str == "false" {
                        PyRep::Bool(false)
                    } else {
                        PyRep::String(val_str.clone())
                    }
                } else {
                    PyRep::None
                }
            });

            Some(Ok(res))
        }

        // parity: rpc:node_graph.process_client_graph_message
        // DATA GAP: `OnClientNodeGraphMessage` (`SendMessageToClientGraph`) requires server-authored node-graph execution scripts not shipped in V24 client/SDE.
        ("node_graph", "process_client_graph_message") => {
            const MAX_NODE_GRAPH_BB_KEYS: usize = 64;
            const MAX_NODE_GRAPH_BB_KEY_BYTES: usize = 128;
            const MAX_NODE_GRAPH_BB_VAL_BYTES: usize = 1024;
            const MAX_NODE_GRAPH_BB_BLOB_BYTES: usize = 8192;

            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_gm = session.has_gm_role();
            let graph_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "process_client_graph_message requires a valid graph_id".into(),
                        client_error: None,
                    }));
                }
            };
            let is_valid_graph = ctx.sde.node_graphs.contains_key(&(graph_id as u32))
                || ctx.db.with_snapshot(|snap| {
                    snap.npe_progress
                        .get(&char_id)
                        .is_some_and(|n| n.node_graph_id == graph_id as u32)
                });
            if !is_valid_graph {
                return Some(Err(BridgeRpcError {
                    code: "NodeGraphNotFound".into(),
                    message: format!("Node graph {graph_id} not found"),
                    client_error: None,
                }));
            }
            let Some(node_id) = args
                .get(1)
                .and_then(|a| a.as_i64())
                .filter(|&n| n >= 0 && n <= u32::MAX as i64)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "process_client_graph_message requires a valid non-negative node_id"
                        .into(),
                    client_error: None,
                }));
            };
            let node_u32 = node_id as u32;
            let graph_node_count = ctx
                .sde
                .node_graphs
                .get(&(graph_id as u32))
                .map(|g| g.node_count)
                .unwrap_or(0);
            if !is_gm && graph_node_count > 0 && node_u32 > graph_node_count {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Node {node_id} not in node graph {graph_id}"),
                    client_error: None,
                }));
            }
            let Some(msg_name) = args
                .get(2)
                .and_then(|a| a.as_str())
                .filter(|s| !s.is_empty() && s.len() <= MAX_NODE_GRAPH_BB_KEY_BYTES)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "process_client_graph_message requires a valid message key".into(),
                    client_error: None,
                }));
            };
            if !is_gm
                && matches!(
                    msg_name.trim().to_ascii_lowercase().as_str(),
                    "complete_air_npe"
                        | "checkpoint_reached"
                        | "advance_checkpoint"
                        | "objective_completed"
                )
            {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "GM role required for AIR NPE control graph message".into(),
                    client_error: None,
                }));
            }
            let msg_data = args.get(3).cloned().unwrap_or(PyRep::None);

            let mut data_str = match &msg_data {
                PyRep::String(s) => s.clone(),
                PyRep::Integer(i) => i.to_string(),
                PyRep::Bool(b) => b.to_string(),
                PyRep::Real(r) => r.to_string(),
                PyRep::None => "None".to_string(),
                other => format!("{other:?}"),
            };
            if data_str.len() > MAX_NODE_GRAPH_BB_VAL_BYTES {
                let mut end = MAX_NODE_GRAPH_BB_VAL_BYTES;
                while end > 0 && !data_str.is_char_boundary(end) {
                    end -= 1;
                }
                data_str.truncate(end);
            }

            let update_res = ctx
                .db
                .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    let bb = snap
                        .node_graph_state
                        .node_graph_blackboards
                        .entry((char_id, graph_id))
                        .or_default();

                    if !bb.contains_key(msg_name) && bb.len() >= MAX_NODE_GRAPH_BB_KEYS {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "Node graph blackboard key limit reached".into(),
                            client_error: None,
                        });
                    }
                    bb.insert(msg_name.to_string(), data_str);
                    if bb.contains_key("last_message") || bb.len() < MAX_NODE_GRAPH_BB_KEYS {
                        bb.insert("last_message".to_string(), msg_name.to_string());
                    }
                    let node_msg_key = format!("node.{node_id}.last_message");
                    if bb.contains_key(&node_msg_key) || bb.len() < MAX_NODE_GRAPH_BB_KEYS {
                        bb.insert(node_msg_key, msg_name.to_string());
                    }

                    let total_bb_bytes: usize =
                        bb.iter().map(|(k, v)| k.len() + v.len()).sum();
                    if total_bb_bytes > MAX_NODE_GRAPH_BB_BLOB_BYTES {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "Node graph blackboard size limit exceeded".into(),
                            client_error: None,
                        });
                    }

                    if msg_name.starts_with("client_") {
                        if let Some(npe) = snap.npe_progress.get_mut(&char_id) {
                            if npe.node_graph_id == graph_id as u32 && !npe.completed {
                                if is_gm {
                                    npe.current_node_id = node_u32.saturating_add(1);
                                } else {
                                    if node_u32 != npe.current_node_id
                                        && node_u32.saturating_add(1) != npe.current_node_id
                                    {
                                        return Err(BridgeRpcError {
                                            code: "InvalidArgument".into(),
                                            message: format!(
                                                "Out-of-order client node_id {node_id} (expected {})",
                                                npe.current_node_id
                                            ),
                                            client_error: None,
                                        });
                                    }
                                    if node_u32 == npe.current_node_id {
                                        npe.current_node_id =
                                            npe.current_node_id.saturating_add(1);
                                    }
                                }
                            }
                        }
                    }

                    Ok(())
                });
            if let Err(err) = update_res {
                return Some(Err(err));
            }

            if !crate::services::air_npe_nodegraph_runner::on_client_graph_message(
                ctx,
                session,
                char_id,
                graph_id,
                node_id,
                msg_name,
                &msg_data,
            ) {
                crate::services::notify_emitters_operations_nodegraph::on_client_graph_message(
                    ctx,
                    char_id,
                    graph_id,
                    node_id,
                    msg_name,
                    &msg_data,
                );
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

/// Helper for AIR operation activation in space; under DATA GAP no guessed client node graphs are started.
// DATA GAP: V24 operation node graph mappings are Tranquility server-only
pub fn start_client_node_graph_for_operation(
    _ctx: &ServerContext,
    _char_id: i64,
    _op_id: i64,
    _cat_id: i64,
    _solar_system_id: u32,
) {
    // DATA GAP: V24 operation node graph mappings are Tranquility server-only;
    // do not emit OnStartClientNodeGraph with guessed graph IDs.
}

/// Helper for AIR category completion; under DATA GAP no guessed client node graphs are stopped.
// DATA GAP: V24 operation node graph mappings are Tranquility server-only
pub fn stop_active_operation_node_graph(ctx: &ServerContext, char_id: i64, cat_id: i64) {
    crate::services::notify_emitters_operations_nodegraph::stop_active_operation_node_graph_for_category(ctx, char_id, cat_id);
}

/// Marks AIR NPE completed for `char_id`, stops any active AIR NPE client graphs and pocket,
/// and removes any active NPE node-graph blackboard from `node_graph_state`.
pub fn skip_air_npe_and_stop_graph(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> Result<(u32, u32, bool), BridgeRpcError> {
    let (graph_id, curr_node, completed) =
        crate::services::air_npe_nodegraph_runner::on_skip_air_npe(ctx, None, char_id, now_unix)?;
    if graph_id > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.node_graph_state
                .node_graph_blackboards
                .remove(&(char_id, i64::from(graph_id)));
        });
    }
    Ok((graph_id, curr_node, completed))
}

#[cfg(test)] #[path = "character_expert_systems_tests.rs"] mod character_expert_systems_tests;

