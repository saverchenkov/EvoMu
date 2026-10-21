//! `pve_special_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale), Abyssal Deadspace (3-Room Pockets, Gates, 5 Weather Dbuffs, T0–T6 Tiers &
//! Collapse Timer), Homefront Operations, Metaliminal Roaming Storms, Drifter Hives, Triglavian
//! Pochven/EDENCOM Sites, and Solar System Interference (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership
// parity: feat:pve.abyssal-pocket, feat:pve.abyssal-gates, feat:pve.abyssal-weather, feat:pve.abyssal-tiers
// parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.triglavian-sites, feat:pve.interference

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::ctx_pve_key;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WalletJournalEntry};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, AttributeId, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{LazyLock, Mutex};

type CharacterId = i64;
type ItemId = i64;
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
pub struct AbyssalRoomRecord {
    pub room_index: u8,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub cache_item_id: ItemId,
    pub cleared: bool,
}

#[derive(Debug, Clone)]
pub struct AbyssalPocketSession {
    pub pocket_id: i64,
    pub character_id: CharacterId,
    pub ship_id: ItemId,
    pub origin_solar_system_id: SolarSystemId,
    pub abyssal_solar_system_id: SolarSystemId,
    pub filament_type_id: TypeId,
    pub tier: u8,
    pub weather: AbyssalWeatherKind,
    pub weather_penalty_pct: f64,
    pub weather_bonus_pct: f64,
    pub npc_difficulty_mult: f64,
    pub current_room: u8,
    pub rooms: [AbyssalRoomRecord; 3],
    pub started_at_sec: u64,
    pub collapse_at_sec: u64,
    pub saved_ship_attrs: HashMap<AttributeId, f64>,
    pub completed: bool,
    pub collapsed_destroyed: bool,
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
}

#[derive(Debug, Clone)]
pub struct SystemInterferenceRecord {
    pub solar_system_id: SolarSystemId,
    pub interference_level: f64,
    pub band: u8,
}

#[derive(Default)]
pub struct PveSpecialSitesRegistry {
    pub initialized: bool,
    pub incursions: HashMap<ConstellationId, IncursionTaleRecord>,
    pub abyssal_pockets: HashMap<CharacterId, AbyssalPocketSession>,
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
    let reg = map.entry(ctx_pve_key(ctx)).or_default();
    f(reg)
}

fn next_spec_seq(reg: &mut PveSpecialSitesRegistry) -> i64 {
    reg.next_seq += 1;
    reg.next_seq
}

fn spec_now_sec(reg: &PveSpecialSitesRegistry) -> u64 {
    if reg.sim_clock_sec > 0 {
        reg.sim_clock_sec
    } else {
        1_775_000_000
    }
}

pub fn advance_pve_special_clock(ctx: &ServerContext, delta_sec: u64) {
    with_pve_special_reg(ctx, |reg| {
        if reg.sim_clock_sec == 0 {
            reg.sim_clock_sec = 1_775_000_000;
        }
        reg.sim_clock_sec += delta_sec;
    });
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
                    instance_id: 710_001,
                    dungeon_id: 10346,
                    dungeon_name: "Abyssal Artifact Recovery".into(),
                    solar_system_id: 30_000_142,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 17_230_000,
                    completed: false,
                },
                HomefrontSiteRecord {
                    instance_id: 710_002,
                    dungeon_id: 10347,
                    dungeon_name: "Raid: Hall of Sacrifice".into(),
                    solar_system_id: 30_000_142,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 15_500_000,
                    completed: false,
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
                storm_id: sid,
                weather,
                center_solar_system_id: center_sys,
                strong_core_systems: core,
                weak_periphery_systems: periphery,
                step_count: 0,
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
                    site_id: format!("DRIFTER-HIVE-{did}"),
                    dungeon_id: did,
                    dungeon_name: dname.into(),
                    solar_system_id: sys_id,
                    element_type_id: elem_tid,
                    element_name: elem_name.into(),
                    current_room: 1,
                    total_rooms: 3,
                    warden_npc_ball_id: 1_898_000_000 + (did as i64),
                    cleared: false,
                },
            );
        }

        reg.triglavian_sites.insert(
            30_000_142,
            vec![
                TriglavianSiteRecord {
                    instance_id: 720_001,
                    dungeon_id: 8922,
                    dungeon_name: "Observatory Flashpoint".into(),
                    solar_system_id: 30_000_142,
                    faction_owner_id: 500_027,
                    base_isk: 35_000_000,
                    survey_db_drop: 150,
                    completed: false,
                },
                TriglavianSiteRecord {
                    instance_id: 720_002,
                    dungeon_id: 8802,
                    dungeon_name: "Major Conduit".into(),
                    solar_system_id: 30_000_142,
                    faction_owner_id: 500_026,
                    base_isk: 25_000_000,
                    survey_db_drop: 100,
                    completed: false,
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

// parity: feat:pve.abyssal-tiers, feat:pve.abyssal-weather
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

// parity: feat:pve.abyssal-weather
fn apply_abyssal_weather_dogma_modifiers(
    ctx: &ServerContext,
    ship_id: ItemId,
    weather: AbyssalWeatherKind,
    penalty_pct: f64,
    bonus_pct: f64,
) -> HashMap<AttributeId, f64> {
    let mut saved = HashMap::new();
    let pen_mult = 1.0 + (penalty_pct / 100.0);
    let range_pen_mult = (1.0 - (penalty_pct / 100.0)).max(0.2);
    let bonus_mult = 1.0 + (bonus_pct / 100.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            let sde_defaults = ctx
                .sde
                .types
                .get(&ship.type_id)
                .map(|t| t.default_attributes.clone())
                .unwrap_or_default();
            let get_base = |attr: AttributeId, fallback: f64, dyn_map: &HashMap<AttributeId, f64>| {
                dyn_map.get(&attr).copied().or_else(|| sde_defaults.get(&attr).copied()).unwrap_or(fallback)
            };

            let mod_attrs: Vec<(AttributeId, f64, f64)> = match weather {
                AbyssalWeatherKind::Electrical => vec![
                    (attr_ids::SHIELD_EM_DAMAGE_RESONANCE, 1.0, pen_mult),
                    (attr_ids::ARMOR_EM_DAMAGE_RESONANCE, 0.5, pen_mult),
                    (attr_ids::RECHARGE_RATE, 250_000.0, 0.5),
                ],
                AbyssalWeatherKind::Dark => vec![
                    (attr_ids::MAX_RANGE, 15_000.0, range_pen_mult),
                    (attr_ids::FALLOFF, 10_000.0, range_pen_mult),
                    (attr_ids::MAX_VELOCITY, 300.0, bonus_mult),
                ],
                AbyssalWeatherKind::Exotic => vec![
                    (attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, 0.6, pen_mult),
                    (attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, 0.75, pen_mult),
                    (attr_ids::SCAN_RESOLUTION, 250.0, bonus_mult),
                ],
                AbyssalWeatherKind::Firestorm => vec![
                    (attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, 0.8, pen_mult),
                    (attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, 0.65, pen_mult),
                    (attr_ids::ARMOR_HP, 1_500.0, bonus_mult),
                ],
                AbyssalWeatherKind::Gamma => vec![
                    (attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 0.5, pen_mult),
                    (attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, 0.9, pen_mult),
                    (attr_ids::SHIELD_CAPACITY, 1_500.0, bonus_mult),
                ],
            };

            for (attr_id, fallback, factor) in mod_attrs {
                let old_val = get_base(attr_id, fallback, &ship.dynamic_attributes);
                saved.insert(attr_id, old_val);
                ship.dynamic_attributes.insert(attr_id, old_val * factor);
            }
        }
    });
    saved
}

fn restore_abyssal_weather_dogma_modifiers(
    ctx: &ServerContext,
    ship_id: ItemId,
    saved: &HashMap<AttributeId, f64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            for (&attr_id, &orig_val) in saved {
                ship.dynamic_attributes.insert(attr_id, orig_val);
            }
        }
    });
}

pub async fn handle_pve_special_sites(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    ensure_special_sites_initialized(ctx);

    match (service, method) {
        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetActiveIncursionData")
        | ("incursion", "GetAllIncursions")
        | ("incursion", "GetIncursionGlobalReport")
        | ("incursionSvc", "GetActiveIncursionData")
        | ("taleMgr", "GetActiveIncursionData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list = with_pve_special_reg(ctx, |reg| {
                reg.incursions
                    .values()
                    .map(|inc| {
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
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetAllIncursionSites")
        | ("incursion", "GetIncursionDungeons")
        | ("incursion", "GetIncursionState")
        | ("incursion", "GetSystemsInIncursions")
        | ("incursion", "JoinIncursionChat") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let const_id = args.first().and_then(|a| a.as_i64()).map(|v| v as ConstellationId).unwrap_or(20_000_020);
            let res = with_pve_special_reg(ctx, |reg| {
                let Some(inc) = reg.incursions.get(&const_id) else {
                    return Err(BridgeRpcError {
                        code: "IncursionNotFound".into(),
                        message: format!("No active incursion in constellation {const_id}"),
                    });
                };
                let sites_py: Vec<PyRep> = inc
                    .sites
                    .iter()
                    .map(|s| {
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
                            ("available", PyRep::Bool(available)),
                        ])
                    })
                    .collect();
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("INC-VG-5165").to_string();
            let fleet_pilots: Vec<CharacterId> = if let Some(PyRep::List(list)) = args.get(1) {
                let parsed: Vec<CharacterId> = list.iter().filter_map(|p| p.as_i64()).collect();
                if parsed.is_empty() { vec![char_id] } else { parsed }
            } else {
                vec![char_id]
            };

            let step_res = with_pve_special_reg(ctx, |reg| {
                let Some(inc) = reg.incursions.get_mut(&20_000_020) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveIncursion".into(),
                        message: "No active incursion in constellation 20000020".into(),
                    });
                };
                if inc.state == "Ended" {
                    return Err(BridgeRpcError {
                        code: "IncursionAlreadyEnded".into(),
                        message: "This incursion has already ended".into(),
                    });
                }
                let Some(site) = inc.sites.iter_mut().find(|s| s.site_id == site_id) else {
                    return Err(BridgeRpcError {
                        code: "IncursionSiteNotFound".into(),
                        message: format!("Incursion site {site_id} not found"),
                    });
                };
                if site.locked_until_low_influence && !inc.mothership_spawned {
                    return Err(BridgeRpcError {
                        code: "MothershipSiteLocked".into(),
                        message: format!(
                            "Mothership HQ site {} is locked while Sansha influence ({:.2}) > 0.25",
                            site.dungeon_name, inc.influence
                        ),
                    });
                }

                site.completed_count += 1;
                let (b_isk, b_lp, opt, s_tier, delta) = (
                    site.base_isk,
                    site.base_concord_lp,
                    site.max_optimal_pilots,
                    site.tier.clone(),
                    site.influence_delta,
                );
                if s_tier == "Mothership" {
                    inc.influence = 0.0;
                    inc.mothership_defeated = true;
                    inc.state = "Ended".into();
                } else {
                    inc.influence = (inc.influence - delta).max(0.0);
                    if inc.influence <= 0.25 {
                        inc.mothership_spawned = true;
                        inc.state = "Mobilizing".into();
                    }
                }
                Ok((b_isk, b_lp, opt, s_tier, inc.influence, inc.state.clone(), inc.mothership_spawned, inc.mothership_defeated))
            });
            let (base_isk, base_lp, max_opt, tier, new_influence, new_state, mothership_unlocked, incursion_ended) =
                match step_res {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

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
                            entry_id: j_id,
                            character_id: pid,
                            entry_type: "CorporateRewardPayout".into(),
                            amount: IskCents::from_isk(pilot_isk),
                            balance_after: new_bal,
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

        // parity: feat:pve.abyssal-pocket, feat:pve.abyssal-weather, feat:pve.abyssal-tiers
        ("abyssal_content_manager", "ActivateAbyssalKey")
        | ("abyssal_content_manager", "AbyssalEntranceDeployment")
        | ("abyssal_content_manager", "AbyssalEntranceGateActivation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate Abyssal filament while docked".into(),
                }));
            }
            let origin_sys = session.solar_system_id.unwrap_or(30_000_142);
            let ship_id = session.ship_id.unwrap_or(100_000_002);
            let filament_arg = args.first().and_then(|a| a.as_i64()).unwrap_or(47_765);

            let resolved_filament = ctx.db.try_with_mut_snapshot(|snap| {
                let found_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && (i.item_id == filament_arg || i.type_id as i64 == filament_arg) && i.quantity >= 1)
                    .map(|i| (i.item_id, i.type_id));
                let Some((iid, tid)) = found_item_id else {
                    return Err(BridgeRpcError {
                        code: "FilamentNotFound".into(),
                        message: format!("Abyssal filament {filament_arg} not found in character inventory"),
                    });
                };
                let Some((tier, weather)) = classify_abyssal_filament(tid) else {
                    return Err(BridgeRpcError {
                        code: "InvalidAbyssalFilament".into(),
                        message: format!("Type {tid} is not a valid T0-T6 Abyssal filament"),
                    });
                };
                let origin_sec = ctx
                    .sde
                    .solar_systems
                    .get(&origin_sys)
                    .map(|s| s.security_status)
                    .unwrap_or(0.9);
                // High-sec Abyssal filament tier restrictions (`feat:crimewatch.abyssal-suspect-flag-tier`):
                // Activating high-tier (T4-T6) filaments in 0.8-1.0 is blocked per V24 rules.
                if tier >= 4 && origin_sec >= 0.75 {
                    return Err(BridgeRpcError {
                        code: "FilamentDisallowedInHighSec".into(),
                        message: "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems".into(),
                    });
                }
                if let Some(item) = snap.items.get_mut(&iid) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&iid);
                    }
                }
                Ok((tid, tier, weather))
            });
            let (filament_type_id, tier, weather) = match resolved_filament {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let penalty_pct = match tier { 0 | 1 => 30.0, 2 | 3 => 50.0, _ => 70.0 };
            let bonus_pct = 50.0;
            let diff_mult = 1.0 + (tier as f64) * 1.25;
            let saved_attrs = apply_abyssal_weather_dogma_modifiers(ctx, ship_id, weather, penalty_pct, bonus_pct);

            let abyssal_sys_id: SolarSystemId = 32_000_001;
            let sys_handle = ctx.systems.get_or_spawn(abyssal_sys_id);

            let (pocket_id, started_ts, collapse_ts, room1_npcs) = with_pve_special_reg(ctx, |reg| {
                let pid = 600_000 + next_spec_seq(reg);
                let now = spec_now_sec(reg);
                let collapse = now + 1_200;
                let mut rooms_vec = Vec::with_capacity(3);
                for (r_idx, did, r_name) in [(1_u8, 5810_u32, "Canyon"), (2, 6047, "Rise"), (3, 6048, "Forest Opening")] {
                    let npc_count = (2 + (tier as usize / 2)).clamp(2, 5);
                    let npc_ids: Vec<i64> = (0..npc_count).map(|n| pid * 100 + (r_idx as i64) * 10 + (n as i64) + 1).collect();
                    rooms_vec.push(AbyssalRoomRecord {
                        room_index: r_idx,
                        dungeon_id: did,
                        dungeon_name: r_name.into(),
                        npc_ball_ids: npc_ids,
                        cache_item_id: 1_896_000_000 + pid * 10 + (r_idx as i64),
                        cleared: false,
                    });
                }
                let rooms: [AbyssalRoomRecord; 3] = rooms_vec.try_into().unwrap();
                let r1_npcs = rooms[0].npc_ball_ids.clone();
                reg.abyssal_pockets.insert(
                    char_id,
                    AbyssalPocketSession {
                        pocket_id: pid,
                        character_id: char_id,
                        ship_id,
                        origin_solar_system_id: origin_sys,
                        abyssal_solar_system_id: abyssal_sys_id,
                        filament_type_id,
                        tier,
                        weather,
                        weather_penalty_pct: penalty_pct,
                        weather_bonus_pct: bonus_pct,
                        npc_difficulty_mult: diff_mult,
                        current_room: 1,
                        rooms,
                        started_at_sec: now,
                        collapse_at_sec: collapse,
                        saved_ship_attrs: saved_attrs,
                        completed: false,
                        collapsed_destroyed: false,
                    },
                );
                (pid, now, collapse, r1_npcs)
            });

            for (idx, &bid) in room1_npcs.iter().enumerate() {
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: bid,
                        type_id: 47_956,
                        position: DVec3::new((idx as f64) * 3_000.0, 5_000.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: 450.0,
                        inertia: 1.0,
                        mass: 1_500_000.0,
                        radius: 140.0,
                        speed_fraction: 0.8,
                        mode: MovementMode::Stop,
                    })
                    .await;
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = abyssal_sys_id;
                }
            });
            session.solar_system_id = Some(abyssal_sys_id);

            Some(Ok(PyRep::keyval([
                ("pocketID", PyRep::Integer(pocket_id)),
                ("solarSystemID", PyRep::Integer(abyssal_sys_id as i64)),
                ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
                ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
                ("tier", PyRep::Integer(tier as i64)),
                ("suspectFlagOnExit", PyRep::Bool(tier >= 4)),
                ("weather", PyRep::String(weather.as_str().into())),
                ("weatherPenaltyPct", PyRep::Real(penalty_pct)),
                ("weatherBonusPct", PyRep::Real(bonus_pct)),
                ("npcDifficultyMultiplier", PyRep::Real(diff_mult)),
                ("currentRoom", PyRep::Integer(1)),
                ("totalRooms", PyRep::Integer(3)),
                ("startedAt", PyRep::Integer(started_ts as i64)),
                ("collapseAt", PyRep::Integer(collapse_ts as i64)),
                ("timerSeconds", PyRep::Integer(1_200)),
            ])))
        }

        // parity: feat:pve.abyssal-gates, feat:pve.abyssal-pocket
        ("abyssal_content_manager", "AbyssalGateActivation")
        | ("abyssal_content_manager", "ActivateAbyssalAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let clear_current_room = args.first().and_then(|a| a.as_bool()).unwrap_or(true);
            let res = with_pve_special_reg(ctx, |reg| {
                let now = spec_now_sec(reg);
                let Some(pocket) = reg.abyssal_pockets.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInAbyssalPocket".into(),
                        message: "Character has no active Abyssal pocket".into(),
                    });
                };
                if now >= pocket.collapse_at_sec {
                    pocket.collapsed_destroyed = true;
                    return Err(BridgeRpcError {
                        code: "AbyssalPocketCollapsed".into(),
                        message: "Abyssal pocket 20-minute timer expired; ship and capsule destroyed".into(),
                    });
                }
                let r_idx = (pocket.current_room.saturating_sub(1) as usize).min(2);
                if clear_current_room {
                    pocket.rooms[r_idx].cleared = true;
                }
                if !pocket.rooms[r_idx].cleared {
                    return Err(BridgeRpcError {
                        code: "AbyssalGateLocked".into(),
                        message: "Cannot activate Triglavian Transfer Conduit while room hostiles remain".into(),
                    });
                }
                if pocket.current_room >= 3 {
                    return Err(BridgeRpcError {
                        code: "AlreadyInFinalAbyssalRoom".into(),
                        message: "Already in Room 3; use AbyssalEndGateActivation to exit".into(),
                    });
                }
                pocket.current_room += 1;
                let next_room_num = pocket.current_room;
                let next_room_name = pocket.rooms[(next_room_num - 1) as usize].dungeon_name.clone();
                let next_room_did = pocket.rooms[(next_room_num - 1) as usize].dungeon_id;
                Ok((next_room_num, next_room_did, next_room_name))
            });
            let (next_room_num, next_room_did, next_room_name) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            session.push_notification(
                "AbyssalRoomChanged",
                vec![PyRep::Integer(next_room_num as i64), PyRep::Integer(next_room_did as i64)],
            );

            Some(Ok(PyRep::keyval([
                ("currentRoom", PyRep::Integer(next_room_num as i64)),
                ("dungeonID", PyRep::Integer(next_room_did as i64)),
                ("dungeonName", PyRep::String(next_room_name)),
            ])))
        }

        // parity: feat:pve.abyssal-gates, feat:pve.abyssal-pocket, feat:pve.abyssal-tiers
        ("abyssal_content_manager", "AbyssalEndGateActivation")
        | ("abyssal_content_manager", "ActivateAbyssalEndGate")
        | ("abyssal_content_manager", "AbyssalContentFinished") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let exit_res = with_pve_special_reg(ctx, |reg| {
                let now = spec_now_sec(reg);
                let iid = 1_897_000_000 + next_spec_seq(reg);
                let Some(pocket) = reg.abyssal_pockets.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInAbyssalPocket".into(),
                        message: "Character has no active Abyssal pocket".into(),
                    });
                };
                if now >= pocket.collapse_at_sec {
                    pocket.collapsed_destroyed = true;
                    return Err(BridgeRpcError {
                        code: "AbyssalPocketCollapsed".into(),
                        message: "Abyssal pocket 20-minute timer expired; ship and capsule destroyed".into(),
                    });
                }
                if pocket.current_room < 3 {
                    return Err(BridgeRpcError {
                        code: "NotInFinalAbyssalRoom".into(),
                        message: format!("Origin Conduit only exists in Room 3 (currently in Room {})", pocket.current_room),
                    });
                }
                pocket.rooms[2].cleared = true;
                pocket.completed = true;
                let qty = ((pocket.tier as i64) + 1) * 15;
                Ok((pocket.origin_solar_system_id, pocket.ship_id, pocket.tier, pocket.saved_ship_attrs.clone(), iid, qty))
            });
            let (origin_sys, ship_id, tier, saved_attrs, loot_item_id, survey_qty) = match exit_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            restore_abyssal_weather_dogma_modifiers(ctx, ship_id, &saved_attrs);

            // Exiting high-tier (T4-T6) Abyssal in highsec (0.5-0.7) triggers a 15-minute Suspect flag (`feat:crimewatch.abyssal-suspect-flag-tier`):
            let ret_sec = ctx.sde.solar_systems.get(&origin_sys).map(|s| s.security_status).unwrap_or(0.5);
            if tier >= 4 && ret_sec >= 0.45 {
                ctx.crimewatch.write().unwrap().trigger_suspect_flag(char_id, ctx.now_unix());
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = origin_sys;
                }
                snap.items.insert(
                    loot_item_id,
                    InventoryItemRecord {
                        item_id: loot_item_id,
                        type_id: 48_121,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::Cargo,
                        quantity: survey_qty as i32,
                        singleton: false,
                        ..Default::default()
                    },
                );
            });
            session.solar_system_id = Some(origin_sys);

            Some(Ok(PyRep::keyval([
                ("completed", PyRep::Bool(true)),
                ("returnedSolarSystemID", PyRep::Integer(origin_sys as i64)),
                ("tier", PyRep::Integer(tier as i64)),
                ("rewardLootItemID", PyRep::Integer(loot_item_id)),
                ("rewardTypeID", PyRep::Integer(48_121)),
                ("rewardQuantity", PyRep::Integer(survey_qty)),
            ])))
        }

        // parity: feat:pve.abyssal-pocket
        ("abyssal_content_manager", "AbyssalContentExpiration")
        | ("abyssal_content_manager", "GetAbyssalLocationInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let exp_res = with_pve_special_reg(ctx, |reg| {
                let now = spec_now_sec(reg);
                let Some(pocket) = reg.abyssal_pockets.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInAbyssalPocket".into(),
                        message: "Character has no active Abyssal pocket".into(),
                    });
                };
                if method == "AbyssalContentExpiration" || now >= pocket.collapse_at_sec {
                    pocket.collapsed_destroyed = true;
                }
                let rem = pocket.collapse_at_sec.saturating_sub(now);
                Ok((pocket.collapsed_destroyed, pocket.ship_id, pocket.origin_solar_system_id, rem, pocket.current_room, pocket.weather.as_str(), pocket.tier))
            });
            let (collapsed, ship_id, origin_sys, rem_sec, current_room, weather_str, tier) = match exp_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if collapsed {
                let sys_handle = ctx.systems.get_or_spawn(32_000_001);
                let _ = sys_handle.remove_ball(ship_id).await;
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.solar_system_id = origin_sys;
                    }
                });
                session.solar_system_id = Some(origin_sys);
            }

            Some(Ok(PyRep::keyval([
                ("currentRoom", PyRep::Integer(current_room as i64)),
                ("tier", PyRep::Integer(tier as i64)),
                ("weather", PyRep::String(weather_str.into())),
                ("remainingSeconds", PyRep::Integer(rem_sec as i64)),
                ("shipDestroyed", PyRep::Bool(collapsed)),
                ("podDestroyed", PyRep::Bool(collapsed)),
            ])))
        }

        // parity: feat:pve.homefront-ops
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);
            let complete_instance_id = args.get(1).and_then(|a| a.as_i64());
            let fleet_count = args.get(2).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 10) as usize;

            let mut reward_paid: i64 = 0;
            let sites_py = with_pve_special_reg(ctx, |reg| {
                let list = reg.homefront_sites.entry(sys_id).or_default();
                if let Some(target_id) = complete_instance_id {
                    if let Some(site) = list.iter_mut().find(|s| s.instance_id == target_id) {
                        site.completed = true;
                        let mult = if fleet_count == site.optimal_fleet_size {
                            1.0
                        } else if fleet_count < site.optimal_fleet_size {
                            (fleet_count as f64) / (site.optimal_fleet_size as f64)
                        } else {
                            0.75
                        };
                        reward_paid = ((site.base_pilot_isk as f64) * mult).round() as i64;
                    }
                }
                list.iter()
                    .map(|s| {
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(s.instance_id)),
                            ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                            ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                            ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                            ("optimalFleetSize", PyRep::Integer(s.optimal_fleet_size as i64)),
                            ("basePilotIsk", PyRep::Integer(s.base_pilot_isk)),
                            ("completed", PyRep::Bool(s.completed)),
                            ("rewardPaidIsk", PyRep::Integer(reward_paid)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });

            if reward_paid > 0 {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(reward_paid)).unwrap_or(ch.balance);
                    }
                });
            }
            Some(Ok(PyRep::List(sites_py)))
        }

        // parity: feat:pve.triglavian-sites, feat:pve.drifter-hives
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances")
        | ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);
            let complete_id = args.get(1).and_then(|a| a.as_i64());

            if (31_000_001..=31_000_005).contains(&sys_id) {
                let hive_res = with_pve_special_reg(ctx, |reg| {
                    let next_iid = 1_899_000_000 + next_spec_seq(reg);
                    let Some(hive) = reg.drifter_hives.get_mut(&sys_id) else {
                        return Err(BridgeRpcError {
                            code: "DrifterHiveNotFound".into(),
                            message: format!("No Drifter Hive in system {sys_id}"),
                        });
                    };
                    if complete_id.is_some() {
                        hive.cleared = true;
                        hive.current_room = hive.total_rooms;
                    }
                    let dropped = if hive.cleared && complete_id.is_some() { Some(next_iid) } else { None };
                    let kv = PyRep::keyval([
                        ("siteID", PyRep::String(hive.site_id.clone())),
                        ("dungeonID", PyRep::Integer(hive.dungeon_id as i64)),
                        ("dungeonName", PyRep::String(hive.dungeon_name.clone())),
                        ("solarSystemID", PyRep::Integer(hive.solar_system_id as i64)),
                        ("elementTypeID", PyRep::Integer(hive.element_type_id as i64)),
                        ("elementName", PyRep::String(hive.element_name.clone())),
                        ("wardenNpcBallID", PyRep::Integer(hive.warden_npc_ball_id)),
                        ("cleared", PyRep::Bool(hive.cleared)),
                    ]);
                    Ok((kv, dropped, hive.element_type_id))
                });
                let (kv, dropped_item_id, elem_tid) = match hive_res {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if let Some(iid) = dropped_item_id {
                    let ship_id = session.ship_id.unwrap_or(char_id);
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.items.insert(
                            iid,
                            InventoryItemRecord {
                                item_id: iid,
                                type_id: elem_tid,
                                owner_id: char_id,
                                location_id: ship_id,
                                flag: InventoryFlag::Cargo,
                                quantity: 1,
                                singleton: false,
                                ..Default::default()
                            },
                        );
                    });
                }
                return Some(Ok(kv));
            }

            let mut completed_faction_owner: Option<(u32, i64, i64, ItemId)> = None;
            let trig_py = with_pve_special_reg(ctx, |reg| {
                let seq_id = 1_899_500_000 + next_spec_seq(reg);
                let list = reg.triglavian_sites.entry(sys_id).or_default();
                if let Some(cid) = complete_id {
                    if let Some(site) = list.iter_mut().find(|s| s.instance_id == cid) {
                        site.completed = true;
                        completed_faction_owner = Some((site.faction_owner_id, site.base_isk, site.survey_db_drop, seq_id));
                    }
                }
                list.iter()
                    .map(|s| {
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(s.instance_id)),
                            ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                            ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                            ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                            ("factionOwnerID", PyRep::Integer(s.faction_owner_id as i64)),
                            ("baseIsk", PyRep::Integer(s.base_isk)),
                            ("surveyDatabaseDrop", PyRep::Integer(s.survey_db_drop)),
                            ("completed", PyRep::Bool(s.completed)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });

            if let Some((owner_faction, isk, survey_qty, loot_iid)) = completed_faction_owner {
                let (gain_faction, lose_faction) = if owner_faction == 500_027 {
                    (500_026_i64, 500_027_i64)
                } else {
                    (500_027_i64, 500_026_i64)
                };
                let ship_id = session.ship_id.unwrap_or(char_id);
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let g = ch.standings.entry(gain_faction).or_insert(0.0);
                        *g = (*g + 0.15).clamp(-10.0, 10.0);
                        let l = ch.standings.entry(lose_faction).or_insert(0.0);
                        *l = (*l - 0.15).clamp(-10.0, 10.0);
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(isk)).unwrap_or(ch.balance);
                    }
                    snap.items.insert(
                        loot_iid,
                        InventoryItemRecord {
                            item_id: loot_iid,
                            type_id: 48_121,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag: InventoryFlag::Cargo,
                            quantity: survey_qty as i32,
                            singleton: false,
                            ..Default::default()
                        },
                    );
                });
            }

            Some(Ok(PyRep::List(trig_py)))
        }

        // parity: feat:pve.metaliminal-storms
        ("map", "GetAllRoamingWeatherSystems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let advance_storm_step = args.first().and_then(|a| a.as_bool()).unwrap_or(false);
            let out = with_pve_special_reg(ctx, |reg| {
                if advance_storm_step {
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
                reg.roaming_storms
                    .iter()
                    .map(|s| {
                        PyRep::keyval([
                            ("stormID", PyRep::Integer(s.storm_id as i64)),
                            ("weather", PyRep::String(s.weather.as_str().into())),
                            ("centerSolarSystemID", PyRep::Integer(s.center_solar_system_id as i64)),
                            ("strongCoreSystems", PyRep::List(s.strong_core_systems.iter().map(|&id| PyRep::Integer(id as i64)).collect())),
                            ("weakPeripherySystems", PyRep::List(s.weak_periphery_systems.iter().map(|&id| PyRep::Integer(id as i64)).collect())),
                            ("stepCount", PyRep::Integer(s.step_count as i64)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(out)))
        }

        // parity: feat:pve.interference
        ("solarsystemInterferenceMgr", "GetLocalInterferenceState")
        | ("solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand")
        | ("solarsystemInterferenceMgr", "GetAllInterferenceBands") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).or(session.solar_system_id).unwrap_or(30_000_142);
            let activity_delta = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0).clamp(0.0, 1.0);

            let (level, old_band, new_band) = with_pve_special_reg(ctx, |reg| {
                let entry = reg.interference_by_system.entry(sys_id).or_insert(SystemInterferenceRecord {
                    solar_system_id: sys_id,
                    interference_level: 0.10,
                    band: 1,
                });
                let prev_band = entry.band;
                if activity_delta > 0.0 {
                    entry.interference_level = (entry.interference_level + activity_delta).clamp(0.0, 1.0);
                    entry.band = if entry.interference_level >= 0.70 {
                        3
                    } else if entry.interference_level >= 0.35 {
                        2
                    } else {
                        1
                    };
                }
                (entry.interference_level, prev_band, entry.band)
            });

            if new_band != old_band {
                session.push_notification(
                    "OnSolarsystemInterferenceChanged",
                    vec![PyRep::Integer(sys_id as i64), PyRep::Real(level), PyRep::Integer(new_band as i64)],
                );
            }

            let band_label = match new_band { 1 => "Low", 2 => "Medium", _ => "High" };
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("interferenceLevel", PyRep::Real(level)),
                ("interferenceBand", PyRep::Integer(new_band as i64)),
                ("bandLabel", PyRep::String(band_label.into())),
            ])))
        }

        _ => None,
    }
}
