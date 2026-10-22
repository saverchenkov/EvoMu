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
    pub fn new(
        match_id: i64,
        tourney_id: i64,
        solar_system_id: i64,
        max_points: i64,
        match_time: i64,
        overtime: i64,
        external_ruleset_id: i64,
    ) -> Self {
        Self {
            match_id,
            tourney_id,
            solar_system_id,
            state: TourneyMatchState::Setup,
            red_team_id: 1001,
            red_team_name: "Red Alliance".into(),
            blue_team_id: 1002,
            blue_team_name: "Blue Alliance".into(),
            red_players: Vec::new(),
            blue_players: Vec::new(),
            banned_ship_types_red: Vec::new(),
            banned_ship_types_blue: Vec::new(),
            banned_ships: Vec::new(),
            bans_finalized: false,
            member_list_locked_red: false,
            member_list_locked_blue: false,
            countdown_active: false,
            countdown_start_unix: 0,
            countdown_seconds: 60,
            max_points,
            match_time,
            overtime,
            external_ruleset_id,
            red_score: 100,
            blue_score: 100,
            red_fleet_boss_id: None,
            blue_fleet_boss_id: None,
            referees: Vec::new(),
            beacon_ids: vec![100_001_001, 100_001_002],
            pregame_checks_complete: false,
            grid_cleaned: false,
            autobanning_active: false,
            autoban_qty: 0,
            drone_restrict: false,
            lock_restrict: false,
            move_restrict: false,
            warp_restrict: false,
            pilot_errors: HashMap::new(),
            teleported_teams: Vec::new(),
            returned_teams: Vec::new(),
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
    pub fn ensure_match(&mut self, match_id: i64, solar_system_id: i64) -> &mut TourneyMatch {
        self.matches
            .entry(match_id)
            .or_insert_with(|| TourneyMatch::new(match_id, 1, solar_system_id, 100, 600, 0, 0))
    }

    pub fn resolve_match_mut(&mut self, target_id: Option<i64>, sys_id: i64) -> &mut TourneyMatch {
        let mid = match target_id {
            Some(id) if id > 0 => id,
            _ => self.active_match_id.unwrap_or(1),
        };
        self.ensure_match(mid, sys_id)
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
    snap.owner_notes
        .get(&TOURNEY_STORE_KEY)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
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
    args.get(idx)
        .and_then(|a| match a {
            PyRep::Bool(b) => Some(*b),
            PyRep::Integer(i) => Some(*i != 0),
            _ => None,
        })
        .unwrap_or(false)
}

fn extract_list_i64(rep: Option<&PyRep>) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) = rep {
        for it in items {
            if let Some(v) = it.as_i64() {
                out.push(v);
            }
        }
    } else if let Some(PyRep::Integer(v)) = rep {
        out.push(*v);
    }
    out
}

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "tourneyMgr" {
        return None;
    }

    let caller_char_id = session.character_id.map(|c| c as i64).unwrap_or(0);
    let solarsystem_id = session.solar_system_id.map(|s| s as i64).unwrap_or(30000142);
    let now_unix = ctx.now_unix();

    let res = match (service, method) {
        ("tourneyMgr", "AbortCountdown") => {
            let target_match_id = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_match_id, solarsystem_id);
                m.countdown_active = false;
                if m.state == TourneyMatchState::Countdown {
                    m.state = TourneyMatchState::Setup;
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "AddPlayer") => {
            // Can be called as (matchID, whichTeam, charID) or (whichTeam, charID)
            let (target_mid, which_team, char_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                let pilot = TourneyPilot {
                    character_id: char_id,
                    ship_type_id: 603,
                    ship_id: 10_000_000 + char_id,
                    points: 10,
                };
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
            let ban_id = extract_id(args, 0).unwrap_or(1);
            let ban_types = extract_list_i64(args.get(1));

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                store.ban_sessions.insert(ban_id, ban_types.clone());
                if let Some(mid) = store.active_match_id {
                    let m = store.ensure_match(mid, solarsystem_id);
                    for bt in &ban_types {
                        if !m.banned_ships.contains(bt) {
                            m.banned_ships.push(*bt);
                        }
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "BanShipType") => {
            // (matchID, teamIdx, shipTypeID) or (teamIdx, shipTypeID)
            let (target_mid, team_idx, ship_type_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
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
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.grid_cleaned = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "CompletePregameChecks") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.pregame_checks_complete = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "CreateMatch") => {
            // (tourneyID, matchID, maxPoints, matchTime, overtime, externalRulesetID)
            let tourney_id = extract_id(args, 0).unwrap_or(1);
            let match_id = extract_id(args, 1).unwrap_or(1001);
            let max_points = extract_id(args, 2).unwrap_or(100);
            let match_time = extract_id(args, 3).unwrap_or(600);
            let overtime = extract_id(args, 4).unwrap_or(0);
            let external_ruleset_id = extract_id(args, 5).unwrap_or(0);

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let mut m = TourneyMatch::new(
                    match_id,
                    tourney_id,
                    solarsystem_id,
                    max_points,
                    match_time,
                    overtime,
                    external_ruleset_id,
                );
                if caller_char_id > 0 && !m.referees.contains(&caller_char_id) {
                    m.referees.push(caller_char_id);
                }
                store.active_match_id = Some(match_id);
                let beacon_reps: Vec<PyRep> = m.beacon_ids.iter().copied().map(PyRep::Integer).collect();
                let captain_reps: Vec<PyRep> = vec![
                    PyRep::Integer(m.red_fleet_boss_id.unwrap_or(90000001)),
                    PyRep::Integer(m.blue_fleet_boss_id.unwrap_or(90000002)),
                ];
                store.matches.insert(match_id, m);
                save_store(snap, &store, now_unix);

                // Unpackable 3-element tuple for StartMatch: (matchID, beaconIDs, captainIDs)
                Ok(PyRep::Tuple(vec![
                    PyRep::Integer(match_id),
                    PyRep::List(beacon_reps),
                    PyRep::List(captain_reps),
                ]))
            })
        }
        ("tourneyMgr", "ExecuteMemberChange") => {
            // Client notification / handshake for fleet boss or roster change
            Ok(PyRep::None)
        }
        ("tourneyMgr", "FinalizeBans") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
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
                let m_opt = store
                    .matches
                    .values()
                    .find(|m| m.solar_system_id == solarsystem_id && m.state != TourneyMatchState::Completed)
                    .or_else(|| store.resolve_match(None));

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

                // barDetails[0] = (totalEHP, totalDPS, rep, cont, score, bans)
                let red_bar = PyRep::Tuple(vec![
                    PyRep::Integer(100_000),
                    PyRep::Integer(1_500),
                    PyRep::Integer(500),
                    PyRep::Integer(200),
                    PyRep::Integer(m.red_score),
                    PyRep::Integer(m.banned_ship_types_red.len() as i64),
                ]);
                let blue_bar = PyRep::Tuple(vec![
                    PyRep::Integer(100_000),
                    PyRep::Integer(1_500),
                    PyRep::Integer(500),
                    PyRep::Integer(200),
                    PyRep::Integer(m.blue_score),
                    PyRep::Integer(m.banned_ship_types_blue.len() as i64),
                ]);

                let dict_fields = vec![
                    (PyRep::Integer(0), red_bar),
                    (PyRep::Integer(1), blue_bar),
                    (PyRep::String("matchID".into()), PyRep::Integer(m.match_id)),
                    (
                        PyRep::String("scores".into()),
                        PyRep::List(vec![PyRep::Integer(m.red_score), PyRep::Integer(m.blue_score)]),
                    ),
                    (
                        PyRep::String("bans".into()),
                        PyRep::Dict(vec![
                            (
                                PyRep::String("red".into()),
                                PyRep::List(m.banned_ship_types_red.iter().copied().map(PyRep::Integer).collect()),
                            ),
                            (
                                PyRep::String("blue".into()),
                                PyRep::List(m.banned_ship_types_blue.iter().copied().map(PyRep::Integer).collect()),
                            ),
                        ]),
                    ),
                    (
                        PyRep::String("details".into()),
                        PyRep::Dict(vec![
                            (PyRep::String("state".into()), PyRep::String(m.state.as_str().into())),
                            (PyRep::String("maxPoints".into()), PyRep::Integer(m.max_points)),
                            (PyRep::String("matchTime".into()), PyRep::Integer(m.match_time)),
                        ]),
                    ),
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

                let red_list: Vec<PyRep> = m
                    .red_players
                    .iter()
                    .map(|p| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(p.character_id),
                            PyRep::Integer(p.ship_type_id),
                            PyRep::Integer(p.ship_id),
                            PyRep::Integer(p.points),
                        ])
                    })
                    .collect();

                let blue_list: Vec<PyRep> = m
                    .blue_players
                    .iter()
                    .map(|p| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(p.character_id),
                            PyRep::Integer(p.ship_type_id),
                            PyRep::Integer(p.ship_id),
                            PyRep::Integer(p.points),
                        ])
                    })
                    .collect();

                PyRep::List(vec![PyRep::List(red_list), PyRep::List(blue_list)])
            }))
        }
        ("tourneyMgr", "IsOtherMatchInProgress") => {
            let target_mid = extract_id(args, 0).unwrap_or_default();
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let in_progress = store.matches.values().any(|m| {
                    m.match_id != target_mid
                        && m.solar_system_id == solarsystem_id
                        && (m.state == TourneyMatchState::InProgress || m.state == TourneyMatchState::Countdown)
                });
                PyRep::Bool(in_progress)
            }))
        }
        ("tourneyMgr", "LockMemberList") => {
            // (matchID, whichTeam, memberList) or (whichTeam, memberList)
            let (target_mid, which_team) = if args.len() >= 2 && args.get(1).and_then(|a| a.as_i64()).is_some() && args.get(0).and_then(|a| a.as_i64()).is_some() && args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if which_team == 0 {
                    m.member_list_locked_red = true;
                } else {
                    m.member_list_locked_blue = true;
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "MatchCompleted") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.state = TourneyMatchState::Completed;
                m.countdown_active = false;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideDroneRestrict") => {
            let (target_mid, can_drone) = if args.len() >= 2 {
                (extract_id(args, 0), extract_bool(args, 1))
            } else {
                (None, extract_bool(args, 0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.drone_restrict = can_drone;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideError") => {
            let (target_mid, char_id, err_str) = if args.len() >= 3 {
                (
                    extract_id(args, 0),
                    extract_id(args, 1).unwrap_or(0),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or(""),
                )
            } else {
                (
                    None,
                    extract_id(args, 0).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                )
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if let Some(errors) = m.pilot_errors.get_mut(&char_id) {
                    if err_str.is_empty() {
                        errors.clear();
                    } else {
                        errors.retain(|e| e != err_str);
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideFleetBossID") => {
            let (target_mid, team_idx, new_boss) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if team_idx == 0 {
                    m.red_fleet_boss_id = Some(new_boss);
                } else {
                    m.blue_fleet_boss_id = Some(new_boss);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideLockRestrict") => {
            let (target_mid, can_lock) = if args.len() >= 2 {
                (extract_id(args, 0), extract_bool(args, 1))
            } else {
                (None, extract_bool(args, 0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.lock_restrict = can_lock;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideMoveRestrict") => {
            let (target_mid, can_move) = if args.len() >= 2 {
                (extract_id(args, 0), extract_bool(args, 1))
            } else {
                (None, extract_bool(args, 0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.move_restrict = can_move;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "OverrideWarpRestrict") => {
            let (target_mid, can_warp) = if args.len() >= 2 {
                (extract_id(args, 0), extract_bool(args, 1))
            } else {
                (None, extract_bool(args, 0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.warp_restrict = can_warp;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "QueryShipList") => {
            let allowed_ships = vec![
                620, 621, 622, 623, 624, 625, 626, 627, 628, 629, 630, 631, 632, 633, 634, 638,
                644, 29986, 33468,
            ];
            let ship_reps: Vec<PyRep> = allowed_ships.into_iter().map(PyRep::Integer).collect();
            Ok(PyRep::Dict(vec![(
                PyRep::String("allowedShipList".into()),
                PyRep::List(ship_reps),
            )]))
        }
        ("tourneyMgr", "RemovePlayer") => {
            let (target_mid, team_idx, char_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if team_idx == 0 {
                    m.red_players.retain(|p| p.character_id != char_id);
                } else {
                    m.blue_players.retain(|p| p.character_id != char_id);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "RemoveSelfFromReferees") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if caller_char_id > 0 {
                    m.referees.retain(|&r| r != caller_char_id);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "ResetMatch") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
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
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.returned_teams = vec![0, 1];
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "ReturnTeamPlayers") => {
            let (target_mid, which_team) = if args.len() >= 2 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if !m.returned_teams.contains(&which_team) {
                    m.returned_teams.push(which_team);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "StartAutobanning") => {
            // (matchID, tourneyID, redCharID, blueCharID, totalNumBans)
            let (target_mid, qty) = if args.len() >= 5 {
                (extract_id(args, 0), extract_id(args, 4).unwrap_or(3))
            } else {
                (extract_id(args, 0), 3)
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.autobanning_active = true;
                m.autoban_qty = qty;
                m.state = TourneyMatchState::Banning;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "StartCountdown") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.countdown_active = true;
                m.countdown_start_unix = now_unix;
                m.state = TourneyMatchState::Countdown;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "TeleportPlayers") => {
            let (target_mid, which_team, _beacon_idx) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if !m.teleported_teams.contains(&which_team) {
                    m.teleported_teams.push(which_team);
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "TeleportRefHome") => {
            // Referees teleporting home after tournament operations
            Ok(PyRep::Bool(true))
        }
        ("tourneyMgr", "UnBanShipType") => {
            let (target_mid, team_idx, ship_type_id) = if args.len() >= 3 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0), extract_id(args, 2).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0), extract_id(args, 1).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
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
            let (target_mid, which_team) = if args.len() >= 2 {
                (extract_id(args, 0), extract_id(args, 1).unwrap_or(0))
            } else {
                (None, extract_id(args, 0).unwrap_or(0))
            };

            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                if which_team == 0 {
                    m.member_list_locked_red = false;
                } else {
                    m.member_list_locked_blue = false;
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "UpdateFleetDetails") => {
            let target_mid = extract_id(args, 0);
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid, solarsystem_id);
                m.fleet_details_updated = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        _ => return None,
    };

    Some(res)
}
