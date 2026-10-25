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
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type CharacterId = i64;
type ItemId = i64;
pub use crate::services::pve_incursion_sites::*;
pub use crate::services::pve_homefront_sites::*;
pub use crate::services::pve_metaliminal_storms::*;
pub use crate::services::pve_special_sites_abyssal::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomefrontSiteRecord {
    pub instance_id: i64, pub dungeon_id: u32, pub dungeon_name: String, pub solar_system_id: SolarSystemId,
    pub optimal_fleet_size: usize, pub base_pilot_isk: i64, pub completed: bool,
    pub reward_paid_isk: i64, pub boss_npc_ball_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrifterHiveRoom {
    pub room_index: u8,
    pub room_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub npc_type_ids: Vec<u32>,
    pub beacon_ball_id: i64,
    pub cleared: bool,
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
    sync_metaliminal_storm_effects_and_sites(ctx);
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
                boss_type_id: 48122, // Striking Leshak (Battleship)
                escort_ball_ids: vec![1_897_700_000 + (base_inst + 1) * 10 + 1, 1_897_700_000 + (base_inst + 1) * 10 + 2],
                escort_type_ids: vec![48086, 48092], // Ghosting Damavik, Striking Damavik
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
                boss_type_id: 48126, // Warding Leshak (Battleship)
                escort_ball_ids: vec![1_897_700_000 + (base_inst + 2) * 10 + 1],
                escort_type_ids: vec![48088], // Tangling Damavik
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
                },
                DrifterHiveRoom {
                    room_index: 2,
                    room_name: "Assembler Chamber".into(),
                    npc_ball_ids: vec![base_ball + 21, base_ball + 22],
                    npc_type_ids: vec![47956, 47957], // Drifter Vanguard Battleship, Drifter Assault Battleship
                    beacon_ball_id: base_ball + 20,
                    cleared: false,
                },
                DrifterHiveRoom {
                    room_index: 3,
                    room_name: "Hive Sanctuary".into(),
                    npc_ball_ids: vec![warden_npc_ball_id],
                    npc_type_ids: vec![37473], // Artemis Tyrannos / Drifter Response Battleship
                    beacon_ball_id: base_ball + 31,
                    cleared: false,
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

        for &sys_id in &triglavian_minor_victory_systems() {
            ensure_triglavian_sites_for_system(reg, sys_id);
        }
    });
    sync_metaliminal_storm_effects_and_sites(ctx);
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
            if !hive.cleared {
                if let Some(room) = hive.rooms.iter().find(|r| r.room_index == hive.current_room && !r.cleared) {
                    for (&bid, &tid) in room.npc_ball_ids.iter().zip(&room.npc_type_ids) {
                        list.push((hive.solar_system_id, bid, tid, DVec3::new(10_000.0, 0.0, 0.0)));
                    }
                    if room.beacon_ball_id != 0 {
                        list.push((hive.solar_system_id, room.beacon_ball_id, 42_538, DVec3::new(12_000.0, 0.0, 0.0)));
                    }
                }
            }
        }
        for sites in reg.triglavian_sites.values() {
            for s in sites {
                if !s.completed {
                    let boss_tid = if s.boss_type_id != 0 { s.boss_type_id } else { 48_122 };
                    list.push((s.solar_system_id, s.boss_npc_ball_id, boss_tid, DVec3::new(10_000.0, 0.0, 0.0)));
                    for (&esc_bid, &esc_tid) in s.escort_ball_ids.iter().zip(&s.escort_type_ids) {
                        list.push((s.solar_system_id, esc_bid, esc_tid, DVec3::new(15_000.0, 2_000.0, 0.0)));
                    }
                }
            }
        }
        list
    });
    for (sys_id, bid, tid, pos) in to_spawn {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, tid, pos)).await;
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
        if hive.current_room < hive.total_rooms {
            let current_room_idx = hive.current_room;
            if let Some(room) = hive.rooms.iter().find(|r| r.room_index == current_room_idx) {
                for &bid in &room.npc_ball_ids {
                    if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                        return Err(rpc_err("WaveNpcsRemaining", format!("Room {} NPC {} has not been destroyed", current_room_idx, bid)));
                    }
                }
            }
            let (kv, next_to_spawn) = with_pve_special_reg(ctx, |reg| {
                let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
                if let Some(r) = h.rooms.iter_mut().find(|r| r.room_index == current_room_idx) {
                    r.cleared = true;
                }
                h.current_room += 1;
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
                    let _ = sys_handle.spawn_ball(make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0))).await;
                }
                if next_room.beacon_ball_id != 0 {
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
        let ship_id = session.ship_id.unwrap_or(char_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: elem_tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: false, ..Default::default()
            });
            snap.items.insert(blue_iid, InventoryItemRecord {
                item_id: blue_iid, type_id: 30_745, owner_id: char_id, location_id: ship_id, // Sleeper Data Library
                flag: InventoryFlag::Cargo, quantity: 5, singleton: false, ..Default::default()
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
        // parity: feat:pve.homefront-ops, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") => {
            Some(handle_get_homefront_site_instances(ctx, session, args))
        }

        // parity: feat:pve.triglavian-sites
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

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

        // parity: feat:pve.triglavian-sites, rpc:map.GetTriglavianMinorVictorySystems
        // const-ok: No active invasion campaigns configured
        ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
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

pub async fn on_special_site_npc_destroyed_core(
    _systems: &Arc<crate::system_actor::SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<eve_sde::SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) {
    let now_sec = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    // 1. Check Triglavian sites
    let trig_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.triglavian_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
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
        if let Some(cid) = killer_char_id {
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    let g = ch.standings.entry(g_fac).or_insert(0.0);
                    *g = (*g + 0.15).clamp(-10.0, 10.0);
                    let l = ch.standings.entry(l_fac).or_insert(0.0);
                    *l = (*l - 0.15).clamp(-10.0, 10.0);
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(base_isk)).unwrap_or(ch.balance);
                    let ship_id = ch.active_ship_id;
                    snap.items.insert(loot_id, InventoryItemRecord {
                        item_id: loot_id,
                        type_id: 48_121, // Sealed Survey Database
                        owner_id: cid,
                        location_id: if ship_id > 0 { ship_id } else { cid },
                        flag: InventoryFlag::Cargo,
                        quantity: db_drop as i32,
                        singleton: false,
                        ..Default::default()
                    });
                }
            });
        }
        return;
    }

    // 2. Check Drifter Hives
    let drifter_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found_tid = None;
        if let Some(hive) = reg.drifter_hives.get_mut(&solar_system_id) {
            if hive.warden_npc_ball_id == npc_ball_id && !hive.cleared {
                hive.cleared = true;
                hive.current_room = hive.total_rooms;
                if let Some(r) = hive.rooms.iter_mut().find(|r| r.room_index == hive.total_rooms) {
                    r.cleared = true;
                }
                found_tid = Some(hive.element_type_id);
            }
        }
        found_tid.map(|elem_tid| {
            reg.next_seq += 1;
            let elem_id = 1_899_000_000 + reg.next_seq;
            reg.next_seq += 1;
            let blue_id = 1_899_100_000 + reg.next_seq;
            (elem_tid, elem_id, blue_id)
        })
    });

    if let Some((elem_tid, elem_id, blue_id)) = drifter_res {
        if let Some(cid) = killer_char_id {
            db.with_mut_snapshot(|snap| {
                let ship_id = snap.characters.get(&cid).map(|c| c.active_ship_id).unwrap_or(cid);
                snap.items.insert(elem_id, InventoryItemRecord {
                    item_id: elem_id,
                    type_id: elem_tid,
                    owner_id: cid,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    ..Default::default()
                });
                snap.items.insert(blue_id, InventoryItemRecord {
                    item_id: blue_id,
                    type_id: 30_745, // Sleeper Data Library
                    owner_id: cid,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                });
            });
        }
    }
}
