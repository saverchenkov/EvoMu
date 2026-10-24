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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvpEventSchedule {
    pub event_id: i64,
    pub schedule_id: i64,
    pub event_name: String,
    pub description: String,
    pub match_type_id: i64,
    pub start_unix: i64,
    pub end_unix: i64,
    pub type_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PvpFilamentStore {
    /// Queued pilots: characterID -> PvpQueueCharacterEntry
    pub queue: HashMap<i64, PvpQueueCharacterEntry>,
    /// Match statistics: characterID -> (wins, losses, score, kills, damage)
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
    /// Scheduled and historical Proving Grounds events
    #[serde(default)]
    pub events: Vec<PvpEventSchedule>,
    #[serde(default)]
    pub seeded_default_events: bool,
}

#[inline]
fn unix_to_filetime(ts: i64) -> i64 {
    ts * 10_000_000 + 116_444_736_000_000_000
}

// SYNTHETIC_PVP_PROVING_GROUNDS_SCHEDULE: server-configured Proving Grounds schedule
pub fn ensure_events(store: &mut PvpFilamentStore, now_unix: i64) {
    let expired = !store.events.is_empty() && store.events.iter().all(|e| e.end_unix <= now_unix);
    if store.events.is_empty() || expired {
        store.seeded_default_events = true;
        store.events = vec![
            PvpEventSchedule {
                event_id: 1,
                schedule_id: 1000,
                event_name: "Frigate Proving Grounds".into(),
                description: "Historical 1v1 Frigates".into(),
                match_type_id: 1,
                start_unix: now_unix - 86400 * 14,
                end_unix: now_unix - 86400 * 7,
                type_ids: vec![54951],
            },
            PvpEventSchedule {
                event_id: 2,
                schedule_id: 1001,
                event_name: "1v1 Cruiser Proving Grounds".into(),
                description: "Active Proving Grounds combat arena".into(),
                match_type_id: 1,
                start_unix: now_unix - 86400 * 2,
                end_unix: now_unix + 86400 * 5,
                type_ids: vec![53965],
            },
            PvpEventSchedule {
                event_id: 3,
                schedule_id: 1002,
                event_name: "Destroyer 2v2 Proving Grounds".into(),
                description: "Upcoming 2v2 Destroyers".into(),
                match_type_id: 2,
                start_unix: now_unix + 86400 * 7,
                end_unix: now_unix + 86400 * 14,
                type_ids: vec![63726],
            },
        ];
    }
}

fn event_to_keyval(e: &PvpEventSchedule) -> PyRep {
    PyRep::keyval([
        ("eventID", PyRep::Integer(e.event_id)),
        ("name", PyRep::String(e.event_name.clone())),
        ("description", PyRep::String(e.description.clone())),
        ("matchTypeID", PyRep::Integer(e.match_type_id)),
        ("scheduleID", PyRep::Integer(e.schedule_id)),
        ("startDate", PyRep::Integer(unix_to_filetime(e.start_unix))),
        ("endDate", PyRep::Integer(unix_to_filetime(e.end_unix))),
        ("typeIDs", PyRep::List(e.type_ids.iter().copied().map(PyRep::Integer).collect())),
    ])
}

fn with_pvp_store_events<R, F>(ctx: &ServerContext, now_unix: i64, f: F) -> R
where
    F: FnOnce(&PvpFilamentStore) -> R,
{
    let needs_seed = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        store.events.is_empty() || store.events.iter().all(|e| e.end_unix <= now_unix)
    });

    if needs_seed {
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_pvp_store(snap);
            ensure_events(&mut store, now_unix);
            save_pvp_store(snap, &store);
        });
    }

    ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        f(&store)
    })
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
        if !store.queue.contains_key(&winner_char_id) || !store.queue.contains_key(&loser_char_id) {
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
        store.queue.contains_key(&attacker_char_id) && store.queue.contains_key(&victim_char_id)
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
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInSpace".into(),
                    message: "Cannot activate filament while docked in a station or structure".into(),
                }));
            }
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
                let active_ship_id = session.ship_id.ok_or_else(|| BridgeRpcError::user_error("MustBeInSpace"))?;
                if it.location_id != active_ship_id || it.flag != eve_db::InventoryFlag::Cargo {
                    return Err(BridgeRpcError::user_error("ItemNotInCargo"));
                }
                if type_id > 0 && it.type_id != type_id as u32 {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: "Item type mismatch".into(),
                    });
                }
                // Proving Grounds / Abyssal Battlefield filaments (SDE groups 4050 and 4743)
                // or active event-scheduled PvP filament types; Abyssal PvE (1979) and Jump (4041) filaments are rejected.
                let now_i64 = now_unix as i64;
                ensure_events(&mut store, now_i64);
                let in_active_event = store
                    .events
                    .iter()
                    .any(|ev| ev.start_unix <= now_i64 && now_i64 <= ev.end_unix && ev.type_ids.contains(&i64::from(it.type_id)));
                let in_inactive_only = store
                    .events
                    .iter()
                    .any(|ev| (ev.end_unix < now_i64 || ev.start_unix > now_i64) && ev.type_ids.contains(&i64::from(it.type_id)))
                    && !in_active_event;
                let is_sde_pvp_filament = ctx
                    .sde
                    .types
                    .get(&it.type_id)
                    .map(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
                    .unwrap_or(false);
                if !is_sde_pvp_filament {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: format!("Item type {} is not a valid PvP filament", it.type_id),
                    });
                }
                if in_inactive_only {
                    return Err(BridgeRpcError {
                        code: "EventNotActive".into(),
                        message: format!("Proving Grounds event for filament type {} is not currently active", it.type_id),
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
                Ok(_) => {
                    // SYNTHETIC_PVP_ESTIMATED_QUEUE_TIME_SECS: default synthetic estimated queue time
                    Some(Ok(PyRep::Integer(30)))
                }
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
            let events_rep = with_pvp_store_events(ctx, now_unix as i64, |store| {
                let dict = store.events.iter().map(|e| (PyRep::Integer(e.event_id), event_to_keyval(e))).collect();
                PyRep::Dict(dict)
            });
            Some(Ok(events_rep))
        }

        ("pvpFilamentMgr", "GetActiveEvents") => {
            let now_i64 = now_unix as i64;
            let active_rep = with_pvp_store_events(ctx, now_i64, |store| {
                let dict = store
                    .events
                    .iter()
                    .filter(|e| e.start_unix <= now_i64 && now_i64 <= e.end_unix)
                    .map(|e| (PyRep::Integer(e.event_id), event_to_keyval(e)))
                    .collect();
                PyRep::Dict(dict)
            });
            Some(Ok(active_rep))
        }

        ("pvpFilamentMgr", "GetMostRecentEvent") => {
            let now_i64 = now_unix as i64;
            let recent_rep = with_pvp_store_events(ctx, now_i64, |store| {
                store
                    .events
                    .iter()
                    .filter(|e| e.start_unix <= now_i64)
                    .max_by_key(|e| e.start_unix)
                    .map(event_to_keyval)
                    .unwrap_or(PyRep::None)
            });
            Some(Ok(recent_rep))
        }

        ("pvpFilamentMgr", "GetNextEventDate") => {
            let now_i64 = now_unix as i64;
            let next_date = with_pvp_store_events(ctx, now_i64, |store| {
                store
                    .events
                    .iter()
                    .filter(|e| e.start_unix > now_i64)
                    .map(|e| e.start_unix)
                    .min()
            });
            let rep = match next_date {
                Some(ts) => PyRep::Integer(unix_to_filetime(ts)),
                None => PyRep::None,
            };
            Some(Ok(rep))
        }

        ("pvpFilamentMgr", "GetLeaderboard") => {
            let entries = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = store
                    .stats
                    .iter()
                    .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0).then_some((cid, st)))
                    .collect();
                rows.sort_by(|(cid_a, a), (cid_b, b)| {
                    b.2.cmp(&a.2)
                        .then_with(|| b.0.cmp(&a.0))
                        .then_with(|| b.3.cmp(&a.3))
                        .then_with(|| b.4.total_cmp(&a.4))
                        .then_with(|| cid_a.cmp(cid_b))
                });
                rows.into_iter()
                    .take(100)
                    .enumerate()
                    .map(|(idx, (cid, (wins, losses, score, _k, _d)))| {
                        PyRep::keyval([
                            ("characterID", PyRep::Integer(cid)),
                            ("rank", PyRep::Integer((idx + 1) as i64)),
                            ("score", PyRep::Integer(score)),
                            ("wins", PyRep::Integer(wins)),
                            ("losses", PyRep::Integer(losses)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(entries)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let stats = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                let (w, l, s, k, d) = store.stats.get(&char_id).copied().unwrap_or((0, 0, 0, 0, 0.0));
                let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = store
                    .stats
                    .iter()
                    .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0).then_some((cid, st)))
                    .collect();
                rows.sort_by(|(cid_a, a), (cid_b, b)| {
                    b.2.cmp(&a.2)
                        .then_with(|| b.0.cmp(&a.0))
                        .then_with(|| b.3.cmp(&a.3))
                        .then_with(|| b.4.total_cmp(&a.4))
                        .then_with(|| cid_a.cmp(cid_b))
                });
                let rank = rows
                    .iter()
                    .position(|(cid, _)| *cid == char_id)
                    .map(|idx| (idx + 1) as i64)
                    .unwrap_or(0);
                PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("rank", PyRep::Integer(rank)),
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
