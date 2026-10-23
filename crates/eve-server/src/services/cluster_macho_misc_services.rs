//! Cluster Miscellaneous and Character Setting RPC Service Handlers.
//!
//! Provides solid implementations for:
//! - `activityMgr.ActivitySeen` and `activityMgr.GetActivityBadgeStatuses`
//! - `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`
//! - `expertSystemMgr.ConsumeExpertSystem`, `expertSystemMgr.GetMyExpertSystems`, `expertSystemMgr.RemoveMyExpertSystem`
//! - `devIndexManager.GetDevelopmentIndicesForSystem`
//! - `i2.GetItem`
//! - `search.Query` and `search.QuickQuery`
//!
//! All unhandled, hollow, or mock RPC arms are omitted to fall through to `UnimplementedRPC`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{OwnerNoteRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

const MISC_STATE_NOTE_ID: i64 = -88_000_001;
const MISC_STATE_OWNER_ID: i64 = -88;

const MAX_ACTIVITY_SEEN_PER_CHAR: usize = 1_000;
const MAX_SETTINGS_PER_CHAR: usize = 500;
const MAX_SETTING_KEY_LEN: usize = 128;

#[derive(Default, Clone, Serialize, Deserialize)]
struct PersistedClusterMiscState {
    #[serde(default)]
    activity_seen: HashMap<i64, BTreeSet<i64>>,
    #[serde(default)]
    char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    #[serde(default)]
    expert_systems: HashMap<i64, BTreeMap<u32, u64>>,
}

#[derive(Default)]
pub(crate) struct ClusterMiscRuntimeState {
    pub(crate) activity_seen: HashMap<i64, BTreeSet<i64>>,
    pub(crate) char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    pub(crate) expert_systems: HashMap<i64, BTreeMap<u32, u64>>,
}

impl ClusterMiscRuntimeState {
    pub(crate) fn from_db(db: &Arc<WorldRepository>) -> Self {
        let mut st = Self::default();
        let persisted: Option<PersistedClusterMiscState> = db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&MISC_STATE_NOTE_ID)
                .and_then(|n| serde_json::from_str(&n.note).ok())
        });
        if let Some(p) = persisted {
            st.activity_seen = p.activity_seen;
            st.char_settings = p.char_settings;
            st.expert_systems = p.expert_systems;
        }
        st
    }
}

pub(crate) fn with_misc_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ClusterMiscRuntimeState) -> R,
) -> R {
    let mut guard = ctx
        .cluster_misc_state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

fn save_persisted_to_snap(snap: &mut eve_db::WorldStateSnapshot, guard: &ClusterMiscRuntimeState) {
    let p = PersistedClusterMiscState {
        activity_seen: guard.activity_seen.clone(),
        char_settings: guard.char_settings.clone(),
        expert_systems: guard.expert_systems.clone(),
    };
    if let Ok(json) = serde_json::to_string(&p) {
        snap.owner_notes.insert(
            MISC_STATE_NOTE_ID,
            OwnerNoteRecord {
                note_id: MISC_STATE_NOTE_ID,
                character_id: MISC_STATE_OWNER_ID,
                item_id: 0,
                label: "__cluster_misc_state__".into(),
                note: json,
                timestamp_unix: 0,
            },
        );
    }
}

pub(crate) fn persist_misc_state(ctx: &ServerContext) {
    let guard = ctx
        .cluster_misc_state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        save_persisted_to_snap(snap, &guard);
        Ok::<(), String>(())
    });
}

pub(crate) fn get_character_settings_dict(ctx: &ServerContext, char_id: i64) -> PyRep {
    with_misc_state(ctx, |st| {
        let settings = st.char_settings.get(&char_id).cloned().unwrap_or_default();
        let entries: Vec<(PyRep, PyRep)> = settings
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect();
        PyRep::Dict(entries)
    })
}

fn rpc_err(code: impl Into<String>, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

/// Dispatches requests for miscellaneous cluster RPC services.
///
/// Only solid, accepted rows are implemented here. Any other RPC returns `None`
/// to fall through to the global `UnimplementedRPC` handler.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &std::collections::HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // --------------------------------------------------------------------
        // activityMgr: Activity tracking
        // --------------------------------------------------------------------
        ("activityMgr", "ActivitySeen") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(activity_id) = args.first().and_then(|v| v.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(rpc_err("CustomError", "ActivitySeen requires a positive activityID")));
            };
            with_misc_state(ctx, |st| {
                let seen = st.activity_seen.entry(char_id).or_default();
                if seen.len() < MAX_ACTIVITY_SEEN_PER_CHAR {
                    seen.insert(activity_id);
                }
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        ("activityMgr", "GetActivityBadgeStatuses") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let seen: Vec<PyRep> = with_misc_state(ctx, |st| {
                st.activity_seen
                    .get(&char_id)
                    .map(|s| s.iter().copied().map(PyRep::Integer).collect())
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(seen)))
        }

        // --------------------------------------------------------------------
        // charMgr: Client UI settings persistence
        // --------------------------------------------------------------------
        ("charMgr", "SaveCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err("CustomError", "SaveCharacterSetting requires a non-empty setting key")));
            };
            if key.len() > MAX_SETTING_KEY_LEN {
                return Some(Err(rpc_err("CustomError", "Setting key exceeds maximum length")));
            }
            let val = args.get(1).cloned().unwrap_or(PyRep::Bool(true));
            with_misc_state(ctx, |st| {
                let user_settings = st.char_settings.entry(char_id).or_default();
                if user_settings.len() < MAX_SETTINGS_PER_CHAR || user_settings.contains_key(key) {
                    user_settings.insert(key.to_owned(), val);
                }
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        ("charMgr", "DeleteCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err("CustomError", "DeleteCharacterSetting requires a non-empty setting key")));
            };
            let _ = with_misc_state(ctx, |st| {
                st.char_settings
                    .get_mut(&char_id)
                    .and_then(|m| m.remove(key))
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        // --------------------------------------------------------------------
        // devIndexManager: Sovereign/Solar System development indices
        // --------------------------------------------------------------------
        ("devIndexManager", "GetDevelopmentIndicesForSystem") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let sys_id = args
                .first()
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let (mil, ind, strat) = ctx.db.with_snapshot(|snap| {
                snap.system_states
                    .get(&sys_id)
                    .map(|s| (s.military_index, s.industrial_index, s.strategic_index))
                    .unwrap_or((1, 1, 1))
            });
            let mk_idx = |attr_id: i64, lvl: u8| {
                PyRep::keyval([
                    ("attributeID", PyRep::Integer(attr_id)),
                    ("points", PyRep::Integer((lvl as i64) * 1_000)),
                    ("level", PyRep::Integer(lvl as i64)),
                    ("increasing", PyRep::Bool(true)),
                ])
            };
            Some(Ok(PyRep::Dict(vec![
                (PyRep::Integer(1583), mk_idx(1583, mil)),
                (PyRep::Integer(1584), mk_idx(1584, ind)),
                (PyRep::Integer(1615), mk_idx(1615, strat)),
                (PyRep::String("military".into()), PyRep::Integer(mil as i64)),
                (PyRep::String("industrial".into()), PyRep::Integer(ind as i64)),
                (PyRep::String("strategic".into()), PyRep::Integer(strat as i64)),
            ])))
        }

        // --------------------------------------------------------------------
        // expertSystemMgr: Expert System consumption, querying, and removal
        // --------------------------------------------------------------------
        ("expertSystemMgr", "ConsumeExpertSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(type_id) = args
                .first()
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&tid| tid > 0 && ctx.sde.types.contains_key(&tid))
            else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "ConsumeExpertSystem requires a valid SDE expert system typeID",
                )));
            };
            let exp = ctx.now_unix() + 7 * 86_400;
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                let item_id_opt = snap
                    .items
                    .values()
                    .find(|it| it.owner_id == char_id && it.type_id == type_id && it.quantity > 0)
                    .map(|it| it.item_id);
                let Some(item_id) = item_id_opt else {
                    return Err("Character does not own the requested Expert System item".to_string());
                };
                if let Some(it) = snap.items.get_mut(&item_id) {
                    if it.quantity > 1 {
                        it.quantity -= 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }
                let mut guard = ctx
                    .cluster_misc_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                guard
                    .expert_systems
                    .entry(char_id)
                    .or_default()
                    .insert(type_id, exp);
                save_persisted_to_snap(snap, &guard);
                Ok(())
            }) {
                return Some(Err(rpc_err("CustomError", e)));
            }
            Some(Ok(PyRep::keyval([
                ("typeID", PyRep::Integer(type_id as i64)),
                ("expiresAt", PyRep::Integer(exp as i64)),
                ("consumed", PyRep::Bool(true)),
            ])))
        }

        ("expertSystemMgr", "GetMyExpertSystems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let entries: Vec<(PyRep, PyRep)> = with_misc_state(ctx, |st| {
                st.expert_systems
                    .get(&char_id)
                    .map(|m| {
                        m.iter()
                            .map(|(&tid, &exp)| {
                                (
                                    PyRep::Integer(tid as i64),
                                    PyRep::keyval([
                                        ("typeID", PyRep::Integer(tid as i64)),
                                        ("expiresAt", PyRep::Integer(exp as i64)),
                                        ("isExpired", PyRep::Bool(now >= exp)),
                                    ]),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("expertSystemMgr", "RemoveMyExpertSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(type_id) = args
                .first()
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
            else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "RemoveMyExpertSystem requires a positive typeID",
                )));
            };
            let removed = with_misc_state(ctx, |st| {
                st.expert_systems
                    .get_mut(&char_id)
                    .and_then(|m| m.remove(&type_id))
                    .is_some()
            });
            if !removed {
                return Some(Err(rpc_err(
                    "ExpertSystemNotFound",
                    format!("Expert system {type_id} is not active on character {char_id}"),
                )));
            }
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        // --------------------------------------------------------------------
        // i2: Item inspection
        // --------------------------------------------------------------------
        // rpc-lint: allow legacy i2 service in parity ledger
        ("i2", "GetItem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|v| v.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(rpc_err("CustomError", "i2.GetItem requires a positive itemID")));
            };
            let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            let Some(it) = item_opt else {
                return Some(Err(rpc_err("CustomError", format!("Item {item_id} does not exist"))));
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let ship_id = session.ship_id.unwrap_or(0);
            if it.owner_id != char_id
                && it.owner_id != corp_id
                && it.location_id != char_id
                && it.location_id != ship_id
                && !session.has_gm_role()
            {
                return Some(Err(rpc_err("CustomError", "Caller is not authorized to inspect this item")));
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(it.item_id)),
                ("typeID", PyRep::Integer(it.type_id as i64)),
                ("ownerID", PyRep::Integer(it.owner_id)),
                ("locationID", PyRep::Integer(it.location_id)),
                ("flagID", PyRep::Integer(it.flag as i64)),
                ("quantity", PyRep::Integer(it.quantity.into())),
                ("singleton", PyRep::Bool(it.singleton)),
            ])))
        }

        // --------------------------------------------------------------------
        // search: In-game entity search across solar systems, stations, pilots, corps
        // --------------------------------------------------------------------
        ("search", "Query") | ("search", "QuickQuery") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let q = args
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if q.is_empty() {
                return Some(Err(rpc_err("CustomError", "search query string must not be empty")));
            }
            let (chars, corps) = ctx.db.with_snapshot(|snap| {
                let cs: Vec<PyRep> = snap
                    .characters
                    .values()
                    .filter(|c| c.name.to_lowercase().contains(&q))
                    .map(|c| PyRep::Integer(c.character_id))
                    .collect();
                let co: Vec<PyRep> = snap
                    .corporations
                    .values()
                    .filter(|c| c.name.to_lowercase().contains(&q))
                    .map(|c| PyRep::Integer(c.corporation_id))
                    .collect();
                (cs, co)
            });
            let mut systems: Vec<PyRep> = ctx
                .sde
                .solar_systems
                .values()
                .filter(|s| s.name.to_lowercase().contains(&q))
                .take(20)
                .map(|s| PyRep::Integer(s.solar_system_id as i64))
                .collect();
            systems.sort_by_key(|r| r.as_i64().unwrap_or(0));
            let mut stations: Vec<PyRep> = ctx
                .sde
                .stations
                .values()
                .filter(|s| s.name.to_lowercase().contains(&q))
                .take(20)
                .map(|s| PyRep::Integer(s.station_id))
                .collect();
            stations.sort_by_key(|r| r.as_i64().unwrap_or(0));
            if method == "Query" {
                Some(Ok(PyRep::Dict(vec![
                    (PyRep::Integer(1), PyRep::List(chars)),
                    (PyRep::Integer(2), PyRep::List(corps)),
                    (PyRep::Integer(3), PyRep::List(stations)),
                    (PyRep::Integer(5), PyRep::List(systems)),
                ])))
            } else {
                let mut flat = Vec::new();
                flat.extend(chars);
                flat.extend(corps);
                flat.extend(systems);
                flat.extend(stations);
                Some(Ok(PyRep::List(flat)))
            }
        }

        _ => None,
    }
}
