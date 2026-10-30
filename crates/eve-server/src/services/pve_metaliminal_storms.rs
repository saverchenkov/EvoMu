//! `pve_metaliminal_storms` — Metaliminal Roaming Storms simulation, stargate propagation,
//! system-wide environment dogma effects, and bonus exploration/mining site spawning
//! (`feat:pve.metaliminal-storms`, `feat:pve.metaliminal-storm-movement`, `rpc:map.GetAllRoamingWeatherSystems`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    spec_now_sec, with_pve_special_reg, AbyssalWeatherKind, PveSpecialSitesRegistry,
};
use eve_db::{CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use std::collections::{HashMap, HashSet};

/// Minimum storm center movement cadence: 24 hours (`86,400s`).
pub const STORM_MOVE_MIN_SECS: u64 = 24 * 3_600;
/// Maximum storm center movement cadence: 48 hours (`172,800s`).
pub const STORM_MOVE_MAX_SECS: u64 = 48 * 3_600;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoamingStormRecord {
    pub storm_id: u32,
    pub weather: AbyssalWeatherKind,
    pub center_solar_system_id: SolarSystemId,
    pub strong_core_systems: Vec<SolarSystemId>,
    pub weak_periphery_systems: Vec<SolarSystemId>,
    pub step_count: u32,
    #[serde(default)]
    pub next_move_at_sec: u64,
    #[serde(default)]
    pub recent_centers: Vec<SolarSystemId>,
}

pub fn storm_weather_beacons(weather: AbyssalWeatherKind) -> (TypeId, TypeId) {
    match weather {
        AbyssalWeatherKind::Electrical => (47381, 47382),
        AbyssalWeatherKind::Exotic => (47384, 47385),
        AbyssalWeatherKind::Gamma => (47387, 47388),
        AbyssalWeatherKind::Firestorm => (47390, 47391),
        AbyssalWeatherKind::Dark => (47378, 47379),
    }
}

pub fn is_storm_beacon_type(tid: TypeId) -> bool {
    matches!(tid, 47378..=47392)
}

/// Returns `true` if `sys_id` is a known-space null-sec solar system (`security_status <= 0.0`).
pub fn is_nullsec_storm_system(sde: &SdeRegistry, sys_id: SolarSystemId) -> bool {
    if sys_id >= 30_100_000 {
        return false;
    }
    let Some(sys) = sde.solar_systems.get(&sys_id) else {
        return false;
    };
    matches!(sys.wormhole_class_id, None | Some(7..=9))
        && !matches!(sys.region_id, 10_000_004 | 10_000_017 | 10_000_019)
        && sys.security_status <= 0.0
}

fn nullsec_neighbors(sde: &SdeRegistry, sys_id: SolarSystemId) -> Vec<SolarSystemId> {
    let mut out: Vec<SolarSystemId> = sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.to_solar_system_id)
        .filter(|&ns| is_nullsec_storm_system(sde, ns))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Computes the storm footprint around `center_sys`:
/// - `core`: `center_sys` (0 jumps) + null-sec systems 1 jump away.
/// - `periphery`: null-sec systems 2–3 jumps away.
pub fn compute_storm_footprint(
    ctx: &ServerContext,
    center_sys: SolarSystemId,
) -> (Vec<SolarSystemId>, Vec<SolarSystemId>) {
    let mut visited: HashSet<SolarSystemId> = HashSet::new();
    visited.insert(center_sys);
    let mut frontier = vec![center_sys];

    let mut core = vec![center_sys];
    let mut periphery = Vec::new();

    for depth in 0..3u8 {
        let mut next_frontier = Vec::new();
        for &curr in &frontier {
            for next_sys in nullsec_neighbors(&ctx.sde, curr) {
                if visited.insert(next_sys) {
                    if depth == 0 {
                        core.push(next_sys);
                    } else {
                        periphery.push(next_sys);
                    }
                    next_frontier.push(next_sys);
                }
            }
        }
        frontier = next_frontier;
    }
    if core.len() > 1 {
        core[1..].sort_unstable();
    }
    periphery.sort_unstable();
    (core, periphery)
}

/// Deterministic semi-random movement interval strictly inside 24–48h (`90,000s..=169,200s`, i.e. 25h–47h).
pub fn storm_move_interval_secs(storm_id: u32, step_count: u32) -> u64 {
    let mix = (u64::from(storm_id).wrapping_mul(6_364_136_223_846_793_005))
        ^ (u64::from(step_count).wrapping_mul(1_442_695_040_888_963_407))
        ^ 0x9E37_79B9_7F4A_7C15;
    let extra_hours = (mix >> 32) % 23; // 0..=22 hours -> 25h..=47h (strictly within 24–48h)
    (25 + extra_hours) * 3_600
}

fn pick_non_retracing_neighbor(
    sde: &SdeRegistry,
    storm: &RoamingStormRecord,
) -> Option<SolarSystemId> {
    let candidates = nullsec_neighbors(sde, storm.center_solar_system_id);
    if candidates.is_empty() {
        return None;
    }
    // Prefer neighbors not in recent path history (avoids retracing recent jumps)
    let non_recent: Vec<SolarSystemId> = candidates
        .iter()
        .copied()
        .filter(|sid| !storm.recent_centers.contains(sid))
        .collect();
    let prev_center = storm
        .recent_centers
        .iter()
        .rev()
        .find(|&&sid| sid != storm.center_solar_system_id)
        .copied();
    let pool: Vec<SolarSystemId> = if !non_recent.is_empty() {
        non_recent
    } else if let Some(prev) = prev_center {
        let non_prev: Vec<SolarSystemId> = candidates
            .iter()
            .copied()
            .filter(|&sid| sid != prev)
            .collect();
        if !non_prev.is_empty() {
            non_prev
        } else {
            candidates
        }
    } else {
        candidates
    };
    let mix = (u64::from(storm.storm_id).wrapping_mul(2_862_933_555_777_941_757))
        ^ (u64::from(storm.step_count).wrapping_mul(3_037_000_493))
        ^ (u64::from(storm.center_solar_system_id).wrapping_mul(1_140_071_481_932_319_8485));
    let idx = ((mix >> 16) as usize) % pool.len();
    Some(pool[idx])
}

pub fn init_roaming_storms(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    if !reg.roaming_storms.is_empty() {
        return;
    }
    let now_sec = spec_now_sec(reg);
    for (sid, weather, center_sys) in [
        (1_u32, AbyssalWeatherKind::Electrical, 30_000_208_u32),
        (2_u32, AbyssalWeatherKind::Exotic, 30_000_212_u32),
        (3_u32, AbyssalWeatherKind::Gamma, 30_000_215_u32),
        (4_u32, AbyssalWeatherKind::Firestorm, 30_000_238_u32),
    ] {
        let (core, periphery) = compute_storm_footprint(ctx, center_sys);
        reg.roaming_storms.push(RoamingStormRecord {
            storm_id: sid,
            weather,
            center_solar_system_id: center_sys,
            strong_core_systems: core,
            weak_periphery_systems: periphery,
            step_count: 0,
            next_move_at_sec: now_sec + storm_move_interval_secs(sid, 0),
            recent_centers: vec![center_sys],
        });
    }
}

pub fn advance_roaming_storms(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    _delta_sec: u64,
) {
    let target_now = spec_now_sec(reg);
    for storm in &mut reg.roaming_storms {
        if storm.recent_centers.is_empty() {
            storm.recent_centers.push(storm.center_solar_system_id);
        }
        if storm.next_move_at_sec == 0 {
            storm.next_move_at_sec =
                target_now + storm_move_interval_secs(storm.storm_id, storm.step_count);
            continue;
        }
        for _ in 0..8 {
            if target_now < storm.next_move_at_sec {
                break;
            }
            if let Some(next_center) = pick_non_retracing_neighbor(&ctx.sde, storm) {
                if next_center != storm.center_solar_system_id {
                    let (new_core, new_periph) = compute_storm_footprint(ctx, next_center);
                    storm.center_solar_system_id = next_center;
                    storm.strong_core_systems = new_core;
                    storm.weak_periphery_systems = new_periph;
                    storm.step_count += 1;
                    storm.recent_centers.push(next_center);
                    if storm.recent_centers.len() > 8 {
                        let excess = storm.recent_centers.len() - 8;
                        storm.recent_centers.drain(0..excess);
                    }
                }
            }
            storm.next_move_at_sec = storm
                .next_move_at_sec
                .saturating_add(storm_move_interval_secs(storm.storm_id, storm.step_count));
        }
    }
}

pub fn sync_metaliminal_storm_effects_and_sites(ctx: &ServerContext) {
    let storms = with_pve_special_reg(ctx, |reg| reg.roaming_storms.clone());
    let now = ctx.now_unix();

    let mut effect_lists_by_sys: HashMap<SolarSystemId, Vec<TypeId>> = HashMap::new();
    let mut core_systems: HashSet<SolarSystemId> = HashSet::new();
    let mut all_storm_systems: HashSet<SolarSystemId> = HashSet::new();

    for storm in &storms {
        let (weak_tid, strong_tid) = storm_weather_beacons(storm.weather);
        let storm_core_set: HashSet<SolarSystemId> =
            storm.strong_core_systems.iter().copied().collect();
        for &cs in &storm.strong_core_systems {
            core_systems.insert(cs);
            all_storm_systems.insert(cs);
            let list = effect_lists_by_sys.entry(cs).or_default();
            if !list.contains(&strong_tid) {
                list.push(strong_tid);
            }
        }
        for &ps in &storm.weak_periphery_systems {
            if storm_core_set.contains(&ps) {
                continue;
            }
            all_storm_systems.insert(ps);
            let list = effect_lists_by_sys.entry(ps).or_default();
            if !list.contains(&weak_tid) {
                list.push(weak_tid);
            }
        }
    }

    if let Ok(mut sw_st) =
        crate::services::inventory_fitting::scanners_superweapons_state().write()
    {
        sw_st
            .system_wide_effects
            .retain(|k, v| !is_storm_beacon_type(*v) || all_storm_systems.contains(k));
        sw_st
            .system_wide_effect_lists
            .retain(|k, _| all_storm_systems.contains(k));
        for (sid, tids) in &effect_lists_by_sys {
            if let Some(&first_tid) = tids.first() {
                sw_st.system_wide_effects.insert(*sid, first_tid);
            }
            sw_st.system_wide_effect_lists.insert(*sid, tids.clone());
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| {
            if it.flag == InventoryFlag::None && is_storm_beacon_type(it.type_id) {
                let sys_id = it.location_id as u32;
                effect_lists_by_sys
                    .get(&sys_id)
                    .is_some_and(|list| list.contains(&it.type_id))
            } else {
                true
            }
        });

        for (sid, tids) in &effect_lists_by_sys {
            for &tid in tids {
                let has_item = snap.items.values().any(|it| {
                    it.location_id == *sid as i64
                        && it.type_id == tid
                        && it.flag == InventoryFlag::None
                });
                if !has_item {
                    snap.next_entity_id += 1;
                    let item_id = snap.next_entity_id;
                    snap.items.insert(
                        item_id,
                        InventoryItemRecord {
                            item_id,
                            type_id: tid,
                            owner_id: 1,
                            location_id: *sid as i64,
                            flag: InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            ..Default::default()
                        },
                    );
                }
            }
        }

        // Extra storm PvE sites spawn ONLY in the core; remove any storm sites outside active cores.
        for (&sid, anoms) in snap.cosmic_anomalies.iter_mut() {
            if !core_systems.contains(&sid) {
                anoms.retain(|a| {
                    !a.anomaly_id.starts_with("DNG-9042-")
                        && !a.anomaly_id.starts_with("DNG-9043-")
                });
            }
        }

        for &cs in &core_systems {
            let anoms = snap.cosmic_anomalies.entry(cs).or_default();
            if !anoms.iter().any(|a| a.anomaly_id.starts_with("DNG-9042-")) {
                anoms.push(CosmicAnomalyRecord {
                    anomaly_id: format!("DNG-9042-{cs}"),
                    name: "Fresh Volatile Ice Field".into(),
                    solar_system_id: cs,
                    anomaly_type: "OreAnomaly".into(),
                    position: [100_000_000.0, 50_000_000.0, -25_000_000.0],
                    signal_strength: 1.0,
                    ore_types: vec![56142], // Metaliminal volatile ice
                    remaining_volume: 500_000.0,
                    cleared: false,
                    created_unix: now,
                    lifetime_seconds: 7200,
                });
            }
            if !anoms.iter().any(|a| a.anomaly_id.starts_with("DNG-9043-")) {
                anoms.push(CosmicAnomalyRecord {
                    anomaly_id: format!("DNG-9043-{cs}"),
                    name: "Metaliminal Storm Surge Site".into(),
                    solar_system_id: cs,
                    anomaly_type: "CombatAnomaly".into(),
                    position: [-85_000_000.0, 40_000_000.0, 30_000_000.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    cleared: false,
                    created_unix: now,
                    lifetime_seconds: 7200,
                });
            }
        }
    });
}

pub fn get_all_roaming_weather_systems(ctx: &ServerContext) -> Vec<PyRep> {
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
    with_pve_special_reg(ctx, |reg| {
        let mut out = Vec::new();
        let mut seen: HashSet<(u32, SolarSystemId)> = HashSet::new();
        for storm in &reg.roaming_storms {
            if seen.insert((storm.storm_id, storm.center_solar_system_id)) {
                out.push(PyRep::keyval([
                    ("locationID", PyRep::Integer(storm.center_solar_system_id as i64)),
                    ("solarSystemID", PyRep::Integer(storm.center_solar_system_id as i64)),
                    ("sceneType", PyRep::Integer(4)),
                    ("weather", PyRep::String(storm.weather.as_str().into())),
                    ("stormID", PyRep::Integer(storm.storm_id as i64)),
                    ("isCore", PyRep::Bool(true)),
                ]));
            }
            for &core_sys in &storm.strong_core_systems {
                if seen.insert((storm.storm_id, core_sys)) {
                    out.push(PyRep::keyval([
                        ("locationID", PyRep::Integer(core_sys as i64)),
                        ("solarSystemID", PyRep::Integer(core_sys as i64)),
                        ("sceneType", PyRep::Integer(4)),
                        ("weather", PyRep::String(storm.weather.as_str().into())),
                        ("stormID", PyRep::Integer(storm.storm_id as i64)),
                        ("isCore", PyRep::Bool(true)),
                    ]));
                }
            }
            for &periph_sys in &storm.weak_periphery_systems {
                if seen.insert((storm.storm_id, periph_sys)) {
                    out.push(PyRep::keyval([
                        ("locationID", PyRep::Integer(periph_sys as i64)),
                        ("solarSystemID", PyRep::Integer(periph_sys as i64)),
                        ("sceneType", PyRep::Integer(1)),
                        ("weather", PyRep::String(storm.weather.as_str().into())),
                        ("stormID", PyRep::Integer(storm.storm_id as i64)),
                        ("isCore", PyRep::Bool(false)),
                    ]));
                }
            }
        }
        out
    })
}
