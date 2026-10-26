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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Record for a character's active or expired Expert System subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertSystemRecord {
    pub type_id: i64,
    pub inserted_at_ft: i64,
    pub expires_at_ft: i64,
    #[serde(default)]
    pub skills_granted: BTreeMap<i32, i32>,
}

/// SDE path to Expert Systems binary definitions.
const SDE_EXPERT_SYSTEMS_PATH: &str = "res:/staticdata/expertsystems.fsdbinary";

/// 1 day in 100ns BlueOS FileTime ticks: 24 * 3600 * 10_000_000.
const TICKS_PER_DAY: i64 = 864_000_000_000;

fn expert_system_blob_key(char_id: i64) -> String {
    format!("expert_systems:{}", char_id)
}

fn node_graph_bb_blob_key(char_id: i64, graph_id: i64) -> String {
    format!("node_graph_bb:{}:{}", char_id, graph_id)
}

fn load_expert_systems(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> BTreeMap<i64, ExpertSystemRecord> {
    snap.service_state_blobs
        .get(&expert_system_blob_key(char_id))
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

fn save_expert_systems(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    records: &BTreeMap<i64, ExpertSystemRecord>,
) {
    if let Ok(json) = serde_json::to_string(records) {
        snap.service_state_blobs
            .insert(expert_system_blob_key(char_id), json);
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

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
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
            let mut dict = Vec::with_capacity(records.len());
            for (&tid, rec) in &records {
                dict.push((
                    PyRep::Integer(tid),
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(rec.type_id)),
                        ("insertedAt", PyRep::Integer(rec.inserted_at_ft)),
                        ("expiresAt", PyRep::Integer(rec.expires_at_ft)),
                    ]),
                ));
            }
            Some(Ok(PyRep::Dict(dict)))
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
                    }));
                }
            };

            let now_ft = ctx.now_blueos_filetime_ticks();

            let consume_res = ctx.db.with_mut_snapshot(|snap| {
                let item = match snap.items.get_mut(&item_id) {
                    Some(it) => it,
                    None => {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Inventory item {item_id} not found"),
                        });
                    }
                };
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotOwned".into(),
                        message: format!("Item {item_id} is not owned by caller"),
                    });
                }
                let resolved_type_id = item.type_id as i64;

                let sde_map = ctx
                    .sde
                    .load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH)
                    .map_err(|e| BridgeRpcError {
                        code: "SdeError".into(),
                        message: e.to_string(),
                    })?;
                let sde_rec = match sde_map.get(&resolved_type_id.to_string()) {
                    Some(r) => r,
                    None => {
                        return Err(BridgeRpcError {
                            code: "InvalidItemType".into(),
                            message: format!("Item type {resolved_type_id} is not an expert system"),
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

                if item.quantity > 1 {
                    item.quantity -= 1;
                } else {
                    snap.items.remove(&item_id);
                }

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
                Ok(())
            });

            match consume_res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnExpertSystemsUpdated_Local",
                        "charid",
                        vec![],
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
                    }));
                }
            };
            ctx.db.with_mut_snapshot(|snap| {
                let mut records = load_expert_systems(snap, char_id);
                records.remove(&type_id);
                save_expert_systems(snap, char_id, &records);
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnExpertSystemsUpdated_Local",
                "charid",
                vec![],
            );
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
                }));
            }
            let _node_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let bb_key = match args.get(2).and_then(|a| a.as_str()) {
                Some(k) => k,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "process_client_blackboard_request requires a blackboard key".into(),
                    }));
                }
            };

            let res = ctx.db.with_snapshot(|snap| {
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

                let blob_key = node_graph_bb_blob_key(char_id, graph_id);
                let bb: HashMap<String, String> = snap
                    .service_state_blobs
                    .get(&blob_key)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();

                if let Some(val_str) = bb.get(bb_key) {
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
        ("node_graph", "process_client_graph_message") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let graph_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "process_client_graph_message requires a valid graph_id".into(),
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
                }));
            }
            let node_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let msg_name = args.get(2).and_then(|a| a.as_str()).unwrap_or("ObjectiveCompleted");
            let msg_data = args.get(3).cloned().unwrap_or(PyRep::None);

            let data_str = match &msg_data {
                PyRep::String(s) => s.clone(),
                PyRep::Integer(i) => i.to_string(),
                PyRep::Bool(b) => b.to_string(),
                PyRep::Real(r) => r.to_string(),
                PyRep::None => "None".to_string(),
                other => format!("{other:?}"),
            };

            ctx.db.with_mut_snapshot(|snap| {
                let blob_key = node_graph_bb_blob_key(char_id, graph_id);
                let mut bb: HashMap<String, String> = snap
                    .service_state_blobs
                    .get(&blob_key)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();

                bb.insert(msg_name.to_string(), data_str);
                bb.insert("last_message".to_string(), msg_name.to_string());
                bb.insert(format!("node.{node_id}.last_message"), msg_name.to_string());

                if let Ok(json) = serde_json::to_string(&bb) {
                    snap.service_state_blobs.insert(blob_key, json);
                }

                if matches!(msg_name, "NodeCompleted" | "ObjectiveCompleted" | "StepCompleted") {
                    if let Some(npe) = snap.npe_progress.get_mut(&char_id) {
                        if npe.node_graph_id == graph_id as u32 {
                            npe.current_node_id = (node_id as u32).saturating_add(1);
                        }
                    }
                }
            });

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
