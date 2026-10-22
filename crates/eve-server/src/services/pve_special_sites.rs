//! `pve_special_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale), Homefront Operations, Metaliminal Roaming Storms, Drifter Hives, Triglavian
//! Pochven/EDENCOM Sites, and Solar System Interference (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership
// parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.triglavian-sites, feat:pve.interference

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::ctx_pve_key;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WalletJournalEntry};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{LazyLock, Mutex};

type CharacterId = i64;
pub type ConstellationId = u32;

#[derive(Debug, Clone)]
pub struct IncursionSiteEntry {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub tier: String,
    pub min_pilots: usize,
    pub max_optimal_pilots: usize,
    pub base_isk: i64,
    pub base_concord_lp: i64,
    pub influence_delta: f64,
    pub locked_until_low_influence: bool,
    pub completed_count: u32,
    pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone)]
pub struct IncursionTaleRecord {
    pub tale_id: i64,
    pub constellation_id: ConstellationId,
    pub constellation_name: String,
    pub staging_solar_system_id: SolarSystemId,
    pub vanguard_solar_system_ids: Vec<SolarSystemId>,
    pub assault_solar_system_ids: Vec<SolarSystemId>,
    pub hq_solar_system_id: SolarSystemId,
    pub influence: f64,
    pub state: String,
    pub mothership_spawned: bool,
    pub mothership_defeated: bool,
    pub sites: Vec<IncursionSiteEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbyssalWeatherKind {
    Electrical,
    Dark,
    Exotic,
    Firestorm,
    Gamma,
}

impl AbyssalWeatherKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Electrical => "Electrical",
            Self::Dark => "Dark",
            Self::Exotic => "Exotic",
            Self::Firestorm => "Firestorm",
            Self::Gamma => "Gamma",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HomefrontSiteRecord {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub optimal_fleet_size: usize,
    pub base_pilot_isk: i64,
    pub completed: bool,
    pub reward_paid_isk: i64,
    pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone)]
pub struct RoamingStormRecord {
    pub storm_id: u32,
    pub weather: AbyssalWeatherKind,
    pub center_solar_system_id: SolarSystemId,
    pub strong_core_systems: Vec<SolarSystemId>,
    pub weak_periphery_systems: Vec<SolarSystemId>,
    pub step_count: u32,
}

#[derive(Debug, Clone)]
pub struct DrifterHiveRecord {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub element_type_id: TypeId,
    pub element_name: String,
    pub current_room: u8,
    pub total_rooms: u8,
    pub warden_npc_ball_id: i64,
    pub cleared: bool,
}

#[derive(Debug, Clone)]
pub struct TriglavianSiteRecord {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub faction_owner_id: u32,
    pub base_isk: i64,
    pub survey_db_drop: i64,
    pub completed: bool,
    pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone)]
pub struct SystemInterferenceRecord {
    pub solar_system_id: SolarSystemId,
    pub interference_level: f64,
    pub band: u8,
}

#[derive(Default)]
pub struct PveSpecialSitesRegistry {
    pub db_weak: std::sync::Weak<eve_db::WorldRepository>,
    pub initialized: bool,
    pub balls_spawned: bool,
    pub incursions: HashMap<ConstellationId, IncursionTaleRecord>,
    pub homefront_sites: HashMap<SolarSystemId, Vec<HomefrontSiteRecord>>,
    pub roaming_storms: Vec<RoamingStormRecord>,
    pub drifter_hives: HashMap<SolarSystemId, DrifterHiveRecord>,
    pub triglavian_sites: HashMap<SolarSystemId, Vec<TriglavianSiteRecord>>,
    pub interference_by_system: HashMap<SolarSystemId, SystemInterferenceRecord>,
    pub sim_clock_sec: u64,
    pub next_seq: i64,
}

pub static PVE_SPECIAL_STATE: LazyLock<Mutex<HashMap<usize, PveSpecialSitesRegistry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn with_pve_special_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveSpecialSitesRegistry) -> R,
) -> R {
    let mut map = PVE_SPECIAL_STATE.lock().unwrap();
    map.retain(|_, v| v.db_weak.strong_count() > 0);
    let reg = map.entry(ctx_pve_key(ctx)).or_insert_with(|| PveSpecialSitesRegistry {
        db_weak: std::sync::Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(reg)
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn make_site_npc_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 380.0,
        inertia: 1.0,
        mass: 4_000_000.0,
        radius: 220.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

fn next_spec_seq(reg: &mut PveSpecialSitesRegistry) -> i64 {
    reg.next_seq += 1;
    reg.next_seq
}

pub fn advance_pve_special_clock(ctx: &ServerContext, delta_sec: u64) {
    ensure_special_sites_initialized(ctx);
    with_pve_special_reg(ctx, |reg| {
        if reg.sim_clock_sec == 0 {
            reg.sim_clock_sec = 1_775_000_000;
        }
        reg.sim_clock_sec += delta_sec;
        if delta_sec >= 3_600 {
            for storm in &mut reg.roaming_storms {
                if let Some(&next_center) = storm
                    .strong_core_systems
                    .iter()
                    .find(|&&s| s != storm.center_solar_system_id)
                {
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
    let (level, old_band, new_band) = with_pve_special_reg(ctx, |reg| {
        let entry = reg.interference_by_system.entry(solar_system_id).or_insert(SystemInterferenceRecord {
            solar_system_id,
            interference_level: 0.10,
            band: 1,
        });
        let prev = entry.band;
        if delta > 0.0 {
            entry.interference_level = (entry.interference_level + delta).clamp(0.0, 1.0);
            entry.band = if entry.interference_level >= 0.70 {
                3
            } else if entry.interference_level >= 0.35 {
                2
            } else {
                1
            };
        }
        (entry.interference_level, prev, entry.band)
    });
    if new_band != old_band {
        session.push_notification(
            "OnSolarsystemInterferenceChanged",
            vec![
                PyRep::Integer(solar_system_id as i64),
                PyRep::Real(level),
                PyRep::Integer(new_band as i64),
            ],
        );
    }
}

fn make_incursion_site(
    site_id: &str,
    dungeon_id: u32,
    dungeon_name: &str,
    solar_system_id: SolarSystemId,
    tier: &str,
    min_pilots: usize,
    max_optimal_pilots: usize,
    base_isk: i64,
    base_concord_lp: i64,
    influence_delta: f64,
    locked_until_low_influence: bool,
) -> IncursionSiteEntry {
    IncursionSiteEntry {
        site_id: site_id.into(),
        dungeon_id,
        dungeon_name: dungeon_name.into(),
        solar_system_id,
        tier: tier.into(),
        min_pilots,
        max_optimal_pilots,
        base_isk,
        base_concord_lp,
        influence_delta,
        locked_until_low_influence,
        completed_count: 0,
        boss_npc_ball_id: 1_896_000_000 + (dungeon_id as i64),
    }
}

pub fn ensure_special_sites_initialized(ctx: &ServerContext) {
    with_pve_special_reg(ctx, |reg| {
        if reg.initialized {
            return;
        }
        reg.initialized = true;

        let constellation_id: ConstellationId = 20_000_020;
        let sites = vec![
            make_incursion_site("INC-STG-5164", 5164, "Override Transfer Array (Staging)", 30_000_140, "Staging", 3, 5, 3_500_000, 400, 0.01, false),
            make_incursion_site("INC-VG-5165", 5165, "Nation Mining Colony", 30_000_142, "Vanguard", 5, 10, 10_395_000, 1_400, 0.05, false),
            make_incursion_site("INC-AS-5168", 5168, "Nation Commander Stronghold", 30_000_139, "Assault", 10, 20, 18_200_000, 3_500, 0.10, false),
            make_incursion_site("INC-HQ-5175", 5175, "True Creations Research Center", 30_000_145, "Headquarters", 20, 40, 31_500_000, 7_000, 0.25, false),
            make_incursion_site("INC-MS-5158", 5158, "The Kundalini Manifest", 30_000_145, "Mothership", 40, 80, 63_000_000, 14_000, 1.0, true),
        ];
        reg.incursions.insert(
            constellation_id,
            IncursionTaleRecord {
                tale_id: 800_001,
                constellation_id,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: 30_000_140,
                vanguard_solar_system_ids: vec![30_000_142, 30_000_144],
                assault_solar_system_ids: vec![30_000_139, 30_000_141],
                hq_solar_system_id: 30_000_145,
                influence: 1.0,
                state: "Establishing".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites,
            },
        );

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

        reg.triglavian_sites.insert(
            30_000_142,
            vec![
                TriglavianSiteRecord {
                    instance_id: 720_001, dungeon_id: 8922, dungeon_name: "Observatory Flashpoint".into(),
                    solar_system_id: 30_000_142, faction_owner_id: 500_027, base_isk: 35_000_000,
                    survey_db_drop: 150, completed: false, boss_npc_ball_id: 1_897_720_001,
                },
                TriglavianSiteRecord {
                    instance_id: 720_002, dungeon_id: 8802, dungeon_name: "Major Conduit".into(),
                    solar_system_id: 30_000_142, faction_owner_id: 500_026, base_isk: 25_000_000,
                    survey_db_drop: 100, completed: false, boss_npc_ball_id: 1_897_720_002,
                },
            ],
        );
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
    let to_spawn: Vec<(SolarSystemId, i64)> = with_pve_special_reg(ctx, |reg| {
        if reg.balls_spawned {
            return Vec::new();
        }
        reg.balls_spawned = true;
        let mut list = Vec::new();
        for inc in reg.incursions.values() {
            for s in &inc.sites { list.push((s.solar_system_id, s.boss_npc_ball_id)); }
        }
        for sites in reg.homefront_sites.values() {
            for s in sites { list.push((s.solar_system_id, s.boss_npc_ball_id)); }
        }
        for hive in reg.drifter_hives.values() {
            list.push((hive.solar_system_id, hive.warden_npc_ball_id));
        }
        for sites in reg.triglavian_sites.values() {
            for s in sites { list.push((s.solar_system_id, s.boss_npc_ball_id)); }
        }
        list
    });
    for (sys_id, bid) in to_spawn {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, 10_962, DVec3::new(10_000.0, 0.0, 0.0))).await;
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

    match (service, method) {
        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetActiveIncursionData")
        | ("incursion", "GetAllIncursions")
        | ("incursion", "GetIncursionGlobalReport")
        | ("incursionSvc", "GetActiveIncursionData")
        | ("taleMgr", "GetActiveIncursionData") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let list = with_pve_special_reg(ctx, |reg| {
                reg.incursions.values().map(|inc| {
                    PyRep::keyval([
                        ("taleID", PyRep::Integer(inc.tale_id)),
                        ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                        ("constellationName", PyRep::String(inc.constellation_name.clone())),
                        ("stagingSolarSystemID", PyRep::Integer(inc.staging_solar_system_id as i64)),
                        ("hqSolarSystemID", PyRep::Integer(inc.hq_solar_system_id as i64)),
                        ("influence", PyRep::Real(inc.influence)),
                        ("state", PyRep::String(inc.state.clone())),
                        ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
                        ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
                    ])
                }).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetAllIncursionSites")
        | ("incursion", "GetIncursionDungeons")
        | ("incursion", "GetIncursionState")
        | ("incursion", "GetSystemsInIncursions")
        | ("incursion", "JoinIncursionChat") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let const_id = args.first().and_then(|a| a.as_i64()).map(|v| v as ConstellationId).unwrap_or(20_000_020);
            let res = with_pve_special_reg(ctx, |reg| {
                let Some(inc) = reg.incursions.get(&const_id) else {
                    return Err(rpc_err("IncursionNotFound", format!("No active incursion in constellation {const_id}")));
                };
                let sites_py: Vec<PyRep> = inc.sites.iter().map(|s| {
                    let available = !s.locked_until_low_influence || inc.mothership_spawned;
                    PyRep::keyval([
                        ("siteID", PyRep::String(s.site_id.clone())),
                        ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                        ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                        ("tier", PyRep::String(s.tier.clone())),
                        ("minPilots", PyRep::Integer(s.min_pilots as i64)),
                        ("maxOptimalPilots", PyRep::Integer(s.max_optimal_pilots as i64)),
                        ("baseIsk", PyRep::Integer(s.base_isk)),
                        ("baseConcordLP", PyRep::Integer(s.base_concord_lp)),
                        ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                        ("available", PyRep::Bool(available)),
                    ])
                }).collect();
                Ok(PyRep::keyval([
                    ("taleID", PyRep::Integer(inc.tale_id)),
                    ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                    ("influence", PyRep::Real(inc.influence)),
                    ("state", PyRep::String(inc.state.clone())),
                    ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
                    ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
                    ("sites", PyRep::List(sites_py)),
                ]))
            });
            Some(res)
        }

        // parity: feat:pve.incursion-payout, feat:pve.incursion-mothership
        ("incursion", "UpdateIncursionLPLog") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("INC-VG-5165").to_string();
            let raw_pilots: Vec<CharacterId> = if let Some(PyRep::List(list)) = args.get(1) {
                let parsed: Vec<CharacterId> = list.iter().filter_map(|p| p.as_i64()).collect();
                if parsed.is_empty() { vec![char_id] } else { parsed }
            } else {
                vec![char_id]
            };

            let (site_sys, boss_ball_id) = match with_pve_special_reg(ctx, |reg| {
                let Some(inc) = reg.incursions.get(&20_000_020) else {
                    return Err(rpc_err("NoActiveIncursion", "No active incursion in constellation 20000020"));
                };
                if inc.state == "Ended" {
                    return Err(rpc_err("IncursionAlreadyEnded", "This incursion has already ended"));
                }
                let Some(site) = inc.sites.iter().find(|s| s.site_id == site_id) else {
                    return Err(rpc_err("IncursionSiteNotFound", format!("Incursion site {site_id} not found")));
                };
                if site.locked_until_low_influence && !inc.mothership_spawned {
                    return Err(rpc_err("MothershipSiteLocked", format!("Mothership HQ site {} is locked while Sansha influence ({:.2}) > 0.25", site.dungeon_name, inc.influence)));
                }
                Ok((site.solar_system_id, site.boss_npc_ball_id))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.solar_system_id != Some(site_sys) {
                return Some(Err(rpc_err("NotInIncursionSiteSystem", format!("Caller must be in incursion site system {site_sys} to complete {site_id}"))));
            }
            let sys_handle = ctx.systems.get_or_spawn(site_sys);
            if sys_handle.get_ball_snapshot(boss_ball_id).await.ok().flatten().is_some() {
                return Some(Err(rpc_err("IncursionSiteNpcsRemaining", format!("Incursion site {site_id} boss wave NPC {boss_ball_id} has not been destroyed"))));
            }

            let step_res = with_pve_special_reg(ctx, |reg| {
                let inc = reg.incursions.get_mut(&20_000_020).unwrap();
                let site = inc.sites.iter_mut().find(|s| s.site_id == site_id).unwrap();
                site.completed_count += 1;
                let (b_isk, b_lp, opt, s_tier, delta) = (site.base_isk, site.base_concord_lp, site.max_optimal_pilots, site.tier.clone(), site.influence_delta);
                let next_boss_ball = if s_tier == "Mothership" {
                    inc.influence = 0.0;
                    inc.mothership_defeated = true;
                    inc.state = "Ended".into();
                    None
                } else {
                    inc.influence = (inc.influence - delta).max(0.0);
                    if inc.influence <= 0.25 {
                        inc.mothership_spawned = true;
                        inc.state = "Mobilizing".into();
                    }
                    site.boss_npc_ball_id += 1_000;
                    Some(site.boss_npc_ball_id)
                };
                Ok((b_isk, b_lp, opt, s_tier, inc.influence, inc.state.clone(), inc.mothership_spawned, inc.mothership_defeated, next_boss_ball))
            });
            let (base_isk, base_lp, max_opt, tier, new_influence, new_state, mothership_unlocked, incursion_ended, next_boss_ball) = match step_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if let Some(new_bid) = next_boss_ball {
                let _ = sys_handle.spawn_ball(make_site_npc_ball(new_bid, 10_962, DVec3::new(10_000.0, 0.0, 0.0))).await;
            }

            let fleet_pilots: Vec<CharacterId> = ctx.db.with_snapshot(|snap| {
                raw_pilots.into_iter().filter(|&pid| {
                    pid == char_id || snap.characters.get(&pid).is_some_and(|ch| ch.solar_system_id == site_sys && ch.station_id.is_none())
                }).collect()
            });

            let fleet_count = fleet_pilots.len().max(1);
            let over_pilots = fleet_count.saturating_sub(max_opt);
            let fleet_scale = (1.0_f64 - (over_pilots as f64) * 0.12).clamp(0.15, 1.0);
            let pilot_isk = ((base_isk as f64) * fleet_scale).round() as i64;
            let pilot_lp = ((base_lp as f64) * fleet_scale).round() as i64;
            let concord_corp_id: i64 = 1_000_125;

            ctx.db.with_mut_snapshot(|snap| {
                for &pid in &fleet_pilots {
                    let j_id = (snap.wallet_journal.len() as i64) + 950_000;
                    if let Some(ch) = snap.characters.get_mut(&pid) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(pilot_isk)).unwrap_or(ch.balance);
                        let new_bal = ch.balance;
                        *ch.loyalty_points.entry(concord_corp_id).or_insert(0) += pilot_lp;
                        snap.wallet_journal.push(WalletJournalEntry {
                            entry_id: j_id, character_id: pid, entry_type: "CorporateRewardPayout".into(),
                            amount: IskCents::from_isk(pilot_isk), balance_after: new_bal,
                            reason: format!("Sansha Incursion {tier} Payout ({site_id})"),
                        });
                    }
                }
            });

            Some(Ok(PyRep::keyval([
                ("siteID", PyRep::String(site_id)),
                ("tier", PyRep::String(tier)),
                ("fleetSize", PyRep::Integer(fleet_count as i64)),
                ("fleetScaleFactor", PyRep::Real(fleet_scale)),
                ("iskPerPilot", PyRep::Integer(pilot_isk)),
                ("concordLpPerPilot", PyRep::Integer(pilot_lp)),
                ("influence", PyRep::Real(new_influence)),
                ("state", PyRep::String(new_state)),
                ("mothershipSpawned", PyRep::Bool(mothership_unlocked)),
                ("incursionEnded", PyRep::Bool(incursion_ended)),
            ])))
        }

        // parity: feat:pve.homefront-ops
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);
            let sites_py = with_pve_special_reg(ctx, |reg| {
                reg.homefront_sites.get(&sys_id).map(|list| {
                    list.iter().map(|s| PyRep::keyval([
                        ("instanceID", PyRep::Integer(s.instance_id)),
                        ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                        ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                        ("optimalFleetSize", PyRep::Integer(s.optimal_fleet_size as i64)),
                        ("basePilotIsk", PyRep::Integer(s.base_pilot_isk)),
                        ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                        ("completed", PyRep::Bool(s.completed)),
                        ("rewardPaidIsk", PyRep::Integer(s.reward_paid_isk)),
                    ])).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::List(sites_py)))
        }

        // parity: feat:pve.triglavian-sites, feat:pve.drifter-hives
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances")
        | ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);

            if (31_000_001..=31_000_005).contains(&sys_id) {
                let hive_res = with_pve_special_reg(ctx, |reg| {
                    let Some(hive) = reg.drifter_hives.get(&sys_id) else {
                        return Err(rpc_err("DrifterHiveNotFound", format!("No Drifter Hive in system {sys_id}")));
                    };
                    Ok(PyRep::keyval([
                        ("siteID", PyRep::String(hive.site_id.clone())),
                        ("dungeonID", PyRep::Integer(hive.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(hive.dungeon_name.clone())),
                        ("solarSystemID", PyRep::Integer(hive.solar_system_id as i64)),
                        ("elementTypeID", PyRep::Integer(hive.element_type_id as i64)),
                        ("elementName", PyRep::String(hive.element_name.clone())),
                        ("wardenNpcBallID", PyRep::Integer(hive.warden_npc_ball_id)),
                        ("cleared", PyRep::Bool(hive.cleared)),
                    ]))
                });
                return Some(hive_res);
            }

            let trig_py = with_pve_special_reg(ctx, |reg| {
                reg.triglavian_sites.get(&sys_id).map(|list| {
                    list.iter().map(|s| PyRep::keyval([
                        ("instanceID", PyRep::Integer(s.instance_id)),
                        ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                        ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                        ("factionOwnerID", PyRep::Integer(s.faction_owner_id as i64)),
                        ("baseIsk", PyRep::Integer(s.base_isk)),
                        ("surveyDatabaseDrop", PyRep::Integer(s.survey_db_drop)),
                        ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                        ("completed", PyRep::Bool(s.completed)),
                    ])).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::List(trig_py)))
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
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);

            let (level, band) = with_pve_special_reg(ctx, |reg| {
                reg.interference_by_system
                    .get(&sys_id)
                    .map(|entry| (entry.interference_level, entry.band))
                    .unwrap_or((0.10, 1))
            });

            let band_label = match band { 1 => "Low", 2 => "Medium", _ => "High" };
            let local_state = PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("interferenceLevel", PyRep::Real(level)),
                ("interferenceBand", PyRep::Integer(band as i64)),
                ("bandLabel", PyRep::String(band_label.into())),
            ]);
            if method == "GetAllInterferenceBands" {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "solarsystemInterferenceMgr.GetAllInterferenceBands is not implemented".into(),
                }));
            }
            Some(Ok(local_state))
        }

        _ => None,
    }
}
