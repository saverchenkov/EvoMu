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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvpQueueCharacterEntry {
    pub item_id: i64,
    pub type_id: u32,
    pub location_id: i64,
    pub flag: u16,
    pub joined_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PvpFilamentStore {
    /// Queued pilots: characterID -> PvpQueueCharacterEntry
    pub queue: HashMap<i64, PvpQueueCharacterEntry>,
    /// Match statistics: characterID -> (wins, losses, score, kills, damage)
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
}

pub fn load_pvp_store(snap: &eve_db::WorldStateSnapshot) -> PvpFilamentStore {
    snap.service_state_blobs
        .get("pvp_filament_store")
        .or_else(|| snap.service_state_blobs.get("pvp_filament:global"))
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn save_pvp_store(snap: &mut eve_db::WorldStateSnapshot, store: &PvpFilamentStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.service_state_blobs.insert("pvp_filament_store".into(), json.clone());
        snap.service_state_blobs.insert("pvp_filament:global".into(), json);
    }
}

/// Authoritative server-side combat resolution hook for Proving Grounds events.
pub fn record_proving_grounds_combat_result(
    ctx: &ServerContext,
    winner_char_id: i64,
    loser_char_id: i64,
    damage_done: f64,
) -> Result<(), BridgeRpcError> {
    if winner_char_id == loser_char_id {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Winner and loser must be distinct characters".into(),
        });
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        if !store.queue.contains_key(&winner_char_id) && !store.queue.contains_key(&loser_char_id) {
            return Err(BridgeRpcError {
                code: "NotInPvpQueue".into(),
                message: "Both combatants must be queued in Proving Grounds".into(),
            });
        }
        store.queue.remove(&winner_char_id);
        store.queue.remove(&loser_char_id);

        let w = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        w.0 += 1;
        w.2 += 100;
        w.3 += 1;
        w.4 += damage_done;

        let l = store.stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        l.1 += 1;
        l.2 += 25;

        save_pvp_store(snap, &store);
        Ok(())
    })
}

pub fn record_proving_grounds_combat_result_if_queued(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_id: i64,
    damage_done: f64,
) {
    if attacker_char_id == victim_char_id || attacker_char_id <= 0 || victim_char_id <= 0 {
        return;
    }
    let queued = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        store.queue.contains_key(&attacker_char_id) || store.queue.contains_key(&victim_char_id)
    });
    if queued {
        let _ = record_proving_grounds_combat_result(ctx, attacker_char_id, victim_char_id, damage_done);
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
            if let Err(e) = crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id) {
                return Some(Err(e));
            }
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            if item_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Valid filament itemID is required to join queue".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                if store.queue.contains_key(&char_id) {
                    return Err(BridgeRpcError {
                        code: "AlreadyInPvpQueue".into(),
                        message: "Character is already queued for a PvP filament event".into(),
                    });
                }
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
                let is_filament = ctx.sde.types.get(&it.type_id).map(|t| {
                    t.group_id == 1979 || t.group_id == 4041 || t.group_id == 4040 || it.type_id == 56000 || t.name.to_lowercase().contains("filament")
                }).unwrap_or(it.type_id == 56000);
                if !is_filament {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: format!("Item type {} is not a valid PvP filament", it.type_id),
                    });
                }
                let it_type_id = it.type_id;
                let it_location_id = it.location_id;
                let it_flag = it.flag as u16;

                if it.quantity > 1 {
                    it.quantity -= 1;
                } else {
                    snap.items.remove(&item_id);
                }

                store.queue.insert(char_id, PvpQueueCharacterEntry {
                    item_id,
                    type_id: it_type_id,
                    location_id: it_location_id,
                    flag: it_flag,
                    joined_unix: now_unix,
                });
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
            let item_id_arg = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                let Some(entry) = store.queue.remove(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInPvpQueue".into(),
                        message: "Character is not queued for PvP".into(),
                    });
                };
                if let Some(expected_id) = item_id_arg {
                    if entry.item_id != expected_id {
                        store.queue.insert(char_id, entry);
                        save_pvp_store(snap, &store);
                        return Err(BridgeRpcError {
                            code: "FilamentNotFound".into(),
                            message: "Filament item does not match queued filament".into(),
                        });
                    }
                }
                if let Some(it) = snap
                    .items
                    .get_mut(&entry.item_id)
                    .filter(|it| it.owner_id == char_id && it.type_id == entry.type_id)
                {
                    it.quantity += 1;
                } else {
                    snap.next_entity_id += 1;
                    let new_iid = snap.next_entity_id;
                    snap.items.insert(
                        new_iid,
                        eve_db::InventoryItemRecord {
                            item_id: new_iid,
                            type_id: entry.type_id,
                            owner_id: char_id,
                            location_id: entry.location_id,
                            flag: eve_db::InventoryFlag::from_u16(entry.flag),
                            quantity: 1,
                            singleton: false,
                            ..Default::default()
                        },
                    );
                }
                save_pvp_store(snap, &store);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(e)),
            }
        }

        ("pvpFilamentMgr", "GetAllEvents") => {
            let start1 = (now_unix.saturating_sub(86400)) as i64;
            let end1 = (now_unix + 86400 * 7) as i64;
            let event1 = PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start1)),
                ("endDate", PyRep::Integer(end1)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ]);

            let mut dict = Vec::new();
            dict.push((PyRep::Integer(1), event1));
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
