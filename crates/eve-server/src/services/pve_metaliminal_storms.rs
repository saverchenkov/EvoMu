//! `pve_metaliminal_storms` — Metaliminal Roaming Storms simulation, stargate propagation,
//! system-wide environment dogma effects, and bonus exploration/mining site spawning
//! (`feat:pve.metaliminal-storms`, `rpc:map.GetAllRoamingWeatherSystems`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    with_pve_special_reg, AbyssalWeatherKind, PveSpecialSitesRegistry,
};
use eve_db::{CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::{SolarSystemId, TypeId};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoamingStormRecord {
    pub storm_id: u32,
    pub weather: AbyssalWeatherKind,
    pub center_solar_system_id: SolarSystemId,
    pub strong_core_systems: Vec<SolarSystemId>,
    pub weak_periphery_systems: Vec<SolarSystemId>,
    pub step_count: u32,
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

pub fn compute_storm_footprint(
    ctx: &ServerContext,
    center_sys: SolarSystemId,
) -> (Vec<SolarSystemId>, Vec<SolarSystemId>) {
    let mut visited: HashSet<SolarSystemId> = HashSet::new();
    let mut queue: VecDeque<(SolarSystemId, u8)> = VecDeque::new();
    visited.insert(center_sys);
    queue.push_back((center_sys, 0));

    let mut core = vec![center_sys];
    let mut periphery = Vec::new();

    while let Some((curr, depth)) = queue.pop_front() {
        if depth >= 2 {
            continue;
        }
        for jump in ctx.sde.jumps.values() {
            if jump.from_solar_system_id == curr {
                let next_sys = jump.to_solar_system_id;
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&next_sys)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                if sec >= 0.45 {
                    continue;
                }
                if visited.insert(next_sys) {
                    let next_depth = depth + 1;
                    if next_depth == 1 {
                        core.push(next_sys);
                    } else {
                        periphery.push(next_sys);
                    }
                    queue.push_back((next_sys, next_depth));
                }
            }
        }
    }
    if core.len() > 1 {
        core[1..].sort_unstable();
    }
    periphery.sort_unstable();
    (core, periphery)
}

pub fn init_roaming_storms(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    if !reg.roaming_storms.is_empty() {
        return;
    }
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
        });
    }
}

pub fn advance_roaming_storms(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    delta_sec: u64,
) {
    if delta_sec < 3_600 {
        return;
    }
    for storm in &mut reg.roaming_storms {
        let mut candidates: Vec<SolarSystemId> = ctx
            .sde
            .jumps
            .values()
            .filter(|j| j.from_solar_system_id == storm.center_solar_system_id)
            .map(|j| j.to_solar_system_id)
            .filter(|&sid| {
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&sid)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                sec < 0.45
            })
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        if !candidates.is_empty() {
            let next_center = candidates[(storm.step_count as usize) % candidates.len()];
            if next_center != storm.center_solar_system_id {
                let (new_core, new_periph) = compute_storm_footprint(ctx, next_center);
                storm.center_solar_system_id = next_center;
                storm.strong_core_systems = new_core;
                storm.weak_periphery_systems = new_periph;
                storm.step_count += 1;
            }
        }
    }
}

pub fn sync_metaliminal_storm_effects_and_sites(ctx: &ServerContext) {
    let storms = with_pve_special_reg(ctx, |reg| reg.roaming_storms.clone());
    let now = ctx.now_unix();

    let mut new_effects: HashMap<u32, u32> = HashMap::new();
    let mut storm_systems: HashSet<SolarSystemId> = HashSet::new();

    for storm in &storms {
        let (weak_tid, strong_tid) = storm_weather_beacons(storm.weather);
        for &cs in &storm.strong_core_systems {
            new_effects.insert(cs, strong_tid);
            storm_systems.insert(cs);
        }
        for &ps in &storm.weak_periphery_systems {
            new_effects.entry(ps).or_insert(weak_tid);
            storm_systems.insert(ps);
        }
    }

    if let Ok(mut sw_st) =
        crate::services::inventory_fitting::scanners_superweapons_state().write()
    {
        sw_st
            .system_wide_effects
            .retain(|k, v| !is_storm_beacon_type(*v) || storm_systems.contains(k));
        for (sid, tid) in &new_effects {
            sw_st.system_wide_effects.insert(*sid, *tid);
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| {
            if it.flag == InventoryFlag::None && is_storm_beacon_type(it.type_id) {
                let sys_id = it.location_id as u32;
                storm_systems.contains(&sys_id)
            } else {
                true
            }
        });

        for (sid, tid) in &new_effects {
            let has_item = snap.items.values().any(|it| {
                it.location_id == *sid as i64
                    && it.type_id == *tid
                    && it.flag == InventoryFlag::None
            });
            if !has_item {
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: *tid,
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

        for (&sid, anoms) in snap.cosmic_anomalies.iter_mut() {
            if !storm_systems.contains(&sid) {
                anoms.retain(|a| !a.anomaly_id.starts_with("DNG-9042-") && !a.anomaly_id.starts_with("DNG-9043-"));
            }
        }

        for storm in &storms {
            for &cs in &storm.strong_core_systems {
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
            }
            for &ps in &storm.weak_periphery_systems {
                let anoms = snap.cosmic_anomalies.entry(ps).or_default();
                if !anoms.iter().any(|a| a.anomaly_id.starts_with("DNG-9043-")) {
                    anoms.push(CosmicAnomalyRecord {
                        anomaly_id: format!("DNG-9043-{ps}"),
                        name: "Friable Volatile Ice Field".into(),
                        solar_system_id: ps,
                        anomaly_type: "OreAnomaly".into(),
                        position: [100_000_000.0, 50_000_000.0, -25_000_000.0],
                        signal_strength: 1.0,
                        ore_types: vec![56142],
                        remaining_volume: 300_000.0,
                        cleared: false,
                        created_unix: now,
                        lifetime_seconds: 7200,
                    });
                }
            }
        }
    });
}

pub fn get_all_roaming_weather_systems(ctx: &ServerContext) -> Vec<PyRep> {
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
    with_pve_special_reg(ctx, |reg| {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for storm in &reg.roaming_storms {
            if seen.insert(storm.center_solar_system_id) {
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
                if seen.insert(core_sys) {
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
                if seen.insert(periph_sys) {
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
