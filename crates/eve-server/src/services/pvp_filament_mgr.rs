//! `pvp_filament_mgr` — Proving Grounds / Abyssal PvP Filament manager (`pvpFilamentMgr`).
//!
//! Handles matchmaking queue registration, active/scheduled Proving Grounds events,
//! leaderboards, character statistics, and Abyssal end-gate extraction.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::router::cache_notifications::NotificationTarget;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvpArenaMatchInstance {
    pub match_id: i64,
    pub event_id: i64,
    pub arena_solar_system_id: u32,
    pub team_a: Vec<i64>,
    pub team_b: Vec<i64>,
    pub started_unix: u64,
    pub active: bool,
    pub winner_char_id: Option<i64>,
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
    /// Active arena match instances: match_id -> PvpArenaMatchInstance
    #[serde(default)]
    pub active_matches: HashMap<i64, PvpArenaMatchInstance>,
    #[serde(default)]
    pub next_match_id: i64,
}

#[inline]
fn unix_to_filetime(ts: i64) -> i64 {
    ts * 10_000_000 + 116_444_736_000_000_000
}

/// Seeds fixed schedule Proving Grounds events if none exist in the store.
/// Schedule contains historical, currently active, and upcoming events with fixed timestamps.
pub fn ensure_events(store: &mut PvpFilamentStore) {
    if store.events.is_empty() {
        store.seeded_default_events = true;
        store.events = vec![
            PvpEventSchedule {
                event_id: 1,
                schedule_id: 1000,
                event_name: "Frigate Proving Grounds".into(),
                description: "Historical 1v1 Frigates".into(),
                match_type_id: 1,
                start_unix: 1704067200, // 2024-01-01 00:00:00 UTC
                end_unix: 1704672000,   // 2024-01-08 00:00:00 UTC
                type_ids: vec![54951],
            },
            PvpEventSchedule {
                event_id: 2,
                schedule_id: 1001,
                event_name: "1v1 Cruiser Proving Grounds".into(),
                description: "Active Proving Grounds combat arena".into(),
                match_type_id: 1,
                start_unix: 1735689600, // 2025-01-01 00:00:00 UTC
                end_unix: 1893456000,   // 2030-01-01 00:00:00 UTC
                type_ids: vec![53965],
            },
            PvpEventSchedule {
                event_id: 3,
                schedule_id: 1002,
                event_name: "Destroyer 2v2 Proving Grounds".into(),
                description: "Upcoming 2v2 Destroyers".into(),
                match_type_id: 2,
                start_unix: 1893456000, // 2030-01-01 00:00:00 UTC
                end_unix: 1894060800,   // 2030-01-08 00:00:00 UTC
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

fn with_pvp_store_events<R, F>(ctx: &ServerContext, f: F) -> R
where
    F: FnOnce(&PvpFilamentStore) -> R,
{
    let needs_seed = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        store.events.is_empty()
    });

    if needs_seed {
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_pvp_store(snap);
            ensure_events(&mut store);
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

pub fn build_leaderboard_rows(store: &PvpFilamentStore) -> Vec<PyRep> {
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
}

pub fn build_character_statistics(store: &PvpFilamentStore, char_id: i64) -> PyRep {
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
    let (event_id, winner_stats, loser_stats, leaderboard) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let has_active_match = store.active_matches.values().any(|m| {
            m.active
                && ((m.team_a.contains(&winner_char_id) && m.team_b.contains(&loser_char_id))
                    || (m.team_b.contains(&winner_char_id) && m.team_a.contains(&loser_char_id)))
        });
        let both_queued = store.queue.contains_key(&winner_char_id) && store.queue.contains_key(&loser_char_id);

        if !has_active_match && !both_queued {
            return Err(BridgeRpcError {
                code: "NotInPvpQueue".into(),
                message: "Both combatants must be queued in Proving Grounds or in an active match".into(),
            });
        }

        // Dequeue both combatants
        store.queue.remove(&winner_char_id);
        store.queue.remove(&loser_char_id);

        let mut matched_event_id = 2; // Default to active event
        for m in store.active_matches.values_mut() {
            if m.active
                && ((m.team_a.contains(&winner_char_id) && m.team_b.contains(&loser_char_id))
                    || (m.team_b.contains(&winner_char_id) && m.team_a.contains(&loser_char_id)))
            {
                m.active = false;
                m.winner_char_id = Some(winner_char_id);
                matched_event_id = m.event_id;
            }
        }

        let w = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        w.0 += 1;
        w.2 += 100;
        w.3 += 1;
        w.4 += damage_done;

        let l = store.stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        l.1 += 1;
        l.2 += 25;

        save_pvp_store(snap, &store);

        let w_stats = build_character_statistics(&store, winner_char_id);
        let l_stats = build_character_statistics(&store, loser_char_id);
        let lb_rows = build_leaderboard_rows(&store);

        Ok((matched_event_id, w_stats, l_stats, lb_rows))
    })?;

    // MachoNet client notifications: OnPVPFilamentsCharacterStatistics and OnPVPFilamentsLeaderboard
    let w_stat_notif = PyRep::keyval([
        ("eventID", PyRep::Integer(event_id)),
        ("event_id", PyRep::Integer(event_id)),
        ("statistics", winner_stats),
    ]);
    let l_stat_notif = PyRep::keyval([
        ("eventID", PyRep::Integer(event_id)),
        ("event_id", PyRep::Integer(event_id)),
        ("statistics", loser_stats),
    ]);
    let lb_notif = PyRep::keyval([
        ("eventID", PyRep::Integer(event_id)),
        ("event_id", PyRep::Integer(event_id)),
        ("entries", PyRep::List(leaderboard)),
    ]);

    ctx.notify(
        NotificationTarget::Character(winner_char_id),
        "OnPVPFilamentsCharacterStatistics",
        "charid",
        vec![w_stat_notif],
    );
    ctx.notify(
        NotificationTarget::Character(loser_char_id),
        "OnPVPFilamentsCharacterStatistics",
        "charid",
        vec![l_stat_notif],
    );
    ctx.notify(
        NotificationTarget::Character(winner_char_id),
        "OnPVPFilamentsLeaderboard",
        "charid",
        vec![lb_notif.clone()],
    );
    ctx.notify(
        NotificationTarget::Character(loser_char_id),
        "OnPVPFilamentsLeaderboard",
        "charid",
        vec![lb_notif],
    );

    Ok(())
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
        (store.queue.contains_key(&attacker_char_id) && store.queue.contains_key(&victim_char_id))
            || store.active_matches.values().any(|m| {
                m.active
                    && ((m.team_a.contains(&attacker_char_id) && m.team_b.contains(&victim_char_id))
                        || (m.team_b.contains(&attacker_char_id) && m.team_a.contains(&victim_char_id)))
            })
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
                ensure_events(&mut store);
                let active_event = store
                    .events
                    .iter()
                    .find(|ev| ev.start_unix <= now_i64 && now_i64 <= ev.end_unix && ev.type_ids.contains(&i64::from(it.type_id)));
                let in_active_event = active_event.is_some();
                let active_event_id = active_event.map(|e| e.event_id).unwrap_or(2);

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

                // Matchmaking: check if another pilot is already queued for this filament type
                let matched_opponent = store
                    .queue
                    .iter()
                    .find(|(&cid, qe)| cid != char_id && qe.type_id == it_type_id)
                    .map(|(&cid, _)| cid);

                let queue_wait_secs = match matched_opponent {
                    Some(opp_id) => {
                        store.next_match_id += 1;
                        let mid = store.next_match_id;
                        let match_inst = PvpArenaMatchInstance {
                            match_id: mid,
                            event_id: active_event_id,
                            arena_solar_system_id: 32000001, // AD001 Proving Grounds arena system
                            team_a: vec![opp_id],
                            team_b: vec![char_id],
                            started_unix: now_unix,
                            active: true,
                            winner_char_id: None,
                        };
                        store.active_matches.insert(mid, match_inst);
                        5i64 // Match formed and launching in 5 seconds
                    }
                    None => 30i64, // Initial estimated queue wait time
                };

                store.queue.insert(char_id, PvpQueueCharacterEntry {
                    item_id,
                    type_id: it_type_id,
                    location_id: it_location_id,
                    flag: it_flag,
                    joined_unix: now_unix,
                });
                store.stats.entry(char_id).or_insert((0, 0, 0, 0, 0.0));
                save_pvp_store(snap, &store);
                Ok(queue_wait_secs)
            });

            match res {
                Ok(wait_secs) => {
                    Some(Ok(PyRep::Integer(wait_secs)))
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
                // Cancel active arena match if formed
                for m in store.active_matches.values_mut() {
                    if m.active && (m.team_a.contains(&char_id) || m.team_b.contains(&char_id)) {
                        m.active = false;
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
            let events_rep = with_pvp_store_events(ctx, |store| {
                let dict = store.events.iter().map(|e| (PyRep::Integer(e.event_id), event_to_keyval(e))).collect();
                PyRep::Dict(dict)
            });
            Some(Ok(events_rep))
        }

        ("pvpFilamentMgr", "GetActiveEvents") => {
            let now_i64 = now_unix as i64;
            let active_rep = with_pvp_store_events(ctx, |store| {
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
            let recent_rep = with_pvp_store_events(ctx, |store| {
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
            let next_date = with_pvp_store_events(ctx, |store| {
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
                build_leaderboard_rows(&store)
            });
            Some(Ok(PyRep::List(entries)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let stats = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                build_character_statistics(&store, char_id)
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
