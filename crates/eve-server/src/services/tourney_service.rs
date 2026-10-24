//! `tourney_service` — Tournament Manager (`tourneyMgr`).
//!
//! Handles Alliance Tournament and Arena matchmaking, pre-game pilot & ship validation,
//! ship bans, member list locks, countdown orchestration, referee overrides, and telemetry.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

pub const TOURNEY_STORE_KEY: i64 = -10_000_055;

/// State of a tournament match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TourneyMatchState {
    Setup,
    Banning,
    Countdown,
    InProgress,
    Completed,
}

impl TourneyMatchState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Banning => "Banning",
            Self::Countdown => "Countdown",
            Self::InProgress => "InProgress",
            Self::Completed => "Completed",
        }
    }
}

/// A pilot in a tournament team roster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TourneyPilot {
    pub character_id: i64,
    pub ship_type_id: i64,
    pub ship_id: i64,
    pub points: i64,
}

/// Representation of an active or configured tournament match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourneyMatch {
    pub match_id: i64,
    pub tourney_id: i64,
    pub solar_system_id: i64,
    pub state: TourneyMatchState,
    pub red_team_id: i64,
    pub red_team_name: String,
    pub blue_team_id: i64,
    pub blue_team_name: String,
    pub red_players: Vec<TourneyPilot>,
    pub blue_players: Vec<TourneyPilot>,
    pub banned_ship_types_red: Vec<i64>,
    pub banned_ship_types_blue: Vec<i64>,
    pub banned_ships: Vec<i64>,
    pub bans_finalized: bool,
    pub member_list_locked_red: bool,
    pub member_list_locked_blue: bool,
    pub countdown_active: bool,
    pub countdown_start_unix: u64,
    pub countdown_seconds: i64,
    pub max_points: i64,
    pub match_time: i64,
    pub overtime: i64,
    pub external_ruleset_id: i64,
    pub red_score: i64,
    pub blue_score: i64,
    pub red_fleet_boss_id: Option<i64>,
    pub blue_fleet_boss_id: Option<i64>,
    pub referees: Vec<i64>,
    pub beacon_ids: Vec<i64>,
    pub pregame_checks_complete: bool,
    pub grid_cleaned: bool,
    pub autobanning_active: bool,
    pub autoban_qty: i64,
    pub drone_restrict: bool,
    pub lock_restrict: bool,
    pub move_restrict: bool,
    pub warp_restrict: bool,
    pub pilot_errors: HashMap<i64, Vec<String>>,
    pub teleported_teams: Vec<i64>,
    pub returned_teams: Vec<i64>,
    pub fleet_details_updated: bool,
}

impl TourneyMatch {
    pub fn new(match_id: i64, tourney_id: i64, solar_system_id: i64, max_points: i64, match_time: i64, overtime: i64, external_ruleset_id: i64) -> Self {
        Self {
            match_id, tourney_id, solar_system_id,
            state: TourneyMatchState::Setup,
            red_team_id: 1001, red_team_name: "Red Alliance".into(),
            blue_team_id: 1002, blue_team_name: "Blue Alliance".into(),
            red_players: Vec::new(), blue_players: Vec::new(),
            banned_ship_types_red: Vec::new(), banned_ship_types_blue: Vec::new(),
            banned_ships: Vec::new(), bans_finalized: false,
            member_list_locked_red: false, member_list_locked_blue: false,
            countdown_active: false, countdown_start_unix: 0, countdown_seconds: 60,
            max_points, match_time, overtime, external_ruleset_id,
            red_score: 100, blue_score: 100,
            red_fleet_boss_id: None, blue_fleet_boss_id: None,
            referees: Vec::new(), beacon_ids: Vec::new(),
            pregame_checks_complete: false, grid_cleaned: false,
            autobanning_active: false, autoban_qty: 0,
            drone_restrict: false, lock_restrict: false, move_restrict: false, warp_restrict: false,
            pilot_errors: HashMap::new(), teleported_teams: Vec::new(), returned_teams: Vec::new(),
            fleet_details_updated: false,
        }
    }
}

/// Persistent tournament data store stored in `WorldStateSnapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TourneyStore {
    pub matches: HashMap<i64, TourneyMatch>,
    pub active_match_id: Option<i64>,
    pub ban_sessions: HashMap<i64, Vec<i64>>,
}

impl TourneyStore {
    pub fn resolve_match_mut(&mut self, target_id: Option<i64>) -> Result<&mut TourneyMatch, BridgeRpcError> {
        let mid = match target_id {
            Some(id) if id > 0 => id,
            _ => self.active_match_id.ok_or_else(|| BridgeRpcError {
                code: "MatchNotFound".into(),
                message: "No active tournament match found".into(),
            })?,
        };
        self.matches.get_mut(&mid).ok_or_else(|| BridgeRpcError {
            code: "MatchNotFound".into(),
            message: format!("Match ID {mid} not found"),
        })
    }

    pub fn resolve_match(&self, target_id: Option<i64>) -> Option<&TourneyMatch> {
        let mid = match target_id {
            Some(id) if id > 0 => id,
            _ => self.active_match_id.unwrap_or(1),
        };
        self.matches.get(&mid)
    }
}

pub fn load_store(snap: &eve_db::WorldStateSnapshot) -> TourneyStore {
    snap.owner_notes.get(&TOURNEY_STORE_KEY).and_then(|n| serde_json::from_str(&n.note).ok()).unwrap_or_default()
}

pub fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &TourneyStore, now_unix: u64) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.owner_notes.insert(
            TOURNEY_STORE_KEY,
            eve_db::OwnerNoteRecord {
                note_id: TOURNEY_STORE_KEY,
                character_id: -1,
                item_id: 0,
                label: "tourney_store".into(),
                note: json,
                timestamp_unix: now_unix,
            },
        );
    }
}

fn extract_id(args: &[PyRep], idx: usize) -> Option<i64> {
    args.get(idx).and_then(|a| a.as_i64())
}

fn extract_bool(args: &[PyRep], idx: usize) -> bool {
    args.get(idx).and_then(|a| match a {
        PyRep::Bool(b) => Some(*b),
        PyRep::Integer(i) => Some(*i != 0),
        _ => None,
    }).unwrap_or(false)
}

fn extract_list_i64(rep: Option<&PyRep>) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) = rep {
        for it in items {
            if let Some(v) = it.as_i64() { out.push(v); }
        }
    } else if let Some(PyRep::Integer(v)) = rep {
        out.push(*v);
    }
    out
}

/// Verifies that the caller has GM role or is registered in the tournament match referee roster.
fn check_referee_auth(session: &SessionState, caller_char_id: i64, m: &TourneyMatch) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() || m.referees.contains(&caller_char_id) {
        Ok(())
    } else {
        Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Caller is not an authorized tournament referee or GM".into(),
        })
    }
}

/// Computes Alliance Tournament tier point costs based on ship hull classification.
fn compute_ship_points(ctx: &ServerContext, ship_type_id: i64) -> i64 {
    match ctx.sde.types.get(&(ship_type_id as u32)).map(|t| t.group_id) {
        Some(27) => 19, // Battleship
        Some(540) => 16, // Command Ship
        Some(358) | Some(832) | Some(833) | Some(906) => 14, // HAC / Logi / Recons
        Some(419) | Some(1201) | Some(1202) => 13, // Battlecruiser
        Some(26) | Some(894) => 9, // Cruiser / HIC
        Some(1305) => 6, // Tactical Destroyer
        Some(324) | Some(420) => 4, // Assault Frigate / Destroyer
        Some(830) | Some(831) | Some(834) | Some(893) => 3, // Covert Ops / Inty / Bomber / EAF
        Some(25) => 2, // Frigate
        _ => 10,
    }
}

/// Computes team spectator telemetry dynamically from active team member ship fits and stats.
fn compute_team_telemetry(ctx: &ServerContext, players: &[TourneyPilot], score: i64, bans_count: usize) -> PyRep {
    let mut total_ehp: i64 = 0;
    let mut total_dps: i64 = 0;
    let mut total_rep: i64 = 0;
    let mut total_cont: i64 = 0;

    for p in players {
        let (ehp, dps, rep, cont) = if let Some(t) = ctx.sde.types.get(&(p.ship_type_id as u32)) {
            let struct_hp = t.default_attributes.get(&9).copied().unwrap_or(500.0);
            let armor_hp = t.default_attributes.get(&265).copied().unwrap_or(500.0);
            let shield_hp = t.default_attributes.get(&263).copied().unwrap_or(500.0);
            let raw_hp = struct_hp + armor_hp + shield_hp;
            let ship_ehp = (raw_hp * 2.0).round() as i64;

            let (ship_dps, ship_rep, ship_cont) = match t.group_id {
                27 => (850, 400, 100),
                419 | 1201 | 1202 => (600, 250, 80),
                26 | 358 | 832 | 833 | 894 | 906 => (400, 200, 150),
                420 | 1305 => (280, 100, 60),
                25 | 324 | 830 | 831 | 834 | 893 => (150, 50, 80),
                _ => (300, 100, 50),
            };
            (ship_ehp, ship_dps, ship_rep, ship_cont)
        } else {
            (25_000, 250, 100, 50)
        };
        total_ehp += ehp;
        total_dps += dps;
        total_rep += rep;
        total_cont += cont;
    }

    PyRep::Tuple(vec![
        PyRep::Integer(total_ehp),
        PyRep::Integer(total_dps),
        PyRep::Integer(total_rep),
        PyRep::Integer(total_cont),
        PyRep::Integer(score),
        PyRep::Integer(bans_count as i64),
    ])
}

fn resolve_pilot(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, char_id: i64) -> TourneyPilot {
    let (ship_id, ship_type_id) = snap
        .characters
        .get(&char_id)
        .and_then(|ch| (ch.active_ship_id > 0).then_some(ch.active_ship_id))
        .and_then(|sid| snap.items.get(&sid).map(|it| (sid, i64::from(it.type_id))))
        .unwrap_or((0, 0));
    let points = if ship_type_id > 0 { compute_ship_points(ctx, ship_type_id) } else { 0 };
    TourneyPilot { character_id: char_id, ship_type_id, ship_id, points }
}

macro_rules! require_cid {
    ($session:expr) => {
        match ServerContext::require_character($session) {
            Ok((_, cid)) => cid,
            Err(e) => return Some(Err(e)),
        }
    };
}

pub async fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "tourneyMgr" {
        return None;
    }

    let solarsystem_id = session.solar_system_id.map(|s| s as i64).unwrap_or(30000142);
    let now_unix = ctx.now_unix();

    let res = match (service, method) {
        ("tourneyMgr", "AbortCountdown") => {
            let caller_cid = require_cid!(session);
            let target_match_id = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_match_id)?;
                check_referee_auth(session, caller_cid, m)?;
                m.countdown_active = false;
                if m.state == TourneyMatchState::Countdown {
                    m.state = TourneyMatchState::Setup;
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "AddPlayer") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team, char_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                let is_locked = if which_team == 0 { m.member_list_locked_red } else { m.member_list_locked_blue };
                if is_locked {
                    return Err(BridgeRpcError {
                        code: "MemberListLocked".into(),
                        message: "Cannot modify roster while team member list is locked".into(),
                    });
                }
                let pilot = resolve_pilot(ctx, snap, char_id);
                if which_team == 0 {
                    if !m.red_players.iter().any(|p| p.character_id == char_id) {
                        m.red_players.push(pilot);
                    }
                } else if !m.blue_players.iter().any(|p| p.character_id == char_id) {
                    m.blue_players.push(pilot);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "BanShip") => {
            let caller_cid = require_cid!(session);
            let ban_id = extract_id(args, 0).unwrap_or(1);
            let ban_types = extract_list_i64(args.get(1));

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(None)?;
                check_referee_auth(session, caller_cid, m)?;
                for bt in &ban_types {
                    if !m.banned_ships.contains(bt) {
                        m.banned_ships.push(*bt);
                    }
                }
                store.ban_sessions.insert(ban_id, ban_types.clone());
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "BanShipType") => {
            let caller_cid = require_cid!(session);
            let (target_mid, team_idx, ship_type_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if team_idx == 0 {
                    if !m.banned_ship_types_red.contains(&ship_type_id) {
                        m.banned_ship_types_red.push(ship_type_id);
                    }
                } else if !m.banned_ship_types_blue.contains(&ship_type_id) {
                    m.banned_ship_types_blue.push(ship_type_id);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "CleanupGrid") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut removed_item_ids = Vec::new();
            let mut match_sys_id = solarsystem_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.grid_cleaned = true;
                match_sys_id = m.solar_system_id;

                let sys_items: Vec<i64> = snap.items.values().filter(|it| {
                    if it.location_id == match_sys_id {
                        if let Some(t) = ctx.sde.types.get(&it.type_id) {
                            matches!(t.group_id, 12 | 186 | 340 | 448 | 649)
                        } else {
                            it.type_id == 26557
                        }
                    } else {
                        false
                    }
                }).map(|it| it.item_id).collect();

                for &iid in &sys_items {
                    snap.items.remove(&iid);
                    removed_item_ids.push(iid);
                }
                let child_ids: Vec<i64> = snap.items.values().filter(|c| removed_item_ids.contains(&c.location_id)).map(|c| c.item_id).collect();
                for cid in child_ids {
                    snap.items.remove(&cid);
                }

                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() && !removed_item_ids.is_empty() {
                let sys_handle = ctx.systems.get_or_spawn(match_sys_id as u32);
                for &iid in &removed_item_ids {
                    let _ = sys_handle.remove_ball(iid).await;
                }
            }
            res
        }
        ("tourneyMgr", "CompletePregameChecks") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.pregame_checks_complete = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "CreateMatch") => {
            let caller_cid = require_cid!(session);
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "Creating a tournament match requires GM administrator privileges".into(),
                }));
            }
            let tourney_id = extract_id(args, 0).unwrap_or(1);
            let match_id = extract_id(args, 1).unwrap_or(1001);
            let max_points = extract_id(args, 2).unwrap_or(100);
            let match_time = extract_id(args, 3).unwrap_or(600);
            let overtime = extract_id(args, 4).unwrap_or(0);
            let external_ruleset_id = extract_id(args, 5).unwrap_or(0);

            let (red_team, blue_team) = if let Some(PyRep::List(t_list)) = kwargs.get("teams").or_else(|| args.get(6)) {
                (t_list.first().and_then(|v| v.as_i64()).unwrap_or(1001), t_list.get(1).and_then(|v| v.as_i64()).unwrap_or(1002))
            } else {
                let r = kwargs.get("teamOne").or_else(|| kwargs.get("redTeamID")).and_then(|v| v.as_i64()).unwrap_or(1001);
                let b = kwargs.get("teamTwo").or_else(|| kwargs.get("blueTeamID")).and_then(|v| v.as_i64()).unwrap_or(1002);
                (r, b)
            };

            let (red_capt, blue_capt) = if let Some(PyRep::List(c_list)) = kwargs.get("captains").or_else(|| args.get(7)) {
                (c_list.first().and_then(|v| v.as_i64()), c_list.get(1).and_then(|v| v.as_i64()))
            } else {
                let r = kwargs.get("captainOne").or_else(|| kwargs.get("redCaptainID")).and_then(|v| v.as_i64());
                let b = kwargs.get("captainTwo").or_else(|| kwargs.get("blueCaptainID")).and_then(|v| v.as_i64());
                (r, b)
            };

            let match_sys_id = kwargs.get("solarSystemID")
                .or_else(|| kwargs.get("systemID"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(8).and_then(|v| v.as_i64()))
                .unwrap_or(solarsystem_id);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                if store.matches.contains_key(&match_id) {
                    return Err(BridgeRpcError {
                        code: "DuplicateMatchID".into(),
                        message: format!("Match ID {match_id} already exists"),
                    });
                }
                let mut m = TourneyMatch::new(match_id, tourney_id, match_sys_id, max_points, match_time, overtime, external_ruleset_id);
                m.red_team_id = red_team;
                m.blue_team_id = blue_team;
                m.red_fleet_boss_id = red_capt;
                m.blue_fleet_boss_id = blue_capt;
                if let Some(rc) = red_capt {
                    m.red_players.push(resolve_pilot(ctx, snap, rc));
                }
                if let Some(bc) = blue_capt {
                    m.blue_players.push(resolve_pilot(ctx, snap, bc));
                }
                if caller_cid > 0 && !m.referees.contains(&caller_cid) {
                    m.referees.push(caller_cid);
                }
                snap.next_entity_id += 1;
                let b1 = snap.next_entity_id;
                snap.next_entity_id += 1;
                let b2 = snap.next_entity_id;
                m.beacon_ids = vec![b1, b2];

                store.active_match_id = Some(match_id);
                let beacon_reps: Vec<PyRep> = m.beacon_ids.iter().copied().map(PyRep::Integer).collect();
                let captain_reps: Vec<PyRep> = vec![
                    PyRep::Integer(m.red_fleet_boss_id.unwrap_or(0)),
                    PyRep::Integer(m.blue_fleet_boss_id.unwrap_or(0)),
                ];
                store.matches.insert(match_id, m);
                save_store(snap, &store, now_unix);

                Ok((PyRep::Tuple(vec![PyRep::Integer(match_id), PyRep::List(beacon_reps), PyRep::List(captain_reps)]), b1, b2))
            });

            if let Ok((rep, b1, b2)) = res {
                let sys_handle = ctx.systems.get_or_spawn(match_sys_id as u32);
                let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
                bp.add_static_ball_with_meta(
                    eve_destiny::Ball { id: b1, type_id: 2977, position: glam::DVec3::new(-50_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 100.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                    eve_destiny::SlimItemMeta { group_id: 310, category_id: 2, owner_id: 1, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
                );
                bp.add_static_ball_with_meta(
                    eve_destiny::Ball { id: b2, type_id: 2977, position: glam::DVec3::new(50_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 100.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                    eve_destiny::SlimItemMeta { group_id: 310, category_id: 2, owner_id: 1, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
                );
                drop(bp);
                Ok(rep)
            } else {
                Err(res.unwrap_err())
            }
        }
        ("tourneyMgr", "ExecuteMemberChange") => {
            let caller_cid = require_cid!(session);
            if args.is_empty() {
                Ok(PyRep::None)
            } else {
                let (target_mid, which_team, char_id, is_add) = if args.len() >= 4 {
                    (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0), extract_bool(args, 3))
                } else if args.len() == 3 {
                    (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0), true)
                } else if args.len() == 2 {
                    (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0), true)
                } else {
                    (None, 0, 0, false)
                };

                ctx.db.try_with_mut_snapshot(|snap| {
                    let mut store = load_store(snap);
                    let m = store.resolve_match_mut(target_mid)?;
                    check_referee_auth(session, caller_cid, m)?;
                    let is_locked = if which_team == 0 { m.member_list_locked_red } else { m.member_list_locked_blue };
                    if is_locked {
                        return Err(BridgeRpcError {
                            code: "MemberListLocked".into(),
                            message: "Cannot modify roster while team member list is locked".into(),
                        });
                    }

                    if char_id > 0 {
                        let pilot = resolve_pilot(ctx, snap, char_id);
                        let roster = if which_team == 0 { &mut m.red_players } else { &mut m.blue_players };
                        if is_add {
                            if !roster.iter().any(|p| p.character_id == char_id) {
                                roster.push(pilot);
                            }
                        } else {
                            roster.retain(|p| p.character_id != char_id);
                        }
                    }
                    save_store(snap, &store, now_unix);
                    Ok(PyRep::None)
                })
            }
        }
        ("tourneyMgr", "FinalizeBans") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.bans_finalized = true;
                if m.state == TourneyMatchState::Banning {
                    m.state = TourneyMatchState::Setup;
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "GetActiveMatchForCamera") => {
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let m_opt = store.matches.values().find(|m| m.solar_system_id == solarsystem_id && m.state != TourneyMatchState::Completed).or_else(|| store.resolve_match(None));
                if let Some(m) = m_opt {
                    PyRep::Dict(vec![
                        (PyRep::String("matchID".into()), PyRep::Integer(m.match_id)),
                        (PyRep::String("teamOneName".into()), PyRep::String(m.red_team_name.clone())),
                        (PyRep::String("teamTwoName".into()), PyRep::String(m.blue_team_name.clone())),
                        (PyRep::String("nebulaID".into()), PyRep::Integer(100)),
                    ])
                } else {
                    PyRep::None
                }
            }))
        }
        ("tourneyMgr", "GetFancyDetails") => {
            let target_mid = extract_id(args, 0);
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let default_match = TourneyMatch::new(1, 1, solarsystem_id, 100, 600, 0, 0);
                let m = store.resolve_match(target_mid).unwrap_or(&default_match);

                let red_bar = compute_team_telemetry(ctx, &m.red_players, m.red_score, m.banned_ship_types_red.len());
                let blue_bar = compute_team_telemetry(ctx, &m.blue_players, m.blue_score, m.banned_ship_types_blue.len());

                let dict_fields = vec![
                    (PyRep::Integer(0), red_bar),
                    (PyRep::Integer(1), blue_bar),
                    (PyRep::String("matchID".into()), PyRep::Integer(m.match_id)),
                    (PyRep::String("scores".into()), PyRep::List(vec![PyRep::Integer(m.red_score), PyRep::Integer(m.blue_score)])),
                    (PyRep::String("bans".into()), PyRep::Dict(vec![
                        (PyRep::String("red".into()), PyRep::List(m.banned_ship_types_red.iter().copied().map(PyRep::Integer).collect())),
                        (PyRep::String("blue".into()), PyRep::List(m.banned_ship_types_blue.iter().copied().map(PyRep::Integer).collect())),
                    ])),
                    (PyRep::String("details".into()), PyRep::Dict(vec![
                        (PyRep::String("state".into()), PyRep::String(m.state.as_str().into())),
                        (PyRep::String("maxPoints".into()), PyRep::Integer(m.max_points)),
                        (PyRep::String("matchTime".into()), PyRep::Integer(m.match_time)),
                    ])),
                ];
                PyRep::Dict(dict_fields)
            }))
        }
        ("tourneyMgr", "GetMatchSystem") => {
            let target_mid = extract_id(args, 0);
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let sys_id = store.resolve_match(target_mid).map(|m| m.solar_system_id).unwrap_or(solarsystem_id);
                PyRep::Integer(sys_id)
            }))
        }
        ("tourneyMgr", "GetTeamMembers") => {
            let target_mid = extract_id(args, 0);
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let default_match = TourneyMatch::new(1, 1, solarsystem_id, 100, 600, 0, 0);
                let m = store.resolve_match(target_mid).unwrap_or(&default_match);

                let red_list: Vec<PyRep> = m.red_players.iter().map(|p| {
                    PyRep::Tuple(vec![PyRep::Integer(p.character_id), PyRep::Integer(p.ship_type_id), PyRep::Integer(p.ship_id), PyRep::Integer(p.points)])
                }).collect();
                let blue_list: Vec<PyRep> = m.blue_players.iter().map(|p| {
                    PyRep::Tuple(vec![PyRep::Integer(p.character_id), PyRep::Integer(p.ship_type_id), PyRep::Integer(p.ship_id), PyRep::Integer(p.points)])
                }).collect();

                PyRep::List(vec![PyRep::List(red_list), PyRep::List(blue_list)])
            }))
        }
        ("tourneyMgr", "IsOtherMatchInProgress") => {
            let target_mid = extract_id(args, 0).unwrap_or_default();
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let in_progress = store.matches.values().any(|m| {
                    m.match_id != target_mid && m.solar_system_id == solarsystem_id && (m.state == TourneyMatchState::InProgress || m.state == TourneyMatchState::Countdown)
                });
                PyRep::Bool(in_progress)
            }))
        }
        ("tourneyMgr", "LockMemberList") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team) = if args.len() >= 2 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if which_team == 0 { m.member_list_locked_red = true; } else { m.member_list_locked_blue = true; }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "MatchCompleted") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.state = TourneyMatchState::Completed;
                m.countdown_active = false;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideDroneRestrict") => {
            let caller_cid = require_cid!(session);
            let (target_mid, can_drone) = if args.len() >= 2 { (extract_id(args, 0), extract_bool(args, 1)) } else { (None, extract_bool(args, 0)) };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.drone_restrict = can_drone;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideError") => {
            let caller_cid = require_cid!(session);
            let (target_mid, char_id, err_str) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), args.get(2).and_then(|a| a.as_str()).unwrap_or(""))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), args.get(1).and_then(|a| a.as_str()).unwrap_or(""))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if let Some(errors) = m.pilot_errors.get_mut(&char_id) {
                    if err_str.is_empty() { errors.clear(); } else { errors.retain(|e| e != err_str); }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideFleetBossID") => {
            let caller_cid = require_cid!(session);
            let (target_mid, team_idx, new_boss) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if team_idx == 0 { m.red_fleet_boss_id = Some(new_boss); } else { m.blue_fleet_boss_id = Some(new_boss); }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideLockRestrict") => {
            let caller_cid = require_cid!(session);
            let (target_mid, can_lock) = if args.len() >= 2 { (extract_id(args, 0), extract_bool(args, 1)) } else { (None, extract_bool(args, 0)) };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.lock_restrict = can_lock;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideMoveRestrict") => {
            let caller_cid = require_cid!(session);
            let (target_mid, can_move) = if args.len() >= 2 { (extract_id(args, 0), extract_bool(args, 1)) } else { (None, extract_bool(args, 0)) };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.move_restrict = can_move;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideWarpRestrict") => {
            let caller_cid = require_cid!(session);
            let (target_mid, can_warp) = if args.len() >= 2 { (extract_id(args, 0), extract_bool(args, 1)) } else { (None, extract_bool(args, 0)) };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.warp_restrict = can_warp;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "QueryShipList") => {
            let mut allowed: Vec<i64> = ctx.sde.types.values().filter(|t| {
                t.category_id == 6 && t.published && matches!(t.group_id, 25 | 26 | 324 | 358 | 419 | 420 | 540 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 1201 | 1202 | 1305)
            }).map(|t| t.type_id as i64).collect();
            allowed.sort_unstable();
            allowed.dedup();
            let ship_reps: Vec<PyRep> = allowed.into_iter().map(PyRep::Integer).collect();
            Ok(PyRep::Dict(vec![(PyRep::String("allowedShipList".into()), PyRep::List(ship_reps))]))
        }
        ("tourneyMgr", "RemovePlayer") => {
            let caller_cid = require_cid!(session);
            let (target_mid, team_idx, char_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if team_idx == 0 { m.red_players.retain(|p| p.character_id != char_id); } else { m.blue_players.retain(|p| p.character_id != char_id); }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "RemoveSelfFromReferees") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                if caller_cid > 0 { m.referees.retain(|&r| r != caller_cid); }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "ResetMatch") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.state = TourneyMatchState::Setup;
                m.banned_ship_types_red.clear();
                m.banned_ship_types_blue.clear();
                m.banned_ships.clear();
                m.bans_finalized = false;
                m.member_list_locked_red = false;
                m.member_list_locked_blue = false;
                m.countdown_active = false;
                m.countdown_start_unix = 0;
                m.pregame_checks_complete = false;
                m.drone_restrict = false;
                m.lock_restrict = false;
                m.move_restrict = false;
                m.warp_restrict = false;
                m.teleported_teams.clear();
                m.returned_teams.clear();
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "ReturnAllPlayers") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut pilots_to_return = Vec::new();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.returned_teams = vec![0, 1];
                pilots_to_return.extend(m.red_players.clone());
                pilots_to_return.extend(m.blue_players.clone());

                for p in &pilots_to_return {
                    if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                        let home_st_id = ch.home_station_id;
                        ch.station_id = Some(home_st_id);
                        if let Some(st) = ctx.sde.stations.get(&home_st_id) {
                            ch.solar_system_id = st.solar_system_id;
                        }
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                for p in &pilots_to_return {
                    let mut home_sys = solarsystem_id as u32;
                    let mut home_pos = DVec3::ZERO;
                    ctx.db.with_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get(&p.character_id) {
                            if let Some(st) = ctx.sde.stations.get(&ch.home_station_id) {
                                home_sys = st.solar_system_id;
                                home_pos = st.position;
                            }
                        }
                    });
                    let sys_handle = ctx.systems.get_or_spawn(home_sys);
                    let _ = sys_handle.set_ball_position(p.ship_id, home_pos).await;
                }
            }
            res
        }
        ("tourneyMgr", "ReturnTeamPlayers") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team) = if args.len() >= 2 { (extract_id(args, 0), extract_id(args, 1).unwrap_or(0)) } else { (None, extract_id(args, 0).unwrap_or(0)) };
            let mut pilots_to_return = Vec::new();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if !m.returned_teams.contains(&which_team) {
                    m.returned_teams.push(which_team);
                }
                pilots_to_return = if which_team == 0 { m.red_players.clone() } else { m.blue_players.clone() };

                for p in &pilots_to_return {
                    if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                        let home_st_id = ch.home_station_id;
                        ch.station_id = Some(home_st_id);
                        if let Some(st) = ctx.sde.stations.get(&home_st_id) {
                            ch.solar_system_id = st.solar_system_id;
                        }
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                for p in &pilots_to_return {
                    let mut home_sys = solarsystem_id as u32;
                    let mut home_pos = DVec3::ZERO;
                    ctx.db.with_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get(&p.character_id) {
                            if let Some(st) = ctx.sde.stations.get(&ch.home_station_id) {
                                home_sys = st.solar_system_id;
                                home_pos = st.position;
                            }
                        }
                    });
                    let sys_handle = ctx.systems.get_or_spawn(home_sys);
                    let _ = sys_handle.set_ball_position(p.ship_id, home_pos).await;
                }
            }
            res
        }
        ("tourneyMgr", "StartAutobanning") => {
            let caller_cid = require_cid!(session);
            let (target_mid, qty) = if args.len() >= 5 {
                (extract_id(args, 0), extract_id(args, 4).unwrap_or(3))
            } else if args.len() >= 2 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(3))
            } else {
                (extract_id(args, 0), 3)
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.autobanning_active = true;
                m.autoban_qty = qty;
                m.state = TourneyMatchState::Banning;

                let mut candidate_bans: Vec<i64> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.category_id == 6 && t.group_id == 26 && t.published)
                    .map(|t| i64::from(t.type_id))
                    .collect();
                candidate_bans.sort_unstable();
                for &ship_tid in &candidate_bans {
                    if (m.banned_ship_types_red.len() as i64) < qty && !m.banned_ship_types_red.contains(&ship_tid) {
                        m.banned_ship_types_red.push(ship_tid);
                    }
                }
                for &ship_tid in candidate_bans.iter().rev() {
                    if (m.banned_ship_types_blue.len() as i64) < qty && !m.banned_ship_types_blue.contains(&ship_tid) {
                        m.banned_ship_types_blue.push(ship_tid);
                    }
                }
                m.banned_ships = m.banned_ship_types_red.iter().chain(m.banned_ship_types_blue.iter()).copied().collect();
                m.banned_ships.sort_unstable();
                m.banned_ships.dedup();

                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "StartCountdown") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.countdown_active = true;
                m.countdown_start_unix = now_unix;
                m.state = TourneyMatchState::Countdown;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "TeleportPlayers") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team, beacon_idx) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            let mut pilots_to_move = Vec::new();
            let mut match_sys = solarsystem_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if !m.teleported_teams.contains(&which_team) {
                    m.teleported_teams.push(which_team);
                }
                match_sys = m.solar_system_id;
                pilots_to_move = if which_team == 0 { m.red_players.clone() } else { m.blue_players.clone() };

                for p in &pilots_to_move {
                    if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                        ch.solar_system_id = match_sys as u32;
                        ch.station_id = None;
                        ch.structure_id = None;
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                let base_pos = if beacon_idx == 0 { DVec3::new(-50_000.0, 0.0, 0.0) } else { DVec3::new(50_000.0, 0.0, 0.0) };
                let sys_handle = ctx.systems.get_or_spawn(match_sys as u32);
                for (idx, p) in pilots_to_move.iter().enumerate() {
                    let offset = DVec3::new((idx as f64) * 250.0, 0.0, (idx as f64) * 250.0);
                    let pos = base_pos + offset;
                    let _ = sys_handle.set_ball_position(p.ship_id, pos).await;
                }
            }
            res
        }
        ("tourneyMgr", "TeleportRefHome") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);

            let mut home_sys = solarsystem_id;
            let mut home_pos = DVec3::ZERO;
            let mut ref_ship_id = 0i64;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let store = load_store(snap);
                if let Some(m) = store.resolve_match(target_mid) {
                    check_referee_auth(session, caller_cid, m)?;
                } else if !session.has_gm_role() {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Caller is not an authorized tournament referee or GM".into(),
                    });
                }

                if let Some(ch) = snap.characters.get_mut(&caller_cid) {
                    let home_st_id = ch.home_station_id;
                    ch.station_id = Some(home_st_id);
                    ref_ship_id = ch.active_ship_id;
                    if let Some(st) = ctx.sde.stations.get(&home_st_id) {
                        ch.solar_system_id = st.solar_system_id;
                        home_sys = st.solar_system_id as i64;
                        home_pos = st.position;
                    }
                }
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() && ref_ship_id > 0 {
                let sys_handle = ctx.systems.get_or_spawn(home_sys as u32);
                let _ = sys_handle.set_ball_position(ref_ship_id, home_pos).await;
            }
            res
        }
        ("tourneyMgr", "UnBanShipType") => {
            let caller_cid = require_cid!(session);
            let (target_mid, team_idx, ship_type_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if team_idx == 0 {
                    m.banned_ship_types_red.retain(|&id| id != ship_type_id);
                } else {
                    m.banned_ship_types_blue.retain(|&id| id != ship_type_id);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "UnlockMemberList") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team) = if args.len() >= 2 { (extract_id(args, 0), extract_id(args, 1).unwrap_or(0)) } else { (None, extract_id(args, 0).unwrap_or(0)) };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if which_team == 0 { m.member_list_locked_red = false; } else { m.member_list_locked_blue = false; }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "UpdateFleetDetails") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.fleet_details_updated = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        _ => return None,
    };

    Some(res)
}

pub fn check_tourney_warp_restricted(
    ctx: &ServerContext,
    solarsystem_id: u32,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        for m in store.matches.values() {
            if m.solar_system_id == solarsystem_id as i64
                && matches!(m.state, TourneyMatchState::Countdown | TourneyMatchState::InProgress)
            {
                let is_participant = m.red_players.iter().any(|p| p.character_id == char_id)
                    || m.blue_players.iter().any(|p| p.character_id == char_id);
                if is_participant && !m.warp_restrict {
                    return Err(BridgeRpcError {
                        code: "WarpRestrictedInTourney".into(),
                        message: "Warping is restricted for tournament participants in arena".into(),
                    });
                }
            }
        }
        Ok(())
    })
}
