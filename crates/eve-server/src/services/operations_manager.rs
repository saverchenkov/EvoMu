//! `operations_manager` — Tutorial, AIR NPE Operations, CSAT Surveys & Telemetry.
//!
//! Implements:
//! - `operationsManager` — AIR NPE Career Operations & Tutorial tracking (`V24.01`)
//! - `survey` — In-game player satisfaction CSAT surveys & rewards
//! - `eventLog` — Autopilot, client telemetry, & disconnect logging

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

const OPERATIONS_STORE_KEY: i64 = -10_000_015;

pub use super::operations_manager_helpers::*;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct OperationsStore {
    by_character: HashMap<i64, CharacterOperationsRecord>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CharacterOperationsRecord {
    pub active_category_id: Option<i64>,
    pub completed_categories: HashSet<i64>,
    pub completed_operations: HashSet<i64>,
    #[serde(default)]
    pub completed_tasks: HashSet<i64>,
    pub blocked_tasks: HashSet<i64>,
    pub skipped_tasks: HashSet<i64>,
    pub active_operation_id: Option<i64>,
    #[serde(default)]
    pub active_operation_system_id: Option<u32>,
    #[serde(default)]
    pub active_site_id: Option<i64>,
    #[serde(default)]
    pub active_dungeon_id: Option<u32>,
    pub tutorial_skipped: bool,
    pub tutorial_finished: Option<bool>,
    pub avoidance_systems: Vec<i64>,
    pub active_survey_id: Option<i64>,
    #[serde(default)]
    pub completed_surveys: HashSet<i64>,
    #[serde(default)]
    pub operation_events: Vec<(String, i64)>,
    pub client_events_logged: u64,
    #[serde(default)]
    pub career_path: i64,
    #[serde(default)]
    pub tutorial_state: i64,
}

pub type CharOperationsRecord = CharacterOperationsRecord;
pub type AirNpeRecord = eve_db::NpeProgressRecord;

pub const MAX_AVOIDANCE_SYSTEMS: usize = 256;
pub const MAX_OPERATION_EVENTS: usize = 128;

fn push_operation_event(rec: &mut CharacterOperationsRecord, event_name: String, val: i64) {
    rec.operation_events.push((event_name, val));
    if rec.operation_events.len() > MAX_OPERATION_EVENTS {
        let excess = rec.operation_events.len() - MAX_OPERATION_EVENTS;
        rec.operation_events.drain(0..excess);
    }
}

pub fn sync_air_npe_skip(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, now_unix: u64) {
    let mut store = load_store(snap);
    let rec = store.by_character.entry(char_id).or_default();
    rec.tutorial_skipped = true;
    rec.tutorial_finished = Some(true);
    push_operation_event(rec, "skip_air_npe".to_string(), 0);
    save_store(snap, &store, now_unix);
}

/// Resolves the character's active operation/tutorial dungeon tuple `(dungeon_id, room_id, instance_id, dungeon_name)`
/// if the character has started an operation site (`start_site`) and is still in that solar system.
pub fn active_operation_dungeon_for_character(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<(u32, u32, i64, String)> {
    let (dungeon_id, site_id) = ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id)?;
        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id);
        if let Some(op_sys) = rec.active_operation_system_id {
            if char_sys != Some(op_sys) {
                return None;
            }
        }
        Some((rec.active_dungeon_id?, rec.active_site_id?))
    })?;
    let d = ctx.sde.dungeons.get(&dungeon_id)?;
    let dname = d.name.clone();
    // DERIVED: When an SDE dungeon has no asteroid room row in `sde_dungeon_asteroid_counts`,
    // derive a deterministic primary room ID from `dungeon_id * 100 + 1`.
    let room_id = ctx
        .sde
        .dungeon_asteroids
        .get(&dungeon_id)
        .and_then(|rows| rows.first().map(|r| r.room_id))
        .unwrap_or(dungeon_id.saturating_mul(100).saturating_add(1));
    Some((dungeon_id, room_id, site_id, dname))
}

/// Resolves the NPE/Career operation spawnpoint solar system for `(char_id, operation_id)`.
/// Uses the character's activated operation system if present, or resolves the character's
/// starter school solar system from `ctx.sde.schools`.
pub fn resolve_operation_spawnpoint_solar_system(
    ctx: &ServerContext,
    char_id: i64,
    operation_id: Option<i64>,
) -> Option<u32> {
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        if let Some(rec) = store.by_character.get(&char_id) {
            if let Some(sys_id) = rec.active_operation_system_id {
                if operation_id.is_none() || rec.active_operation_id == operation_id {
                    return Some(sys_id);
                }
            }
        }
        if let Some(ch) = snap.characters.get(&char_id) {
            if let Some(school) = ctx.sde.schools.get(&ch.school_id) {
                if school.solar_system_id > 0 {
                    return Some(school.solar_system_id);
                }
            }
            if let Some(school) = ctx
                .sde
                .schools
                .values()
                .find(|s| s.race_id == ch.race_id && s.solar_system_id > 0)
            {
                return Some(school.solar_system_id);
            }
            if ch.solar_system_id > 0 && ctx.sde.solar_systems.contains_key(&ch.solar_system_id) {
                return Some(ch.solar_system_id);
            }
        }
        None
    })
}

/// Checks whether `target_id` (a site ID, dungeon ID, or solar system ID) is an active
/// operation site or system for `char_id` in the character's current solar system.
pub fn is_operation_site_or_system(ctx: &ServerContext, char_id: i64, target_id: i64) -> bool {
    if target_id <= 0 {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        let Some(rec) = store.by_character.get(&char_id) else {
            return false;
        };
        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id);
        if let Some(op_sys) = rec.active_operation_system_id {
            if char_sys != Some(op_sys) {
                return false;
            }
        }
        rec.active_operation_id == Some(target_id)
            || rec.active_site_id == Some(target_id)
            || rec.active_dungeon_id.map(i64::from) == Some(target_id)
            || rec.active_operation_system_id.map(i64::from) == Some(target_id)
    })
}

fn load_store(snap: &eve_db::WorldStateSnapshot) -> OperationsStore {
    snap.owner_notes
        .get(&OPERATIONS_STORE_KEY)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &OperationsStore, now_unix: u64) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.owner_notes.insert(
            OPERATIONS_STORE_KEY,
            eve_db::OwnerNoteRecord {
                note_id: OPERATIONS_STORE_KEY,
                character_id: -1,
                item_id: 0,
                label: "operations_store".into(),
                note: json,
                timestamp_unix: now_unix,
            },
        );
    }
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // ====================================================================
        // operationsManager — AIR NPE & Operations Manager
        // ====================================================================
        ("operationsManager", "get_tutorial_state") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (is_active, tut_state, career_path) = ctx.db.with_snapshot(|snap| {
                let npe_completed = snap
                    .npe_progress
                    .get(&char_id)
                    .is_some_and(|n| n.completed || n.current_node_id >= 4);
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .map(|r| {
                        let active = if npe_completed || r.tutorial_skipped {
                            false
                        } else {
                            !r.tutorial_finished.unwrap_or(true)
                        };
                        let path = if r.career_path != 0 {
                            r.career_path
                        } else {
                            r.active_category_id.unwrap_or(0)
                        };
                        (active, r.tutorial_state, path)
                    })
                    .unwrap_or((false, 0, 0))
            });
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("is_tutorial_active".into()), PyRep::Bool(is_active)),
                (PyRep::String("tutorial_state".into()), PyRep::Integer(tut_state)),
                (PyRep::String("career_path".into()), PyRep::Integer(career_path)),
            ])))
        }

        ("operationsManager", "has_skipped_tutorial") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let skipped = ctx.db.with_snapshot(|snap| {
                let npe_skipped = snap
                    .npe_progress
                    .get(&char_id)
                    .is_some_and(|n| n.completed || n.current_node_id >= 4);
                let op_skipped = load_store(snap)
                    .by_character
                    .get(&char_id)
                    .is_some_and(|r| r.tutorial_skipped);
                npe_skipped || op_skipped
            });
            Some(Ok(PyRep::Bool(skipped)))
        }

        ("operationsManager", "is_main_tutorial_finished") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if target_char_id != char_id && !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "cannot query another character".into(),
                }));
            }
            let finished = ctx.db.with_snapshot(|snap| {
                let npe_finished = snap
                    .npe_progress
                    .get(&target_char_id)
                    .is_some_and(|n| n.completed || n.current_node_id >= 4);
                if npe_finished {
                    return true;
                }
                load_store(snap)
                    .by_character
                    .get(&target_char_id)
                    .and_then(|r| r.tutorial_finished)
                    .unwrap_or(true)
            });
            Some(Ok(PyRep::Bool(finished)))
        }

        ("operationsManager", "get_active_category_id") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cat_id = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .and_then(|rec| rec.active_category_id)
            });
            Some(Ok(cat_id.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }

        ("operationsManager", "is_category_complete") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cat_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if !(1..=100_000).contains(&cat_id) {
                return Some(Ok(PyRep::Bool(false)));
            }
            let is_complete = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .is_some_and(|rec| rec.completed_categories.contains(&cat_id))
            });
            Some(Ok(PyRep::Bool(is_complete)))
        }

        ("operationsManager", "get_category_state") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // DERIVED: V24 operation category/operation/task IDs are bounded positive integers in 1..=100_000.
            let Some(cat_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let state = ctx.db.with_snapshot(|snap| {
                if let Some(rec) = load_store(snap).by_character.get(&char_id) {
                    if rec.completed_categories.contains(&cat_id) {
                        return 3; // COMPLETE
                    }
                    if rec.active_category_id == Some(cat_id) {
                        return 2; // ACTIVE
                    }
                }
                1 // AVAILABLE
            });
            Some(Ok(PyRep::Integer(state)))
        }

        ("operationsManager", "complete_category_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(cat_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.completed_categories.insert(cat_id);
                if rec.active_category_id == Some(cat_id) {
                    rec.active_category_id = None;
                }
                save_store(snap, &store, now);
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnOperationCategoryCompleted",
                "charid",
                vec![PyRep::Integer(cat_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "get_character_progress") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let op_defs = get_operation_defs(&ctx.sde);
            let progress = ctx.db.with_snapshot(|snap| {
                if let Some(rec) = load_store(snap).by_character.get(&char_id) {
                    let mut map: HashMap<i64, i64> = HashMap::new();
                    let active_op_pct = if let Some(active_op) = rec.active_operation_id {
                        let pct = if let Some(tasks) = op_defs.op_tasks.get(&active_op) {
                            if !tasks.is_empty() {
                                let finished_in_op = tasks
                                    .iter()
                                    .filter(|t| {
                                        rec.completed_tasks.contains(t)
                                            || rec.skipped_tasks.contains(t)
                                    })
                                    .count();
                                ((finished_in_op as f64 / tasks.len() as f64) * 100.0) as i64
                            } else {
                                0
                            }
                        } else {
                            let finished_task_count =
                                (rec.completed_tasks.len() + rec.skipped_tasks.len()) as i64;
                            finished_task_count * 25
                        };
                        let clamped = pct.clamp(0, 95);
                        map.insert(active_op, clamped);
                        clamped
                    } else {
                        0
                    };
                    if let Some(active_cat) = rec.active_category_id {
                        map.insert(active_cat, active_op_pct);
                    }
                    for &task_id in &rec.blocked_tasks {
                        map.insert(task_id, 0);
                    }
                    for &task_id in &rec.skipped_tasks {
                        map.insert(task_id, 100);
                    }
                    for &task_id in &rec.completed_tasks {
                        map.insert(task_id, 100);
                    }
                    for &op_id in &rec.completed_operations {
                        map.insert(op_id, 100);
                    }
                    for &cat in &rec.completed_categories {
                        map.insert(cat, 100);
                    }
                    let mut items: Vec<(PyRep, PyRep)> = map
                        .into_iter()
                        .map(|(k, v)| (PyRep::Integer(k), PyRep::Integer(v)))
                        .collect();
                    items.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                    PyRep::Dict(items)
                } else {
                    PyRep::Dict(Vec::new())
                }
            });
            Some(Ok(progress))
        }

        ("operationsManager", "get_mission_avoidance_systems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .map(|r| r.avoidance_systems.iter().copied().map(PyRep::Integer).collect())
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(systems)))
        }

        ("operationsManager", "get_operations_completed_at_least_once") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ops = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .map(|rec| {
                        rec.completed_operations
                            .iter()
                            .map(|op| PyRep::Integer(*op))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(ops)))
        }

        ("operationsManager", "activate_operation_in_solar_system") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(op_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let char_sys = session.solar_system_id.or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.solar_system_id))
            });
            let Some(curr_sys) = char_sys.filter(|id| ctx.sde.solar_systems.contains_key(id)) else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let op_sys = if let Some(req_arg) = args.get(1) {
                let Some(req_sys) = req_arg
                    .as_i64()
                    .and_then(|s| u32::try_from(s).ok())
                    .filter(|id| ctx.sde.solar_systems.contains_key(id) && *id == curr_sys)
                else {
                    return Some(Err(BridgeRpcError::user_error("CustomInfo")));
                };
                req_sys
            } else {
                curr_sys
            };
            let now = ctx.now_unix();
            let cat_id = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = op_sys;
                }
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.active_operation_id = Some(op_id);
                rec.active_operation_system_id = Some(op_sys);
                let cat_id = args
                    .get(2)
                    .and_then(|a| a.as_i64())
                    .or(rec.active_category_id)
                    .unwrap_or(1);
                rec.active_category_id = Some(cat_id);
                save_store(snap, &store, now);
                cat_id
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnOperationCategoryChanged",
                "charid",
                vec![PyRep::Integer(cat_id), PyRep::Integer(op_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "block_task") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(task_id) = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.blocked_tasks.insert(task_id);
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "unblock_task") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(task_id) = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.blocked_tasks.remove(&task_id);
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "skip_task") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(task_id) = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .filter(|id| (1..=100_000).contains(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let call_cat_id = if args.len() >= 3 {
                args.first().and_then(|a| a.as_i64())
            } else {
                None
            };
            let call_op_id = if args.len() >= 3 {
                args.get(1).and_then(|a| a.as_i64())
            } else {
                None
            };
            let op_defs = get_operation_defs(&ctx.sde);
            let now = ctx.now_unix();
            let (cat_id, op_id) = ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.skipped_tasks.insert(task_id);
                let cat_id = call_cat_id.or(rec.active_category_id).unwrap_or(1);
                let op_id = call_op_id
                    .or(rec.active_operation_id)
                    .or_else(|| op_defs.task_to_op.get(&task_id).copied())
                    .unwrap_or(1);
                save_store(snap, &store, now);
                (cat_id, op_id)
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnOperationTaskCompleted",
                "charid",
                vec![
                    PyRep::Integer(cat_id),
                    PyRep::Integer(op_id),
                    PyRep::Integer(task_id),
                ],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "start_site") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(site_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let Some(d_id) = u32::try_from(site_id)
                .ok()
                .filter(|id| ctx.sde.dungeons.contains_key(id))
            else {
                return Some(Err(BridgeRpcError::user_error("CustomInfo")));
            };
            let char_sys = session.solar_system_id.or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.solar_system_id))
            });
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                if let (Some(ch), Some(sys)) = (snap.characters.get_mut(&char_id), char_sys) {
                    ch.solar_system_id = sys;
                }
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.active_site_id = Some(site_id);
                rec.active_dungeon_id = Some(d_id);
                if let Some(sys) = char_sys {
                    rec.active_operation_system_id = Some(sys);
                }
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "process_client_event") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let event_name = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("event_name").and_then(|v| v.as_str()))
                .or_else(|| kwargs.get("eventName").and_then(|v| v.as_str()))
                .unwrap_or("client_operation_event")
                .to_string();

            let amount = kwargs
                .get("amount")
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()));

            let extra_data = kwargs
                .get("extra_data")
                .or_else(|| kwargs.get("extraData"))
                .or_else(|| args.get(2))
                .or_else(|| {
                    args.get(1).filter(|a| matches!(a, PyRep::List(_) | PyRep::Dict(_)))
                });

            let now = ctx.now_unix();

            if event_name == "set_career_path" {
                let path_id = extra_data
                    .and_then(|ed| match ed {
                        PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                            if k.as_str() == Some("career_path") || k.as_str() == Some("path_id") {
                                v.as_i64()
                            } else {
                                None
                            }
                        }),
                        PyRep::Integer(i) => Some(*i),
                        _ => None,
                    })
                    .or(amount)
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                    .unwrap_or(1);
                ctx.db.with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let rec = store.by_character.entry(char_id).or_default();
                    rec.career_path = path_id;
                    rec.tutorial_finished = Some(false);
                    if rec.tutorial_state == 0 {
                        rec.tutorial_state = 1;
                    }
                    push_operation_event(rec, event_name, path_id);
                    save_store(snap, &store, now);
                });
                return Some(Ok(PyRep::Bool(true)));
            }

            if event_name == "start_tutorial" {
                let (state_id, path_id) = match extra_data {
                    Some(PyRep::Dict(dict)) => {
                        let sid = dict.iter().find_map(|(k, v)| {
                            if k.as_str() == Some("state_id") || k.as_str() == Some("tutorial_state") {
                                v.as_i64()
                            } else {
                                None
                            }
                        });
                        let pid = dict.iter().find_map(|(k, v)| {
                            if k.as_str() == Some("path_id") || k.as_str() == Some("career_path") {
                                v.as_i64()
                            } else {
                                None
                            }
                        });
                        (
                            sid.or(amount)
                                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                                .unwrap_or(1),
                            pid.or_else(|| args.get(2).and_then(|a| a.as_i64())),
                        )
                    }
                    Some(PyRep::Integer(i)) => (*i, args.get(2).and_then(|a| a.as_i64())),
                    _ => (
                        amount
                            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                            .unwrap_or(1),
                        args.get(2).and_then(|a| a.as_i64()),
                    ),
                };
                ctx.db.with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let rec = store.by_character.entry(char_id).or_default();
                    rec.tutorial_finished = Some(false);
                    rec.tutorial_skipped = false;
                    rec.tutorial_state = state_id;
                    if let Some(p) = path_id {
                        rec.career_path = p;
                    }
                    push_operation_event(rec, event_name, state_id);
                    save_store(snap, &store, now);
                    let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                        eve_db::NpeProgressRecord {
                            character_id: char_id,
                            node_graph_id: 473,
                            current_node_id: 1,
                            completed: false,
                            career_goal_progress: Default::default(),
                            claimed_goals: Default::default(),
                        }
                    });
                    npe.completed = false;
                    npe.current_node_id = 1;
                });
                return Some(Ok(PyRep::Bool(true)));
            }

            if event_name == "skip_tutorial" {
                ctx.db.with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let rec = store.by_character.entry(char_id).or_default();
                    rec.tutorial_skipped = true;
                    rec.tutorial_finished = Some(true);
                    push_operation_event(rec, event_name, 0);
                    save_store(snap, &store, now);
                    let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                        eve_db::NpeProgressRecord {
                            character_id: char_id,
                            node_graph_id: 473,
                            current_node_id: 1,
                            completed: false,
                            career_goal_progress: Default::default(),
                            claimed_goals: Default::default(),
                        }
                    });
                    npe.completed = true;
                    npe.current_node_id = 4;
                });
                return Some(Ok(PyRep::Bool(true)));
            }

            if event_name == "set_active_category_id" || event_name == "activate_category" {
                let cat_id = extra_data
                    .and_then(|ed| match ed {
                        PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                            if k.as_str() == Some("category_id")
                                || k.as_str() == Some("cat_id")
                                || k.as_str() == Some("categoryId")
                            {
                                v.as_i64()
                            } else {
                                None
                            }
                        }),
                        PyRep::Integer(i) => Some(*i),
                        _ => None,
                    })
                    .or(amount)
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                    .filter(|id| (1..=100_000).contains(id))
                    .unwrap_or(1);
                ctx.db.with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let rec = store.by_character.entry(char_id).or_default();
                    rec.active_category_id = Some(cat_id);
                    push_operation_event(rec, event_name, cat_id);
                    save_store(snap, &store, now);
                });
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnOperationCategoryChanged",
                    "charid",
                    vec![PyRep::Integer(cat_id), PyRep::Integer(0)],
                );
                return Some(Ok(PyRep::Bool(true)));
            }

            if event_name == "add_mission_avoidance_system" {
                let sys_id = extra_data
                    .and_then(|ed| match ed {
                        PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                            if k.as_str() == Some("system_id")
                                || k.as_str() == Some("systemID")
                                || k.as_str() == Some("solar_system_id")
                            {
                                v.as_i64()
                            } else {
                                None
                            }
                        }),
                        PyRep::Integer(i) => Some(*i),
                        _ => None,
                    })
                    .or(amount)
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()));
                if let Some(sys_id) = sys_id {
                    let is_valid = u32::try_from(sys_id)
                        .ok()
                        .is_some_and(|id| ctx.sde.solar_systems.contains_key(&id));
                    if is_valid {
                        ctx.db.with_mut_snapshot(|snap| {
                            let mut store = load_store(snap);
                            let rec = store.by_character.entry(char_id).or_default();
                            if !rec.avoidance_systems.contains(&sys_id)
                                && rec.avoidance_systems.len() < MAX_AVOIDANCE_SYSTEMS
                            {
                                rec.avoidance_systems.push(sys_id);
                            }
                            push_operation_event(rec, event_name, sys_id);
                            save_store(snap, &store, now);
                        });
                        return Some(Ok(PyRep::Bool(true)));
                    }
                }
                return Some(Ok(PyRep::Bool(false)));
            }

            if event_name == "set_mission_avoidance_systems" {
                let mut systems: Vec<i64> = match extra_data {
                    Some(PyRep::List(list)) => list.iter().filter_map(|x| x.as_i64()).collect(),
                    Some(PyRep::Dict(dict)) => dict
                        .iter()
                        .find_map(|(k, v)| {
                            if k.as_str() == Some("systems") || k.as_str() == Some("system_ids") {
                                if let PyRep::List(list) = v {
                                    Some(list.iter().filter_map(|x| x.as_i64()).collect())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default(),
                    _ => match args.get(1) {
                        Some(PyRep::List(list)) => list.iter().filter_map(|x| x.as_i64()).collect(),
                        _ => args.iter().skip(1).filter_map(|x| x.as_i64()).collect(),
                    },
                };
                systems.retain(|&id| {
                    u32::try_from(id)
                        .ok()
                        .is_some_and(|u| ctx.sde.solar_systems.contains_key(&u))
                });
                systems.truncate(MAX_AVOIDANCE_SYSTEMS);
                let first_sys = systems.first().copied().unwrap_or(0);
                ctx.db.with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let rec = store.by_character.entry(char_id).or_default();
                    rec.avoidance_systems = systems;
                    push_operation_event(rec, event_name, first_sys);
                    save_store(snap, &store, now);
                });
                return Some(Ok(PyRep::Bool(true)));
            }

            let explicit_task_id = extra_data
                .and_then(|ed| match ed {
                    PyRep::Dict(dict) => dict.iter().find_map(|(k, v)| {
                        if k.as_str() == Some("task_id") || k.as_str() == Some("taskId") {
                            v.as_i64()
                        } else {
                            None
                        }
                    }),
                    PyRep::Integer(i) => Some(*i),
                    _ => None,
                })
                .or(amount)
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .filter(|id| (1..=100_000).contains(id));
            let op_defs = get_operation_defs(&ctx.sde);
            let completed_task = ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                let target_task = explicit_task_id.or_else(|| {
                    if !args.is_empty() || !kwargs.is_empty() {
                        if let Some(op) = rec.active_operation_id {
                            if let Some(tasks) = op_defs.op_tasks.get(&op) {
                                tasks
                                    .iter()
                                    .find(|t| {
                                        !rec.completed_tasks.contains(t)
                                            && !rec.skipped_tasks.contains(t)
                                    })
                                    .copied()
                                    .or_else(|| tasks.first().copied())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                push_operation_event(rec, event_name, target_task.unwrap_or(0));
                let mut transitioned = None;
                if let Some(tid) = target_task {
                    if !rec.blocked_tasks.contains(&tid) {
                        rec.completed_tasks.insert(tid);
                        if let Some(op_id) = rec.active_operation_id {
                            rec.completed_operations.insert(op_id);
                        }
                        let cat_id = rec.active_category_id.unwrap_or(1);
                        let op_id = rec
                            .active_operation_id
                            .or_else(|| op_defs.task_to_op.get(&tid).copied())
                            .unwrap_or(1);
                        transitioned = Some((cat_id, op_id, tid));
                    }
                }
                save_store(snap, &store, now);
                transitioned
            });
            if let Some((cat_id, op_id, tid)) = completed_task {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnOperationTaskCompleted",
                    "charid",
                    vec![
                        PyRep::Integer(cat_id),
                        PyRep::Integer(op_id),
                        PyRep::Integer(tid),
                    ],
                );
                Some(Ok(PyRep::Bool(true)))
            } else if !args.is_empty() || !kwargs.is_empty() {
                Some(Ok(PyRep::Bool(false)))
            } else {
                Some(Ok(PyRep::None))
            }
        }

        // ====================================================================
        // survey — In-game Player Satisfaction Surveys & Rewards
        // ====================================================================
        ("survey", "GetActiveSurveyID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let survey_id = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .and_then(|r| r.active_survey_id)
            });
            Some(Ok(survey_id.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }

        ("survey", "PerformSurveyChecks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let catalog = get_survey_catalog(&ctx.sde);
            let now = ctx.now_unix();
            let surveys = ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let eligible = {
                    let rec = store.by_character.entry(char_id).or_default();
                    let list = eligible_surveys(rec, catalog, now);
                    if rec.active_survey_id.is_none()
                        || rec
                            .active_survey_id
                            .is_some_and(|id| !list.contains(&id) || rec.completed_surveys.contains(&id))
                    {
                        rec.active_survey_id = list.first().copied();
                    }
                    list
                };
                save_store(snap, &store, now);
                eligible.into_iter().map(PyRep::Integer).collect()
            });
            Some(Ok(PyRep::List(surveys)))
        }

        ("survey", "ClaimSurveyRewards") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let explicit_survey_id = args.first().and_then(|a| a.as_i64());
            let catalog = get_survey_catalog(&ctx.sde);
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let (survey_id, sp_reward, isk_reward) = {
                    let rec = store.by_character.entry(char_id).or_default();
                    let survey_id = if let Some(id) = explicit_survey_id {
                        id
                    } else if let Some(active_id) = rec.active_survey_id {
                        active_id
                    } else {
                        return Err(BridgeRpcError::user_error("NoActiveSurvey"));
                    };
                    let Some(survey) = catalog.iter().find(|s| s.survey_id == survey_id) else {
                        return Err(rpc_err(
                            "InvalidSurveyID",
                            format!("Invalid surveyID {survey_id}"),
                        ));
                    };
                    if let Some(exp) = survey.expires_at_unix {
                        if now > exp {
                            return Err(rpc_err(
                                "SurveyExpired",
                                format!("Survey {survey_id} has expired"),
                            ));
                        }
                    }
                    let eligible = eligible_surveys(rec, catalog, now);
                    let is_claimable = rec.active_survey_id == Some(survey_id)
                        || eligible.contains(&survey_id);
                    if !is_claimable || rec.completed_surveys.contains(&survey_id) {
                        return Err(rpc_err(
                            "SurveyNotActive",
                            format!("Survey {survey_id} is not active or has already been claimed"),
                        ));
                    }
                    rec.completed_surveys.insert(survey_id);
                    rec.active_survey_id = None;
                    (survey_id, survey.sp_reward, survey.isk_reward)
                };
                save_store(snap, &store, now);
                let ch = snap
                    .characters
                    .get_mut(&char_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", "Character not found"))?;
                ch.free_skill_points += sp_reward;
                if isk_reward > 0 {
                    let reward_cents = eve_db::IskCents::from_isk(isk_reward);
                    ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                    snap.next_entity_id += 1;
                    let entry_id = snap.next_entity_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id,
                        character_id: char_id,
                        entry_type: "MarketEscrow".to_string(),
                        amount: reward_cents,
                        balance_after: ch.balance,
                        reason: format!("SurveyReward:{survey_id}"),
                    });
                }
                Ok(survey_id)
            });
            match res {
                Ok(survey_id) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSurveyRewardsAvailable",
                        "charid",
                        vec![PyRep::Integer(survey_id)],
                    );
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // ====================================================================
        // eventLog — Autopilot & Client Telemetry Logging
        // ====================================================================
        ("eventLog", "LogClientEvent") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                store.by_character.entry(char_id).or_default().client_events_logged += 1;
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::None))
        }

        ("eventLog", "LogClientStats") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                store.by_character.entry(char_id).or_default().client_events_logged += 1;
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::None))
        }

        ("eventLog", "LogPlayerRequestedDisconnect") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                store.by_character.entry(char_id).or_default().client_events_logged += 1;
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
