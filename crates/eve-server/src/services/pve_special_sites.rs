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
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

type CharacterId = i64;
type ItemId = i64;
pub use crate::services::pve_incursion_sites::*;
pub use crate::services::pve_homefront_sites::*;
pub use crate::services::pve_metaliminal_storms::*;
pub use crate::services::pve_special_sites_abyssal::*;
pub use crate::services::pve_crab_beacons::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomefrontSiteRecord {
    pub instance_id: i64, pub dungeon_id: u32, pub dungeon_name: String, pub solar_system_id: SolarSystemId,
    pub optimal_fleet_size: usize, pub base_pilot_isk: i64, pub completed: bool,
    pub reward_paid_isk: i64, pub boss_npc_ball_id: i64,
    #[serde(default)]
    pub boss_type_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrifterHiveRoom {
    pub room_index: u8,
    pub room_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub npc_type_ids: Vec<u32>,
    pub beacon_ball_id: i64,
    pub cleared: bool,
    #[serde(default)]
    pub destroyed_npc_ball_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub rooms: Vec<DrifterHiveRoom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub boss_type_id: u32,
    #[serde(default)]
    pub escort_ball_ids: Vec<i64>,
    #[serde(default)]
    pub escort_type_ids: Vec<u32>,
    #[serde(default)]
    pub beacon_ball_id: i64,
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
    pub incursion_lifecycle: HashMap<ConstellationId, crate::services::pve_incursion_lifecycle::IncursionLifecycleMeta>,
    #[serde(default)]
    pub incursion_site_waves: HashMap<String, crate::services::pve_incursion_lifecycle::IncursionSiteWaveMeta>,
    #[serde(default)]
    pub world_event_tales: HashMap<i64, crate::services::pve_incursion_lifecycle::WorldEventTaleRecord>,
    #[serde(default)]
    pub incursion_lp_logs: HashMap<CharacterId, Vec<IncursionLPLogEntry>>,
    #[serde(default)]
    pub abyssal_pockets: HashMap<CharacterId, AbyssalPocketSession>,
    #[serde(default)]
    pub abyssal_traces: HashMap<i64, AbyssalTraceRecord>,
    #[serde(default)]
    pub void_space_sessions: HashMap<CharacterId, VoidSpaceSessionRecord>,
    #[serde(default)]
    pub homefront_sites: HashMap<SolarSystemId, Vec<HomefrontSiteRecord>>,
    #[serde(default)]
    pub roaming_storms: Vec<RoamingStormRecord>,
    #[serde(default)]
    pub drifter_hives: HashMap<SolarSystemId, DrifterHiveRecord>,
    #[serde(default)]
    pub triglavian_sites: HashMap<SolarSystemId, Vec<TriglavianSiteRecord>>,
    #[serde(default)]
    pub edencom_fortress_systems: Vec<SolarSystemId>,
    #[serde(default)]
    pub edencom_minor_victory_systems: Vec<SolarSystemId>,
    #[serde(default)]
    pub triglavian_minor_victory_systems: Vec<SolarSystemId>,
    #[serde(default)]
    pub interference_by_system: HashMap<SolarSystemId, SystemInterferenceRecord>,
    #[serde(default)]
    pub crab_beacons: HashMap<i64, crate::services::pve_crab_beacons::CrabBeaconRecord>,
    #[serde(default)]
    pub sim_clock_sec: u64,
    #[serde(default)]
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
                        let mut infested = vec![inc.staging_solar_system_id];
                        for &s in &inc.vanguard_solar_system_ids {
                            if !infested.contains(&s) {
                                infested.push(s);
                            }
                        }
                        for &s in &inc.assault_solar_system_ids {
                            if !infested.contains(&s) {
                                infested.push(s);
                            }
                        }
                        if !infested.contains(&inc.hq_solar_system_id) {
                            infested.push(inc.hq_solar_system_id);
                        }
                        snap.incursions.insert(
                            cid,
                            eve_db::IncursionRecord {
                                constellation_id: cid,
                                faction_id: 500_019,
                                has_boss: inc.mothership_spawned && !inc.mothership_defeated,
                                infested_solar_systems: infested,
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

pub(crate) fn next_spec_seq(reg: &mut PveSpecialSitesRegistry) -> i64 {
    reg.next_seq += 1;
    reg.next_seq
}

pub(crate) fn spec_now_sec(reg: &PveSpecialSitesRegistry) -> u64 {
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
            advance_roaming_storms(ctx, reg, delta_sec);
        }
    });
    crate::services::pve_crab_beacons::tick_all_crab_beacons(ctx);
    if delta_sec > 0 {
        crate::services::pve_incursion_lifecycle::advance_incursion_lifecycles(ctx);
    }
    sync_metaliminal_storm_effects_and_sites(ctx);
}

fn empire_navy_ships(empire_fac: u32) -> (u32, u32, u32) {
    match empire_fac {
        500_001 => (52_940, 52_938, 52_934), // Caldari: State Navy Raven, Drake, Raptor
        500_002 => (52_963, 52_961, 52_957), // Minmatar: Republic Fleet Tempest, Hurricane, Claw
        500_003 => (52_931, 52_929, 52_925), // Amarr: Imperial Navy Apocalypse, Harbinger, Crusader
        _ => (52_954, 52_952, 52_948),       // Gallente/default: Federal Navy Megathron, Brutix, Taranis
    }
}

pub fn edencom_fortress_systems() -> Vec<SolarSystemId> {
    vec![
        30000004, 30000005, 30000105, 30000113, 30000118, 30000188, 30002242, 30002243, 30002251, 30002253, 30002266,
        30002385, 30002386, 30002530, 30002651, 30002662, 30002665, 30002700, 30002704, 30002986, 30003050, 30003392,
        30003397, 30003398, 30003490, 30003514, 30003515, 30003539, 30003541, 30003548, 30003553, 30003556, 30003573,
        30003574, 30003883, 30003885, 30004084, 30004090, 30004100, 30004103, 30004141, 30004150, 30004248, 30004250,
        30004305, 30004973, 30004992, 30005052, 30005058, 30005251, 30005252, 30005260, 30045322,
    ]
}

pub fn edencom_minor_victory_systems() -> Vec<SolarSystemId> {
    vec![
        30000012, 30000048, 30000060, 30000062, 30000102, 30000109, 30000160, 30001376, 30001660, 30001696, 30001718,
        30002048, 30002051, 30002239, 30002241, 30002397, 30002506, 30002513, 30002644, 30002724, 30002755, 30002772,
        30002999, 30003058, 30003061, 30003074, 30003078, 30003088, 30003090, 30003460, 30003463, 30003478, 30003480,
        30003481, 30003482, 30003558, 30003570, 30003587, 30003788, 30003794, 30003809, 30003823, 30003824, 30003829,
        30003854, 30003894, 30003900, 30003904, 30003908, 30003918, 30003919, 30003927, 30003931, 30003932, 30004108,
        30004231, 30004254, 30004256, 30004257, 30004263, 30004268, 30004284, 30004287, 30004289, 30004295, 30004301,
        30004302, 30004978, 30004999, 30005034, 30005066, 30005074, 30005086, 30005209, 30005213, 30005219, 30005222,
        30005236, 30005255, 30005263, 30005267, 30005284, 30005308, 30005334,
    ]
}

pub fn triglavian_minor_victory_systems() -> Vec<SolarSystemId> {
    vec![
        30000163, 30000182, 30000205, 30001358, 30001383, 30001390, 30001391, 30001400, 30001401, 30001447, 30001685,
        30002557, 30002575, 30002645, 30002760, 30002771, 30002795, 30003073, 30003076, 30003464, 30003856, 30004244,
        30004981, 30005330, 30045331, 30045338, 30045345, 30045354,
    ]
}

pub fn ensure_triglavian_sites_for_system(reg: &mut PveSpecialSitesRegistry, sys_id: SolarSystemId, empire_fac: u32) {
    if reg.triglavian_sites.contains_key(&sys_id) {
        return;
    }
    if reg.edencom_fortress_systems.is_empty() { reg.edencom_fortress_systems = edencom_fortress_systems(); }
    if reg.edencom_minor_victory_systems.is_empty() { reg.edencom_minor_victory_systems = edencom_minor_victory_systems(); }
    if reg.triglavian_minor_victory_systems.is_empty() { reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems(); }
    if reg.edencom_fortress_systems.contains(&sys_id) {
        return;
    }
    let is_edencom = reg.edencom_minor_victory_systems.contains(&sys_id);
    let base_inst = (sys_id as i64) * 100;
    let make_site = |inst, did: u32, name: &str, fac, isk, drop, boss_tid, esc_tids: Vec<u32>| {
        let esc_bids = (1..=esc_tids.len()).map(|i| 1_897_700_000 + inst * 10 + (i as i64)).collect();
        TriglavianSiteRecord {
            instance_id: inst, dungeon_id: did, dungeon_name: name.into(), solar_system_id: sys_id,
            faction_owner_id: fac, base_isk: isk, survey_db_drop: drop, completed: false,
            boss_npc_ball_id: 1_897_700_000 + inst, boss_type_id: boss_tid,
            escort_ball_ids: esc_bids, escort_type_ids: esc_tids,
            beacon_ball_id: 1_897_700_000 + inst * 10 + 9,
        }
    };
    let sites = if is_edencom {
        let (bs, bc, fr) = empire_navy_ships(empire_fac);
        vec![
            make_site(base_inst + 1, 8808, "EDENCOM Field Base", 500_027, 35_000_000, 150, bs, vec![bc, fr]),
            make_site(base_inst + 2, 8792, "EDENCOM Forward Post", 500_027, 25_000_000, 100, bc, vec![fr]),
        ]
    } else {
        vec![
            make_site(base_inst + 1, 8802, "Major Conduit", 500_026, 35_000_000, 150, 48_122, vec![48_086, 48_092]),
            make_site(base_inst + 2, 8742, "Minor Conduit", 500_026, 25_000_000, 100, 48_126, vec![48_088]),
        ]
    };
    reg.triglavian_sites.insert(sys_id, sites);
}

pub fn ensure_special_sites_initialized(ctx: &ServerContext) {
    let started_tales: Vec<(i64, SolarSystemId)> = with_pve_special_reg(ctx, |reg| {
        if reg.initialized {
            return Vec::new();
        }
        reg.initialized = true;

        let eligible_constellations =
            crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(ctx);
        for &(cid, _) in eligible_constellations.iter().take(3) {
            seed_incursion_for_constellation(ctx, reg, cid);
        }

        if reg.edencom_fortress_systems.is_empty() {
            reg.edencom_fortress_systems = edencom_fortress_systems();
        }
        if reg.edencom_minor_victory_systems.is_empty() {
            reg.edencom_minor_victory_systems = edencom_minor_victory_systems();
        }
        if reg.triglavian_minor_victory_systems.is_empty() {
            reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems();
        }

        init_homefront_sites(ctx, reg);
        init_roaming_storms(ctx, reg);

        for (sys_id, did, dname, elem_tid, elem_name) in [
            (31_000_001_u32, 5648_u32, "Barbican Hive", 34_557_u32, "Barbican Element"),
            (31_000_002, 5649, "Vidette Hive", 34_558, "Vidette Element"),
            (31_000_003, 5650, "Conflux Hive", 34_559, "Conflux Element"),
            (31_000_004, 5651, "Redoubt Hive", 34_560, "Redoubt Element"),
            (31_000_005, 5642, "Sentinel Hive", 34_556, "Sentinel Element"),
        ] {
            let base_ball = 1_898_000_000 + ((did as i64) * 100);
            let warden_npc_ball_id = base_ball + 30;
            let rooms = vec![
                DrifterHiveRoom {
                    room_index: 1,
                    room_name: "Perimeter Approach".into(),
                    npc_ball_ids: vec![base_ball + 11, base_ball + 12],
                    npc_type_ids: vec![47958, 47959], // Drifter Entanglement Cruiser, Drifter Nullwarp Cruiser
                    beacon_ball_id: base_ball + 10,
                    cleared: false,
                    destroyed_npc_ball_ids: Vec::new(),
                },
                DrifterHiveRoom {
                    room_index: 2,
                    room_name: "Assembler Chamber".into(),
                    npc_ball_ids: vec![base_ball + 21, base_ball + 22],
                    npc_type_ids: vec![47956, 47957], // Drifter Vanguard Battleship, Drifter Assault Battleship
                    beacon_ball_id: base_ball + 20,
                    cleared: false,
                    destroyed_npc_ball_ids: Vec::new(),
                },
                DrifterHiveRoom {
                    room_index: 3,
                    room_name: "Hive Sanctuary".into(),
                    npc_ball_ids: vec![warden_npc_ball_id],
                    npc_type_ids: vec![37473], // Artemis Tyrannos / Drifter Response Battleship
                    beacon_ball_id: base_ball + 31,
                    cleared: false,
                    destroyed_npc_ball_ids: Vec::new(),
                },
            ];
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
                    warden_npc_ball_id,
                    cleared: false,
                    rooms,
                },
            );
        }

        for sys_id in reg
            .edencom_minor_victory_systems
            .iter()
            .chain(reg.triglavian_minor_victory_systems.iter())
            .copied()
            .collect::<Vec<_>>()
        {
            let empire_fac = ctx.sde.solar_systems.get(&sys_id).and_then(|s| {
                ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id)
                    .or_else(|| ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id))
                    .or_else(|| s.stations.iter().find_map(|st| st.owner_faction_id.map(|f| f as u32)))
            }).unwrap_or(500_002);
            ensure_triglavian_sites_for_system(reg, sys_id, empire_fac);
        }
        reg.incursions.values().filter(|inc| inc.state != "Ended").map(|inc| (inc.tale_id, inc.staging_solar_system_id)).collect()
    });
    // Each newly seeded incursion is a running world-event tale; announce it to subscribed
    // clients (published after the registry snapshot lock is released).
    for (tale_id, staging) in started_tales {
        crate::services::pve_incursion_sites::publish_incursion_tale_notice(ctx.quasar_gateway.as_ref(), true, tale_id, staging, ctx.now_unix());
    }
    crate::services::pve_incursion_lifecycle::sync_incursion_system_effect_beacons(ctx);
    sync_metaliminal_storm_effects_and_sites(ctx);
}


pub(crate) fn build_special_site_npc_combat_state(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
) -> eve_destiny::CombatState {
    if let Some(t) = sde.types.get(&type_id) {
        let a = &t.default_attributes;
        let shield = a
            .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
            .copied()
            .unwrap_or(1_200.0)
            .clamp(600.0, 10_000.0);
        let armor = a
            .get(&eve_sde::attr_ids::ARMOR_HP)
            .copied()
            .unwrap_or(1_200.0)
            .clamp(600.0, 10_000.0);
        let hull = a
            .get(&9)
            .copied()
            .unwrap_or(1_000.0)
            .clamp(600.0, 10_000.0);
        let cap = a
            .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
            .copied()
            .unwrap_or(1_000.0);
        let sig = a
            .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
            .copied()
            .unwrap_or_else(|| t.radius.max(35.0));
        return eve_destiny::CombatState {
            shield_hp: shield,
            max_shield_hp: shield,
            armor_hp: armor,
            max_armor_hp: armor,
            structure_hp: hull,
            max_structure_hp: hull,
            capacitor: cap,
            max_capacitor: cap,
            signature_radius: sig,
            shield_resonance: [
                a.get(&eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
            ],
            armor_resonance: [
                a.get(&eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
            ],
            structure_resonance: [
                a.get(&eve_sde::attr_ids::EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
                a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0).max(0.25),
            ],
            ..eve_destiny::CombatState::default()
        };
    }
    eve_destiny::CombatState {
        shield_hp: 1_200.0,
        max_shield_hp: 1_200.0,
        armor_hp: 1_200.0,
        max_armor_hp: 1_200.0,
        structure_hp: 1_000.0,
        max_structure_hp: 1_000.0,
        ..eve_destiny::CombatState::default()
    }
}

fn deposit_loot_into_ship_or_container_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
    solar_system_id: SolarSystemId,
    ship_id: i64,
    items_to_award: &[(i64, u32, i32)],
) -> Option<i64> {
    let loot_vol: f64 = items_to_award
        .iter()
        .map(|&(_, tid, qty)| {
            sde.types
                .get(&tid)
                .map(|t| t.volume.max(0.01))
                .unwrap_or(0.1)
                * f64::from(qty.max(1))
        })
        .sum();
    let ship_can_hold = if ship_id > 0 {
        if let Some(ship_item) = snap.items.get(&ship_id) {
            if let Some(st) = sde.types.get(&ship_item.type_id) {
                if st.category_id == 6 {
                    let cap = st
                        .capacity
                        .max(st.default_attributes.get(&38).copied().unwrap_or(0.0));
                    let used: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                        .map(|i| {
                            sde.types
                                .get(&i.type_id)
                                .map(|t| t.volume.max(0.0))
                                .unwrap_or(1.0)
                                * f64::from(i.quantity.max(1))
                        })
                        .sum();
                    cap <= 0.0 || used + loot_vol <= cap + 1e-4
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    let (target_loc_id, spawned_container_id) = if ship_can_hold {
        (ship_id, None)
    } else {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: 23, // Cargo Container
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (cid, Some(cid))
    };

    for &(iid, tid, qty) in items_to_award {
        snap.items.insert(
            iid,
            InventoryItemRecord {
                item_id: iid,
                type_id: tid,
                owner_id: char_id,
                location_id: target_loc_id,
                flag: InventoryFlag::Cargo,
                quantity: qty,
                singleton: false,
                ..Default::default()
            },
        );
    }
    spawned_container_id
}

pub async fn ensure_special_sites_spawned(ctx: &ServerContext) {
    ensure_special_sites_initialized(ctx);
    let (general_to_spawn, hf_to_spawn, trig_to_spawn) = with_pve_special_reg(ctx, |reg| {
        let mut g_list = Vec::new();
        for inc in reg.incursions.values() {
            if inc.state == "Ended" || inc.state == "Withdrawing" { continue; }
            for s in &inc.sites {
                if s.locked_until_low_influence && (inc.influence > 0.0 || !inc.mothership_spawned) {
                    continue;
                }
                g_list.push((s.solar_system_id, s.boss_npc_ball_id, s.boss_type_id, s.position));
                for esc in &s.escort_spawns {
                    if !esc.destroyed {
                        let esc_pos = s.position + DVec3::new(esc.offset[0], esc.offset[1], esc.offset[2]);
                        g_list.push((s.solar_system_id, esc.npc_ball_id, esc.type_id, esc_pos));
                    }
                }
            }
        }
        let mut hf_list = Vec::new();
        for sites in reg.homefront_sites.values() {
            for (idx, s) in sites.iter().enumerate() {
                if !s.completed {
                    let boss_tid = if s.boss_type_id != 0 { s.boss_type_id } else { 48_090 };
                    let offset_x = 10_000.0 + ((s.instance_id % 100) as f64) * 5_000.0 + (idx as f64) * 2_500.0;
                    let offset_y = ((s.instance_id % 10) as f64) * 1_000.0;
                    let pos = DVec3::new(offset_x, offset_y, 0.0);
                    hf_list.push((s.solar_system_id, s.boss_npc_ball_id, boss_tid, pos));
                }
            }
        }
        for hive in reg.drifter_hives.values() {
            if !hive.cleared {
                if let Some(room) = hive.rooms.iter().find(|r| r.room_index == hive.current_room && !r.cleared) {
                    for (&bid, &tid) in room.npc_ball_ids.iter().zip(&room.npc_type_ids) {
                        if room.destroyed_npc_ball_ids.contains(&bid) { continue; }
                        g_list.push((hive.solar_system_id, bid, tid, DVec3::new(10_000.0, 0.0, 0.0)));
                    }
                    if room.beacon_ball_id != 0 {
                        g_list.push((hive.solar_system_id, room.beacon_ball_id, 42_538, DVec3::new(12_000.0, 0.0, 0.0)));
                    }
                }
            }
        }
        let mut t_list = Vec::new();
        for sites in reg.triglavian_sites.values() {
            for (s_idx, s) in sites.iter().enumerate() {
                if !s.completed {
                    let boss_tid = if s.boss_type_id != 0 { s.boss_type_id } else { 48_122 };
                    let base_x = 10_000.0 + (s_idx as f64) * 20_000.0;
                    t_list.push((s.solar_system_id, s.boss_npc_ball_id, boss_tid, DVec3::new(base_x, 0.0, 0.0)));
                    if s.beacon_ball_id != 0 {
                        t_list.push((s.solar_system_id, s.beacon_ball_id, 42_538, DVec3::new(base_x - 500.0, 0.0, 0.0)));
                    }
                    for (e_idx, (&esc_bid, &esc_tid)) in s.escort_ball_ids.iter().zip(&s.escort_type_ids).enumerate() {
                        let offset_x = base_x + 5_000.0 + (e_idx as f64) * 1_000.0;
                        let offset_y = 2_000.0 + (e_idx as f64) * 1_500.0;
                        t_list.push((s.solar_system_id, esc_bid, esc_tid, DVec3::new(offset_x, offset_y, 0.0)));
                    }
                }
            }
        }
        (g_list, hf_list, t_list)
    });

    // Per-ball "already spawned" tracking on the SystemRegistry (no sentinel ball in space):
    // a ball is spawned the first time it is seen in this ballpark, and never re-spawned after
    // being destroyed. Sites seeded later in any system are picked up on the next call.
    for (sys_id, bid, tid, pos) in hf_to_spawn {
        if !ctx.systems.claim_special_site_ball(sys_id, bid) {
            continue;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none() {
            let cs = ctx.build_type_combat_state(tid);
            let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
        }
    }
    for (sys_id, bid, tid, pos) in general_to_spawn {
        if !ctx.systems.claim_special_site_ball(sys_id, bid) {
            continue;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none() {
            if tid == 42_538 {
                let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, tid, pos)).await;
            } else {
                let cs = build_special_site_npc_combat_state(&ctx.sde, tid);
                let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
            }
        }
    }
    for (sys_id, bid, tid, pos) in trig_to_spawn {
        if !ctx.systems.claim_special_site_ball(sys_id, bid) {
            continue;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none() {
            if tid == 42_538 {
                let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, tid, pos)).await;
            } else {
                let cs = ctx.build_type_combat_state(tid);
                let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
            }
        }
    }
}

pub async fn on_warp_to_homefront_site(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    instance_id: i64,
) {
    ensure_special_sites_initialized(ctx);
    let to_spawn = with_pve_special_reg(ctx, |reg| {
        if let Some(sites) = reg.homefront_sites.get(&solar_system_id) {
            if let Some((idx, s)) = sites.iter().enumerate().find(|(_, s)| s.instance_id == instance_id) {
                if !s.completed {
                    let boss_tid = if s.boss_type_id != 0 { s.boss_type_id } else { 48_090 };
                    let offset_x = 10_000.0 + ((s.instance_id % 100) as f64) * 5_000.0 + (idx as f64) * 2_500.0;
                    let offset_y = ((s.instance_id % 10) as f64) * 1_000.0;
                    let pos = DVec3::new(offset_x, offset_y, 0.0);
                    return Some((s.boss_npc_ball_id, boss_tid, pos));
                }
            }
        }
        None
    });
    if let Some((bid, tid, pos)) = to_spawn {
        if ctx.systems.claim_special_site_ball(solar_system_id, bid) {
            let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
            if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none() {
                let cs = ctx.build_type_combat_state(tid);
                let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
            }
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
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_err(
            "ShipDocked",
            "Cannot complete in-space special dungeon instance while docked",
        ));
    }
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
        if hive.current_room < hive.total_rooms {
            let current_room_idx = hive.current_room;
            let room_already_cleared = hive.rooms.iter().find(|r| r.room_index == current_room_idx).map(|r| r.cleared).unwrap_or(false);
            if !room_already_cleared {
                if let Some(room) = hive.rooms.iter().find(|r| r.room_index == current_room_idx) {
                    for &bid in &room.npc_ball_ids {
                        if !room.destroyed_npc_ball_ids.contains(&bid) && sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                            return Err(rpc_err("WaveNpcsRemaining", format!("Room {} NPC {} has not been destroyed", current_room_idx, bid)));
                        }
                    }
                }
            }
            let (kv, next_to_spawn) = with_pve_special_reg(ctx, |reg| {
                let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
                if let Some(r) = h.rooms.iter_mut().find(|r| r.room_index == current_room_idx) {
                    r.cleared = true;
                }
                if h.current_room == current_room_idx {
                    h.current_room += 1;
                }
                let next_r_idx = h.current_room;
                let next_room_info = h.rooms.iter().find(|r| r.room_index == next_r_idx).cloned();
                let kv = PyRep::keyval([
                    ("siteID", PyRep::String(h.site_id.clone())),
                    ("dungeonID", PyRep::Integer(h.dungeon_id as i64)),
                    ("dungeonName", PyRep::String(h.dungeon_name.clone())),
                    ("solarSystemID", PyRep::Integer(h.solar_system_id as i64)),
                    ("currentRoom", PyRep::Integer(h.current_room as i64)),
                    ("totalRooms", PyRep::Integer(h.total_rooms as i64)),
                    ("cleared", PyRep::Bool(false)),
                ]);
                (kv, next_room_info)
            });
            if let Some(next_room) = next_to_spawn {
                for (&bid, &tid) in next_room.npc_ball_ids.iter().zip(&next_room.npc_type_ids) {
                    ctx.systems.claim_special_site_ball(caller_sys, bid);
                    let cs = build_special_site_npc_combat_state(&ctx.sde, tid);
                    let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0)), cs).await;
                }
                if next_room.beacon_ball_id != 0 {
                    ctx.systems.claim_special_site_ball(caller_sys, next_room.beacon_ball_id);
                    let _ = sys_handle.spawn_ball(make_site_npc_ball(next_room.beacon_ball_id, 42_538, DVec3::new(12_000.0, 0.0, 0.0))).await;
                }
            }
            return Ok(kv);
        }

        if sys_handle.get_ball_snapshot(hive.warden_npc_ball_id).await.ok().flatten().is_some() {
            return Err(rpc_err("WaveNpcsRemaining", format!("Drifter Hive warden {} has not been destroyed", hive.warden_npc_ball_id)));
        }
        let (kv, iid, blue_iid, elem_tid) = with_pve_special_reg(ctx, |reg| {
            let next_iid = 1_899_000_000 + next_spec_seq(reg);
            let next_blue = 1_899_100_000 + next_spec_seq(reg);
            let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
            h.cleared = true;
            if let Some(r) = h.rooms.iter_mut().find(|r| r.room_index == h.total_rooms) {
                r.cleared = true;
            }
            let kv = PyRep::keyval([
                ("siteID", PyRep::String(h.site_id.clone())),
                ("dungeonID", PyRep::Integer(h.dungeon_id as i64)),
                ("dungeonName", PyRep::String(h.dungeon_name.clone())),
                ("solarSystemID", PyRep::Integer(h.solar_system_id as i64)),
                ("elementTypeID", PyRep::Integer(h.element_type_id as i64)),
                ("elementName", PyRep::String(h.element_name.clone())),
                ("wardenNpcBallID", PyRep::Integer(h.warden_npc_ball_id)),
                ("currentRoom", PyRep::Integer(h.total_rooms as i64)),
                ("totalRooms", PyRep::Integer(h.total_rooms as i64)),
                ("cleared", PyRep::Bool(true)),
            ]);
            (kv, next_iid, next_blue, h.element_type_id)
        });
        let ship_id = session.ship_id.unwrap_or(0);
        let spawned_cid = ctx.db.with_mut_snapshot(|snap| {
            deposit_loot_into_ship_or_container_in_snapshot(
                snap,
                &ctx.sde,
                char_id,
                caller_sys,
                ship_id,
                &[(iid, elem_tid, 1), (blue_iid, 30_745, 5)],
            )
        });
        if let Some(cid) = spawned_cid {
            let _ = sys_handle.spawn_ball(make_site_npc_ball(cid, 23, DVec3::new(10_500.0, 0.0, 0.0))).await;
        }
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
        let actual_fleet_count = ctx.db.with_snapshot(|snap| {
            let fleet_id = snap.characters.get(&char_id).and_then(|c| c.fleet_id);
            if let Some(fid) = fleet_id {
                snap.characters
                    .values()
                    .filter(|c| {
                        c.fleet_id == Some(fid)
                            && c.solar_system_id == caller_sys
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })
                    .count()
                    .max(1)
            } else {
                1
            }
        });
        let eff_fleet_count = if fleet_count == 0 {
            actual_fleet_count
        } else {
            fleet_count.min(actual_fleet_count)
        };
        let mult = if eff_fleet_count == hf.optimal_fleet_size {
            1.0
        } else if eff_fleet_count < hf.optimal_fleet_size {
            (eff_fleet_count as f64) / (hf.optimal_fleet_size as f64)
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
        let mut new_bal_isk = 0.0;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = ch.balance.checked_add(IskCents::from_isk(reward_paid)).unwrap_or(ch.balance);
                let new_bal = ch.balance;
                new_bal_isk = new_bal.to_wire_f64();
                snap.append_journal(
                    char_id,
                    "CorporateRewardPayout",
                    IskCents::from_isk(reward_paid),
                    new_bal,
                    format!("Homefront Operation Payout ({})", hf.dungeon_name),
                );
            }
        });
        let _ = ctx.notify_account_change(char_id, "cash", char_id, new_bal_isk);
        session.push_notification(
            "OnAccountChange",
            vec![
                PyRep::String("cash".into()),
                PyRep::Integer(char_id),
                PyRep::Real(new_bal_isk),
            ],
        );
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
        for &ebid in &trig.escort_ball_ids {
            if sys_handle.get_ball_snapshot(ebid).await.ok().flatten().is_some() {
                return Err(rpc_err("WaveNpcsRemaining", format!("Triglavian site escort NPC {ebid} is still alive")));
            }
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
        let ship_id = session.ship_id.unwrap_or(0);
        let spawned_cid = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let g = ch.standings.entry(gain_faction).or_insert(0.0);
                *g = (*g + 0.15).clamp(-10.0, 10.0);
                let l = ch.standings.entry(lose_faction).or_insert(0.0);
                *l = (*l - 0.15).clamp(-10.0, 10.0);
                ch.balance = ch.balance.checked_add(IskCents::from_isk(trig.base_isk)).unwrap_or(ch.balance);
            }
            deposit_loot_into_ship_or_container_in_snapshot(
                snap,
                &ctx.sde,
                char_id,
                caller_sys,
                ship_id,
                &[(loot_iid, 48_121, trig.survey_db_drop as i32)],
            )
        });
        if let Some(cid) = spawned_cid {
            let _ = sys_handle.spawn_ball(make_site_npc_ball(cid, 23, DVec3::new(10_500.0, 0.0, 0.0))).await;
        }
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
        // parity: feat:pve.homefront-ops, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") => {
            let res = handle_get_homefront_site_instances(ctx, session, args);
            ensure_special_sites_spawned(ctx).await;
            Some(res)
        }

        // parity: feat:pve.triglavian-sites, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            ensure_special_sites_spawned(ctx).await;

            let by_dng = with_pve_special_reg(ctx, |reg| {
                let mut map: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
                if let Some(list) = reg.triglavian_sites.get(&sys_id) {
                    for s in list {
                        if !s.completed {
                            map.entry(s.dungeon_id).or_default().push(s.instance_id);
                        }
                    }
                }
                map
            });
            let inner: Vec<(PyRep, PyRep)> = by_dng
                .into_iter()
                .map(|(did, mut ids)| {
                    ids.sort_unstable();
                    (
                        PyRep::Integer(did as i64),
                        PyRep::List(ids.into_iter().map(PyRep::Integer).collect()),
                    )
                })
                .collect();
            Some(Ok(PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::Dict(inner))])))
        }

        ("dungeonInstanceCacheMgr", "GetSignatureInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if (31_000_001..=31_000_005).contains(&sys_id) {
                let sig_py = with_pve_special_reg(ctx, |reg| {
                    if let Some(hive) = reg.drifter_hives.get(&sys_id) {
                        let mut by_dng: Vec<(PyRep, PyRep)> = Vec::new();
                        by_dng.push((
                            PyRep::Integer(hive.dungeon_id as i64),
                            PyRep::List(vec![PyRep::Integer(sys_id as i64)]),
                        ));
                        let mut by_sys: Vec<(PyRep, PyRep)> = Vec::new();
                        by_sys.push((PyRep::Integer(sys_id as i64), PyRep::Dict(by_dng)));
                        PyRep::Dict(by_sys)
                    } else {
                        PyRep::Dict(Vec::new())
                    }
                });
                return Some(Ok(sig_py));
            }
            None
        }

        // Permanent Triglavian Minor Victory systems won in Chapter 3
        ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            ensure_special_sites_initialized(ctx);
            let systems = with_pve_special_reg(ctx, |reg| reg.triglavian_minor_victory_systems.clone());
            Some(Ok(PyRep::List(systems.into_iter().map(|s| PyRep::Integer(s as i64)).collect())))
        }

        // parity: feat:pve.metaliminal-storms, rpc:map.GetAllRoamingWeatherSystems
        ("map", "GetAllRoamingWeatherSystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let list = get_all_roaming_weather_systems(ctx);
            Some(Ok(PyRep::List(list)))
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
                    solar_system_id: sys_id, interference_level: 0.0, band: 1, last_updated_sec: now_sec,
                });
                let mut changed = None;
                if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
                    let elapsed = now_sec - entry.last_updated_sec;
                    let decay = (elapsed as f64) / INTERFERENCE_DECAY_WINDOW_SECS;
                    entry.interference_level = (entry.interference_level - decay).max(0.0);
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
            let crab_remaining = (((1.0 + 1e-6 - level).max(0.0)) / crate::services::pve_crab_beacons::CRAB_INTERFERENCE_DELTA).floor() as i64;
            let local_state = PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("interferenceLevel", PyRep::Real(level)),
                ("interferencePct", PyRep::Real(level * 100.0)),
                ("interferenceBand", PyRep::Integer(band as i64)),
                ("bandLabel", PyRep::String(band_label.into())),
                ("crabActivationsRemaining", PyRep::Integer(crab_remaining)),
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



pub fn get_drifter_hive_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Option<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.get(&sys_id).cloned())
}

pub fn get_all_drifter_hives(
    ctx: &ServerContext,
) -> Vec<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.values().cloned().collect())
}

pub fn notify_special_site_account_changes(
    db: &eve_db::WorldRepository,
    pending_notifications: &std::sync::RwLock<std::collections::HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    changes: &[(i64, f64)],
) {
    if changes.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    for &(char_id, new_balance_isk) in changes {
        let args = vec![
            PyRep::String("cash".into()),
            PyRep::Integer(char_id),
            PyRep::Real(new_balance_isk),
        ];
        let (packet, marshaled_packet) = crate::router::ServerContext::build_macho_notification_packet(
            char_id,
            None,
            "OnAccountChange",
            "charid",
            args.clone(),
        );
        let notif = crate::router::cache_notifications::PendingMachoNotification {
            recipient_char_id: char_id,
            event: "OnAccountChange".into(),
            id_type: "charid".into(),
            args,
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        if let Ok(mut map) = pending_notifications.write() {
            map.entry(char_id).or_default().push(notif.clone());
        }
        let _ = notify_tx.send((char_id, notif));
    }
}

pub async fn on_special_site_npc_destroyed_core(
    _systems: &Arc<crate::system_actor::SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<eve_sde::SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) -> Vec<(CharacterId, f64)> {
    let now_sec = db.clock().now_unix_secs();
    let mut acct_changes = Vec::new();

    // 0. Check Homefront sites when destroyed with attacker attribution in space
    if let Some(cid) = killer_char_id {
        let fleet_pilots: Vec<CharacterId> = db.with_snapshot(|snap| {
            let fleet_id = snap.characters.get(&cid).and_then(|c| c.fleet_id);
            if let Some(fid) = fleet_id {
                let mut members: Vec<CharacterId> = snap
                    .characters
                    .values()
                    .filter(|c| {
                        c.fleet_id == Some(fid)
                            && c.solar_system_id == solar_system_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })
                    .map(|c| c.character_id)
                    .collect();
                if !members.contains(&cid) {
                    members.push(cid);
                }
                members.sort_unstable();
                members
            } else {
                vec![cid]
            }
        });
        let fleet_count = fleet_pilots.len().max(1);
        let hf_res = with_pve_special_reg_db(db, now_sec, |reg| {
            if let Some(list) = reg.homefront_sites.get_mut(&solar_system_id) {
                for site in list.iter_mut() {
                    if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                        let mult = if fleet_count == site.optimal_fleet_size {
                            1.0
                        } else if fleet_count < site.optimal_fleet_size {
                            (fleet_count as f64) / (site.optimal_fleet_size as f64)
                        } else {
                            0.75
                        };
                        let reward_paid = ((site.base_pilot_isk as f64) * mult).round() as i64;
                        site.completed = true;
                        site.reward_paid_isk = reward_paid;
                        return Some((site.dungeon_name.clone(), reward_paid));
                    }
                }
            }
            None
        });
        if let Some((dungeon_name, reward_paid)) = hf_res {
            db.with_mut_snapshot(|snap| {
                for &pid in &fleet_pilots {
                    if let Some(ch) = snap.characters.get_mut(&pid) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(reward_paid)).unwrap_or(ch.balance);
                        let new_bal = ch.balance;
                        acct_changes.push((pid, new_bal.to_wire_f64()));
                        snap.append_journal(
                            pid,
                            "CorporateRewardPayout",
                            IskCents::from_isk(reward_paid),
                            new_bal,
                            format!("Homefront Operation Payout ({dungeon_name})"),
                        );
                    }
                }
            });
            return acct_changes;
        }
    }

    // 1. Check Triglavian sites
    let mut trig_escorts_to_remove = Vec::new();
    let trig_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.triglavian_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
                    trig_escorts_to_remove = site.escort_ball_ids.clone();
                    let (g_fac, l_fac) = if site.faction_owner_id == 500_027 {
                        (500_026_i64, 500_027_i64)
                    } else {
                        (500_027_i64, 500_026_i64)
                    };
                    found = Some((site.instance_id, site.base_isk, site.survey_db_drop, g_fac, l_fac));
                    break;
                }
            }
        }
        found.map(|(inst_id, base_isk, survey_db_drop, g_fac, l_fac)| {
            reg.next_seq += 1;
            let loot_id = 1_899_500_000 + reg.next_seq;
            (inst_id, base_isk, survey_db_drop, g_fac, l_fac, loot_id)
        })
    });

    if let Some((_inst_id, base_isk, db_drop, g_fac, l_fac, loot_id)) = trig_res {
        if !trig_escorts_to_remove.is_empty() {
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            for ebid in trig_escorts_to_remove {
                sys_handle.try_remove_ball(ebid);
            }
        }
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let j_id = (snap.wallet_journal.len() as i64) + 965_000;
                let mut ship_id = 0;
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    let g = ch.standings.entry(g_fac).or_insert(0.0);
                    *g = (*g + 0.15).clamp(-10.0, 10.0);
                    let l = ch.standings.entry(l_fac).or_insert(0.0);
                    *l = (*l - 0.15).clamp(-10.0, 10.0);
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(base_isk)).unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    acct_changes.push((cid, new_bal.to_wire_f64()));
                    ship_id = ch.active_ship_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id: j_id,
                        character_id: cid,
                        entry_type: "CorporateRewardPayout".into(),
                        amount: IskCents::from_isk(base_isk),
                        balance_after: new_bal,
                        reason: "Triglavian Site Payout".into(),
                    });
                }
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &[(loot_id, 48_121, db_drop as i32)],
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle.spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0))).await;
            }
        }
        return acct_changes;
    }

    // 2. Check Drifter Hives
    let drifter_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut spawn_next = None;
        let mut hive_loot = None;
        if let Some(hive) = reg.drifter_hives.get_mut(&solar_system_id) {
            if hive.warden_npc_ball_id == npc_ball_id && !hive.cleared {
                hive.cleared = true;
                hive.current_room = hive.total_rooms;
                if let Some(r) = hive.rooms.iter_mut().find(|r| r.room_index == hive.total_rooms) {
                    r.cleared = true;
                    if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                        r.destroyed_npc_ball_ids.push(npc_ball_id);
                    }
                }
                hive_loot = Some(hive.element_type_id);
            } else if !hive.cleared {
                let cur_room_idx = hive.current_room;
                if let Some(r) = hive.rooms.iter_mut().find(|r| r.room_index == cur_room_idx && !r.cleared) {
                    if r.npc_ball_ids.contains(&npc_ball_id) {
                        if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                            r.destroyed_npc_ball_ids.push(npc_ball_id);
                        }
                        let wave_cleared = r.npc_ball_ids.iter().all(|bid| r.destroyed_npc_ball_ids.contains(bid));
                        if wave_cleared {
                            r.cleared = true;
                            hive.current_room += 1;
                            let next_room_idx = hive.current_room;
                            if let Some(next_r) = hive.rooms.iter().find(|nr| nr.room_index == next_room_idx).cloned() {
                                spawn_next = Some(next_r);
                            }
                        }
                    }
                }
            }
        }
        (spawn_next, hive_loot.map(|elem_tid| {
            reg.next_seq += 1;
            let elem_id = 1_899_000_000 + reg.next_seq;
            reg.next_seq += 1;
            let blue_id = 1_899_100_000 + reg.next_seq;
            (elem_tid, elem_id, blue_id)
        }))
    });

    // Abyssal Bioadaptive Cache destroyed: wreck + loot transfer
    crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(_systems, db, solar_system_id, npc_ball_id, killer_char_id).await;
    // CRAB Rogue Drone wave NPC destroyed: advance next wave if cleared early
    crate::services::pve_crab_beacons::on_crab_wave_npc_destroyed(_systems, db, _sde, solar_system_id, npc_ball_id).await;

    let (spawn_next, hive_loot) = drifter_res;
    if let Some(next_room) = spawn_next {
        let sys_handle = _systems.get_or_spawn(solar_system_id);
        for (&bid, &tid) in next_room.npc_ball_ids.iter().zip(&next_room.npc_type_ids) {
            _systems.claim_special_site_ball(solar_system_id, bid);
            let cs = build_special_site_npc_combat_state(_sde, tid);
            let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0)), cs).await;
        }
        if next_room.beacon_ball_id != 0 {
            _systems.claim_special_site_ball(solar_system_id, next_room.beacon_ball_id);
            let _ = sys_handle.spawn_ball(make_site_npc_ball(next_room.beacon_ball_id, 42_538, DVec3::new(12_000.0, 0.0, 0.0))).await;
        }
    }

    if let Some((elem_tid, elem_id, blue_id)) = hive_loot {
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let ship_id = snap.characters.get(&cid).map(|c| c.active_ship_id).unwrap_or(0);
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &[(elem_id, elem_tid, 1), (blue_id, 30_745, 5)],
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle.spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0))).await;
            }
        }
    }
    acct_changes
}
