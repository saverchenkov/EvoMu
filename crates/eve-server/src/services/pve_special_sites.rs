//! `pve_special_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale), Homefront Operations, Metaliminal Roaming Storms, Drifter Hives, Triglavian
//! Pochven/EDENCOM Sites, and Solar System Interference (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership
// parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.triglavian-sites, feat:pve.interference

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AttributeId, SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

type CharacterId = i64;
type ItemId = i64;
pub use crate::services::pve_incursion_sites::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbyssalWeatherKind { Electrical, Dark, Exotic, Firestorm, Gamma }

impl AbyssalWeatherKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Electrical => "Electrical", Self::Dark => "Dark", Self::Exotic => "Exotic",
            Self::Firestorm => "Firestorm", Self::Gamma => "Gamma",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbyssalRoomRecord {
    pub room_index: u8, pub dungeon_id: u32, pub dungeon_name: String,
    pub npc_ball_ids: Vec<i64>, pub cache_item_id: ItemId, pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbyssalPocketSession {
    pub pocket_id: i64, pub character_id: CharacterId, pub ship_id: ItemId,
    pub origin_solar_system_id: SolarSystemId, pub abyssal_solar_system_id: SolarSystemId,
    #[serde(default)] pub origin_position: [f64; 3],
    pub filament_type_id: TypeId, pub tier: u8, pub weather: AbyssalWeatherKind,
    pub weather_penalty_pct: f64, pub weather_bonus_pct: f64, pub npc_difficulty_mult: f64,
    pub current_room: u8, pub rooms: [AbyssalRoomRecord; 3], pub started_at_sec: u64,
    pub collapse_at_sec: u64, pub saved_ship_attrs: HashMap<AttributeId, f64>,
    pub completed: bool, pub collapsed_destroyed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomefrontSiteRecord {
    pub instance_id: i64, pub dungeon_id: u32, pub dungeon_name: String, pub solar_system_id: SolarSystemId,
    pub optimal_fleet_size: usize, pub base_pilot_isk: i64, pub completed: bool,
    pub reward_paid_isk: i64, pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoamingStormRecord {
    pub storm_id: u32, pub weather: AbyssalWeatherKind, pub center_solar_system_id: SolarSystemId,
    pub strong_core_systems: Vec<SolarSystemId>, pub weak_periphery_systems: Vec<SolarSystemId>,
    pub step_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrifterHiveRecord {
    pub site_id: String, pub dungeon_id: u32, pub dungeon_name: String, pub solar_system_id: SolarSystemId,
    pub element_type_id: TypeId, pub element_name: String, pub current_room: u8, pub total_rooms: u8,
    pub warden_npc_ball_id: i64, pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriglavianSiteRecord {
    pub instance_id: i64, pub dungeon_id: u32, pub dungeon_name: String, pub solar_system_id: SolarSystemId,
    pub faction_owner_id: u32, pub base_isk: i64, pub survey_db_drop: i64,
    pub completed: bool, pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInterferenceRecord {
    pub solar_system_id: SolarSystemId,
    pub interference_level: f64,
    pub band: u8,
    #[serde(default)]
    pub last_updated_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbyssalTraceRecord {
    pub trace_ball_id: i64,
    pub char_id: CharacterId,
    pub filament_type_id: TypeId,
    pub origin_solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub created_unix: u64,
    #[serde(default)]
    pub expires_at_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoidSpaceSessionRecord {
    pub char_id: CharacterId,
    pub ship_id: ItemId,
    pub origin_solar_system_id: SolarSystemId,
    pub origin_position: [f64; 3],
    pub target_id: i64,
    pub entered_unix: u64,
    pub expires_at_sec: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PveSpecialSitesRegistry {
    #[serde(skip)] pub db_weak: std::sync::Weak<eve_db::WorldRepository>,
    #[serde(skip)] pub current_now_sec: u64,
    pub initialized: bool,
    #[serde(skip)] pub balls_spawned: bool,
    pub incursions: HashMap<ConstellationId, IncursionTaleRecord>,
    #[serde(default)]
    pub incursion_lp_logs: HashMap<CharacterId, Vec<IncursionLPLogEntry>>,
    #[serde(default)]
    pub abyssal_pockets: HashMap<CharacterId, AbyssalPocketSession>,
    #[serde(default)]
    pub abyssal_traces: HashMap<i64, AbyssalTraceRecord>,
    #[serde(default)]
    pub void_space_sessions: HashMap<CharacterId, VoidSpaceSessionRecord>,
    pub homefront_sites: HashMap<SolarSystemId, Vec<HomefrontSiteRecord>>,
    pub roaming_storms: Vec<RoamingStormRecord>,
    pub drifter_hives: HashMap<SolarSystemId, DrifterHiveRecord>,
    pub triglavian_sites: HashMap<SolarSystemId, Vec<TriglavianSiteRecord>>,
    #[serde(default)]
    pub edencom_fortress_systems: Vec<SolarSystemId>,
    #[serde(default)]
    pub edencom_minor_victory_systems: Vec<SolarSystemId>,
    #[serde(default)]
    pub triglavian_minor_victory_systems: Vec<SolarSystemId>,
    pub interference_by_system: HashMap<SolarSystemId, SystemInterferenceRecord>,
    pub sim_clock_sec: u64,
    pub next_seq: i64,
}

const PVE_SPECIAL_NOTE_ID: i64 = -8_300_000_000_000;
const INCURSIONS_BLOB_KEY: &str = "incursions_state";

pub fn with_pve_special_reg_db<R>(
    db: &Arc<WorldRepository>,
    now_sec: u64,
    f: impl FnOnce(&mut PveSpecialSitesRegistry) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let (mut reg, orig_json) = {
            if let Some(blob) = snap.service_state_blobs.get(INCURSIONS_BLOB_KEY) {
                if let Ok(parsed) = serde_json::from_str::<PveSpecialSitesRegistry>(blob) {
                    (parsed, blob.clone())
                } else {
                    (PveSpecialSitesRegistry::default(), String::new())
                }
            } else if let Some(note) = snap.owner_notes.get(&PVE_SPECIAL_NOTE_ID) {
                if let Ok(parsed) = serde_json::from_str::<PveSpecialSitesRegistry>(&note.note) {
                    (parsed, note.note.clone())
                } else {
                    (PveSpecialSitesRegistry::default(), String::new())
                }
            } else {
                (PveSpecialSitesRegistry::default(), String::new())
            }
        };
        reg.db_weak = std::sync::Arc::downgrade(db);
        reg.current_now_sec = now_sec;
        let out = f(&mut reg);
        if let Ok(new_json) = serde_json::to_string(&reg) {
            if new_json != orig_json {
                snap.service_state_blobs.insert(INCURSIONS_BLOB_KEY.to_string(), new_json.clone());
                for (&cid, inc) in &reg.incursions {
                    if inc.state == "Ended" {
                        snap.incursions.remove(&cid);
                    } else {
                        snap.incursions.insert(
                            cid,
                            eve_db::IncursionRecord {
                                constellation_id: cid,
                                faction_id: 500_019,
                                has_boss: inc.mothership_spawned && !inc.mothership_defeated,
                                infested_solar_systems: vec![inc.staging_solar_system_id, inc.hq_solar_system_id],
                                influence: inc.influence,
                                staging_solar_system_id: inc.staging_solar_system_id,
                                state: inc.state.to_lowercase(),
                                incursion_type: "Incursion".to_string(),
                            },
                        );
                    }
                }
                snap.owner_notes.insert(
                    PVE_SPECIAL_NOTE_ID,
                    eve_db::OwnerNoteRecord {
                        note_id: PVE_SPECIAL_NOTE_ID,
                        character_id: -83,
                        item_id: 0,
                        label: "pve_special_sites_registry".into(),
                        note: new_json,
                        timestamp_unix: reg.current_now_sec.max(reg.sim_clock_sec),
                    },
                );
            }
        }
        out
    })
}

pub fn with_pve_special_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveSpecialSitesRegistry) -> R,
) -> R {
    with_pve_special_reg_db(&ctx.db, ctx.now_unix(), f)
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn next_spec_seq(reg: &mut PveSpecialSitesRegistry) -> i64 {
    reg.next_seq += 1;
    reg.next_seq
}

fn spec_now_sec(reg: &PveSpecialSitesRegistry) -> u64 {
    let now = reg.current_now_sec.max(reg.sim_clock_sec);
    if now > 0 { now } else { 1_775_000_000 }
}

pub fn advance_pve_special_clock(ctx: &ServerContext, delta_sec: u64) {
    ensure_special_sites_initialized(ctx);
    with_pve_special_reg(ctx, |reg| {
        if reg.sim_clock_sec < reg.current_now_sec {
            reg.sim_clock_sec = reg.current_now_sec.max(1_775_000_000);
        }
        reg.sim_clock_sec += delta_sec;
        if delta_sec >= 3_600 {
            for storm in &mut reg.roaming_storms {
                if let Some(&next_center) = storm.strong_core_systems.iter().find(|&&s| s != storm.center_solar_system_id) {
                    let (new_core, new_periph) = compute_storm_footprint(ctx, next_center);
                    storm.center_solar_system_id = next_center;
                    storm.strong_core_systems = new_core;
                    storm.weak_periphery_systems = new_periph;
                    storm.step_count += 1;
                }
            }
        }
    });
}

pub fn record_system_interference_activity(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    activity_delta: f64,
) {
    ensure_special_sites_initialized(ctx);
    let delta = activity_delta.clamp(0.0, 1.0);
    let now_sec = ctx.now_unix();
    let (level, old_band, new_band) = with_pve_special_reg(ctx, |reg| {
        let entry = reg.interference_by_system.entry(solar_system_id).or_insert(SystemInterferenceRecord {
            solar_system_id, interference_level: 0.10, band: 1, last_updated_sec: now_sec,
        });
        if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
            let elapsed = now_sec - entry.last_updated_sec;
            let decay = (elapsed as f64) * 0.001;
            entry.interference_level = (entry.interference_level - decay).max(0.10);
        }
        let prev = entry.band;
        if delta > 0.0 {
            entry.interference_level = (entry.interference_level + delta).clamp(0.0, 1.0);
        }
        entry.band = if entry.interference_level >= 0.70 { 3 } else if entry.interference_level >= 0.35 { 2 } else { 1 };
        entry.last_updated_sec = now_sec;
        (entry.interference_level, prev, entry.band)
    });
    if new_band != old_band {
        session.push_notification(
            "OnSolarsystemInterferenceChanged",
            vec![PyRep::Integer(solar_system_id as i64), PyRep::Real(level), PyRep::Integer(new_band as i64)],
        );
        ctx.notify(
            crate::router::NotificationTarget::SolarSystem(solar_system_id),
            "OnSolarsystemInterferenceChanged",
            "solarsystemid",
            vec![PyRep::Integer(solar_system_id as i64), PyRep::Real(level), PyRep::Integer(new_band as i64)],
        );
    }
}

// const-ok: No active invasion campaigns configured
pub fn triglavian_minor_victory_systems() -> Vec<SolarSystemId> {
    Vec::new()
}

pub fn ensure_triglavian_sites_for_system(reg: &mut PveSpecialSitesRegistry, sys_id: SolarSystemId) {
    if reg.triglavian_sites.contains_key(&sys_id) {
        return;
    }
    let base_inst = (sys_id as i64) * 100;
    reg.triglavian_sites.insert(
        sys_id,
        vec![
            TriglavianSiteRecord {
                instance_id: base_inst + 1,
                dungeon_id: 8922,
                dungeon_name: "Observatory Flashpoint".into(),
                solar_system_id: sys_id,
                faction_owner_id: 500_027,
                base_isk: 35_000_000,
                survey_db_drop: 150,
                completed: false,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 1),
            },
            TriglavianSiteRecord {
                instance_id: base_inst + 2,
                dungeon_id: 8802,
                dungeon_name: "Major Conduit".into(),
                solar_system_id: sys_id,
                faction_owner_id: 500_026,
                base_isk: 25_000_000,
                survey_db_drop: 100,
                completed: false,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 2),
            },
        ],
    );
}

pub fn ensure_special_sites_initialized(ctx: &ServerContext) {
    with_pve_special_reg(ctx, |reg| {
        if reg.initialized {
            return;
        }
        reg.initialized = true;

        // DERIVED: SDE-filtered non-starter low/high-sec incursion constellations
        // Filter candidate constellations by checking their systems in SDE:
        // average security in 0.1..=0.8, not belonging to starter/school bloodlines or trade hubs (20000020).
        let trade_hubs: &[ConstellationId] = &[20_000_020];
        let starter_corps: std::collections::HashSet<i64> = ctx
            .sde
            .schools
            .values()
            .map(|s| s.corporation_id as i64)
            .chain(ctx.sde.bloodlines.values().map(|b| b.corporation_id as i64))
            .collect();
        let starter_systems: std::collections::HashSet<SolarSystemId> = ctx
            .sde
            .schools
            .values()
            .map(|s| s.solar_system_id)
            .chain(ctx.sde.stations.values().filter(|st| starter_corps.contains(&(st.owner_corporation_id as i64))).map(|st| st.solar_system_id))
            .filter(|&id| id > 0)
            .collect();
        let starter_constellations: std::collections::HashSet<ConstellationId> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| starter_systems.contains(&s.solar_system_id))
            .map(|s| s.constellation_id)
            .collect();

        let mut eligible_constellations: Vec<(ConstellationId, f64)> = ctx
            .sde
            .constellations
            .keys()
            .copied()
            .filter(|cid| !trade_hubs.contains(cid) && !starter_constellations.contains(cid))
            .filter_map(|cid| {
                let sys: Vec<_> = ctx
                    .sde
                    .solar_systems
                    .values()
                    .filter(|s| s.constellation_id == cid)
                    .collect();
                if sys.is_empty() {
                    return None;
                }
                let avg_sec = sys.iter().map(|s| s.security_status).sum::<f64>() / (sys.len() as f64);
                if (0.1..=0.8).contains(&avg_sec) {
                    Some((cid, avg_sec))
                } else {
                    None
                }
            })
            .collect();
        eligible_constellations.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
        for &(cid, _) in eligible_constellations.iter().take(3) {
            seed_incursion_for_constellation(ctx, reg, cid);
        }

        // const-ok: No active invasion campaigns configured
        if reg.triglavian_minor_victory_systems.is_empty() {
            reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems();
        }

        reg.homefront_sites.insert(
            30_000_142,
            vec![
                HomefrontSiteRecord {
                    instance_id: 710_001, dungeon_id: 10346, dungeon_name: "Abyssal Artifact Recovery".into(),
                    solar_system_id: 30_000_142, optimal_fleet_size: 5, base_pilot_isk: 17_230_000,
                    completed: false, reward_paid_isk: 0, boss_npc_ball_id: 1_897_710_001,
                },
                HomefrontSiteRecord {
                    instance_id: 710_002, dungeon_id: 10347, dungeon_name: "Raid: Hall of Sacrifice".into(),
                    solar_system_id: 30_000_142, optimal_fleet_size: 5, base_pilot_isk: 15_500_000,
                    completed: false, reward_paid_isk: 0, boss_npc_ball_id: 1_897_710_002,
                },
            ],
        );

        for (sid, weather, center_sys) in [
            (1_u32, AbyssalWeatherKind::Electrical, 30_002_904_u32),
            (2_u32, AbyssalWeatherKind::Exotic, 30_004_759_u32),
            (3_u32, AbyssalWeatherKind::Gamma, 30_001_161_u32),
            (4_u32, AbyssalWeatherKind::Firestorm, 30_000_142_u32),
        ] {
            let (core, periphery) = compute_storm_footprint(ctx, center_sys);
            reg.roaming_storms.push(RoamingStormRecord {
                storm_id: sid, weather, center_solar_system_id: center_sys,
                strong_core_systems: core, weak_periphery_systems: periphery, step_count: 0,
            });
        }

        for (sys_id, did, dname, elem_tid, elem_name) in [
            (31_000_001_u32, 5648_u32, "Barbican Hive", 34_557_u32, "Barbican Element"),
            (31_000_002, 5649, "Vidette Hive", 34_558, "Vidette Element"),
            (31_000_003, 5650, "Conflux Hive", 34_559, "Conflux Element"),
            (31_000_004, 5651, "Redoubt Hive", 34_560, "Redoubt Element"),
            (31_000_005, 5642, "Sentinel Hive", 34_556, "Sentinel Element"),
        ] {
            reg.drifter_hives.insert(
                sys_id,
                DrifterHiveRecord {
                    site_id: format!("DRIFTER-HIVE-{did}"), dungeon_id: did, dungeon_name: dname.into(),
                    solar_system_id: sys_id, element_type_id: elem_tid, element_name: elem_name.into(),
                    current_room: 1, total_rooms: 3, warden_npc_ball_id: 1_898_000_000 + (did as i64), cleared: false,
                },
            );
        }

        for &sys_id in &triglavian_minor_victory_systems() {
            ensure_triglavian_sites_for_system(reg, sys_id);
        }
    });
}

fn compute_storm_footprint(
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
    core.sort_unstable();
    periphery.sort_unstable();
    (core, periphery)
}

pub fn classify_abyssal_filament(type_id: TypeId) -> Option<(u8, AbyssalWeatherKind)> {
    match type_id {
        56131 => Some((0, AbyssalWeatherKind::Electrical)),
        56132 => Some((0, AbyssalWeatherKind::Dark)),
        56133 => Some((0, AbyssalWeatherKind::Exotic)),
        56134 => Some((0, AbyssalWeatherKind::Firestorm)),
        56136 => Some((0, AbyssalWeatherKind::Gamma)),
        47761 => Some((1, AbyssalWeatherKind::Exotic)),
        47762 => Some((1, AbyssalWeatherKind::Dark)),
        47763 => Some((1, AbyssalWeatherKind::Firestorm)),
        47764 => Some((1, AbyssalWeatherKind::Gamma)),
        47765 => Some((1, AbyssalWeatherKind::Electrical)),
        47888 => Some((2, AbyssalWeatherKind::Exotic)),
        47892 => Some((2, AbyssalWeatherKind::Dark)),
        47896 => Some((2, AbyssalWeatherKind::Firestorm)),
        47900 => Some((2, AbyssalWeatherKind::Gamma)),
        47904 => Some((2, AbyssalWeatherKind::Electrical)),
        47889 => Some((3, AbyssalWeatherKind::Exotic)),
        47893 => Some((3, AbyssalWeatherKind::Dark)),
        47897 => Some((3, AbyssalWeatherKind::Firestorm)),
        47901 => Some((3, AbyssalWeatherKind::Gamma)),
        47905 => Some((3, AbyssalWeatherKind::Electrical)),
        47890 => Some((4, AbyssalWeatherKind::Exotic)),
        47894 => Some((4, AbyssalWeatherKind::Dark)),
        47898 => Some((4, AbyssalWeatherKind::Firestorm)),
        47902 => Some((4, AbyssalWeatherKind::Gamma)),
        47906 => Some((4, AbyssalWeatherKind::Electrical)),
        47891 => Some((5, AbyssalWeatherKind::Exotic)),
        47895 => Some((5, AbyssalWeatherKind::Dark)),
        47899 => Some((5, AbyssalWeatherKind::Firestorm)),
        47903 => Some((5, AbyssalWeatherKind::Gamma)),
        47907 => Some((5, AbyssalWeatherKind::Electrical)),
        56139 => Some((6, AbyssalWeatherKind::Electrical)),
        56140 => Some((6, AbyssalWeatherKind::Dark)),
        56141 => Some((6, AbyssalWeatherKind::Exotic)),
        56142 => Some((6, AbyssalWeatherKind::Firestorm)),
        56143 => Some((6, AbyssalWeatherKind::Gamma)),
        _ => None,
    }
}

pub async fn ensure_special_sites_spawned(ctx: &ServerContext) {
    ensure_special_sites_initialized(ctx);
    let marker_sys = ctx.systems.get_or_spawn(30_000_142);
    if matches!(marker_sys.get_ball_snapshot(1_895_999_999).await, Ok(Some(_))) {
        return;
    }
    let _ = marker_sys.spawn_ball(make_site_npc_ball(1_895_999_999, 10_962, DVec3::ZERO)).await;
    let to_spawn: Vec<(SolarSystemId, i64, u32, DVec3)> = with_pve_special_reg(ctx, |reg| {
        let mut list = Vec::new();
        for inc in reg.incursions.values() {
            if inc.state == "Ended" { continue; }
            for s in &inc.sites {
                if s.locked_until_low_influence && (inc.influence > 0.0 || !inc.mothership_spawned) {
                    continue;
                }
                list.push((s.solar_system_id, s.boss_npc_ball_id, s.boss_type_id, s.position));
                for esc in &s.escort_spawns {
                    if !esc.destroyed {
                        let esc_pos = s.position + DVec3::new(esc.offset[0], esc.offset[1], esc.offset[2]);
                        list.push((s.solar_system_id, esc.npc_ball_id, esc.type_id, esc_pos));
                    }
                }
            }
        }
        for sites in reg.homefront_sites.values() {
            for s in sites { list.push((s.solar_system_id, s.boss_npc_ball_id, 10_962, DVec3::new(10_000.0, 0.0, 0.0))); }
        }
        for hive in reg.drifter_hives.values() {
            list.push((hive.solar_system_id, hive.warden_npc_ball_id, 10_962, DVec3::new(10_000.0, 0.0, 0.0)));
        }
        for sites in reg.triglavian_sites.values() {
            for s in sites { list.push((s.solar_system_id, s.boss_npc_ball_id, 10_962, DVec3::new(10_000.0, 0.0, 0.0))); }
        }
        list
    });
    for (sys_id, bid, tid, pos) in to_spawn {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, tid, pos)).await;
    }
}

pub async fn ensure_abyssal_room_balls_for_ctx(ctx: &ServerContext, char_id: CharacterId) {
    let info = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id)?;
        if p.completed || p.collapsed_destroyed { return None; }
        let r_idx = (p.current_room.saturating_sub(1) as usize).min(2);
        Some((p.abyssal_solar_system_id, p.pocket_id, p.current_room, p.rooms[r_idx].cleared, p.rooms[r_idx].npc_ball_ids.clone()))
    });
    if let Some((sys_id, pid, room_num, cleared, npcs)) = info {
        let marker_id = 1_895_000_000 + pid * 10 + (room_num as i64);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if matches!(sys_handle.get_ball_snapshot(marker_id).await, Ok(None)) {
            let _ = sys_handle.spawn_ball(make_site_npc_ball(marker_id, 47_956, DVec3::ZERO)).await;
            if !cleared {
                for (idx, &bid) in npcs.iter().enumerate() {
                    let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, 47_956, DVec3::new((idx as f64) * 3_000.0, 5_000.0, 0.0))).await;
                }
            }
        }
    }
}

fn restore_abyssal_weather_dogma_modifiers(
    ctx: &ServerContext,
    ship_id: ItemId,
    saved_attrs: &HashMap<AttributeId, f64>,
) {
    if saved_attrs.is_empty() {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            for (&attr_id, &val) in saved_attrs {
                ship.dynamic_attributes.insert(attr_id, val);
            }
        }
    });
}

/// Enters an Abyssal Deadspace pocket for `char_id` on filament activation (`feat:lifecycle.abyssal-restart`).
pub async fn enter_abyssal_pocket_for_character(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: CharacterId,
    filament_type_id: TypeId,
    tier: u8,
    origin_sys: SolarSystemId,
) -> (i64, Vec<i64>) {
    let ship_id = session.ship_id.unwrap_or(100_000_002);
    let weather = classify_abyssal_filament(filament_type_id)
        .map(|(_, w)| w)
        .unwrap_or(AbyssalWeatherKind::Electrical);
    let penalty_pct = match tier { 0 | 1 => 30.0, 2 | 3 => 50.0, _ => 70.0 };
    let bonus_pct = 50.0;
    let diff_mult = 1.0 + (tier as f64) * 1.25;
    let origin_pos = ctx.db.with_snapshot(|snap| {
        snap.item_positions
            .get(&ship_id)
            .copied()
            .unwrap_or([145_000.0, -75_000.0, 42_000.0])
    });
    let abyssal_sys_id: SolarSystemId = 32_000_001;
    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys_id);
    let (pocket_id, room1_npcs) = with_pve_special_reg(ctx, |reg| {
        let pid = 600_000 + next_spec_seq(reg);
        let now = spec_now_sec(reg);
        let collapse = now + 1_200;
        let mut rooms_vec = Vec::with_capacity(3);
        for (r_idx, did, r_name) in [(1_u8, 5810_u32, "Canyon"), (2, 6047, "Rise"), (3, 6048, "Forest Opening")] {
            let npc_count = (2 + (tier as usize / 2)).clamp(2, 5);
            let npc_ids: Vec<i64> = (0..npc_count).map(|n| pid * 100 + (r_idx as i64) * 10 + (n as i64) + 1).collect();
            rooms_vec.push(AbyssalRoomRecord {
                room_index: r_idx, dungeon_id: did, dungeon_name: r_name.into(),
                npc_ball_ids: npc_ids, cache_item_id: 1_896_000_000 + pid * 10 + (r_idx as i64), cleared: false,
            });
        }
        let rooms: [AbyssalRoomRecord; 3] = rooms_vec.try_into().unwrap();
        let r1_npcs = rooms[0].npc_ball_ids.clone();
        reg.abyssal_pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: pid, character_id: char_id, ship_id,
                origin_solar_system_id: origin_sys, origin_position: origin_pos,
                abyssal_solar_system_id: abyssal_sys_id,
                filament_type_id, tier, weather,
                weather_penalty_pct: penalty_pct, weather_bonus_pct: bonus_pct,
                npc_difficulty_mult: diff_mult, current_room: 1, rooms,
                started_at_sec: now, collapse_at_sec: collapse,
                saved_ship_attrs: HashMap::new(), completed: false, collapsed_destroyed: false,
            },
        );
        (pid, r1_npcs)
    });
    let _ = sys_handle.spawn_ball(make_site_npc_ball(1_895_000_000 + pocket_id * 10 + 1, 47_956, DVec3::ZERO)).await;
    for (idx, &bid) in room1_npcs.iter().enumerate() {
        let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, 47_956, DVec3::new((idx as f64) * 3_000.0, 5_000.0, 0.0))).await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = abyssal_sys_id; }
        if let Some(it) = snap.items.get_mut(&ship_id) { it.location_id = abyssal_sys_id as i64; }
    });
    session.solar_system_id = Some(abyssal_sys_id);
    (pocket_id, room1_npcs)
}

fn random_offset_around_trace_origin(ctx: &ServerContext, origin_pos: [f64; 3]) -> [f64; 3] {
    let u1 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.37);
    let u2 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.61);
    let u3 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.83);
    let sign = |u: f64| if u >= 0.5 { 1.0 } else { -1.0 };
    let mag = |u: f64| 500.0 + ((u * 2.0 - 1.0).abs()) * 4_500.0;
    [
        origin_pos[0] + sign(u1) * mag(u1),
        origin_pos[1] + sign(u2) * mag(u2),
        origin_pos[2] + sign(u3) * mag(u3),
    ]
}

/// Destroys the character's ship and capsule on Abyssal pocket 20-minute collapse, records a killmail,
/// and respawns the character in a fresh Capsule at their home station (`feat:lifecycle.abyssal-restart`).
pub fn collapse_abyssal_pocket_and_pod_pilot(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    abyssal_sys: SolarSystemId,
) -> SolarSystemId {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(670);
        if snap.items.contains_key(&ship_id) && ship_type_id != 670 {
            snap.items.retain(|&iid, it| iid != ship_id && it.location_id != ship_id);
            snap.item_positions.remove(&ship_id);
            snap.module_online_states.remove(&ship_id);
            snap.next_entity_id += 1;
            let kill_id = snap.next_entity_id;
            snap.record_killmail(
                eve_db::KillmailRecord {
                    kill_id, victim_character_id: char_id, attacker_character_id: 0,
                    solar_system_id: abyssal_sys, ship_type_id, total_value_isk: 1_000_000.0,
                    attacker_corp_id: 1_000_298, ..Default::default()
                },
                now,
            );
        }
    });
    let _ = ctx.db.respawn_character_at_home_station(char_id, ship_id, &ctx.sde);
    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    ctx.db.get_character(char_id).map(|c| c.solar_system_id).unwrap_or(30_000_142)
}

/// Evacuates active Abyssal pockets on daily downtime (`feat:lifecycle.abyssal-restart`), returning ships and pilots to their filament origin system at a random point near the trace origin.
pub fn evacuate_abyssal_pockets_for_downtime(ctx: &ServerContext) -> i64 {
    let evacuated: Vec<(CharacterId, ItemId, SolarSystemId, [f64; 3], HashMap<AttributeId, f64>)> = with_pve_special_reg(ctx, |reg| {
        let mut out = Vec::new();
        for p in reg.abyssal_pockets.values_mut() {
            if !p.completed && !p.collapsed_destroyed {
                p.completed = true;
                out.push((p.character_id, p.ship_id, p.origin_solar_system_id, p.origin_position, p.saved_ship_attrs.clone()));
            }
        }
        out
    });
    for (cid, sid, origin_sys, origin_pos, saved_attrs) in &evacuated {
        restore_abyssal_weather_dogma_modifiers(ctx, *sid, saved_attrs);
        crate::sp6_lifecycle::clear_abyssal_session(ctx, *cid);
        let evacuated_pos = random_offset_around_trace_origin(ctx, *origin_pos);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(cid) { ch.solar_system_id = *origin_sys; }
            if let Some(sh) = snap.items.get_mut(sid) { sh.location_id = *origin_sys as i64; }
            snap.item_positions.insert(*sid, evacuated_pos);
        });
    }
    evacuated.len() as i64
}

enum AbyssalLoginAction {
    Active(SolarSystemId),
    CollapsedDestroy { ship_id: ItemId, abyssal_sys: SolarSystemId },
    Evacuated { origin_sys: SolarSystemId, origin_pos: [f64; 3], saved_attrs: HashMap<AttributeId, f64> },
}

/// Ensures a character logging in inside Abyssal space (`32_000_000..33_000_000`) either resumes their intact pocket,
/// has their ship+pod destroyed if the 20-minute timer expired, or returns to the filament origin system (`feat:lifecycle.abyssal-restart`).
pub fn reconcile_abyssal_login(ctx: &ServerContext, char_id: CharacterId, cur_sys: SolarSystemId, ship_id: ItemId) -> SolarSystemId {
    if !(32_000_000..33_000_000).contains(&cur_sys) { return cur_sys; }
    let has_sp6_only = with_pve_special_reg(ctx, |reg| !reg.abyssal_pockets.contains_key(&char_id))
        && crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.abyssal_sessions.contains_key(&char_id));
    if has_sp6_only { return cur_sys; }
    let action = with_pve_special_reg(ctx, |reg| {
        let now = spec_now_sec(reg);
        if let Some(p) = reg.abyssal_pockets.get_mut(&char_id) {
            if !p.completed && !p.collapsed_destroyed && now < p.collapse_at_sec {
                return AbyssalLoginAction::Active(p.abyssal_solar_system_id);
            }
            if !p.completed && (p.collapsed_destroyed || now >= p.collapse_at_sec) {
                p.collapsed_destroyed = true;
                p.completed = true;
                return AbyssalLoginAction::CollapsedDestroy { ship_id: p.ship_id, abyssal_sys: p.abyssal_solar_system_id };
            }
            return AbyssalLoginAction::Evacuated { origin_sys: p.origin_solar_system_id, origin_pos: p.origin_position, saved_attrs: p.saved_ship_attrs.clone() };
        }
        AbyssalLoginAction::Evacuated { origin_sys: 30_000_142, origin_pos: [145_000.0, -75_000.0, 42_000.0], saved_attrs: HashMap::new() }
    });
    match action {
        AbyssalLoginAction::Active(abyssal_sys) => abyssal_sys,
        AbyssalLoginAction::CollapsedDestroy { ship_id: sid, abyssal_sys } => collapse_abyssal_pocket_and_pod_pilot(ctx, char_id, sid, abyssal_sys),
        AbyssalLoginAction::Evacuated { origin_sys, origin_pos, saved_attrs } => {
            if !saved_attrs.is_empty() { restore_abyssal_weather_dogma_modifiers(ctx, ship_id, &saved_attrs); }
            let rand_pos = random_offset_around_trace_origin(ctx, origin_pos);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = origin_sys; }
                if let Some(sh) = snap.items.get_mut(&ship_id) {
                    if sh.location_id == cur_sys as i64 { sh.location_id = origin_sys as i64; }
                }
                snap.item_positions.entry(ship_id).or_insert(rand_pos);
            });
            origin_sys
        }
    }
}

pub async fn complete_special_dungeon_instance(
    ctx: &ServerContext,
    session: &mut SessionState,
    instance_id: i64,
    fleet_count: usize,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ensure_special_sites_spawned(ctx).await;

    let caller_sys = session.solar_system_id.ok_or_else(|| rpc_err("NotInSpace", "Caller must be in space"))?;
    let sys_handle = ctx.systems.get_or_spawn(caller_sys);

    // 1. Check Drifter Hives (by solarSystemID 31000001..31000005 or dungeonID 5642..5651)
    let hive_lookup = with_pve_special_reg(ctx, |reg| {
        reg.drifter_hives.values().find(|h| h.solar_system_id as i64 == instance_id || h.dungeon_id as i64 == instance_id).cloned()
    });
    if let Some(hive) = hive_lookup {
        if caller_sys != hive.solar_system_id {
            return Err(rpc_err("NotInSiteSystem", format!("Must be in system {} to complete {}", hive.solar_system_id, hive.dungeon_name)));
        }
        if hive.cleared {
            return Err(rpc_err("SiteAlreadyCompleted", format!("Drifter Hive {} is already cleared", hive.site_id)));
        }
        if sys_handle.get_ball_snapshot(hive.warden_npc_ball_id).await.ok().flatten().is_some() {
            return Err(rpc_err("WaveNpcsRemaining", format!("Drifter Hive warden {} has not been destroyed", hive.warden_npc_ball_id)));
        }
        let (kv, iid, elem_tid) = with_pve_special_reg(ctx, |reg| {
            let next_iid = 1_899_000_000 + next_spec_seq(reg);
            let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
            h.cleared = true;
            h.current_room = h.total_rooms;
            let kv = PyRep::keyval([
                ("siteID", PyRep::String(h.site_id.clone())),
                ("dungeonID", PyRep::Integer(h.dungeon_id as i64)),
                ("dungeonName", PyRep::String(h.dungeon_name.clone())),
                ("solarSystemID", PyRep::Integer(h.solar_system_id as i64)),
                ("elementTypeID", PyRep::Integer(h.element_type_id as i64)),
                ("elementName", PyRep::String(h.element_name.clone())),
                ("wardenNpcBallID", PyRep::Integer(h.warden_npc_ball_id)),
                ("cleared", PyRep::Bool(true)),
            ]);
            (kv, next_iid, h.element_type_id)
        });
        let ship_id = session.ship_id.unwrap_or(char_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: elem_tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: false, ..Default::default()
            });
        });
        return Ok(kv);
    }

    // 2. Check Homefront Sites (710_001..710_002)
    let hf_lookup = with_pve_special_reg(ctx, |reg| {
        reg.homefront_sites.values().flatten().find(|s| s.instance_id == instance_id).cloned()
    });
    if let Some(hf) = hf_lookup {
        if caller_sys != hf.solar_system_id {
            return Err(rpc_err("NotInSiteSystem", format!("Must be in system {} to complete {}", hf.solar_system_id, hf.dungeon_name)));
        }
        if hf.completed {
            return Err(rpc_err("SiteAlreadyCompleted", format!("Homefront site {} is already completed", hf.instance_id)));
        }
        if sys_handle.get_ball_snapshot(hf.boss_npc_ball_id).await.ok().flatten().is_some() {
            return Err(rpc_err("WaveNpcsRemaining", format!("Homefront site wave NPC {} is still alive", hf.boss_npc_ball_id)));
        }
        let mult = if fleet_count == hf.optimal_fleet_size {
            1.0
        } else if fleet_count < hf.optimal_fleet_size {
            (fleet_count as f64) / (hf.optimal_fleet_size as f64)
        } else {
            0.75
        };
        let reward_paid = ((hf.base_pilot_isk as f64) * mult).round() as i64;
        with_pve_special_reg(ctx, |reg| {
            if let Some(list) = reg.homefront_sites.get_mut(&hf.solar_system_id) {
                if let Some(s) = list.iter_mut().find(|s| s.instance_id == instance_id) {
                    s.completed = true;
                    s.reward_paid_isk = reward_paid;
                }
            }
        });
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = ch.balance.checked_add(IskCents::from_isk(reward_paid)).unwrap_or(ch.balance);
            }
        });
        return Ok(PyRep::keyval([
            ("instanceID", PyRep::Integer(hf.instance_id)),
            ("completed", PyRep::Bool(true)),
            ("rewardPaidIsk", PyRep::Integer(reward_paid)),
        ]));
    }

    // 3. Check Triglavian Sites (720_001..720_002)
    let trig_lookup = with_pve_special_reg(ctx, |reg| {
        reg.triglavian_sites.values().flatten().find(|s| s.instance_id == instance_id).cloned()
    });
    if let Some(trig) = trig_lookup {
        if caller_sys != trig.solar_system_id {
            return Err(rpc_err("NotInSiteSystem", format!("Must be in system {} to complete {}", trig.solar_system_id, trig.dungeon_name)));
        }
        if trig.completed {
            return Err(rpc_err("SiteAlreadyCompleted", format!("Triglavian site {} is already completed", trig.instance_id)));
        }
        if sys_handle.get_ball_snapshot(trig.boss_npc_ball_id).await.ok().flatten().is_some() {
            return Err(rpc_err("WaveNpcsRemaining", format!("Triglavian site wave NPC {} is still alive", trig.boss_npc_ball_id)));
        }
        let loot_iid = with_pve_special_reg(ctx, |reg| {
            let seq_id = 1_899_500_000 + next_spec_seq(reg);
            if let Some(list) = reg.triglavian_sites.get_mut(&trig.solar_system_id) {
                if let Some(s) = list.iter_mut().find(|s| s.instance_id == instance_id) {
                    s.completed = true;
                }
            }
            seq_id
        });
        let (gain_faction, lose_faction) = if trig.faction_owner_id == 500_027 { (500_026_i64, 500_027_i64) } else { (500_027_i64, 500_026_i64) };
        let ship_id = session.ship_id.unwrap_or(char_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let g = ch.standings.entry(gain_faction).or_insert(0.0);
                *g = (*g + 0.15).clamp(-10.0, 10.0);
                let l = ch.standings.entry(lose_faction).or_insert(0.0);
                *l = (*l - 0.15).clamp(-10.0, 10.0);
                ch.balance = ch.balance.checked_add(IskCents::from_isk(trig.base_isk)).unwrap_or(ch.balance);
            }
            snap.items.insert(loot_iid, InventoryItemRecord {
                item_id: loot_iid, type_id: 48_121, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: trig.survey_db_drop as i32, singleton: false, ..Default::default()
            });
        });
        return Ok(PyRep::keyval([
            ("instanceID", PyRep::Integer(trig.instance_id)),
            ("completed", PyRep::Bool(true)),
            ("baseIsk", PyRep::Integer(trig.base_isk)),
        ]));
    }

    Err(rpc_err("DungeonNotFound", format!("Dungeon instance {instance_id} not found")))
}

fn resolve_session_system_id(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<SolarSystemId, BridgeRpcError> {
    session
        .solar_system_id
        .or_else(|| {
            session.character_id.and_then(|cid| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
            })
        })
        .ok_or_else(|| rpc_err("NotInSpace", "Action requires an active solar system session"))
}

pub async fn handle_pve_special_sites(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(
        service,
        "incursion"
            | "incursionSvc"
            | "taleMgr"
            | "dungeonInstanceCacheMgr"
            | "map"
            | "solarsystemInterferenceMgr"
    ) {
        return None;
    }
    ensure_special_sites_spawned(ctx).await;

    if let Some(res) = crate::services::pve_incursion_sites::handle_incursion_rpc(
        ctx, session, service, method, args, _kwargs,
    )
    .await
    {
        return Some(res);
    }

    match (service, method) {
        // parity: feat:pve.homefront-ops
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sites_py = with_pve_special_reg(ctx, |reg| {
                reg.homefront_sites.get(&sys_id).map(|list| {
                    list.iter().map(|s| PyRep::keyval([
                        ("instanceID", PyRep::Integer(s.instance_id)), ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(s.dungeon_name.clone())), ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                        ("optimalFleetSize", PyRep::Integer(s.optimal_fleet_size as i64)), ("basePilotIsk", PyRep::Integer(s.base_pilot_isk)),
                        ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)), ("completed", PyRep::Bool(s.completed)),
                        ("rewardPaidIsk", PyRep::Integer(s.reward_paid_isk)),
                    ])).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::List(sites_py)))
        }

        // parity: feat:pve.triglavian-sites, feat:pve.drifter-hives
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if (31_000_001..=31_000_005).contains(&sys_id) {
                let hive_res = with_pve_special_reg(ctx, |reg| {
                    let Some(hive) = reg.drifter_hives.get(&sys_id) else {
                        return Err(rpc_err("DrifterHiveNotFound", format!("No Drifter Hive in system {sys_id}")));
                    };
                    Ok(PyRep::keyval([
                        ("siteID", PyRep::String(hive.site_id.clone())), ("dungeonID", PyRep::Integer(hive.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(hive.dungeon_name.clone())), ("solarSystemID", PyRep::Integer(hive.solar_system_id as i64)),
                        ("elementTypeID", PyRep::Integer(hive.element_type_id as i64)), ("elementName", PyRep::String(hive.element_name.clone())),
                        ("wardenNpcBallID", PyRep::Integer(hive.warden_npc_ball_id)), ("cleared", PyRep::Bool(hive.cleared)),
                    ]))
                });
                return Some(hive_res);
            }

            let trig_py = with_pve_special_reg(ctx, |reg| {
                reg.triglavian_sites.get(&sys_id).map(|list| {
                    list.iter().map(|s| PyRep::keyval([
                        ("instanceID", PyRep::Integer(s.instance_id)), ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(s.dungeon_name.clone())), ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                        ("factionOwnerID", PyRep::Integer(s.faction_owner_id as i64)), ("baseIsk", PyRep::Integer(s.base_isk)),
                        ("surveyDatabaseDrop", PyRep::Integer(s.survey_db_drop)), ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                        ("completed", PyRep::Bool(s.completed)),
                    ])).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::List(trig_py)))
        }

        // parity: feat:pve.triglavian-sites, rpc:map.GetTriglavianMinorVictorySystems
        // const-ok: No active invasion campaigns configured
        ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let systems = with_pve_special_reg(ctx, |reg| reg.triglavian_minor_victory_systems.clone());
            Some(Ok(PyRep::List(systems.into_iter().map(|s| PyRep::Integer(s as i64)).collect())))
        }

        // parity: feat:pve.metaliminal-storms
        ("map", "GetAllRoamingWeatherSystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let out = with_pve_special_reg(ctx, |reg| {
                reg.roaming_storms.iter().map(|s| PyRep::keyval([
                    ("stormID", PyRep::Integer(s.storm_id as i64)),
                    ("weather", PyRep::String(s.weather.as_str().into())),
                    ("centerSolarSystemID", PyRep::Integer(s.center_solar_system_id as i64)),
                    ("strongCoreSystems", PyRep::List(s.strong_core_systems.iter().map(|&id| PyRep::Integer(id as i64)).collect())),
                    ("weakPeripherySystems", PyRep::List(s.weak_periphery_systems.iter().map(|&id| PyRep::Integer(id as i64)).collect())),
                    ("stepCount", PyRep::Integer(s.step_count as i64)),
                ])).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(out)))
        }

        // parity: feat:pve.interference
        ("solarsystemInterferenceMgr", "GetLocalInterferenceState")
        | ("solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand")
        | ("solarsystemInterferenceMgr", "GetAllInterferenceBands") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let now_sec = ctx.now_unix();
            let (level, band, changed_from) = with_pve_special_reg(ctx, |reg| {
                let entry = reg.interference_by_system.entry(sys_id).or_insert(SystemInterferenceRecord {
                    solar_system_id: sys_id, interference_level: 0.10, band: 1, last_updated_sec: now_sec,
                });
                let mut changed = None;
                if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
                    let elapsed = now_sec - entry.last_updated_sec;
                    let decay = (elapsed as f64) * 0.001;
                    entry.interference_level = (entry.interference_level - decay).max(0.10);
                    let old_b = entry.band;
                    entry.band = if entry.interference_level >= 0.70 { 3 } else if entry.interference_level >= 0.35 { 2 } else { 1 };
                    entry.last_updated_sec = now_sec;
                    if entry.band != old_b {
                        changed = Some(old_b);
                    }
                }
                (entry.interference_level, entry.band, changed)
            });

            if changed_from.is_some() {
                session.push_notification(
                    "OnSolarsystemInterferenceChanged",
                    vec![PyRep::Integer(sys_id as i64), PyRep::Real(level), PyRep::Integer(band as i64)],
                );
                ctx.notify(
                    crate::router::NotificationTarget::SolarSystem(sys_id),
                    "OnSolarsystemInterferenceChanged",
                    "solarsystemid",
                    vec![PyRep::Integer(sys_id as i64), PyRep::Real(level), PyRep::Integer(band as i64)],
                );
            }

            let band_label = match band { 1 => "Low", 2 => "Medium", _ => "High" };
            let local_state = PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("interferenceLevel", PyRep::Real(level)),
                ("interferenceBand", PyRep::Integer(band as i64)),
                ("bandLabel", PyRep::String(band_label.into())),
            ]);
            if method == "GetAllInterferenceBands" {
                let entries: Vec<(PyRep, PyRep)> = with_pve_special_reg(ctx, |reg| {
                    let mut out: Vec<(SolarSystemId, u8)> = reg
                        .interference_by_system
                        .iter()
                        .map(|(&sid, rec)| (sid, rec.band))
                        .collect();
                    if !reg.interference_by_system.contains_key(&sys_id) {
                        out.push((sys_id, band));
                    }
                    out.sort_unstable_by_key(|(sid, _)| *sid);
                    out.into_iter()
                        .map(|(sid, b)| (PyRep::Integer(sid as i64), PyRep::Integer(b as i64)))
                        .collect()
                });
                return Some(Ok(PyRep::Dict(entries)));
            }
            Some(Ok(local_state))
        }

        _ => None,
    }
}

pub async fn handle_abyssal_entrance_deployment(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_err("ShipDocked", "Cannot deploy Abyssal filament while docked"));
    }
    let ship_id = session.ship_id.ok_or_else(|| rpc_err("NoActiveShip", "No active ship"))?;
    let cur_sys = session.solar_system_id.ok_or_else(|| rpc_err("NoSolarSystem", "Session has no active solar system"))?;
    let filament_arg = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

    // Validate hull: Allowed are Cruisers, Frigates, Destroyers. Disallowed: Battleships, Capitals, Battlecruisers, Industrials.
    let (ship_type_id, ship_group_id) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&ship_id);
        let tid = it.map(|i| i.type_id).unwrap_or(0);
        let gid = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(0);
        (tid, gid)
    });
    let is_allowed_hull = matches!(
        ship_group_id,
        25 | 26 | 324 | 358 | 420 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 963 | 1305
    );
    if !is_allowed_hull {
        return Err(rpc_err("DisallowedShipType", format!("Ship type {ship_type_id} (group {ship_group_id}) is not permitted to enter Abyssal Deadspace")));
    }

    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;

    let now_unix = ctx.now_unix();
    let (_filament_item_id, filament_type_id, tier, origin_sec) = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap.items.values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && (i.item_id == filament_arg || i.type_id as i64 == filament_arg)
                    && i.quantity >= 1
            })
            .map(|i| (i.item_id, i.type_id));
        let Some((iid, tid)) = found else {
            return Err(rpc_err("ItemNotFound", format!("Abyssal filament {filament_arg} not found in ship cargo")));
        };
        let Some((tier, _)) = classify_abyssal_filament(tid) else {
            return Err(rpc_err("InvalidAbyssalFilament", format!("Type {tid} is not a valid Abyssal Deadspace filament")));
        };
        let origin_sec = ctx.sde.solar_systems.get(&cur_sys).map(|s| s.security_status).unwrap_or(0.9);
        if tier >= 4 && origin_sec >= 0.75 {
            return Err(rpc_err("FilamentDisallowedInHighSec", "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems"));
        }
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > 1 { item.quantity -= 1; item.version += 1; } else { snap.items.remove(&iid); }
        }
        Ok((iid, tid, tier, origin_sec))
    })?;

    if tier >= 4 && (0.45..0.75).contains(&origin_sec) {
        ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).trigger_suspect_flag(char_id, now_unix);
    }

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    let ship_pos = if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ball.position
    } else {
        DVec3::ZERO
    };
    let trace_pos = ship_pos + DVec3::new(500.0, 0.0, 0.0);
    let trace_ball_id = with_pve_special_reg(ctx, |reg| {
        let tid = 1_897_000_000 + next_spec_seq(reg);
        reg.abyssal_traces.insert(
            tid,
            AbyssalTraceRecord {
                trace_ball_id: tid,
                char_id,
                filament_type_id,
                origin_solar_system_id: cur_sys,
                position: [trace_pos.x, trace_pos.y, trace_pos.z],
                created_unix: now_unix,
                expires_at_sec: now_unix + 1_200,
            },
        );
        tid
    });

    let _ = sys_handle.spawn_ball(make_site_npc_ball(trace_ball_id, 47_956, trace_pos)).await;

    Ok(PyRep::keyval([
        ("traceBallID", PyRep::Integer(trace_ball_id)),
        ("solarSystemID", PyRep::Integer(cur_sys as i64)),
        ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
    ]))
}
