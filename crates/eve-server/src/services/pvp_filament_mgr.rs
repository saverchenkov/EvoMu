//! `pvp_filament_mgr` — Proving Grounds / Abyssal PvP Filament manager (`pvpFilamentMgr`).
//!
//! Handles matchmaking queue registration, active/scheduled Proving Grounds events,
//! leaderboards, character statistics, and Abyssal end-gate extraction.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub const PVP_FILAMENT_STORE_KEY: i64 = -990_050;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvpQueueCharacterEntry {
    pub item_id: i64,
    pub type_id: u32,
    pub location_id: i64,
    pub flag: u16,
    #[serde(default)]
    pub solar_system_id: u32,
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
    #[serde(default)]
    pub origin_systems: HashMap<i64, u32>,
    pub started_unix: u64,
    pub active: bool,
    pub winner_char_id: Option<i64>,
    #[serde(default)]
    pub exited_pilots: HashSet<i64>,
    #[serde(default)]
    pub end_gate_id: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PvpFilamentStore {
    /// Queued pilots: characterID -> PvpQueueCharacterEntry
    pub queue: HashMap<i64, PvpQueueCharacterEntry>,
    /// Match statistics: characterID -> (wins, losses, score, kills, damage)
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
    /// Match statistics scoped by event: event_id -> characterID -> (wins, losses, score, kills, damage)
    #[serde(default)]
    pub event_stats: HashMap<i64, HashMap<i64, (i64, i64, i64, i64, f64)>>,
    /// Scheduled and historical Proving Grounds events
    #[serde(default)]
    pub events: Vec<PvpEventSchedule>,
    #[serde(default)]
    pub seeded_default_events: bool,
    /// Active arena match instances: match_id -> PvpArenaMatchInstance
    #[serde(default)]
    pub active_matches: HashMap<i64, PvpArenaMatchInstance>,
    #[serde(default)]
    pub origin_positions: HashMap<i64, [f64; 3]>,
    #[serde(default)]
    pub next_match_id: i64,
}

#[inline]
fn unix_to_filetime(ts: i64) -> i64 {
    ts * 10_000_000 + 116_444_736_000_000_000
}

fn parse_ymd_utc_seconds(ymd: &str, end_of_day: bool) -> Option<i64> {
    let mut parts = ymd.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = (if y_adj >= 0 { y_adj } else { y_adj - 399 }) / 400;
    let yoe = y_adj - era * 400;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + if end_of_day { 86_399 } else { 0 })
}

fn load_sde_pvp_events(sde: &eve_sde::SdeRegistry) -> Vec<PvpEventSchedule> {
    static SDE_PVP_EVENTS: std::sync::OnceLock<Vec<PvpEventSchedule>> = std::sync::OnceLock::new();
    SDE_PVP_EVENTS
        .get_or_init(|| {
            let Ok(dates) = sde.load_cfsd_table("res:/staticdata/pvpfilamenteventdates.fsdbinary") else {
                return Vec::new();
            };
            let match_types = sde
                .load_cfsd_nested("res:/staticdata/pvpfilamentmatchtypes.fsdbinary")
                .unwrap_or_default();
            let mut out = Vec::new();
            for (event_id, rec) in dates {
                if rec.int("QA").unwrap_or(0) != 0 || rec.str("region").unwrap_or("ccp") != "ccp" {
                    continue;
                }
                let Some(start_str) = rec.str("startDate") else { continue };
                let Some(end_str) = rec.str("endDate") else { continue };
                let Some(start_unix) = parse_ymd_utc_seconds(start_str, false) else { continue };
                let Some(end_unix) = parse_ymd_utc_seconds(end_str, true) else { continue };
                let match_type_id = rec.int("matchTypeID").unwrap_or(0);
                let filament_type_id = match_types
                    .get(&match_type_id.to_string())
                    .and_then(|v| v.get("filamentTypeID"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let type_ids = if filament_type_id > 0 { vec![filament_type_id] } else { Vec::new() };
                let (event_name, description) = u32::try_from(filament_type_id)
                    .ok()
                    .and_then(|tid| sde.types.get(&tid))
                    .map(|t| (t.name.clone(), t.description.clone()))
                    .unwrap_or_else(|| (format!("Proving Grounds #{event_id}"), String::new()));
                out.push(PvpEventSchedule {
                    event_id,
                    schedule_id: event_id,
                    event_name,
                    description,
                    match_type_id,
                    start_unix,
                    end_unix,
                    type_ids,
                });
            }
            out
        })
        .clone()
}

/// Seeds the Proving Grounds event schedule from `pvpfilamenteventdates.fsdbinary`
/// and `pvpfilamentmatchtypes.fsdbinary` when no events exist in the store.
pub fn ensure_events_from_sde(sde: &eve_sde::SdeRegistry, store: &mut PvpFilamentStore) {
    if !store.seeded_default_events && store.events.is_empty() {
        store.seeded_default_events = true;
        store.events = load_sde_pvp_events(sde);
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
        !store.seeded_default_events && store.events.is_empty()
    });

    if needs_seed {
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_pvp_store(snap);
            ensure_events_from_sde(&ctx.sde, &mut store);
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

fn get_stats_source(store: &PvpFilamentStore, event_id: i64) -> &HashMap<i64, (i64, i64, i64, i64, f64)> {
    if event_id > 0 {
        if let Some(es) = store.event_stats.get(&event_id) {
            if !es.is_empty() {
                return es;
            }
        }
    }
    &store.stats
}

pub fn build_leaderboard_rows(store: &PvpFilamentStore) -> Vec<PyRep> {
    build_leaderboard_rows_for_event(store, 0)
}

pub fn build_leaderboard_rows_for_event(store: &PvpFilamentStore, event_id: i64) -> Vec<PyRep> {
    let stats_map = get_stats_source(store, event_id);
    let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = stats_map
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
    build_character_statistics_for_event(store, 0, char_id)
}

pub fn build_character_statistics_for_event(store: &PvpFilamentStore, event_id: i64, char_id: i64) -> PyRep {
    let stats_map = get_stats_source(store, event_id);
    let (w, l, s, k, d) = stats_map.get(&char_id).copied().unwrap_or((0, 0, 0, 0, 0.0));
    let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = stats_map
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

        let mut matched_event_id = store.events.last().map(|e| e.event_id).unwrap_or(171);
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

        let ev_stats = store.event_stats.entry(matched_event_id).or_default();
        let ew = ev_stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        ew.0 += 1;
        ew.2 += 100;
        ew.3 += 1;
        ew.4 += damage_done;
        let el = ev_stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        el.1 += 1;
        el.2 += 25;

        save_pvp_store(snap, &store);

        let w_stats = build_character_statistics_for_event(&store, matched_event_id, winner_char_id);
        let l_stats = build_character_statistics_for_event(&store, matched_event_id, loser_char_id);
        let lb_rows = build_leaderboard_rows_for_event(&store, matched_event_id);

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

/// Kill-pipeline hook (`P1-PG-RESULT-OUTSIDE-ARENA`): records a Proving Grounds win/loss ONLY when
/// the kill happened in the arena system of an active (not yet concluded) match that has the attacker
/// and victim on opposite teams. Pilots who are merely queued (still in their origin systems) never
/// produce a result from a normal-space kill.
pub fn record_proving_grounds_combat_result_if_queued(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_id: i64,
    kill_solar_system_id: u32,
    damage_done: f64,
) {
    if attacker_char_id == victim_char_id || attacker_char_id <= 0 || victim_char_id <= 0 {
        return;
    }
    let in_arena_match = ctx.db.with_snapshot(|snap| {
        load_pvp_store(snap).active_matches.values().any(|m| {
            m.active
                && m.arena_solar_system_id == kill_solar_system_id
                && ((m.team_a.contains(&attacker_char_id) && m.team_b.contains(&victim_char_id))
                    || (m.team_b.contains(&attacker_char_id) && m.team_a.contains(&victim_char_id)))
        })
    });
    if in_arena_match {
        let _ = record_proving_grounds_combat_result(ctx, attacker_char_id, victim_char_id, damage_done);
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
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
            let cur_sys_for_check = session.solar_system_id.unwrap_or(30_000_142);
            let cur_ship_for_check = session.ship_id.unwrap_or(0);
            if let Err(e) = crate::sp6_lifecycle::require_ship_ready_for_filament_jump(
                ctx,
                char_id,
                cur_ship_for_check,
                cur_sys_for_check,
            ) {
                return Some(Err(e));
            }
            let caller_origin_pos = ctx
                .systems
                .get_or_spawn(cur_sys_for_check)
                .get_ball_position_sync(cur_ship_for_check)
                .map(|p| [p.x, p.y, p.z])
                .or_else(|| {
                    ctx.db
                        .with_snapshot(|s| s.item_positions.get(&cur_ship_for_check).copied())
                })
                .unwrap_or([0.0, 0.0, 0.0]);
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
                if session.station_id.is_some() || session.structure_id.is_some() {
                    return Err(BridgeRpcError::user_error("MustBeInSpace"));
                }
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
                ensure_events_from_sde(&ctx.sde, &mut store);
                let active_event = store
                    .events
                    .iter()
                    .find(|ev| ev.start_unix <= now_i64 && now_i64 <= ev.end_unix && ev.type_ids.contains(&i64::from(it.type_id)));
                let in_active_event = active_event.is_some();
                let any_event_currently_active = store
                    .events
                    .iter()
                    .any(|ev| ev.start_unix <= now_i64 && now_i64 <= ev.end_unix);
                let active_event_id = active_event
                    .map(|e| e.event_id)
                    .or_else(|| {
                        store
                            .events
                            .iter()
                            .filter(|ev| ev.type_ids.contains(&i64::from(it.type_id)))
                            .max_by_key(|ev| (ev.end_unix, ev.event_id))
                            .map(|ev| ev.event_id)
                    })
                    .unwrap_or(171);

                let in_inactive_only = any_event_currently_active
                    && store
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

                // Matchmaking: check if another pilot is already queued in space for this filament type
                let matched_opponent = store
                    .queue
                    .iter()
                    .find(|(&cid, qe)| {
                        if cid == char_id || qe.type_id != it_type_id {
                            return false;
                        }
                        let Some(opp_ch) = snap.characters.get(&cid) else {
                            return false;
                        };
                        if opp_ch.station_id.is_some()
                            || opp_ch.structure_id.is_some()
                            || opp_ch.active_ship_id <= 0
                        {
                            return false;
                        }
                        let opp_sys_id = if qe.solar_system_id > 0 {
                            qe.solar_system_id
                        } else {
                            opp_ch.solar_system_id
                        };
                        crate::sp6_lifecycle::require_ship_ready_for_filament_jump(
                            ctx,
                            cid,
                            opp_ch.active_ship_id,
                            opp_sys_id,
                        )
                        .is_ok()
                    })
                    .map(|(&cid, qe)| (cid, qe.solar_system_id, qe.location_id));

                store.origin_positions.insert(char_id, caller_origin_pos);
                let match_data = if let Some((opp_id, opp_sys, opp_loc)) = matched_opponent {
                    store.queue.remove(&opp_id);
                    store.next_match_id += 1;
                    let mid = store.next_match_id;
                    let cur_sys = session.solar_system_id.unwrap_or(30000142);
                    let opp_origin_sys = if opp_sys > 0 { opp_sys } else { 30000142 };
                    let mut origin_systems = HashMap::new();
                    origin_systems.insert(opp_id, opp_origin_sys);
                    origin_systems.insert(char_id, cur_sys);

                    let match_inst = PvpArenaMatchInstance {
                        match_id: mid,
                        event_id: active_event_id,
                        arena_solar_system_id: 32000001,
                        team_a: vec![opp_id],
                        team_b: vec![char_id],
                        origin_systems,
                        started_unix: now_unix,
                        active: true,
                        winner_char_id: None,
                        exited_pilots: HashSet::new(),
                        end_gate_id: 70001,
                    };
                    store.active_matches.insert(mid, match_inst);

                    let opp_ship_id = snap.characters.get(&opp_id).map(|c| c.active_ship_id).filter(|&s| s > 0).unwrap_or(opp_loc);
                    if let Some(c) = snap.characters.get_mut(&opp_id) {
                        c.solar_system_id = 32000001;
                    }
                    if let Some(c) = snap.characters.get_mut(&char_id) {
                        c.solar_system_id = 32000001;
                    }

                    Some((opp_id, opp_ship_id, opp_origin_sys, cur_sys, active_ship_id))
                } else {
                    store.queue.insert(char_id, PvpQueueCharacterEntry {
                        item_id,
                        type_id: it_type_id,
                        location_id: it_location_id,
                        flag: it_flag,
                        solar_system_id: session.solar_system_id.unwrap_or(30000142),
                        joined_unix: now_unix,
                    });
                    None
                };

                store.stats.entry(char_id).or_insert((0, 0, 0, 0, 0.0));
                save_pvp_store(snap, &store);
                Ok(match_data)
            });

            match res {
                Ok(Some((opp_id, opp_ship_id, opp_sys, cur_sys, cur_ship_id))) => {
                    session.solar_system_id = Some(32000001);

                    // Update opponent session & emit OnSessionChanged for both pilots
                    let sess_rep_opp = PyRep::keyval([
                        ("charid", PyRep::Integer(opp_id)),
                        ("solarsystemid2", PyRep::Integer(32000001)),
                        ("solarsystemid", PyRep::Integer(32000001)),
                    ]);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(opp_id),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            sess_rep_opp,
                            PyRep::Dict(vec![(
                                PyRep::String("solarsystemid".into()),
                                PyRep::Tuple(vec![PyRep::Integer(opp_sys as i64), PyRep::Integer(32000001)]),
                            )]),
                        ],
                    );

                    let sess_rep_cur = PyRep::keyval([
                        ("charid", PyRep::Integer(char_id)),
                        ("solarsystemid2", PyRep::Integer(32000001)),
                        ("solarsystemid", PyRep::Integer(32000001)),
                    ]);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            sess_rep_cur,
                            PyRep::Dict(vec![(
                                PyRep::String("solarsystemid".into()),
                                PyRep::Tuple(vec![PyRep::Integer(cur_sys as i64), PyRep::Integer(32000001)]),
                            )]),
                        ],
                    );

                    // Transition out of origin systems
                    ctx.systems.get_or_spawn(opp_sys).try_remove_ball(opp_ship_id);
                    ctx.systems.get_or_spawn(cur_sys).try_remove_ball(cur_ship_id);

                    // Spawn into arena Ballpark (32000001 AD001) with SDE physics
                    let arena = ctx.systems.get_or_spawn(32000001);
                    if arena.get_ball_snapshot(70001).await.ok().flatten().is_none() {
                        let gate_ball = Ball {
                            id: 70001,
                            type_id: 47459,
                            position: DVec3::ZERO,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: 10_000_000.0,
                            radius: 500.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        };
                        let _ = arena.spawn_ball_with_combat(gate_ball, Default::default()).await;
                    }
                    let opp_ship_type = ctx.db.with_snapshot(|s| s.items.get(&opp_ship_id).map(|i| i.type_id).unwrap_or(621));
                    let cur_ship_type = ctx.db.with_snapshot(|s| s.items.get(&cur_ship_id).map(|i| i.type_id).unwrap_or(621));

                    let get_ball_physics = |ship_type_id: u32| -> (f64, f64, f64, f64) {
                        ctx.sde.types.get(&ship_type_id).map(|st| {
                            let mv = st.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(250.0);
                            let ag = st.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied().unwrap_or(0.5);
                            (st.mass, st.radius, ag, mv)
                        }).unwrap_or((1_000_000.0, 50.0, 0.5, 250.0))
                    };

                    let (opp_mass, opp_radius, opp_inertia, opp_max_vel) = get_ball_physics(opp_ship_type);
                    let cs_opp = ctx.build_ship_combat_state(opp_id, opp_ship_id, opp_ship_type);
                    let ball_opp = Ball {
                        id: opp_ship_id,
                        type_id: opp_ship_type,
                        position: DVec3::new(-10_000.0, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: opp_max_vel,
                        inertia: opp_inertia,
                        mass: opp_mass,
                        radius: opp_radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    };
                    let _ = arena.spawn_ball_with_combat(ball_opp, cs_opp).await;

                    let (cur_mass, cur_radius, cur_inertia, cur_max_vel) = get_ball_physics(cur_ship_type);
                    let cs_cur = ctx.build_ship_combat_state(char_id, cur_ship_id, cur_ship_type);
                    let ball_cur = Ball {
                        id: cur_ship_id,
                        type_id: cur_ship_type,
                        position: DVec3::new(10_000.0, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: cur_max_vel,
                        inertia: cur_inertia,
                        mass: cur_mass,
                        radius: cur_radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    };
                    let _ = arena.spawn_ball_with_combat(ball_cur, cs_cur).await;

                    Some(Ok(PyRep::Integer(5)))
                }
                Ok(None) => Some(Ok(PyRep::Integer(30))),
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
                    .max_by_key(|e| (e.end_unix, e.start_unix, e.event_id))
                    .or_else(|| store.events.iter().max_by_key(|e| (e.end_unix, e.event_id)))
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
            let event_id = args.first().and_then(|a| a.as_i64()).filter(|&e| e > 0).unwrap_or(171);
            let entries = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                build_leaderboard_rows_for_event(&store, event_id)
            });
            let lb_notif = PyRep::keyval([
                ("eventID", PyRep::Integer(event_id)),
                ("event_id", PyRep::Integer(event_id)),
                ("entries", PyRep::List(entries.clone())),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnPVPFilamentsLeaderboard",
                "charid",
                vec![lb_notif],
            );
            Some(Ok(PyRep::List(entries)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let event_id = args.first().and_then(|a| a.as_i64()).filter(|&e| e > 0).unwrap_or(171);
            let target_cid = args.get(1).and_then(|a| a.as_i64()).filter(|&c| c > 0).unwrap_or(char_id);
            let stats = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                build_character_statistics_for_event(&store, event_id, target_cid)
            });
            let notif = PyRep::keyval([
                ("eventID", PyRep::Integer(event_id)),
                ("event_id", PyRep::Integer(event_id)),
                ("statistics", stats.clone()),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnPVPFilamentsCharacterStatistics",
                "charid",
                vec![notif],
            );
            Some(Ok(stats))
        }

        ("pvpFilamentMgr", "AbyssalPVPEndGateActivation") => {
            let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let (is_in_arena, active_ship_id) = ctx.db.with_snapshot(|snap| {
                let c = snap.characters.get(&char_id);
                let sys = session.solar_system_id.or_else(|| c.map(|c| c.solar_system_id)).unwrap_or(0);
                let ship = session.ship_id.or_else(|| c.map(|c| c.active_ship_id)).filter(|&s| s > 0);
                (sys == 32000001, ship)
            });

            if !is_in_arena {
                return Some(Err(BridgeRpcError::user_error("NotInArena")));
            }

            let match_precheck = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                for m in store.active_matches.values() {
                    if m.arena_solar_system_id == 32000001
                        && (m.team_a.contains(&char_id) || m.team_b.contains(&char_id))
                        && !m.exited_pilots.contains(&char_id)
                    {
                        return Some((m.match_id, m.end_gate_id, m.winner_char_id));
                    }
                }
                None
            });

            let Some((mid, end_gate_id, winner_char_id)) = match_precheck else {
                return Some(Err(BridgeRpcError::user_error("NoActiveMatch")));
            };

            if winner_char_id.is_none() {
                return Some(Err(BridgeRpcError::user_error("MatchNotConcluded")));
            }

            let target_gate_id = if end_gate_id > 0 { end_gate_id } else { 70001 };
            if gate_id > 0 && gate_id != target_gate_id {
                return Some(Err(BridgeRpcError::user_error("TooFarFromGate")));
            }

            let arena = ctx.systems.get_or_spawn(32000001);
            let Some((_, gb)) = arena.get_ball_snapshot(target_gate_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError::user_error("TooFarFromGate")));
            };
            let Some(sid) = active_ship_id else {
                return Some(Err(BridgeRpcError::user_error("TooFarFromGate")));
            };
            let Some((_, sb)) = arena.get_ball_snapshot(sid).await.ok().flatten() else {
                return Some(Err(BridgeRpcError::user_error("TooFarFromGate")));
            };
            if sb.position.distance(gb.position) > 50_000.0 {
                return Some(Err(BridgeRpcError::user_error("TooFarFromGate")));
            }

            let match_info = ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                store.queue.remove(&char_id);
                let mut matched_entry = None;
                let opos = store
                    .origin_positions
                    .get(&char_id)
                    .copied()
                    .unwrap_or([0.0, 0.0, 0.0]);
                for m in store.active_matches.values_mut() {
                    if m.match_id == mid && !m.exited_pilots.contains(&char_id) {
                        let osys = m.origin_systems.get(&char_id).copied().unwrap_or(30000142);
                        m.exited_pilots.insert(char_id);
                        let all_exited = m.team_a.iter().chain(m.team_b.iter()).all(|cid| m.exited_pilots.contains(cid));
                        if all_exited {
                            m.active = false;
                        }
                        matched_entry = Some((m.match_id, osys, opos));
                        break;
                    }
                }
                let Some((mid, osys, opos)) = matched_entry else {
                    save_pvp_store(snap, &store);
                    return None;
                };
                let active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).filter(|&s| s > 0);
                if let Some(c) = snap.characters.get_mut(&char_id) {
                    c.solar_system_id = osys;
                }
                if let Some(sid) = active_ship_id {
                    snap.item_positions.insert(sid, opos);
                }
                save_pvp_store(snap, &store);
                Some((mid, osys, opos, active_ship_id))
            });

            let Some((_mid, osys, opos, active_ship_id)) = match_info else {
                return Some(Err(BridgeRpcError::user_error("NoActiveMatch")));
            };

            session.solar_system_id = Some(osys);
            let cur_ship = active_ship_id.or(session.ship_id).unwrap_or(char_id);
            ctx.systems.get_or_spawn(32000001).try_remove_ball(cur_ship);

            let origin_sys_handle = ctx.systems.get_or_spawn(osys);
            let ship_type = ctx.db.with_snapshot(|s| s.items.get(&cur_ship).map(|i| i.type_id).unwrap_or(621));
            let (mass, radius, inertia, max_vel) = ctx.sde.types.get(&ship_type).map(|st| {
                let mv = st.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(250.0);
                let ag = st.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied().unwrap_or(0.5);
                (st.mass, st.radius, ag, mv)
            }).unwrap_or((1_000_000.0, 50.0, 0.5, 250.0));
            let cs = ctx.build_ship_combat_state(char_id, cur_ship, ship_type);
            let ball = Ball {
                id: cur_ship,
                type_id: ship_type,
                position: DVec3::from_array(opos),
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let _ = origin_sys_handle.spawn_ball_with_combat(ball, cs).await;

            let sess_rep = PyRep::keyval([
                ("charid", PyRep::Integer(char_id)),
                ("solarsystemid2", PyRep::Integer(osys as i64)),
                ("solarsystemid", PyRep::Integer(osys as i64)),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSessionChanged",
                "charid",
                vec![
                    PyRep::Bool(false),
                    sess_rep,
                    PyRep::Dict(vec![(
                        PyRep::String("solarsystemid".into()),
                        PyRep::Tuple(vec![PyRep::Integer(32000001), PyRep::Integer(osys as i64)]),
                    )]),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("gateID", PyRep::Integer(target_gate_id)),
                ("activated", PyRep::Bool(true)),
                ("originSystemID", PyRep::Integer(osys as i64)),
            ])))
        }

        _ => None,
    }
}
