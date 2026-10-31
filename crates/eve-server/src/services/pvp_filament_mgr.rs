//! `pvp_filament_mgr` — Proving Grounds / Abyssal PvP Filament manager (`pvpFilamentMgr`).
//!
//! Handles matchmaking queue registration, active/scheduled Proving Grounds events,
//! leaderboards, character statistics, and delegates multi-team combat resolution
//! and Abyssal end-gate extraction to `pvp_filament_arena`.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::{
    check_filament_1000km_proximity, resolve_character_home_station_and_system,
};
use crate::services::pvp_filament_arena::{
    are_opposing_in_match, handle_pvp_end_gate_activation, match_contains_char,
    notify_pvp_stats_for_pilots, record_proving_grounds_combat_result_in_snap,
    spawn_matched_arena_instance,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

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
    pub queue: HashMap<i64, PvpQueueCharacterEntry>,
    pub stats: HashMap<i64, (i64, i64, i64, i64, f64)>,
    #[serde(default)]
    pub event_stats: HashMap<i64, HashMap<i64, (i64, i64, i64, i64, f64)>>,
    #[serde(default)]
    pub events: Vec<PvpEventSchedule>,
    #[serde(default)]
    pub seeded_default_events: bool,
    #[serde(default)]
    pub active_matches: HashMap<i64, PvpArenaMatchInstance>,
    #[serde(default)]
    pub match_teams: HashMap<i64, Vec<Vec<i64>>>,
    #[serde(default)]
    pub match_eliminated: HashMap<i64, HashSet<i64>>,
    #[serde(default)]
    pub match_arena_offsets: HashMap<i64, [f64; 3]>,
    #[serde(default)]
    pub match_filament_types: HashMap<i64, u32>,
    #[serde(default)]
    pub hazard_ticks: HashMap<i64, u32>,
    #[serde(default)]
    pub queue_refund_targets: HashMap<i64, (i64, i64)>,
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
            let Ok(dates) = sde.load_cfsd_table("res:/staticdata/pvpfilamenteventdates.fsdbinary")
            else {
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
                let type_ids = if filament_type_id > 0 {
                    vec![filament_type_id]
                } else {
                    Vec::new()
                };
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

pub fn ensure_events_from_sde(sde: &eve_sde::SdeRegistry, store: &mut PvpFilamentStore) {
    if !store.seeded_default_events && store.events.is_empty() {
        store.seeded_default_events = true;
        store.events = load_sde_pvp_events(sde);
    }
}

pub fn lookup_sde_event_ids(sde: &eve_sde::SdeRegistry, event_id: i64) -> Option<(i64, i64)> {
    load_sde_pvp_events(sde)
        .into_iter()
        .find(|e| e.event_id == event_id)
        .map(|e| (e.match_type_id, e.schedule_id))
}

#[derive(Debug, Clone)]
pub struct PvpFilamentMatchSpec {
    pub fleet_size: usize,
    pub number_of_fleets: usize,
    pub ship_restrictions_list_id: Option<u32>,
    pub max_meta_level: u32,
    pub banned_modules_list_id: Option<u32>,
    pub limited_modules_list_id: Option<u32>,
    pub number_of_limited_modules: usize,
    pub rewards: Vec<(u32, u32)>,
}

pub fn get_pvp_filament_match_spec(
    sde: &eve_sde::SdeRegistry,
    filament_type_id: u32,
) -> PvpFilamentMatchSpec {
    static SDE_PVP_MATCH_SPECS: std::sync::OnceLock<HashMap<u32, PvpFilamentMatchSpec>> =
        std::sync::OnceLock::new();
    let specs = SDE_PVP_MATCH_SPECS.get_or_init(|| {
        let match_types = sde
            .load_cfsd_nested("res:/staticdata/pvpfilamentmatchtypes.fsdbinary")
            .unwrap_or_default();
        let mut map = HashMap::new();
        for (_mt_id, val) in match_types {
            let fid = val.get("filamentTypeID").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if fid == 0 {
                continue;
            }
            let fleet_size = val.get("fleetSize").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
            let number_of_fleets = val
                .get("numberOfFleets")
                .and_then(|v| v.as_u64())
                .unwrap_or(2)
                .max(2) as usize;
            let ship_restrictions_list_id = val
                .get("shipRestrictions")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .filter(|&v| v > 0);
            let raw_meta = val
                .get("moduleAndImplantMetaLevelRestrictions")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as u32;
            let max_meta_level = if raw_meta == 0 { 5 } else { raw_meta };
            let banned_modules_list_id = val
                .get("bannedModulesTypeListID")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .filter(|&v| v > 0);
            let limited_modules_list_id = val
                .get("limitedAmountOfModulesTypeListID")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .filter(|&v| v > 0);
            let number_of_limited_modules = val
                .get("numberOfLimitedModules")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let mut rewards = Vec::new();
            if let Some(arr) = val.get("rewards").and_then(|v| v.as_array()) {
                for entry in arr {
                    let tid = entry.get("typeID").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let amt = entry.get("amount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    if tid > 0 && amt > 0 {
                        rewards.push((tid, amt));
                    }
                }
            }
            map.insert(
                fid,
                PvpFilamentMatchSpec {
                    fleet_size,
                    number_of_fleets,
                    ship_restrictions_list_id,
                    max_meta_level,
                    banned_modules_list_id,
                    limited_modules_list_id,
                    number_of_limited_modules,
                    rewards,
                },
            );
        }
        map
    });
    if let Some(spec) = specs.get(&filament_type_id) {
        return spec.clone();
    }
    let name = sde
        .types
        .get(&filament_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("");
    let (fleet_size, number_of_fleets) = if name.contains("3v3") {
        (3, 2)
    } else if name.contains("2v2") {
        (2, 2)
    } else if name.contains("FFA") || name.contains("1v1v1v1") {
        (1, 4)
    } else {
        (1, 2)
    };
    PvpFilamentMatchSpec {
        fleet_size,
        number_of_fleets,
        ship_restrictions_list_id: None,
        max_meta_level: 5,
        banned_modules_list_id: None,
        limited_modules_list_id: None,
        number_of_limited_modules: 0,
        rewards: Vec::new(),
    }
}

fn validate_pilot_ship_modules_and_implants(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    pilot_id: i64,
    ship_id: i64,
    filament_type_id: u32,
    spec: &PvpFilamentMatchSpec,
) -> Result<(), BridgeRpcError> {
    let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
    if ship_type_id == 0 || !is_ship_allowed_for_pvp_filament(sde, filament_type_id, ship_type_id) {
        return Err(BridgeRpcError {
            code: "InvalidShipForFilament".into(),
            message: format!(
                "Ship type {ship_type_id} is not permitted by Proving Grounds filament {filament_type_id}"
            ),
        });
    }
    let max_meta = f64::from(spec.max_meta_level);
    let mut limited_count = 0_usize;
    for fit_it in snap.items.values() {
        if fit_it.location_id != ship_id
            || !matches!(fit_it.flag as u16, 11..=34 | 92..=99 | 125..=132 | 164..=171)
        {
            continue;
        }
        if let Some(t) = sde.types.get(&fit_it.type_id) {
            let meta = t
                .default_attributes
                .get(&633)
                .copied()
                .or_else(|| t.meta_level.map(|m| m as f64))
                .unwrap_or(0.0);
            if meta > max_meta {
                return Err(BridgeRpcError {
                    code: "InvalidModuleTierForFilament".into(),
                    message: format!(
                        "Fitted module {} (meta {meta:.0}) exceeds Proving Grounds limit (<= {})",
                        t.name, spec.max_meta_level
                    ),
                });
            }
            if let Some(ban_lid) = spec.banned_modules_list_id {
                if sde.type_lists.get(&ban_lid).is_some_and(|tl| tl.contains(t)) {
                    return Err(BridgeRpcError {
                        code: "BannedModuleForFilament".into(),
                        message: format!("Fitted module {} is banned in this Proving Grounds event", t.name),
                    });
                }
            }
            if let Some(lim_lid) = spec.limited_modules_list_id {
                if sde.type_lists.get(&lim_lid).is_some_and(|tl| tl.contains(t)) {
                    limited_count += 1;
                    if limited_count > spec.number_of_limited_modules.max(1) {
                        return Err(BridgeRpcError {
                            code: "InvalidModuleTierForFilament".into(),
                            message: "Fitted modules exceed Proving Grounds limited-module cap".into(),
                        });
                    }
                }
            }
        }
    }
    let mut implant_type_ids: Vec<u32> = snap
        .active_implants
        .get(&pilot_id)
        .cloned()
        .unwrap_or_default();
    for imp in snap.items.values() {
        if (imp.location_id == pilot_id || imp.owner_id == pilot_id)
            && imp.flag == eve_db::InventoryFlag::Implant
            && !implant_type_ids.contains(&imp.type_id)
        {
            implant_type_ids.push(imp.type_id);
        }
    }
    for imp_tid in implant_type_ids {
        let Some(t) = sde.types.get(&imp_tid) else {
            return Err(BridgeRpcError {
                code: "InvalidImplantsForFilament".into(),
                message: format!("Unknown implant {imp_tid} is not permitted in Proving Grounds"),
            });
        };
        let imp_meta = t
            .default_attributes
            .get(&633)
            .copied()
            .or_else(|| t.meta_level.map(|m| m as f64))
            .unwrap_or(1.0);
        let banned_imp = spec
            .banned_modules_list_id
            .and_then(|lid| sde.type_lists.get(&lid))
            .is_some_and(|tl| tl.contains(t));
        if spec.max_meta_level == 0 || imp_meta > max_meta || banned_imp {
            return Err(BridgeRpcError {
                code: "InvalidImplantsForFilament".into(),
                message: format!(
                    "Implant {} (meta {imp_meta:.0}) violates Proving Grounds implant restrictions",
                    t.name
                ),
            });
        }
    }
    Ok(())
}

fn is_ship_allowed_for_pvp_filament(
    sde: &eve_sde::SdeRegistry,
    filament_type_id: u32,
    ship_type_id: u32,
) -> bool {
    let Some(ship_ty) = sde.types.get(&ship_type_id) else {
        return false;
    };
    let ship_grp = ship_ty.group_id;
    if ship_type_id == 670
        || matches!(
            ship_grp,
            29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 941 | 1538
        )
    {
        return false;
    }
    let spec = get_pvp_filament_match_spec(sde, filament_type_id);
    if let Some(list_id) = spec.ship_restrictions_list_id {
        if let Some(type_list) = sde.type_lists.get(&list_id) {
            return type_list.contains(ship_ty);
        }
    }
    let fil_name = sde
        .types
        .get(&filament_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("");
    if fil_name.contains("Battleship") {
        matches!(ship_grp, 27 | 898 | 900)
    } else if fil_name.contains("Battlecruiser") {
        matches!(ship_grp, 419 | 540 | 1201)
    } else if fil_name.contains("Cruiser") {
        matches!(ship_grp, 26 | 358 | 832 | 833 | 894 | 906 | 963 | 1972)
    } else if fil_name.contains("Destroyer") {
        matches!(ship_grp, 420 | 541 | 1305 | 1534)
    } else if fil_name.contains("Frigate") {
        matches!(ship_grp, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527)
    } else {
        true
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
        (
            "typeIDs",
            PyRep::List(e.type_ids.iter().copied().map(PyRep::Integer).collect()),
        ),
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
        snap.service_state_blobs
            .insert("pvp_filament_store".into(), json.clone());
        snap.service_state_blobs
            .insert("pvp_filament:global".into(), json);
    }
}

fn get_stats_source(
    store: &PvpFilamentStore,
    event_id: i64,
) -> &HashMap<i64, (i64, i64, i64, i64, f64)> {
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
        .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0 || st.3 > 0).then_some((cid, st)))
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

pub fn build_leaderboard_rows_dict_for_event(store: &PvpFilamentStore, event_id: i64) -> Vec<PyRep> {
    let stats_map = get_stats_source(store, event_id);
    let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = stats_map
        .iter()
        .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0 || st.3 > 0).then_some((cid, st)))
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
            PyRep::Dict(vec![
                (PyRep::String("character_id".into()), PyRep::Integer(cid)),
                (PyRep::String("rank".into()), PyRep::Integer((idx + 1) as i64)),
                (PyRep::String("score".into()), PyRep::Integer(score)),
                (PyRep::String("wins".into()), PyRep::Integer(wins)),
                (PyRep::String("losses".into()), PyRep::Integer(losses)),
                (PyRep::String("draws".into()), PyRep::Integer(0)),
            ])
        })
        .collect()
}

pub fn build_character_statistics_dict_for_event(
    store: &PvpFilamentStore,
    event_id: i64,
    char_id: i64,
) -> PyRep {
    let stats_map = get_stats_source(store, event_id);
    let (w, l, s, k, d) = stats_map.get(&char_id).copied().unwrap_or((0, 0, 0, 0, 0.0));
    let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = stats_map
        .iter()
        .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0 || st.3 > 0).then_some((cid, st)))
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
    PyRep::Dict(vec![
        (PyRep::String("character_id".into()), PyRep::Integer(char_id)),
        (PyRep::String("rank".into()), PyRep::Integer(rank)),
        (PyRep::String("score".into()), PyRep::Integer(s)),
        (PyRep::String("wins".into()), PyRep::Integer(w)),
        (PyRep::String("losses".into()), PyRep::Integer(l)),
        (PyRep::String("draws".into()), PyRep::Integer(0)),
        (PyRep::String("kills".into()), PyRep::Integer(k)),
        (PyRep::String("damageDealt".into()), PyRep::Real(d)),
    ])
}

pub fn build_character_statistics(store: &PvpFilamentStore, char_id: i64) -> PyRep {
    build_character_statistics_for_event(store, 0, char_id)
}

pub fn build_character_statistics_for_event(
    store: &PvpFilamentStore,
    event_id: i64,
    char_id: i64,
) -> PyRep {
    let stats_map = get_stats_source(store, event_id);
    let (w, l, s, k, d) = stats_map.get(&char_id).copied().unwrap_or((0, 0, 0, 0, 0.0));
    let mut rows: Vec<(i64, (i64, i64, i64, i64, f64))> = stats_map
        .iter()
        .filter_map(|(&cid, &st)| (st.0 > 0 || st.1 > 0 || st.2 > 0 || st.3 > 0).then_some((cid, st)))
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

pub fn record_proving_grounds_combat_result(
    ctx: &ServerContext,
    winner_char_id: i64,
    loser_char_id: i64,
    damage_done: f64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let (event_id, affected_pilots) = ctx.db.try_with_mut_snapshot(|snap| {
        record_proving_grounds_combat_result_in_snap(
            snap,
            Some(&ctx.sde),
            winner_char_id,
            loser_char_id,
            damage_done,
            now_unix,
        )
    })?;
    notify_pvp_stats_for_pilots(ctx, event_id, &affected_pilots);
    Ok(())
}

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
        let store = load_pvp_store(snap);
        store.active_matches.values().any(|m| {
            m.active
                && m.arena_solar_system_id == kill_solar_system_id
                && are_opposing_in_match(&store, m, attacker_char_id, victim_char_id)
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
            let Some(cur_sys_for_check) = session.solar_system_id else {
                return Some(Err(BridgeRpcError::user_error("NoSolarSystem")));
            };
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

            // Pre-check item ownership/cargo BEFORE proximity so invalid item/cargo errors stay prioritized
            let pre_err = ctx.db.with_snapshot(|snap| -> Option<BridgeRpcError> {
                let Some(active_ship_id) = session.ship_id else {
                    return Some(BridgeRpcError::user_error("MustBeInSpace"));
                };
                if let Some(ship_it) = snap.items.get(&active_ship_id) {
                    let ship_grp = ctx.sde.types.get(&ship_it.type_id).map(|t| t.group_id).unwrap_or(0);
                    if ship_it.type_id == 670 || matches!(ship_grp, 29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 941 | 1538) {
                        return Some(BridgeRpcError {
                            code: "InvalidShipForFilament".into(),
                            message: "Capsules and capital/freighter hulls cannot enter Proving Grounds arenas".into(),
                        });
                    }
                }
                let Some(it) = snap.items.get(&item_id) else {
                    return Some(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: "Filament item not found in inventory".into(),
                    });
                };
                if it.owner_id != char_id {
                    return Some(BridgeRpcError {
                        code: "ItemNotOwned".into(),
                        message: "Filament item is not owned by pilot".into(),
                    });
                }
                if it.location_id != active_ship_id || it.flag != eve_db::InventoryFlag::Cargo {
                    return Some(BridgeRpcError::user_error("ItemNotInCargo"));
                }
                None
            });
            if let Some(e) = pre_err {
                return Some(Err(e));
            }
            if let Err(e) = check_filament_1000km_proximity(
                ctx,
                cur_sys_for_check,
                DVec3::from_array(caller_origin_pos),
            )
            .await
            {
                return Some(Err(e));
            }

            let fleet_members_info: Vec<(i64, i64, u32)> = ctx.db.with_snapshot(|snap| {
                let Some(fl) = snap.fleets.values().find(|f| f.members.contains(&char_id)) else {
                    return Vec::new();
                };
                if fl.members.len() <= 1 {
                    return Vec::new();
                }
                let mut members = Vec::with_capacity(fl.members.len());
                members.push((char_id, cur_ship_for_check, cur_sys_for_check));
                for &mcid in &fl.members {
                    if mcid == char_id {
                        continue;
                    }
                    let (mship, msys) = snap
                        .characters
                        .get(&mcid)
                        .filter(|c| c.station_id.is_none() && c.structure_id.is_none())
                        .map(|c| (c.active_ship_id, c.solar_system_id))
                        .unwrap_or((0, 0));
                    members.push((mcid, mship, msys));
                }
                members
            });
            for &(mcid, mship, msys) in &fleet_members_info {
                if mcid == char_id {
                    continue;
                }
                if msys != cur_sys_for_check || mship <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "FleetMemberNotInSystem".into(),
                        message: "All fleet members must be undocked in the same solar system to queue".into(),
                    }));
                }
                if let Err(e) = crate::sp6_lifecycle::require_ship_ready_for_filament_jump(
                    ctx, mcid, mship, msys,
                ) {
                    return Some(Err(e));
                }
            }
            let sys_handle_origin = ctx.systems.get_or_spawn(cur_sys_for_check);
            let mut party_origins: Vec<(i64, i64, [f64; 3])> = Vec::new();
            if fleet_members_info.is_empty() {
                party_origins.push((char_id, cur_ship_for_check, caller_origin_pos));
            } else {
                for &(mcid, mship, _) in &fleet_members_info {
                    let pos = sys_handle_origin
                        .get_ball_position_sync(mship)
                        .map(|p| [p.x, p.y, p.z])
                        .or_else(|| ctx.db.with_snapshot(|s| s.item_positions.get(&mship).copied()))
                        .unwrap_or(caller_origin_pos);
                    party_origins.push((mcid, mship, pos));
                }
            }

            let queued_candidates: Vec<(i64, i64, u32)> = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                store
                    .queue
                    .iter()
                    .filter_map(|(&cid, qe)| {
                        if party_origins.iter().any(|&(pcid, _, _)| pcid == cid) {
                            return None;
                        }
                        let opp_ch = snap.characters.get(&cid)?;
                        if opp_ch.station_id.is_some()
                            || opp_ch.structure_id.is_some()
                            || opp_ch.active_ship_id <= 0
                        {
                            return None;
                        }
                        let opp_sys_id = if qe.solar_system_id > 0 {
                            qe.solar_system_id
                        } else {
                            opp_ch.solar_system_id
                        };
                        Some((cid, opp_ch.active_ship_id, opp_sys_id))
                    })
                    .collect()
            });
            let ready_opponents: HashSet<i64> = queued_candidates
                .into_iter()
                .filter(|&(cid, ship_id, sys_id)| {
                    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(
                        ctx, cid, ship_id, sys_id,
                    )
                    .is_ok()
                })
                .map(|(cid, _, _)| cid)
                .collect();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut store = load_pvp_store(snap);
                for &(pcid, _, _) in &party_origins {
                    if store.queue.contains_key(&pcid) {
                        return Err(BridgeRpcError {
                            code: "AlreadyInPvpQueue".into(),
                            message: "Character or fleet member is already queued for a PvP filament event".into(),
                        });
                    }
                    if cur_sys_for_check == 32_000_001
                        || store.active_matches.values().any(|m| {
                            m.active
                                && match_contains_char(&store, m, pcid)
                                && !m.exited_pilots.contains(&pcid)
                        })
                    {
                        return Err(BridgeRpcError {
                            code: "AlreadyInPvpMatch".into(),
                            message: "Character or fleet member is already inside an active Proving Grounds arena".into(),
                        });
                    }
                }
                let _active_ship_id = session
                    .ship_id
                    .ok_or_else(|| BridgeRpcError::user_error("MustBeInSpace"))?;
                let Some(it) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: "Filament item not found in inventory".into(),
                    });
                };
                if type_id > 0 && it.type_id != type_id as u32 {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: "Item type mismatch".into(),
                    });
                }
                let it_type_id = it.type_id;
                let it_location_id = it.location_id;
                let it_flag = it.flag as u16;

                let now_i64 = now_unix as i64;
                ensure_events_from_sde(&ctx.sde, &mut store);
                let active_event = store.events.iter().find(|ev| {
                    ev.start_unix <= now_i64
                        && now_i64 <= ev.end_unix
                        && ev.type_ids.contains(&i64::from(it_type_id))
                });
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
                            .filter(|ev| ev.type_ids.contains(&i64::from(it_type_id)))
                            .max_by_key(|ev| (ev.end_unix, ev.event_id))
                            .map(|ev| ev.event_id)
                    })
                    .unwrap_or(171);

                let in_inactive_only = any_event_currently_active
                    && store.events.iter().any(|ev| {
                        (ev.end_unix < now_i64 || ev.start_unix > now_i64)
                            && ev.type_ids.contains(&i64::from(it_type_id))
                    })
                    && !in_active_event;
                let is_sde_pvp_filament = ctx
                    .sde
                    .types
                    .get(&it_type_id)
                    .map(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
                    .unwrap_or(false);
                if !is_sde_pvp_filament {
                    return Err(BridgeRpcError {
                        code: "InvalidFilamentType".into(),
                        message: format!("Item type {it_type_id} is not a valid PvP filament"),
                    });
                }
                if in_inactive_only {
                    return Err(BridgeRpcError {
                        code: "EventNotActive".into(),
                        message: format!(
                            "Proving Grounds event for filament type {it_type_id} is not currently active"
                        ),
                    });
                }
                let match_spec = get_pvp_filament_match_spec(&ctx.sde, it_type_id);
                if party_origins.len() > match_spec.fleet_size {
                    return Err(BridgeRpcError {
                        code: "FleetTooLargeForFilament".into(),
                        message: format!(
                            "Fleet size {} exceeds Proving Grounds filament limit {}",
                            party_origins.len(),
                            match_spec.fleet_size
                        ),
                    });
                }
                for &(pcid, pship, _) in &party_origins {
                    validate_pilot_ship_modules_and_implants(
                        &ctx.sde,
                        snap,
                        pcid,
                        pship,
                        it_type_id,
                        &match_spec,
                    )?;
                }

                let total_required_pilots = (match_spec.fleet_size * match_spec.number_of_fleets).max(2);
                let needed_others = total_required_pilots.saturating_sub(party_origins.len());

                if let Some(it) = snap.items.get_mut(&item_id) {
                    if it.quantity > 1 {
                        it.quantity -= 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                let mut matched_opponents: Vec<(i64, u32, i64, u64)> = store
                    .queue
                    .iter()
                    .filter_map(|(&cid, qe)| {
                        if party_origins.iter().any(|&(pcid, _, _)| pcid == cid)
                            || qe.type_id != it_type_id
                        {
                            return None;
                        }
                        let opp_ch = snap.characters.get(&cid)?;
                        if opp_ch.station_id.is_some()
                            || opp_ch.structure_id.is_some()
                            || opp_ch.active_ship_id <= 0
                        {
                            return None;
                        }
                        let opp_ship_ty = snap
                            .items
                            .get(&opp_ch.active_ship_id)
                            .map(|i| i.type_id)
                            .unwrap_or(0);
                        if !is_ship_allowed_for_pvp_filament(&ctx.sde, it_type_id, opp_ship_ty) {
                            return None;
                        }
                        if !ready_opponents.contains(&cid) {
                            return None;
                        }
                        Some((cid, qe.solar_system_id, qe.location_id, qe.joined_unix))
                    })
                    .collect();
                matched_opponents.sort_by_key(|&(cid, _, _, joined)| (joined, cid));

                for &(pcid, _, ppos) in &party_origins {
                    store.origin_positions.insert(pcid, ppos);
                    store.stats.entry(pcid).or_insert((0, 0, 0, 0, 0.0));
                }
                let match_data = if matched_opponents.len() >= needed_others {
                    matched_opponents.truncate(needed_others);
                    for &(opp_id, _, _, _) in &matched_opponents {
                        if let Some(qe) = store.queue.remove(&opp_id) {
                            store.queue_refund_targets.remove(&qe.item_id);
                        }
                    }
                    store.next_match_id += 1;
                    let mid = store.next_match_id;
                    let end_gate_id = 70_000 + mid;

                    let used_slots: HashSet<i64> = store
                        .match_arena_offsets
                        .values()
                        .map(|off| ((off[0] - 50_000_000.0) / 5_000_000.0).round() as i64)
                        .collect();
                    let slot = (0_i64..10_000).find(|s| !used_slots.contains(s)).unwrap_or(0);
                    let arena_offset = [50_000_000.0 + (slot as f64) * 5_000_000.0, 0.0, 0.0];

                    let mut origin_systems = HashMap::new();
                    let mut participants: Vec<(i64, i64, u32)> =
                        Vec::with_capacity(total_required_pilots);
                    for &(opp_id, opp_sys, opp_loc, _) in &matched_opponents {
                        let opp_origin_sys = if opp_sys > 0 {
                            opp_sys
                        } else {
                            resolve_character_home_station_and_system(snap, &ctx.sde, opp_id).1
                        };
                        origin_systems.insert(opp_id, opp_origin_sys);
                        let opp_ship_id = snap
                            .characters
                            .get(&opp_id)
                            .map(|c| c.active_ship_id)
                            .filter(|&s| s > 0)
                            .unwrap_or(opp_loc);
                        participants.push((opp_id, opp_ship_id, opp_origin_sys));
                    }
                    for &(pcid, pship, _) in &party_origins {
                        origin_systems.insert(pcid, cur_sys_for_check);
                        participants.push((pcid, pship, cur_sys_for_check));
                    }

                    let teams: Vec<Vec<i64>> = participants
                        .chunks(match_spec.fleet_size.max(1))
                        .map(|chunk| chunk.iter().map(|p| p.0).collect())
                        .collect();
                    let team_a = teams.first().cloned().unwrap_or_default();
                    let team_b: Vec<i64> = teams.iter().skip(1).flatten().copied().collect();

                    let match_inst = PvpArenaMatchInstance {
                        match_id: mid,
                        event_id: active_event_id,
                        arena_solar_system_id: 32_000_001,
                        team_a,
                        team_b,
                        origin_systems,
                        started_unix: now_unix,
                        active: true,
                        winner_char_id: None,
                        exited_pilots: HashSet::new(),
                        end_gate_id,
                    };
                    store.active_matches.insert(mid, match_inst);
                    store.match_teams.insert(mid, teams.clone());
                    store.match_eliminated.insert(mid, HashSet::new());
                    store.match_arena_offsets.insert(mid, arena_offset);
                    store.match_filament_types.insert(mid, it_type_id);

                    for &(pid, p_ship_id, _) in &participants {
                        if let Some(c) = snap.characters.get_mut(&pid) {
                            c.solar_system_id = 32_000_001;
                            c.station_id = None;
                            c.structure_id = None;
                        }
                        if let Some(sh) = snap.items.get_mut(&p_ship_id) {
                            sh.location_id = 32_000_001;
                        }
                    }

                    Some((mid, end_gate_id, arena_offset, teams, participants))
                } else {
                    store
                        .queue_refund_targets
                        .insert(item_id, (char_id, it_location_id));
                    for &(pcid, pship, _) in &party_origins {
                        store.queue.insert(
                            pcid,
                            PvpQueueCharacterEntry {
                                item_id,
                                type_id: it_type_id,
                                location_id: if pcid == char_id { it_location_id } else { pship },
                                flag: it_flag,
                                solar_system_id: cur_sys_for_check,
                                joined_unix: now_unix,
                            },
                        );
                    }
                    None
                };

                save_pvp_store(snap, &store);
                Ok(match_data)
            });

            match res {
                Ok(Some((mid, end_gate_id, arena_offset, teams, participants))) => {
                    let char_ids: Vec<i64> = participants.iter().map(|(c, _, _)| *c).collect();
                    for &cid in &char_ids {
                        crate::services::notify_emitters_batch3_space::emit_pvp_match_found_and_expiry(
                            ctx,
                            cid,
                            ctx.now_unix().saturating_add(900),
                        );
                    }
                    spawn_matched_arena_instance(
                        ctx,
                        session,
                        mid,
                        end_gate_id,
                        arena_offset,
                        teams,
                        participants,
                    )
                    .await;
                    Some(Ok(PyRep::Integer(5)))
                }
                Ok(None) => Some(Ok(PyRep::Integer(30))),
                Err(e) => {
                    crate::services::notify_emitters_batch3_space::emit_pvp_matchmaking_failure(
                        ctx,
                        char_id,
                        1,
                    );
                    Some(Err(e))
                }
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
                if entry.item_id > 0 {
                    store.queue.retain(|_, qe| qe.item_id != entry.item_id);
                }
                let (refund_owner, refund_loc) = store
                    .queue_refund_targets
                    .remove(&entry.item_id)
                    .unwrap_or((char_id, entry.location_id));
                if let Some(it) = snap
                    .items
                    .get_mut(&entry.item_id)
                    .filter(|it| it.owner_id == refund_owner && it.type_id == entry.type_id)
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
                            owner_id: refund_owner,
                            location_id: refund_loc,
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
                let dict = store
                    .events
                    .iter()
                    .map(|e| (PyRep::Integer(e.event_id), event_to_keyval(e)))
                    .collect();
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
            Some(Ok(PyRep::List(entries)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let event_id = args.first().and_then(|a| a.as_i64()).filter(|&e| e > 0).unwrap_or(171);
            let target_cid = args
                .get(1)
                .and_then(|a| a.as_i64())
                .filter(|&c| c > 0)
                .unwrap_or(char_id);
            let stats = ctx.db.with_snapshot(|snap| {
                let store = load_pvp_store(snap);
                build_character_statistics_for_event(&store, event_id, target_cid)
            });
            Some(Ok(stats))
        }

        ("pvpFilamentMgr", "AbyssalPVPEndGateActivation") => {
            Some(handle_pvp_end_gate_activation(ctx, session, char_id, args, now_unix).await)
        }

        _ => None,
    }
}
