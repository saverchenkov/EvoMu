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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct OperationsStore {
    by_character: HashMap<i64, CharacterOperationsRecord>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct CharacterOperationsRecord {
    active_category_id: Option<i64>,
    completed_categories: HashSet<i64>,
    completed_operations: HashSet<i64>,
    blocked_tasks: HashSet<i64>,
    skipped_tasks: HashSet<i64>,
    active_operation_id: Option<i64>,
    #[serde(default)]
    active_operation_system_id: Option<u32>,
    #[serde(default)]
    active_site_id: Option<i64>,
    #[serde(default)]
    active_dungeon_id: Option<u32>,
    tutorial_skipped: bool,
    tutorial_finished: Option<bool>,
    avoidance_systems: Vec<i64>,
    active_survey_id: Option<i64>,
    #[serde(default)]
    completed_surveys: HashSet<i64>,
    client_events_logged: u64,
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
    _kwargs: &HashMap<String, PyRep>,
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
            let (is_active, career_path) = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .map(|r| (!r.tutorial_finished.unwrap_or(true), r.active_category_id.unwrap_or(0)))
                    .unwrap_or((false, 0))
            });
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("is_tutorial_active".into()), PyRep::Bool(is_active)),
                (PyRep::String("tutorial_state".into()), PyRep::Integer(0)),
                (PyRep::String("career_path".into()), PyRep::Integer(career_path)),
            ])))
        }

        ("operationsManager", "has_skipped_tutorial") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let skipped = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
                    .is_some_and(|r| r.tutorial_skipped)
            });
            Some(Ok(PyRep::Bool(skipped)))
        }

        ("operationsManager", "is_main_tutorial_finished") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let finished = ctx.db.with_snapshot(|snap| {
                load_store(snap)
                    .by_character
                    .get(&char_id)
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
            let progress = ctx.db.with_snapshot(|snap| {
                if let Some(rec) = load_store(snap).by_character.get(&char_id) {
                    let mut items = Vec::new();
                    for cat in &rec.completed_categories {
                        items.push((PyRep::Integer(*cat), PyRep::Integer(100)));
                    }
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
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = op_sys;
                }
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.active_operation_id = Some(op_id);
                rec.active_operation_system_id = Some(op_sys);
                save_store(snap, &store, now);
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnOperationCategoryChanged",
                "charid",
                vec![PyRep::Integer(op_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("operationsManager", "block_task") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(task_id) = args
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
                rec.skipped_tasks.insert(task_id);
                save_store(snap, &store, now);
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnOperationTaskCompleted",
                "charid",
                vec![PyRep::Integer(task_id)],
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
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.client_events_logged += 1;
                save_store(snap, &store, now);
            });
            Some(Ok(PyRep::None))
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
            let now = ctx.now_unix();
            let surveys = ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let active_id = {
                    let rec = store.by_character.entry(char_id).or_default();
                    if rec.active_survey_id.is_none() && !rec.completed_surveys.contains(&1) {
                        rec.active_survey_id = Some(1);
                    }
                    rec.active_survey_id
                };
                save_store(snap, &store, now);
                active_id
                    .map(|id| vec![PyRep::Integer(id)])
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(surveys)))
        }

        ("survey", "ClaimSurveyRewards") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(survey_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err(
                    "InvalidArgument",
                    "ClaimSurveyRewards requires surveyID",
                )));
            };
            if survey_id <= 0 {
                return Some(Err(rpc_err(
                    "InvalidSurveyID",
                    format!("Invalid surveyID {survey_id}"),
                )));
            }
            let now = ctx.now_unix();
            // Award survey reward: 10,000 unallocated skill points only if survey_id is currently active for caller
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                {
                    let rec = store.by_character.entry(char_id).or_default();
                    if rec.active_survey_id != Some(survey_id) {
                        return Err(rpc_err(
                            "SurveyNotActive",
                            format!("Survey {survey_id} is not active or has already been claimed"),
                        ));
                    }
                    rec.active_survey_id = None;
                    rec.completed_surveys.insert(survey_id);
                }
                save_store(snap, &store, now);
                let ch = snap
                    .characters
                    .get_mut(&char_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", "Character not found"))?;
                ch.free_skill_points += 10_000.0;
                Ok(ch.free_skill_points as i64)
            });
            match res {
                Ok(sp) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSurveyRewardsAvailable",
                        "charid",
                        vec![PyRep::Integer(survey_id), PyRep::Integer(sp as i64)],
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
