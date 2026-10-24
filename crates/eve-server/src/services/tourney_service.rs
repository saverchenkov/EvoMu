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
pub enum TourneyMatchState { Setup, Banning, Countdown, InProgress, Completed }

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
    pub match_id: i64, pub tourney_id: i64, pub solar_system_id: i64,
    pub state: TourneyMatchState,
    pub red_team_id: i64, pub red_team_name: String,
    pub blue_team_id: i64, pub blue_team_name: String,
    pub red_players: Vec<TourneyPilot>, pub blue_players: Vec<TourneyPilot>,
    pub banned_ship_types_red: Vec<i64>, pub banned_ship_types_blue: Vec<i64>,
    pub banned_ships: Vec<i64>, pub bans_finalized: bool,
    pub member_list_locked_red: bool, pub member_list_locked_blue: bool,
    pub countdown_active: bool, pub countdown_start_unix: u64, pub countdown_seconds: i64,
    pub max_points: i64, pub match_time: i64, pub overtime: i64, pub external_ruleset_id: i64,
    pub red_score: i64, pub blue_score: i64,
    pub red_fleet_boss_id: Option<i64>, pub blue_fleet_boss_id: Option<i64>,
    #[serde(default)] pub red_fleet_id: Option<i64>,
    #[serde(default)] pub blue_fleet_id: Option<i64>,
    pub referees: Vec<i64>, pub beacon_ids: Vec<i64>,
    pub pregame_checks_complete: bool, pub grid_cleaned: bool,
    pub autobanning_active: bool, pub autoban_qty: i64,
    pub drone_restrict: bool, pub lock_restrict: bool, pub move_restrict: bool, pub warp_restrict: bool,
    pub pilot_errors: HashMap<i64, Vec<String>>,
    pub teleported_teams: Vec<i64>, pub returned_teams: Vec<i64>,
    pub fleet_details_updated: bool,
}

impl TourneyMatch {
    pub fn new(match_id: i64, tourney_id: i64, solar_system_id: i64, max_points: i64, match_time: i64, overtime: i64, external_ruleset_id: i64) -> Self {
        Self {
            match_id, tourney_id, solar_system_id, state: TourneyMatchState::Setup,
            red_team_id: 0, red_team_name: String::new(), blue_team_id: 0, blue_team_name: String::new(),
            red_players: Vec::new(), blue_players: Vec::new(),
            banned_ship_types_red: Vec::new(), banned_ship_types_blue: Vec::new(),
            banned_ships: Vec::new(), bans_finalized: false,
            member_list_locked_red: false, member_list_locked_blue: false,
            countdown_active: false, countdown_start_unix: 0, countdown_seconds: 60,
            max_points, match_time, overtime, external_ruleset_id,
            red_score: 0, blue_score: 0,
            red_fleet_boss_id: None, blue_fleet_boss_id: None, red_fleet_id: None, blue_fleet_id: None,
            referees: Vec::new(), beacon_ids: Vec::new(),
            pregame_checks_complete: false, grid_cleaned: false, autobanning_active: false, autoban_qty: 0,
            drone_restrict: true, lock_restrict: true, move_restrict: true, warp_restrict: true,
            pilot_errors: HashMap::new(), teleported_teams: Vec::new(), returned_teams: Vec::new(),
            fleet_details_updated: false,
        }
    }
}

fn sync_match_countdown(m: &mut TourneyMatch, now_unix: u64) -> bool {
    if m.state == TourneyMatchState::Countdown && m.countdown_active
        && now_unix >= m.countdown_start_unix.saturating_add(m.countdown_seconds.max(1) as u64)
    {
        m.state = TourneyMatchState::InProgress;
        m.countdown_active = false;
        m.drone_restrict = false;
        m.lock_restrict = false;
        m.move_restrict = false;
        m.warp_restrict = false;
        true
    } else {
        false
    }
}

fn resolve_team_name(snap: &eve_db::WorldStateSnapshot, team_id: i64, capt_id: Option<i64>) -> (i64, String) {
    if team_id > 0 {
        if let Some(ally) = snap.alliances.get(&team_id) { return (team_id, ally.name.clone()); }
        if let Some(corp) = snap.corporations.get(&team_id) { return (team_id, corp.name.clone()); }
        if let Some(ch) = snap.characters.get(&team_id) { return (team_id, ch.name.clone()); }
    }
    if let Some(cid) = capt_id {
        if let Some(ch) = snap.characters.get(&cid) {
            if let Some(aid) = ch.alliance_id {
                if let Some(ally) = snap.alliances.get(&aid) {
                    return (if team_id > 0 { team_id } else { aid }, ally.name.clone());
                }
            }
            if let Some(corp) = snap.corporations.get(&ch.corporation_id) {
                return (if team_id > 0 { team_id } else { ch.corporation_id }, corp.name.clone());
            }
            return (if team_id > 0 { team_id } else { cid }, ch.name.clone());
        }
    }
    (team_id, String::new())
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
            _ => self.active_match_id.ok_or_else(|| BridgeRpcError { code: "MatchNotFound".into(), message: "No active tournament match found".into() })?,
        };
        self.matches.get_mut(&mid).ok_or_else(|| BridgeRpcError { code: "MatchNotFound".into(), message: format!("Match ID {mid} not found") })
    }

    pub fn resolve_match(&self, target_id: Option<i64>) -> Option<&TourneyMatch> {
        let mid = match target_id {
            Some(id) if id > 0 => id,
            _ => self.active_match_id?,
        };
        self.matches.get(&mid)
    }
}

fn maybe_sync_expired_countdowns(ctx: &ServerContext, now_unix: u64) {
    let needs_sync = ctx.db.with_snapshot(|snap| {
        snap.owner_notes.get(&TOURNEY_STORE_KEY).is_some_and(|n| n.item_id > 0 && now_unix >= n.item_id as u64)
    });
    if needs_sync {
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store(snap);
            let mut changed = false;
            for m in store.matches.values_mut() {
                if sync_match_countdown(m, now_unix) { changed = true; }
            }
            if changed { save_store(snap, &store, now_unix); }
        });
    }
}

pub fn load_store(snap: &eve_db::WorldStateSnapshot) -> TourneyStore {
    snap.owner_notes.get(&TOURNEY_STORE_KEY).and_then(|n| serde_json::from_str(&n.note).ok()).unwrap_or_default()
}

pub fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &TourneyStore, now_unix: u64) {
    if let Ok(json) = serde_json::to_string(store) {
        let earliest_countdown_exp = store.matches.values()
            .filter(|m| m.state == TourneyMatchState::Countdown && m.countdown_active && m.countdown_start_unix > 0)
            .map(|m| m.countdown_start_unix.saturating_add(m.countdown_seconds.max(1) as u64) as i64)
            .min().unwrap_or(0);
        let has_active_match = i64::from(store.matches.values().any(|m| matches!(m.state, TourneyMatchState::Countdown | TourneyMatchState::InProgress)));
        snap.owner_notes.insert(
            TOURNEY_STORE_KEY,
            eve_db::OwnerNoteRecord { note_id: TOURNEY_STORE_KEY, character_id: has_active_match, item_id: earliest_countdown_exp, label: "tourney_store".into(), note: json, timestamp_unix: now_unix },
        );
    }
}

fn extract_id(args: &[PyRep], idx: usize) -> Option<i64> { args.get(idx).and_then(|a| a.as_i64()) }

fn extract_bool(args: &[PyRep], idx: usize) -> bool {
    args.get(idx).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(false)
}

fn extract_list_i64(rep: Option<&PyRep>) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) = rep {
        for it in items { if let Some(v) = it.as_i64() { out.push(v); } }
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
        Err(BridgeRpcError { code: "AccessDenied".into(), message: "Caller is not an authorized tournament referee or GM".into() })
    }
}

// DERIVED: Alliance Tournament ship hull point values mapped from SDE ship group_id.
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

// DERIVED: spectator telemetry aggregated from SDE dogma attributes on each pilot's ship hull
// (hp 9, armorHP 265, shieldCapacity 263, armor/shield resonances 267..=274, hardpoints 101/102,
// droneBandwidth 1271) and fitted modules in snap.items (speed 51, duration 73, damageMultiplier 64,
// armorDamageAmount 84, shieldBonus 68, warpScrambleStrength 105, speedFactor 20).
fn compute_team_telemetry(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, players: &[TourneyPilot], score: i64, bans_count: usize) -> PyRep {
    let (mut total_ehp, mut total_dps, mut total_rep, mut total_cont) = (0_i64, 0_i64, 0_i64, 0_i64);
    for p in players {
        let (mut hull_ehp, mut base_dps) = (1500.0, 25.0);
        if let Some(t) = ctx.sde.types.get(&(p.ship_type_id as u32)) {
            let da = &t.default_attributes;
            let (s_hp, a_hp, sh_hp) = (da.get(&9).copied().unwrap_or(500.0), da.get(&265).copied().unwrap_or(500.0), da.get(&263).copied().unwrap_or(500.0));
            let a_res = ((da.get(&267).copied().unwrap_or(1.0) + da.get(&268).copied().unwrap_or(1.0) + da.get(&269).copied().unwrap_or(1.0) + da.get(&270).copied().unwrap_or(1.0)) * 0.25).clamp(0.1, 1.0);
            let sh_res = ((da.get(&271).copied().unwrap_or(1.0) + da.get(&272).copied().unwrap_or(1.0) + da.get(&273).copied().unwrap_or(1.0) + da.get(&274).copied().unwrap_or(1.0)) * 0.25).clamp(0.1, 1.0);
            hull_ehp = s_hp + (a_hp / a_res) + (sh_hp / sh_res);
            let hardpoints = da.get(&102).copied().unwrap_or(0.0) + da.get(&101).copied().unwrap_or(0.0) + (da.get(&1271).copied().unwrap_or(0.0) / 25.0);
            base_dps = (hardpoints.max(1.0) * 25.0).max(25.0);
        }
        let (mut mod_dps, mut mod_rep, mut mod_cont) = (0.0, 0.0, 0.0);
        if p.ship_id > 0 {
            for it in snap.items.values().filter(|i| i.location_id == p.ship_id && (11..=34).contains(&(i.flag as u16))) {
                if let Some(mt) = ctx.sde.types.get(&it.type_id) {
                    let attr = |id: u16| it.dynamic_attributes.get(&id).copied().or_else(|| mt.default_attributes.get(&id).copied()).unwrap_or(0.0);
                    let cycle_s = (attr(51).max(attr(73)) / 1000.0).max(0.5);
                    if attr(64) > 0.0 { mod_dps += (attr(64) * (attr(114) + attr(116) + attr(117) + attr(118)).max(25.0)) / cycle_s; }
                    if attr(84) > 0.0 || attr(68) > 0.0 { mod_rep += (attr(84) + attr(68)) / cycle_s.max(1.0); }
                    if attr(105) > 0.0 || attr(20) != 0.0 { mod_cont += attr(105) * 50.0 + attr(20).abs(); }
                }
            }
        }
        total_ehp += hull_ehp.round() as i64;
        total_dps += (base_dps + mod_dps).round() as i64;
        total_rep += mod_rep.round() as i64;
        total_cont += mod_cont.round() as i64;
    }
    PyRep::Tuple(vec![PyRep::Integer(total_ehp), PyRep::Integer(total_dps), PyRep::Integer(total_rep), PyRep::Integer(total_cont), PyRep::Integer(score), PyRep::Integer(bans_count as i64)])
}

fn resolve_pilot(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, char_id: i64) -> TourneyPilot {
    let (ship_id, ship_type_id) = snap.characters.get(&char_id)
        .and_then(|ch| (ch.active_ship_id > 0).then_some(ch.active_ship_id))
        .and_then(|sid| snap.items.get(&sid).map(|it| (sid, i64::from(it.type_id))))
        .unwrap_or((0, 0));
    let points = if ship_type_id > 0 { compute_ship_points(ctx, ship_type_id) } else { 0 };
    TourneyPilot { character_id: char_id, ship_type_id, ship_id, points }
}

fn is_ship_type_banned(m: &TourneyMatch, ship_type_id: i64) -> bool {
    ship_type_id > 0 && (m.banned_ships.contains(&ship_type_id) || m.banned_ship_types_red.contains(&ship_type_id) || m.banned_ship_types_blue.contains(&ship_type_id))
}

fn validate_pilot_for_team(m: &mut TourneyMatch, which_team: i64, pilot: &TourneyPilot) -> Result<(), BridgeRpcError> {
    if is_ship_type_banned(m, pilot.ship_type_id) {
        let errs = m.pilot_errors.entry(pilot.character_id).or_default();
        if !errs.iter().any(|e| e == "TourneyShipTypeBanned") { errs.push("TourneyShipTypeBanned".into()); }
        return Err(BridgeRpcError { code: "TourneyShipTypeBanned".into(), message: format!("Ship type {} is banned in this tournament match", pilot.ship_type_id) });
    }
    let existing_pts: i64 = (if which_team == 0 { &m.red_players } else { &m.blue_players })
        .iter().filter(|p| p.character_id != pilot.character_id).map(|p| p.points).sum();
    if m.max_points > 0 && existing_pts + pilot.points > m.max_points {
        let errs = m.pilot_errors.entry(pilot.character_id).or_default();
        if !errs.iter().any(|e| e == "TourneyMaxPointsExceeded") { errs.push("TourneyMaxPointsExceeded".into()); }
        return Err(BridgeRpcError { code: "TourneyMaxPointsExceeded".into(), message: format!("Team point total {} exceeds match max_points {}", existing_pts + pilot.points, m.max_points) });
    }
    Ok(())
}

fn resolve_valid_home_station(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, home_st_id: i64, fallback_sys: u32) -> (i64, u32) {
    if let Some(st) = ctx.sde.stations.get(&home_st_id) {
        return (home_st_id, st.solar_system_id);
    }
    if let Some(s) = snap.structures.get(&home_st_id) {
        return (home_st_id, s.solar_system_id);
    }
    if let Some(st) = ctx.sde.stations.values().find(|s| s.solar_system_id == fallback_sys).or_else(|| ctx.sde.stations.values().min_by_key(|s| s.station_id)) {
        (st.station_id, st.solar_system_id)
    } else {
        (home_st_id.max(1), fallback_sys)
    }
}

macro_rules! require_cid {
    ($session:expr) => { match ServerContext::require_character($session) { Ok((_, cid)) => cid, Err(e) => return Some(Err(e)) } };
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
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
                    return Err(BridgeRpcError { code: "MemberListLocked".into(), message: "Cannot modify roster while team member list is locked".into() });
                }
                let pilot = resolve_pilot(ctx, snap, char_id);
                if let Err(e) = validate_pilot_for_team(m, which_team, &pilot) {
                    save_store(snap, &store, now_unix);
                    return Err(e);
                }
                let roster = if which_team == 0 { &mut m.red_players } else { &mut m.blue_players };
                if let Some(existing) = roster.iter_mut().find(|p| p.character_id == char_id) { *existing = pilot; } else { roster.push(pilot); }
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
                let target_mid = store.matches.contains_key(&ban_id).then_some(ban_id);
                let m = store.resolve_match_mut(target_mid)?;
                let is_capt = m.red_fleet_boss_id == Some(caller_cid) || m.blue_fleet_boss_id == Some(caller_cid);
                if !is_capt { check_referee_auth(session, caller_cid, m)?; }
                let is_blue_capt = m.blue_fleet_boss_id == Some(caller_cid) && m.red_fleet_boss_id != Some(caller_cid);
                for &bt in &ban_types {
                    if !m.banned_ships.contains(&bt) { m.banned_ships.push(bt); }
                    let team_bans = if is_blue_capt { &mut m.banned_ship_types_blue } else { &mut m.banned_ship_types_red };
                    if !team_bans.contains(&bt) { team_bans.push(bt); }
                }
                store.ban_sessions.insert(ban_id, ban_types);
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
                let team_bans = if team_idx == 0 { &mut m.banned_ship_types_red } else { &mut m.banned_ship_types_blue };
                if !team_bans.contains(&ship_type_id) { team_bans.push(ship_type_id); }
                if !m.banned_ships.contains(&ship_type_id) { m.banned_ships.push(ship_type_id); }
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
                    it.location_id == match_sys_id && ctx.sde.types.get(&it.type_id).map(|t| matches!(t.group_id, 12 | 186 | 340 | 448 | 649)).unwrap_or(it.type_id == 26557)
                }).map(|it| it.item_id).collect();
                for &iid in &sys_items { snap.items.remove(&iid); removed_item_ids.push(iid); }
                let child_ids: Vec<i64> = snap.items.values().filter(|c| removed_item_ids.contains(&c.location_id)).map(|c| c.item_id).collect();
                for cid in child_ids { snap.items.remove(&cid); }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });
            if res.is_ok() && !removed_item_ids.is_empty() {
                let sys_handle = ctx.systems.get_or_spawn(match_sys_id as u32);
                for &iid in &removed_item_ids { let _ = sys_handle.remove_ball(iid).await; }
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
                m.red_players = m.red_players.iter().map(|p| resolve_pilot(ctx, snap, p.character_id)).collect();
                m.blue_players = m.blue_players.iter().map(|p| resolve_pilot(ctx, snap, p.character_id)).collect();
                let mut pregame_err: Option<BridgeRpcError> = None;
                for (team_idx, roster) in [(0, m.red_players.clone()), (1, m.blue_players.clone())] {
                    let team_pts: i64 = roster.iter().map(|p| p.points).sum();
                    for p in &roster {
                        if is_ship_type_banned(m, p.ship_type_id) {
                            let errs = m.pilot_errors.entry(p.character_id).or_default();
                            if !errs.iter().any(|e| e == "TourneyShipTypeBanned") { errs.push("TourneyShipTypeBanned".into()); }
                            pregame_err.get_or_insert_with(|| BridgeRpcError { code: "TourneyShipTypeBanned".into(), message: format!("Ship type {} is banned in this tournament match", p.ship_type_id) });
                        } else if m.max_points > 0 && team_pts > m.max_points {
                            let errs = m.pilot_errors.entry(p.character_id).or_default();
                            if !errs.iter().any(|e| e == "TourneyMaxPointsExceeded") { errs.push("TourneyMaxPointsExceeded".into()); }
                            pregame_err.get_or_insert_with(|| BridgeRpcError { code: "TourneyMaxPointsExceeded".into(), message: format!("Team {team_idx} point total {team_pts} exceeds match max_points {}", m.max_points) });
                        } else if let Some(code) = m.pilot_errors.get(&p.character_id).and_then(|v| v.first()).cloned() {
                            pregame_err.get_or_insert_with(|| BridgeRpcError { code: code.clone(), message: format!("Pilot {} has unresolved pregame error: {code}", p.character_id) });
                        }
                    }
                }
                if let Some(err) = pregame_err {
                    save_store(snap, &store, now_unix);
                    return Err(err);
                }
                m.pregame_checks_complete = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "CreateMatch") => {
            let caller_cid = require_cid!(session);
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError { code: "AdminAccessDenied".into(), message: "Creating a tournament match requires GM administrator privileges".into() }));
            }
            let tourney_id = extract_id(args, 0).unwrap_or(1);
            let (match_id, tup_red_team, tup_blue_team, tup_red_capt, tup_blue_capt) = match args.get(1) {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => (items.first().and_then(|v| v.as_i64()).unwrap_or(1001), items.get(1).and_then(|v| v.as_i64()), items.get(2).and_then(|v| v.as_i64()), items.get(3).and_then(|v| v.as_i64()), items.get(4).and_then(|v| v.as_i64())),
                _ => (extract_id(args, 1).unwrap_or(1001), None, None, None, None),
            };
            let max_points = extract_id(args, 2).unwrap_or(100);
            let match_time = extract_id(args, 3).unwrap_or(600);
            let overtime = extract_id(args, 4).unwrap_or(0);
            let external_ruleset_id = extract_id(args, 5).unwrap_or(0);

            let (red_team, blue_team) = if let Some(PyRep::List(t_list) | PyRep::Tuple(t_list)) = kwargs.get("teams").or_else(|| args.get(6)) {
                (t_list.first().and_then(|v| v.as_i64()).unwrap_or(0), t_list.get(1).and_then(|v| v.as_i64()).unwrap_or(0))
            } else {
                (tup_red_team.or_else(|| kwargs.get("teamOne").or_else(|| kwargs.get("redTeamID")).and_then(|v| v.as_i64())).unwrap_or(0), tup_blue_team.or_else(|| kwargs.get("teamTwo").or_else(|| kwargs.get("blueTeamID")).and_then(|v| v.as_i64())).unwrap_or(0))
            };
            let (red_capt, blue_capt) = if let Some(PyRep::List(c_list) | PyRep::Tuple(c_list)) = kwargs.get("captains").or_else(|| args.get(7)) {
                (c_list.first().and_then(|v| v.as_i64()), c_list.get(1).and_then(|v| v.as_i64()))
            } else {
                (tup_red_capt.or_else(|| kwargs.get("captainOne").or_else(|| kwargs.get("redCaptainID")).and_then(|v| v.as_i64())), tup_blue_capt.or_else(|| kwargs.get("captainTwo").or_else(|| kwargs.get("blueCaptainID")).and_then(|v| v.as_i64())))
            };
            let match_sys_id = kwargs.get("solarSystemID").or_else(|| kwargs.get("systemID")).and_then(|v| v.as_i64()).or_else(|| args.get(8).and_then(|v| v.as_i64())).unwrap_or(solarsystem_id);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                if store.matches.contains_key(&match_id) {
                    return Err(BridgeRpcError { code: "DuplicateMatchID".into(), message: format!("Match ID {match_id} already exists") });
                }
                let mut m = TourneyMatch::new(match_id, tourney_id, match_sys_id, max_points, match_time, overtime, external_ruleset_id);
                (m.red_team_id, m.red_team_name) = resolve_team_name(snap, red_team, red_capt);
                (m.blue_team_id, m.blue_team_name) = resolve_team_name(snap, blue_team, blue_capt);
                m.red_fleet_boss_id = red_capt;
                m.blue_fleet_boss_id = blue_capt;
                if let Some(rc) = red_capt { m.red_players.push(resolve_pilot(ctx, snap, rc)); }
                if let Some(bc) = blue_capt { m.blue_players.push(resolve_pilot(ctx, snap, bc)); }
                if caller_cid > 0 && !m.referees.contains(&caller_cid) { m.referees.push(caller_cid); }
                snap.next_entity_id += 1;
                let b1 = snap.next_entity_id;
                snap.next_entity_id += 1;
                let b2 = snap.next_entity_id;
                m.beacon_ids = vec![b1, b2];
                store.active_match_id = Some(match_id);
                let beacon_reps: Vec<PyRep> = m.beacon_ids.iter().copied().map(PyRep::Integer).collect();
                let captain_reps = vec![PyRep::Integer(m.red_fleet_boss_id.unwrap_or(0)), PyRep::Integer(m.blue_fleet_boss_id.unwrap_or(0))];
                store.matches.insert(match_id, m);
                save_store(snap, &store, now_unix);
                Ok((PyRep::Tuple(vec![PyRep::Integer(match_id), PyRep::List(beacon_reps), PyRep::List(captain_reps)]), b1, b2))
            });
            if let Ok((rep, b1, b2)) = res {
                let sys_handle = ctx.systems.get_or_spawn(match_sys_id as u32);
                let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
                for (bid, x) in [(b1, -50_000.0), (b2, 50_000.0)] {
                    bp.add_static_ball_with_meta(
                        eve_destiny::Ball { id: bid, type_id: 2977, position: glam::DVec3::new(x, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 100.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                        eve_destiny::SlimItemMeta { group_id: 310, category_id: 2, owner_id: 1, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
                    );
                }
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
                        return Err(BridgeRpcError { code: "MemberListLocked".into(), message: "Cannot modify roster while team member list is locked".into() });
                    }
                    if char_id > 0 {
                        if is_add {
                            let pilot = resolve_pilot(ctx, snap, char_id);
                            if let Err(e) = validate_pilot_for_team(m, which_team, &pilot) {
                                save_store(snap, &store, now_unix);
                                return Err(e);
                            }
                            let roster = if which_team == 0 { &mut m.red_players } else { &mut m.blue_players };
                            if let Some(existing) = roster.iter_mut().find(|p| p.character_id == char_id) { *existing = pilot; } else { roster.push(pilot); }
                        } else {
                            let roster = if which_team == 0 { &mut m.red_players } else { &mut m.blue_players };
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
                if m.state == TourneyMatchState::Banning { m.state = TourneyMatchState::Setup; }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "GetActiveMatchForCamera") => {
            maybe_sync_expired_countdowns(ctx, now_unix);
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let m_opt = store.matches.values().find(|m| m.solar_system_id == solarsystem_id && m.state != TourneyMatchState::Completed).or_else(|| store.resolve_match(None));
                if let Some(m) = m_opt {
                    let nebula_id = ctx.sde.solar_systems.get(&(m.solar_system_id as u32)).map(|s| i64::from(s.region_id)).unwrap_or(m.solar_system_id);
                    PyRep::Dict(vec![
                        (PyRep::String("matchID".into()), PyRep::Integer(m.match_id)),
                        (PyRep::String("teamOneName".into()), PyRep::String(m.red_team_name.clone())),
                        (PyRep::String("teamTwoName".into()), PyRep::String(m.blue_team_name.clone())),
                        (PyRep::String("nebulaID".into()), PyRep::Integer(nebula_id)),
                    ])
                } else {
                    PyRep::None
                }
            }))
        }
        ("tourneyMgr", "GetFancyDetails") => {
            let target_mid = extract_id(args, 0);
            maybe_sync_expired_countdowns(ctx, now_unix);
            ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let m_clone = store.resolve_match(target_mid).ok_or_else(|| BridgeRpcError {
                    code: "MatchNotFound".into(),
                    message: format!("Match ID {} not found", target_mid.unwrap_or_default()),
                })?;
                let red_bar = compute_team_telemetry(ctx, snap, &m_clone.red_players, m_clone.red_score, m_clone.banned_ship_types_red.len());
                let blue_bar = compute_team_telemetry(ctx, snap, &m_clone.blue_players, m_clone.blue_score, m_clone.banned_ship_types_blue.len());
                let red_bans: Vec<PyRep> = m_clone.banned_ship_types_red.iter().copied().map(PyRep::Integer).collect();
                let blue_bans: Vec<PyRep> = m_clone.banned_ship_types_blue.iter().copied().map(PyRep::Integer).collect();
                let state_int = match m_clone.state {
                    TourneyMatchState::Setup => 0, TourneyMatchState::Banning => 1, TourneyMatchState::Countdown => 2, TourneyMatchState::InProgress => 3, TourneyMatchState::Completed => 4,
                };
                let match_details_tup = PyRep::Tuple(vec![
                    PyRep::Integer(state_int), PyRep::Integer(m_clone.countdown_start_unix as i64), PyRep::Integer(m_clone.match_time),
                    PyRep::Integer(m_clone.overtime), PyRep::None, PyRep::Integer(m_clone.max_points),
                ]);
                let dict_fields = vec![
                    (PyRep::Integer(0), red_bar),
                    (PyRep::Integer(1), blue_bar),
                    (PyRep::Integer(2), match_details_tup),
                    (PyRep::Integer(3), PyRep::Tuple(vec![PyRep::List(red_bans.clone()), PyRep::List(blue_bans.clone())])),
                    (PyRep::String("matchID".into()), PyRep::Integer(m_clone.match_id)),
                    (PyRep::String("scores".into()), PyRep::List(vec![PyRep::Integer(m_clone.red_score), PyRep::Integer(m_clone.blue_score)])),
                    (PyRep::String("bans".into()), PyRep::Dict(vec![(PyRep::String("red".into()), PyRep::List(red_bans)), (PyRep::String("blue".into()), PyRep::List(blue_bans))])),
                    (PyRep::String("details".into()), PyRep::Dict(vec![
                        (PyRep::String("state".into()), PyRep::String(m_clone.state.as_str().into())),
                        (PyRep::String("maxPoints".into()), PyRep::Integer(m_clone.max_points)),
                        (PyRep::String("matchTime".into()), PyRep::Integer(m_clone.match_time)),
                        (PyRep::String("fleetIDs".into()), PyRep::List(vec![m_clone.red_fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None), m_clone.blue_fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None)])),
                        (PyRep::String("fleetBossIDs".into()), PyRep::List(vec![m_clone.red_fleet_boss_id.map(PyRep::Integer).unwrap_or(PyRep::None), m_clone.blue_fleet_boss_id.map(PyRep::Integer).unwrap_or(PyRep::None)])),
                    ])),
                ];
                Ok(PyRep::Dict(dict_fields))
            })
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
            ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let m = store.resolve_match(target_mid).ok_or_else(|| BridgeRpcError {
                    code: "MatchNotFound".into(),
                    message: format!("Match ID {} not found", target_mid.unwrap_or_default()),
                })?;
                let red_list: Vec<PyRep> = m.red_players.iter().map(|p| PyRep::Tuple(vec![PyRep::Integer(p.character_id), PyRep::Integer(p.ship_type_id), PyRep::Integer(p.ship_id), PyRep::Integer(p.points)])).collect();
                let blue_list: Vec<PyRep> = m.blue_players.iter().map(|p| PyRep::Tuple(vec![PyRep::Integer(p.character_id), PyRep::Integer(p.ship_type_id), PyRep::Integer(p.ship_id), PyRep::Integer(p.points)])).collect();
                Ok(PyRep::List(vec![PyRep::List(red_list), PyRep::List(blue_list)]))
            })
        }
        ("tourneyMgr", "IsOtherMatchInProgress") => {
            let target_mid = extract_id(args, 0).unwrap_or_default();
            maybe_sync_expired_countdowns(ctx, now_unix);
            Ok(ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let in_progress = store.matches.values().any(|m| {
                    m.match_id != target_mid && m.solar_system_id == solarsystem_id && (m.state == TourneyMatchState::InProgress || m.state == TourneyMatchState::Countdown)
                });
                PyRep::Bool(in_progress)
            }))
        }
        ("tourneyMgr", "LockMemberList") | ("tourneyMgr", "UnlockMemberList") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team) = if args.len() >= 2 { (extract_id(args, 0), extract_id(args, 1).unwrap_or(0)) } else { (None, extract_id(args, 0).unwrap_or(0)) };
            let locked = method == "LockMemberList";
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                if which_team == 0 { m.member_list_locked_red = locked; } else { m.member_list_locked_blue = locked; }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "MatchCompleted") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut beacons_to_remove = Vec::new();
            let mut match_sys = solarsystem_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.state = TourneyMatchState::Completed;
                m.countdown_active = false;
                match_sys = m.solar_system_id;
                beacons_to_remove = std::mem::take(&mut m.beacon_ids);
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });
            if res.is_ok() && !beacons_to_remove.is_empty() {
                let sys_handle = ctx.systems.get_or_spawn(match_sys as u32);
                for bid in beacons_to_remove { let _ = sys_handle.remove_ball(bid).await; }
            }
            res
        }
        ("tourneyMgr", "OverrideDroneRestrict")
        | ("tourneyMgr", "OverrideLockRestrict")
        | ("tourneyMgr", "OverrideMoveRestrict")
        | ("tourneyMgr", "OverrideWarpRestrict") => {
            let caller_cid = require_cid!(session);
            let (target_mid, restricted) = if args.len() >= 2 { (extract_id(args, 0), extract_bool(args, 1)) } else { (None, extract_bool(args, 0)) };
            ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                match method {
                    "OverrideDroneRestrict" => m.drone_restrict = restricted,
                    "OverrideLockRestrict" => m.lock_restrict = restricted,
                    "OverrideMoveRestrict" => m.move_restrict = restricted,
                    _ => m.warp_restrict = restricted,
                }
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
        ("tourneyMgr", "QueryShipList") => {
            // DERIVED: tournament ship eligibility filtered by match external_ruleset_id (ruleset 1 restricts
            // to frigate/destroyer hulls; ruleset 2 restricts to cruiser/battlecruiser hulls; default 0 allows
            // standard subcapital tournament groups) and excludes finalized match bans.
            let query_id = extract_id(args, 0);
            let (ruleset_id, finalized_bans) = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let m_opt = query_id
                    .and_then(|qid| store.matches.get(&qid).or_else(|| store.matches.values().find(|m| m.tourney_id == qid)))
                    .or_else(|| store.resolve_match(None));
                match m_opt {
                    Some(m) => (m.external_ruleset_id, if m.bans_finalized { m.banned_ships.clone() } else { Vec::new() }),
                    None => (0, Vec::new()),
                }
            });
            let mut allowed: Vec<i64> = ctx.sde.types.values().filter(|t| {
                let group_ok = match ruleset_id {
                    1 => matches!(t.group_id, 25 | 324 | 420 | 830 | 831 | 834 | 893 | 1305),
                    2 => matches!(t.group_id, 26 | 358 | 419 | 540 | 832 | 833 | 894 | 906 | 1201 | 1202),
                    _ => matches!(t.group_id, 25 | 26 | 27 | 324 | 358 | 419 | 420 | 540 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 1201 | 1202 | 1305),
                };
                t.category_id == 6 && t.published && group_ok && !finalized_bans.contains(&i64::from(t.type_id))
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
                if !m.referees.contains(&caller_cid) {
                    return Err(BridgeRpcError { code: "TourneyNotReferee".into(), message: "Caller is not in the match referee list".into() });
                }
                m.referees.retain(|&r| r != caller_cid);
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        ("tourneyMgr", "ResetMatch") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut beacons_to_remove = Vec::new();
            let mut match_sys = solarsystem_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.state = TourneyMatchState::Setup;
                m.banned_ship_types_red.clear();
                m.banned_ship_types_blue.clear();
                m.banned_ships.clear();
                m.bans_finalized = false;
                m.autobanning_active = false;
                m.autoban_qty = 0;
                m.member_list_locked_red = false;
                m.member_list_locked_blue = false;
                m.countdown_active = false;
                m.countdown_start_unix = 0;
                m.pregame_checks_complete = false;
                m.drone_restrict = true;
                m.lock_restrict = true;
                m.move_restrict = true;
                m.warp_restrict = true;
                m.red_score = 0;
                m.blue_score = 0;
                m.pilot_errors.clear();
                m.teleported_teams.clear();
                m.returned_teams.clear();
                match_sys = m.solar_system_id;
                beacons_to_remove = std::mem::take(&mut m.beacon_ids);
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });
            if res.is_ok() && !beacons_to_remove.is_empty() {
                let sys_handle = ctx.systems.get_or_spawn(match_sys as u32);
                for bid in beacons_to_remove { let _ = sys_handle.remove_ball(bid).await; }
            }
            res
        }
        ("tourneyMgr", "ReturnAllPlayers") | ("tourneyMgr", "ReturnTeamPlayers") => {
            let caller_cid = require_cid!(session);
            let (target_mid, which_team_opt) = if method == "ReturnAllPlayers" {
                (extract_id(args, 0), None)
            } else if args.len() >= 2 {
                (extract_id(args, 0), Some(extract_id(args, 1).unwrap_or(0)))
            } else {
                (None, Some(extract_id(args, 0).unwrap_or(0)))
            };
            let mut returned_transfers = Vec::new();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                let pilots: Vec<TourneyPilot> = match which_team_opt {
                    None => { m.returned_teams = vec![0, 1]; m.red_players.iter().chain(m.blue_players.iter()).cloned().collect() }
                    Some(wt) => { if !m.returned_teams.contains(&wt) { m.returned_teams.push(wt); } if wt == 0 { m.red_players.clone() } else { m.blue_players.clone() } }
                };
                for p in &pilots {
                    let (old_sys, old_st, raw_home, active_sid) = match snap.characters.get(&p.character_id) {
                        Some(ch) => (ch.solar_system_id, ch.station_id, ch.home_station_id, if ch.active_ship_id > 0 { ch.active_ship_id } else { p.ship_id }),
                        None => continue,
                    };
                    let (home_st_id, home_sys) = resolve_valid_home_station(ctx, snap, raw_home, old_sys);
                    if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                        ch.station_id = Some(home_st_id);
                        ch.structure_id = None;
                        ch.solar_system_id = home_sys;
                    }
                    let mut valid_ship_id = 0;
                    if active_sid > 0 {
                        if let Some(ship_it) = snap.items.get_mut(&active_sid).filter(|it| it.owner_id == p.character_id) {
                            ship_it.location_id = home_st_id;
                            ship_it.flag = eve_db::InventoryFlag::Hangar;
                            ship_it.version += 1;
                            valid_ship_id = active_sid;
                        }
                    }
                    returned_transfers.push((p.character_id, valid_ship_id, old_sys, old_st, home_st_id, home_sys));
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });
            if res.is_ok() {
                for (cid, ship_id, old_sys, old_st, home_st_id, home_sys) in returned_transfers {
                    if ship_id > 0 {
                        let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ship_id).await;
                        if home_sys != old_sys { let _ = ctx.systems.get_or_spawn(home_sys).remove_ball(ship_id).await; }
                    }
                    if session.character_id == Some(cid) {
                        session.station_id = Some(home_st_id);
                        session.structure_id = None;
                        session.solar_system_id = Some(home_sys);
                    }
                    if let Ok(mut bounds) = ctx.bound_objects.write() {
                        for b in bounds.values_mut().filter(|b| b.character_id == cid && b.service == "beyonce") { b.object_id = i64::from(home_sys); }
                    }
                    ctx.notify(
                        crate::router::NotificationTarget::Character(cid), "OnSessionChanged", "charid",
                        vec![PyRep::Bool(false), PyRep::Dict(vec![
                            (PyRep::String("stationid".into()), PyRep::Tuple(vec![old_st.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::Integer(home_st_id)])),
                            (PyRep::String("solarsystemid".into()), PyRep::Tuple(vec![PyRep::Integer(i64::from(old_sys)), PyRep::Integer(i64::from(home_sys))])),
                        ])],
                    );
                }
            }
            res
        }
        ("tourneyMgr", "StartAutobanning") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let qty = kwargs.get("totalNumBans").and_then(|v| v.as_i64())
                .or_else(|| if args.len() >= 5 { extract_id(args, 4) } else if args.len() >= 2 { extract_id(args, 1) } else { None })
                .unwrap_or(3);
            let red_capt_kw = kwargs.get("redCharID").and_then(|v| v.as_i64()).or_else(|| if args.len() >= 4 { extract_id(args, 2) } else { None });
            let blue_capt_kw = kwargs.get("blueCharID").and_then(|v| v.as_i64()).or_else(|| if args.len() >= 4 { extract_id(args, 3) } else { None });
            let mut notify_info = None;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                m.autobanning_active = true;
                m.autoban_qty = qty;
                m.state = TourneyMatchState::Banning;
                if let Some(rc) = red_capt_kw { m.red_fleet_boss_id = Some(rc); }
                if let Some(bc) = blue_capt_kw { m.blue_fleet_boss_id = Some(bc); }

                // DERIVED: deterministic autoban selection from published cruiser hulls in SDE when captains auto-ban.
                let mut candidate_bans: Vec<i64> = ctx.sde.types.values().filter(|t| t.category_id == 6 && t.group_id == 26 && t.published).map(|t| i64::from(t.type_id)).collect();
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
                notify_info = Some((m.match_id, m.tourney_id, m.red_fleet_boss_id, m.blue_fleet_boss_id, m.banned_ships.clone()));
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });
            if let Some((mid, tid, r_boss, b_boss, cur_bans)) = notify_info {
                let ban_args = vec![
                    PyRep::Integer(mid), PyRep::Integer(qty), PyRep::List(cur_bans.into_iter().map(PyRep::Integer).collect()),
                    PyRep::Integer((now_unix + 60) as i64), PyRep::Integer(1), PyRep::Integer(tid),
                ];
                for cid in [Some(caller_cid), r_boss, b_boss].into_iter().flatten() {
                    ctx.notify(crate::router::NotificationTarget::Character(cid), "OnTournamentPerformBan", "charid", ban_args.clone());
                }
            }
            res
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

            let mut moved_pilots = Vec::new();
            let mut match_sys = solarsystem_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_store(snap);
                let m = store.resolve_match_mut(target_mid)?;
                check_referee_auth(session, caller_cid, m)?;
                let refreshed: Vec<TourneyPilot> = (if which_team == 0 { &m.red_players } else { &m.blue_players })
                    .iter()
                    .map(|p| {
                        let rp = resolve_pilot(ctx, snap, p.character_id);
                        if rp.ship_id > 0 { rp } else { p.clone() }
                    })
                    .collect();
                for p in &refreshed {
                    if is_ship_type_banned(m, p.ship_type_id) {
                        return Err(BridgeRpcError {
                            code: "TourneyShipTypeBanned".into(),
                            message: format!("Ship type {} is banned in this tournament match", p.ship_type_id),
                        });
                    }
                }
                let team_pts: i64 = refreshed.iter().map(|p| p.points).sum();
                if m.max_points > 0 && team_pts > m.max_points {
                    return Err(BridgeRpcError {
                        code: "TourneyMaxPointsExceeded".into(),
                        message: format!("Team point total {team_pts} exceeds match max_points {}", m.max_points),
                    });
                }
                if which_team == 0 { m.red_players.clone_from(&refreshed); } else { m.blue_players.clone_from(&refreshed); }
                if !m.teleported_teams.contains(&which_team) {
                    m.teleported_teams.push(which_team);
                }
                match_sys = m.solar_system_id;

                for p in &refreshed {
                    if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                        let old_sys = ch.solar_system_id;
                        let old_st = ch.station_id;
                        let user_id = ch.user_id;
                        let active_sid = if ch.active_ship_id > 0 { ch.active_ship_id } else { p.ship_id };
                        ch.solar_system_id = match_sys as u32;
                        ch.station_id = None;
                        ch.structure_id = None;
                        let mut owned_ship = (0_i64, 0_u32);
                        if active_sid > 0 {
                            if let Some(ship_it) = snap.items.get_mut(&active_sid).filter(|it| it.owner_id == p.character_id) {
                                ship_it.location_id = match_sys;
                                ship_it.flag = eve_db::InventoryFlag::None;
                                ship_it.version += 1;
                                owned_ship = (ship_it.item_id, ship_it.type_id);
                            }
                        }
                        moved_pilots.push((p.character_id, user_id, owned_ship.0, owned_ship.1, old_sys, old_st));
                    }
                }
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                let base_pos = if beacon_idx == 0 { DVec3::new(-50_000.0, 0.0, 0.0) } else { DVec3::new(50_000.0, 0.0, 0.0) };
                let sys_handle = ctx.systems.get_or_spawn(match_sys as u32);
                for (idx, (cid, user_id, ship_id, stid, old_sys, old_st)) in moved_pilots.into_iter().enumerate() {
                    let offset = DVec3::new((idx as f64) * 250.0, 0.0, (idx as f64) * 250.0);
                    let pos = base_pos + offset;
                    let is_online = session.character_id == Some(cid) || ctx.is_character_online(cid, user_id);
                    if ship_id > 0 && stid > 0 {
                        if old_sys != match_sys as u32 {
                            let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ship_id).await;
                        }
                        if sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().is_some() {
                            let _ = sys_handle.set_ball_position(ship_id, pos).await;
                        } else if is_online {
                            let (mass, radius, max_vel, inertia) = ctx.sde.types.get(&stid).map(|t| {
                                (if t.mass > 0.0 { t.mass } else { 1_200_000.0 }, if t.radius > 0.0 { t.radius } else { 80.0 }, t.default_attributes.get(&37).copied().unwrap_or(300.0), t.default_attributes.get(&70).copied().unwrap_or(2.5))
                            }).unwrap_or((1_200_000.0, 80.0, 300.0, 2.5));
                            let cs = ctx.build_ship_combat_state(cid, ship_id, stid);
                            let _ = sys_handle.spawn_ball_with_combat(
                                eve_destiny::Ball { id: ship_id, type_id: stid, position: pos, velocity: DVec3::ZERO, max_velocity: max_vel, inertia, mass, radius, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop },
                                cs,
                            ).await;
                        }
                    }
                    if session.character_id == Some(cid) {
                        session.solar_system_id = Some(match_sys as u32);
                        session.station_id = None;
                        session.structure_id = None;
                    }
                    if let Ok(mut bounds) = ctx.bound_objects.write() {
                        for b in bounds.values_mut().filter(|b| b.character_id == cid && b.service == "beyonce") {
                            b.object_id = match_sys;
                        }
                    }
                    ctx.notify(
                        crate::router::NotificationTarget::Character(cid),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            PyRep::Dict(vec![
                                (PyRep::String("stationid".into()), PyRep::Tuple(vec![old_st.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::None])),
                                (PyRep::String("solarsystemid".into()), PyRep::Tuple(vec![PyRep::Integer(i64::from(old_sys)), PyRep::Integer(match_sys)])),
                            ]),
                        ],
                    );
                }
            }
            res
        }
        ("tourneyMgr", "TeleportRefHome") => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut transfer_info = None;

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

                if let Some((old_sys, old_st, raw_home, ref_ship_id)) = snap.characters.get(&caller_cid).map(|ch| (ch.solar_system_id, ch.station_id, ch.home_station_id, ch.active_ship_id)) {
                    let (home_st_id, home_sys) = resolve_valid_home_station(ctx, snap, raw_home, old_sys);
                    if let Some(ch) = snap.characters.get_mut(&caller_cid) {
                        ch.station_id = Some(home_st_id);
                        ch.structure_id = None;
                        ch.solar_system_id = home_sys;
                    }
                    let mut valid_ref_ship = 0;
                    if ref_ship_id > 0 {
                        if let Some(ship_it) = snap.items.get_mut(&ref_ship_id).filter(|it| it.owner_id == caller_cid) {
                            ship_it.location_id = home_st_id;
                            ship_it.flag = eve_db::InventoryFlag::Hangar;
                            ship_it.version += 1;
                            valid_ref_ship = ref_ship_id;
                        }
                    }
                    transfer_info = Some((valid_ref_ship, old_sys, old_st, home_st_id, home_sys));
                }
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                if let Some((ref_ship_id, old_sys, old_st, home_st_id, home_sys)) = transfer_info {
                    if ref_ship_id > 0 {
                        let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ref_ship_id).await;
                        if home_sys != old_sys {
                            let _ = ctx.systems.get_or_spawn(home_sys).remove_ball(ref_ship_id).await;
                        }
                    }
                    session.station_id = Some(home_st_id);
                    session.structure_id = None;
                    session.solar_system_id = Some(home_sys);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(caller_cid),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            PyRep::Dict(vec![
                                (PyRep::String("stationid".into()), PyRep::Tuple(vec![old_st.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::Integer(home_st_id)])),
                                (PyRep::String("solarsystemid".into()), PyRep::Tuple(vec![PyRep::Integer(i64::from(old_sys)), PyRep::Integer(i64::from(home_sys))])),
                            ]),
                        ],
                    );
                }
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
                if !m.banned_ship_types_red.contains(&ship_type_id) && !m.banned_ship_types_blue.contains(&ship_type_id) {
                    m.banned_ships.retain(|&id| id != ship_type_id);
                }
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
                for fl in snap.fleets.values() {
                    let is_red = m.red_fleet_boss_id == Some(fl.boss_id)
                        || m.red_players.iter().any(|p| fl.members.contains(&p.character_id));
                    if is_red {
                        m.red_fleet_id = Some(fl.fleet_id);
                        m.red_fleet_boss_id = Some(fl.boss_id);
                        if !m.member_list_locked_red {
                            for &mcid in &fl.members {
                                if !m.red_players.iter().any(|p| p.character_id == mcid) {
                                    m.red_players.push(resolve_pilot(ctx, snap, mcid));
                                }
                            }
                        }
                    }
                    let is_blue = !is_red
                        && (m.blue_fleet_boss_id == Some(fl.boss_id)
                            || m.blue_players.iter().any(|p| fl.members.contains(&p.character_id)));
                    if is_blue {
                        m.blue_fleet_id = Some(fl.fleet_id);
                        m.blue_fleet_boss_id = Some(fl.boss_id);
                        if !m.member_list_locked_blue {
                            for &mcid in &fl.members {
                                if !m.blue_players.iter().any(|p| p.character_id == mcid) {
                                    m.blue_players.push(resolve_pilot(ctx, snap, mcid));
                                }
                            }
                        }
                    }
                }
                m.red_players = m.red_players.iter().map(|p| resolve_pilot(ctx, snap, p.character_id)).collect();
                m.blue_players = m.blue_players.iter().map(|p| resolve_pilot(ctx, snap, p.character_id)).collect();
                for roster in [&m.red_players, &m.blue_players] {
                    for p in roster {
                        if is_ship_type_banned(m, p.ship_type_id) {
                            return Err(BridgeRpcError {
                                code: "TourneyShipTypeBanned".into(),
                                message: format!("Ship type {} is banned in this tournament match", p.ship_type_id),
                            });
                        }
                    }
                    let pts: i64 = roster.iter().map(|p| p.points).sum();
                    if m.max_points > 0 && pts > m.max_points {
                        return Err(BridgeRpcError {
                            code: "TourneyMaxPointsExceeded".into(),
                            message: format!("Team point total {pts} exceeds match max_points {}", m.max_points),
                        });
                    }
                }
                m.fleet_details_updated = true;
                save_store(snap, &store, now_unix);
                Ok(PyRep::Bool(true))
            })
        }
        _ => return None,
    };

    Some(res)
}

#[derive(Debug, Clone, Copy)]
enum TourneyRestriction {
    Warp,
    Drone,
    Lock,
    Move,
}

fn check_tourney_restriction(
    ctx: &ServerContext,
    solarsystem_id: u32,
    char_id: i64,
    kind: TourneyRestriction,
) -> Result<(), BridgeRpcError> {
    maybe_sync_expired_countdowns(ctx, ctx.now_unix());
    ctx.db.with_snapshot(|snap| {
        if !snap.owner_notes.get(&TOURNEY_STORE_KEY).is_some_and(|n| n.character_id != 0) {
            return Ok(());
        }
        let store = load_store(snap);
        for m in store.matches.values() {
            if m.solar_system_id == solarsystem_id as i64
                && matches!(m.state, TourneyMatchState::Countdown | TourneyMatchState::InProgress)
            {
                let is_participant = m.red_players.iter().any(|p| p.character_id == char_id)
                    || m.blue_players.iter().any(|p| p.character_id == char_id);
                if is_participant {
                    let (restricted, code, msg) = match kind {
                        TourneyRestriction::Warp => (m.warp_restrict, "WarpRestrictedInTourney", "Warping is restricted for tournament participants in arena"),
                        TourneyRestriction::Drone => (m.drone_restrict, "DroneRestrictedInTourney", "Drone launch is restricted for tournament participants in arena"),
                        TourneyRestriction::Lock => (m.lock_restrict, "LockRestrictedInTourney", "Target locking is restricted for tournament participants in arena"),
                        TourneyRestriction::Move => (m.move_restrict, "MoveRestrictedInTourney", "Ship movement is restricted for tournament participants in arena"),
                    };
                    if restricted {
                        return Err(BridgeRpcError { code: code.into(), message: msg.into() });
                    }
                }
            }
        }
        Ok(())
    })
}

pub fn check_tourney_warp_restricted(ctx: &ServerContext, solarsystem_id: u32, char_id: i64) -> Result<(), BridgeRpcError> {
    check_tourney_restriction(ctx, solarsystem_id, char_id, TourneyRestriction::Warp)
}

pub fn check_tourney_drone_restricted(ctx: &ServerContext, solarsystem_id: u32, char_id: i64) -> Result<(), BridgeRpcError> {
    check_tourney_restriction(ctx, solarsystem_id, char_id, TourneyRestriction::Drone)
}

pub fn check_tourney_lock_restricted(ctx: &ServerContext, solarsystem_id: u32, char_id: i64) -> Result<(), BridgeRpcError> {
    check_tourney_restriction(ctx, solarsystem_id, char_id, TourneyRestriction::Lock)
}

pub fn check_tourney_move_restricted(ctx: &ServerContext, solarsystem_id: u32, char_id: i64) -> Result<(), BridgeRpcError> {
    check_tourney_restriction(ctx, solarsystem_id, char_id, TourneyRestriction::Move)
}

