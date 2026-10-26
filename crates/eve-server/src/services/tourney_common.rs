//! `tourney_common` — Shared types and helpers for Tournament Manager (`tourneyMgr`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

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

pub(crate) fn sync_match_countdown(m: &mut TourneyMatch, now_unix: u64) -> bool {
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

pub(crate) fn resolve_team_name(snap: &eve_db::WorldStateSnapshot, team_id: i64, capt_id: Option<i64>) -> (i64, String) {
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
    /// Registered tournaments: tourneyID -> display name (`GetTourneySetup` element 0).
    #[serde(default)]
    pub tournaments: std::collections::BTreeMap<i64, String>,
    /// Registered arena systems: solarSystemID -> beacons/bubbles (`GetTourneySetup` element 1).
    #[serde(default)]
    pub arena_systems: std::collections::BTreeMap<i64, TourneyArenaSystem>,
}

/// Arena grid furniture for one tournament solar system, as unpacked by the V24 referee tool
/// (`tournamentRefereeTools.pyj`: `for systemID, (_combatBeacons, _warpBubbles) in tourneySystems.iteritems()`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TourneyArenaSystem {
    pub combat_beacons: Vec<i64>,
    pub warp_bubbles: Vec<i64>,
}

/// Registers (or updates) a tournament and its arena system in the persisted tourney store.
/// The arena system must exist in the SDE; returns its SDE solar system name.
pub fn register_tourney_setup(
    ctx: &ServerContext,
    tourney_id: i64,
    tourney_name: &str,
    arena_system_id: i64,
    arena: TourneyArenaSystem,
) -> Result<String, BridgeRpcError> {
    let sys_name = u32::try_from(arena_system_id)
        .ok()
        .and_then(|sid| ctx.sde.solar_systems.get(&sid))
        .map(|s| s.name.clone())
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidSolarSystem".into(),
            message: format!("Arena solar system {arena_system_id} not found in SDE"),
        })?;
    if tourney_id <= 0 || tourney_name.trim().is_empty() {
        return Err(BridgeRpcError { code: "InvalidTournament".into(), message: "Tournament needs a positive ID and a name".into() });
    }
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        store.tournaments.insert(tourney_id, tourney_name.to_string());
        store.arena_systems.insert(arena_system_id, arena);
        save_store(snap, &store, now_unix);
    });
    Ok(sys_name)
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

pub(crate) fn maybe_sync_expired_countdowns(ctx: &ServerContext, now_unix: u64) {
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

pub(crate) fn extract_id(args: &[PyRep], idx: usize) -> Option<i64> { args.get(idx).and_then(|a| a.as_i64()) }

pub(crate) fn extract_bool(args: &[PyRep], idx: usize) -> bool {
    args.get(idx).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(false)
}

pub(crate) fn extract_list_i64(rep: Option<&PyRep>) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) = rep {
        for it in items { if let Some(v) = it.as_i64() { out.push(v); } }
    } else if let Some(PyRep::Integer(v)) = rep {
        out.push(*v);
    }
    out
}

/// Verifies that the caller has GM role or is registered in the tournament match referee roster.
pub(crate) fn check_referee_auth(session: &SessionState, caller_char_id: i64, m: &TourneyMatch) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() || m.referees.contains(&caller_char_id) {
        Ok(())
    } else {
        Err(BridgeRpcError { code: "AccessDenied".into(), message: "Caller is not an authorized tournament referee or GM".into() })
    }
}

// DERIVED: Alliance Tournament ship hull point values mapped from SDE ship group_id.
pub(crate) fn compute_ship_points(ctx: &ServerContext, ship_type_id: i64) -> i64 {
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
pub(crate) fn compute_team_telemetry(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, players: &[TourneyPilot], score: i64, bans_count: usize) -> PyRep {
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

pub(crate) fn resolve_pilot(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, char_id: i64) -> TourneyPilot {
    let (ship_id, ship_type_id) = snap.characters.get(&char_id)
        .and_then(|ch| (ch.active_ship_id > 0).then_some(ch.active_ship_id))
        .and_then(|sid| snap.items.get(&sid).map(|it| (sid, i64::from(it.type_id))))
        .unwrap_or((0, 0));
    let points = if ship_type_id > 0 { compute_ship_points(ctx, ship_type_id) } else { 0 };
    TourneyPilot { character_id: char_id, ship_type_id, ship_id, points }
}

pub(crate) fn is_ship_type_banned(m: &TourneyMatch, ship_type_id: i64) -> bool {
    ship_type_id > 0 && (m.banned_ships.contains(&ship_type_id) || m.banned_ship_types_red.contains(&ship_type_id) || m.banned_ship_types_blue.contains(&ship_type_id))
}

pub(crate) fn validate_pilot_for_team(m: &mut TourneyMatch, which_team: i64, pilot: &TourneyPilot) -> Result<(), BridgeRpcError> {
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

pub(crate) fn resolve_valid_home_station(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, home_st_id: i64, fallback_sys: u32) -> (i64, u32) {
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
