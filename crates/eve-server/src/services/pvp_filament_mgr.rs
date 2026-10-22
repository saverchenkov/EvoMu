//! `pvp_filament_mgr` — Proving Grounds / Abyssal PvP Filament manager (`pvpFilamentMgr`).
//!
//! Handles matchmaking queue registration, active/scheduled Proving Grounds events,
//! leaderboards, character statistics, and Abyssal end-gate extraction.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub const PVP_FILAMENT_STORE_KEY: i64 = -990_050;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PvpFilamentStore {
    /// Queued pilots: characterID -> (itemID, typeID, joined_unix)
    pub queue: HashMap<i64, (i64, i64, u64)>,
    /// Match statistics: characterID -> (wins, losses, score, kills, damage)
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
}

pub fn load_pvp_store(snap: &eve_db::WorldStateSnapshot) -> PvpFilamentStore {
    snap.item_custom_names
        .get(&PVP_FILAMENT_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn save_pvp_store(snap: &mut eve_db::WorldStateSnapshot, store: &PvpFilamentStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.item_custom_names.insert(PVP_FILAMENT_STORE_KEY, json);
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
    if service != "pvpFilamentMgr" {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let now_unix = ctx.now_unix();

    match (service, method) {
        ("pvpFilamentMgr", "JoinPVPQueue") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            if item_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Valid filament itemID is required to join queue".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(it) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: "Filament item not found in inventory".into(),
                    });
                };
                if it.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotOwned".into(),
                        message: "Filament item is not owned by pilot".into(),
                    });
                }
                if type_id > 0 && it.type_id != type_id as u32 {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: "Item type mismatch".into(),
                    });
                }
                if it.quantity > 1 {
                    it.quantity -= 1;
                } else {
                    snap.items.remove(&item_id);
                }

                let mut store = load_pvp_store(snap);
                store.queue.insert(char_id, (item_id, type_id, now_unix));
                store.stats.entry(char_id).or_insert((0, 0, 0, 0, 0.0));
                save_pvp_store(snap, &store);
                Ok(())
            });

            match res {
                Ok(_) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        ("pvpFilamentMgr", "LeavePVPQueue") => {
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                store.queue.remove(&char_id);
                save_pvp_store(snap, &store);
            });
            Some(Ok(PyRep::None))
        }

        ("pvpFilamentMgr", "GetAllEvents") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            let event = PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ]);
            let mut dict = Vec::new();
            dict.push((PyRep::Integer(1), event));
            Some(Ok(PyRep::Dict(dict)))
        }

        ("pvpFilamentMgr", "GetActiveEvents") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            let event = PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ]);
            let mut dict = Vec::new();
            dict.push((PyRep::Integer(1), event));
            Some(Ok(PyRep::Dict(dict)))
        }

        ("pvpFilamentMgr", "GetMostRecentEvent") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            Some(Ok(PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ])))
        }

        ("pvpFilamentMgr", "GetNextEventDate") => {
            let next_date = (now_unix + 86400 * 7) as i64;
            Some(Ok(PyRep::Integer(next_date)))
        }

        ("pvpFilamentMgr", "GetLeaderboard") => {
            let entries = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                let mut list = Vec::new();
                for (&cid, &(wins, losses, score, _k, _d)) in &store.stats {
                    if wins > 0 || losses > 0 || score > 0 {
                        list.push(PyRep::keyval([
                            ("characterID", PyRep::Integer(cid)),
                            ("rank", PyRep::Integer(1)),
                            ("score", PyRep::Integer(score)),
                            ("wins", PyRep::Integer(wins)),
                            ("losses", PyRep::Integer(losses)),
                        ]));
                    }
                }
                list
            });
            Some(Ok(PyRep::List(entries)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let stats = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                let (w, l, s, k, d) = store.stats.get(&char_id).copied().unwrap_or((0, 0, 0, 0, 0.0));
                PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("rank", PyRep::Integer(if s > 0 { 1 } else { 0 })),
                    ("score", PyRep::Integer(s)),
                    ("wins", PyRep::Integer(w)),
                    ("losses", PyRep::Integer(l)),
                    ("kills", PyRep::Integer(k)),
                    ("damageDealt", PyRep::Real(d)),
                ])
            });
            Some(Ok(stats))
        }

        ("pvpFilamentMgr", "AbyssalPVPEndGateActivation") => {
            let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                store.queue.remove(&char_id);
                save_pvp_store(snap, &store);
            });
            Some(Ok(PyRep::keyval([
                ("gateID", PyRep::Integer(gate_id)),
                ("activated", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}
