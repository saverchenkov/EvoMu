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
}

/// SDE path to Expert Systems binary definitions.
const SDE_EXPERT_SYSTEMS_PATH: &str = "res:/staticdata/expertsystems.fsdbinary";

/// 1 day in 100ns BlueOS FileTime ticks: 24 * 3600 * 10_000_000.
const TICKS_PER_DAY: i64 = 864_000_000_000;

fn expert_system_blob_key(char_id: i64) -> String {
    format!("expert_systems:{}", char_id)
}

fn node_graph_bb_blob_key(char_id: i64) -> String {
    format!("node_graph_bb:{}", char_id)
}

fn node_graph_msg_blob_key(char_id: i64) -> String {
    format!("node_graph_msg:{}", char_id)
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
            let item_or_type_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "ConsumeExpertSystem requires an itemID or typeID".into(),
                    }));
                }
            };

            // Resolve duration from SDE expertsystems.fsdbinary
            let sde_duration_days = ctx
                .sde
                .load_cfsd_nested(SDE_EXPERT_SYSTEMS_PATH)
                .ok()
                .and_then(|map| {
                    map.get(&item_or_type_id.to_string())
                        .and_then(|rec| rec.get("durationDays"))
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(7);
            let duration_ticks = (sde_duration_days as i64) * TICKS_PER_DAY;
            let now_ft = ctx.now_blueos_filetime_ticks();

            let consume_res = ctx.db.with_mut_snapshot(|snap| {
                let resolved_type_id = if let Some(item) = snap.items.get_mut(&item_or_type_id) {
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "ItemNotOwned".into(),
                            message: format!("Item {item_or_type_id} is not owned by caller"),
                        });
                    }
                    let tid = item.type_id as i64;
                    if item.quantity > 1 {
                        item.quantity -= 1;
                    } else {
                        snap.items.remove(&item_or_type_id);
                    }
                    tid
                } else {
                    item_or_type_id
                };

                let mut records = load_expert_systems(snap, char_id);
                let entry = records.entry(resolved_type_id).or_insert(ExpertSystemRecord {
                    type_id: resolved_type_id,
                    inserted_at_ft: now_ft,
                    expires_at_ft: now_ft,
                });
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

            let op_or_key = args.first().and_then(|a| a.as_str());
            let result_rep = ctx.db.with_mut_snapshot(|snap| {
                let bb_key = node_graph_bb_blob_key(char_id);
                let mut bb: HashMap<String, String> = snap
                    .service_state_blobs
                    .get(&bb_key)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();

                let res = match op_or_key {
                    Some("set") => {
                        let k = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                        let v_rep = args.get(2).cloned().unwrap_or(PyRep::None);
                        let v_str = match &v_rep {
                            PyRep::String(s) => s.clone(),
                            other => format!("{other:?}"),
                        };
                        bb.insert(k.to_string(), v_str);
                        v_rep
                    }
                    Some("get") => {
                        let k = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                        let default = args.get(2).cloned().unwrap_or(PyRep::None);
                        if let Some(s) = bb.get(k) {
                            PyRep::String(s.clone())
                        } else {
                            default
                        }
                    }
                    Some("has") => {
                        let k = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                        PyRep::Bool(bb.contains_key(k))
                    }
                    Some(k) => {
                        if let Some(v_rep) = args.get(1) {
                            let v_str = match v_rep {
                                PyRep::String(s) => s.clone(),
                                other => format!("{other:?}"),
                            };
                            bb.insert(k.to_string(), v_str);
                            v_rep.clone()
                        } else if let Some(s) = bb.get(k) {
                            PyRep::String(s.clone())
                        } else {
                            PyRep::None
                        }
                    }
                    None => {
                        let k = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                        let val_or_def = args.get(2).cloned().unwrap_or(PyRep::None);
                        if val_or_def != PyRep::None {
                            let v_str = match &val_or_def {
                                PyRep::String(s) => s.clone(),
                                other => format!("{other:?}"),
                            };
                            bb.insert(k.to_string(), v_str);
                            val_or_def
                        } else if let Some(s) = bb.get(k) {
                            PyRep::String(s.clone())
                        } else {
                            val_or_def
                        }
                    }
                };

                if let Ok(json) = serde_json::to_string(&bb) {
                    snap.service_state_blobs.insert(bb_key, json);
                }
                res
            });

            Some(Ok(result_rep))
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
                        message: "process_client_graph_message requires a positive graph_id".into(),
                    }));
                }
            };
            let msg_name = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("ObjectiveCompleted");

            ctx.db.with_mut_snapshot(|snap| {
                let msg_key = node_graph_msg_blob_key(char_id);
                let mut msgs: Vec<(i64, String, u64)> = snap
                    .service_state_blobs
                    .get(&msg_key)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                msgs.push((graph_id, msg_name.to_string(), ctx.now_unix()));
                if let Ok(json) = serde_json::to_string(&msgs) {
                    snap.service_state_blobs.insert(msg_key, json);
                }
            });

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
