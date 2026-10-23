//! `pve_exploration` — PvE Cosmic Combat Anomalies, Combat Signatures, DED Rated Complexes,
//! Escalations & Expeditions, and Wormhole Sleeper Combat Sites (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_hacking_sites::handle_pve_hacking;
use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

type CharacterId = i64;
type ItemId = i64;

mod tuple_map_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use std::hash::Hash;

    pub fn serialize<S, K1, K2, V>(
        map: &HashMap<(K1, K2), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K1: Serialize + Copy,
        K2: Serialize + Copy,
        V: Serialize,
    {
        let entries: Vec<((K1, K2), &V)> = map.iter().map(|(&(k1, k2), v)| ((k1, k2), v)).collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, K1, K2, V>(deserializer: D) -> Result<HashMap<(K1, K2), V>, D::Error>
    where
        D: Deserializer<'de>,
        K1: Deserialize<'de> + Eq + Hash,
        K2: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
    {
        let entries: Vec<((K1, K2), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PveSiteInstance {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub site_category: String,
    pub ded_rating: Option<u8>,
    pub wormhole_class: Option<u8>,
    pub position: [f64; 3],
    pub probe_required: bool,
    pub signal_strength: f64,
    pub current_room: u8,
    pub total_rooms: u8,
    pub current_wave: u8,
    pub total_waves: u8,
    pub active_npc_ball_ids: Vec<i64>,
    pub hackable_can_ids: Vec<ItemId>,
    pub npc_guards_spawned: bool,
    pub ghost_timer_started_at: Option<u64>,
    pub ghost_detonate_after_sec: u64,
    pub ghost_detonated: bool,
    pub sleeper_rift_stabilized: bool,
    pub sleeper_sentry_disabled: bool,
    pub sleeper_gas_cloud_active: bool,
    pub cleared: bool,
    #[serde(default)]
    pub created_unix: u64,
    #[serde(default)]
    pub lifetime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalatingPathEntry {
    pub instance_id: i64,
    pub character_id: CharacterId,
    pub source_dungeon_id: u32,
    pub expedition_dungeon_id: u32,
    pub expedition_name: String,
    pub destination_solar_system_id: SolarSystemId,
    pub step_index: u8,
    pub total_steps: u8,
    pub journal_text: String,
    pub created_at_sec: u64,
    pub expires_at_sec: u64,
    pub dungeons_entered: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingNodeKind {
    Empty,
    SystemCore,
    Firewall,
    Antivirus,
    RestorationNode,
    VirusSuppressor,
    DataCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingUtilityKind {
    SelfRepair = 1,
    KernelRot = 2,
    PolymorphicShield = 3,
    SecondaryVector = 4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingTileState {
    pub tile_index: u32,
    pub revealed: bool,
    pub blocked_by_defensive: bool,
    pub kind: HackingNodeKind,
    pub coherence: i32,
    pub strength: i32,
    pub utility_drop: Option<HackingUtilityKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingGameSession {
    pub game_id: i64,
    pub character_id: CharacterId,
    pub solar_system_id: SolarSystemId,
    pub container_id: ItemId,
    pub site_id: String,
    pub is_relic: bool,
    pub is_ghost_site: bool,
    pub is_sleeper_cache_puzzle: bool,
    pub virus_coherence: i32,
    pub max_virus_coherence: i32,
    pub base_virus_strength: i32,
    pub polymorphic_shield_charges: u8,
    pub secondary_vector_target: Option<(u32, u8)>,
    pub utilities: Vec<Option<HackingUtilityKind>>,
    pub tiles: Vec<HackingTileState>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpewedLootCan {
    pub can_item_id: ItemId,
    pub parent_container_id: ItemId,
    pub solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub loot_type_id: TypeId,
    pub loot_quantity: i64,
    pub spawned_at_sec: u64,
    pub despawn_at_sec: u64,
    pub scooped: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PveExplorationRegistry {
    #[serde(skip)]
    pub db_weak: std::sync::Weak<eve_db::WorldRepository>,
    #[serde(skip)]
    pub current_now_sec: u64,
    pub seeded_systems: HashMap<SolarSystemId, bool>,
    pub sites_by_system: HashMap<SolarSystemId, Vec<PveSiteInstance>>,
    pub escalations_by_char: HashMap<CharacterId, Vec<EscalatingPathEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    pub hacking_games: HashMap<(CharacterId, ItemId), HackingGameSession>,
    pub spewed_cans: HashMap<ItemId, SpewedLootCan>,
    pub sim_clock_sec: u64,
    pub next_id_seq: i64,
}

const PVE_EXPLORATION_NOTE_ID: i64 = -8_200_000_000_000;

pub fn ctx_pve_key(ctx: &ServerContext) -> usize {
    std::sync::Arc::as_ptr(&ctx.db) as usize
}

pub fn with_pve_exploration_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveExplorationRegistry) -> R,
) -> R {
    let (mut reg, orig_json) = ctx.db.with_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID) {
            if let Ok(parsed) = serde_json::from_str::<PveExplorationRegistry>(&note.note) {
                return (parsed, note.note.clone());
            }
        }
        (PveExplorationRegistry::default(), String::new())
    });
    reg.db_weak = std::sync::Arc::downgrade(&ctx.db);
    reg.current_now_sec = ctx.now_unix();
    let out = f(&mut reg);
    if let Ok(new_json) = serde_json::to_string(&reg) {
        if new_json != orig_json {
            ctx.db.with_mut_snapshot(|snap| {
                snap.owner_notes.insert(
                    PVE_EXPLORATION_NOTE_ID,
                    eve_db::OwnerNoteRecord {
                        note_id: PVE_EXPLORATION_NOTE_ID,
                        character_id: -82,
                        item_id: 0,
                        label: "pve_exploration_registry".into(),
                        note: new_json,
                        timestamp_unix: reg.sim_clock_sec,
                    },
                );
            });
        }
    }
    out
}

pub fn next_pve_seq(reg: &mut PveExplorationRegistry) -> i64 {
    reg.next_id_seq += 1;
    reg.next_id_seq
}

pub fn now_sec(reg: &PveExplorationRegistry) -> u64 {
    let base = if reg.current_now_sec > 0 {
        reg.current_now_sec
    } else {
        1_775_000_000
    };
    base.max(reg.sim_clock_sec)
}

pub fn advance_pve_exploration_clock(ctx: &ServerContext, delta_sec: u64) {
    let cur_now = ctx.now_unix();
    with_pve_exploration_reg(ctx, |reg| {
        if reg.sim_clock_sec < cur_now {
            reg.sim_clock_sec = cur_now;
        }
        reg.sim_clock_sec += delta_sec;
    });
}

// parity: feat:pve.wormhole-sites
pub fn wormhole_class_for_system(solar_system_id: SolarSystemId) -> Option<u8> {
    if !(31_000_000..32_000_000).contains(&solar_system_id) {
        return None;
    }
    Some(((solar_system_id - 31_000_001) % 6) as u8 + 1)
}

// parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites
pub fn anomaly_dungeons_for_system(
    solar_system_id: SolarSystemId,
    true_sec: f64,
) -> Vec<(u32, &'static str, &'static str, u8, Option<u8>)> {
    if let Some(wh_class) = wormhole_class_for_system(solar_system_id) {
        return match wh_class {
            1 | 2 => vec![
                (3707, "Perimeter Ambush Point", "WormholeCombat", 3, Some(wh_class)),
                (3708, "Perimeter Camp", "WormholeCombat", 3, Some(wh_class)),
            ],
            3 | 4 => vec![
                (3710, "Fortification Frontier Stronghold", "WormholeCombat", 3, Some(wh_class)),
                (3713, "Outpost Frontier Stronghold", "WormholeCombat", 3, Some(wh_class)),
            ],
            _ => vec![
                (3704, "Core Citadel", "WormholeCombat", 4, Some(wh_class)),
                (3716, "Core Garrison", "WormholeCombat", 4, Some(wh_class)),
            ],
        };
    }
    if true_sec >= 0.5 {
        vec![
            (2132, "Angel Hideaway", "CombatAnomaly", 2, None),
            (1214, "Angel Watch", "CombatAnomaly", 3, None),
        ]
    } else if true_sec > 0.0 {
        vec![
            (1228, "Angel Vigil", "CombatAnomaly", 3, None),
            (2115, "Angel Haven", "CombatAnomaly", 3, None),
        ]
    } else {
        vec![
            (2115, "Angel Haven", "CombatAnomaly", 3, None),
            (2311, "Angel Sanctum", "CombatAnomaly", 4, None),
        ]
    }
}

// parity: feat:pve.ded-complexes
pub fn ded_complex_for_rating(ded_rating: u8) -> (u32, &'static str) {
    match ded_rating {
        1 => (1012, "Knaaninn Aranuri"),
        2 => (5317, "Blood Raider Human Farm"),
        3 => (5302, "Guristas Guerilla Grounds"),
        4 => (106, "Mul-Zatah Monastery"),
        5 => (168, "Angel's Red Light District"),
        6 => (47, "Crimson Hand Supply Depot"),
        7 => (59, "Angel Military Operations Complex"),
        8 => (5221, "Blood Raider Prison Camp"),
        9 => (2519, "Angel Domination Fleet Staging Point"),
        _ => (140, "The Maze"),
    }
}

// parity: feat:pve.ded-complexes
pub fn ded_ship_restriction_for_rating(ded_rating: u8) -> (f64, &'static str, &'static [u32]) {
    match ded_rating {
        1 => (5_000_000.0, "Frigate Hulls Only (1/10)", &[25, 324, 830, 831, 834, 1283, 1527]),
        2 => (
            10_000_000.0,
            "Frigate and Destroyer Hulls Only (2/10)",
            &[25, 324, 420, 541, 830, 831, 834, 1283, 1305, 1527, 1534],
        ),
        3 => (
            25_000_000.0,
            "Up to Cruiser Hulls (3/10)",
            &[25, 26, 324, 358, 420, 541, 830, 831, 832, 833, 834, 894, 906, 963, 1283, 1305, 1527, 1534, 1972],
        ),
        4 => (
            75_000_000.0,
            "Up to Battlecruiser Hulls (4/10)",
            &[25, 26, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972],
        ),
        _ => (
            250_000_000.0,
            "Up to Battleship Hulls (5/10-10/10, No Capitals)",
            &[25, 26, 27, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 898, 900, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972],
        ),
    }
}

fn make_sig_record(
    signature_id: String,
    solar_system_id: SolarSystemId,
    sig_group: &str,
    type_name: String,
    position: [f64; 3],
    created_unix: u64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: sig_group.into(),
        type_name,
        position,
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    }
}

fn make_can_record(can_id: ItemId, type_id: TypeId, solar_system_id: SolarSystemId) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: can_id,
        type_id,
        owner_id: 1_000_125,
        location_id: solar_system_id as i64,
        flag: InventoryFlag::None,
        quantity: 1,
        singleton: true,
        ..Default::default()
    }
}

// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.ghost-sites, feat:pve.sleeper-caches, feat:pve.wormhole-sites
pub async fn ensure_system_pve_sites_seeded(ctx: &ServerContext, solar_system_id: SolarSystemId) {
    let already_seeded = with_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.get(&solar_system_id).copied().unwrap_or(false)
    });
    if already_seeded {
        return;
    }

    let true_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let wh_class = wormhole_class_for_system(solar_system_id);

    let now = ctx.now_unix();
    let mut sites = Vec::new();
    let mut new_anoms = Vec::new();
    let mut new_sigs = Vec::new();
    let mut cans_to_insert = Vec::new();
    let mut balls_to_spawn = Vec::new();

    // 1. True-Sec Combat Anomalies (or Wormhole Sleeper Combat Anomalies)
    for (idx, (did, default_name, cat, waves, wh_c)) in
        anomaly_dungeons_for_system(solar_system_id, true_sec).into_iter().enumerate()
    {
        let d_name = ctx
            .sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .unwrap_or_else(|| default_name.to_string());
        let site_id = format!("PVE-ANOM-{solar_system_id}-{}", idx + 1);
        let pos = [25_000_000.0 + (idx as f64) * 5_000_000.0, 12_000_000.0, -8_000_000.0];
        let wave1_npcs = build_wave_npc_balls(solar_system_id, did, 1, pos, wh_c);
        let npc_ids: Vec<i64> = wave1_npcs.iter().map(|b| b.id).collect();
        balls_to_spawn.extend(wave1_npcs);

        new_anoms.push(CosmicAnomalyRecord {
            anomaly_id: site_id.clone(),
            solar_system_id,
            anomaly_type: "Combat".into(),
            name: d_name.clone(),
            position: pos,
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: now,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        sites.push(PveSiteInstance {
            site_id,
            dungeon_id: did,
            dungeon_name: d_name,
            solar_system_id,
            site_category: cat.into(),
            ded_rating: None,
            wormhole_class: wh_c,
            position: pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: waves,
            active_npc_ball_ids: npc_ids,
            hackable_can_ids: Vec::new(),
            npc_guards_spawned: true,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 90,
            ghost_detonated: false,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            cleared: false,
            created_unix: now,
            lifetime_seconds: 86_400,
        });
    }

    // 2. Scan-only Rated DED Complex Signature
    let default_ded_rating: u8 = if true_sec >= 0.5 { 3 } else if true_sec > 0.0 { 5 } else { 10 };
    let (ded_did, ded_fallback_name) = ded_complex_for_rating(default_ded_rating);
    let ded_name = ctx
        .sde
        .dungeons
        .get(&ded_did)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| ded_fallback_name.to_string());
    let ded_sig_id = format!("DED-{solar_system_id}-{default_ded_rating}");
    let ded_pos = [45_000_000.0, -20_000_000.0, 15_000_000.0];
    let ded_room1_npcs = build_wave_npc_balls(solar_system_id, ded_did, 1, ded_pos, None);
    let ded_npc_ids: Vec<i64> = ded_room1_npcs.iter().map(|b| b.id).collect();
    balls_to_spawn.extend(ded_room1_npcs);
    new_sigs.push(make_sig_record(ded_sig_id.clone(), solar_system_id, "Combat", ded_name.clone(), ded_pos, now));
    sites.push(PveSiteInstance {
        site_id: ded_sig_id,
        dungeon_id: ded_did,
        dungeon_name: ded_name,
        solar_system_id,
        site_category: "DED".into(),
        ded_rating: Some(default_ded_rating),
        wormhole_class: None,
        position: ded_pos,
        probe_required: true,
        signal_strength: 0.0,
        current_room: 1,
        total_rooms: 3,
        current_wave: 1,
        total_waves: 3,
        active_npc_ball_ids: ded_npc_ids,
        hackable_can_ids: Vec::new(),
        npc_guards_spawned: true,
        ghost_timer_started_at: None,
        ghost_detonate_after_sec: 90,
        ghost_detonated: false,
        sleeper_rift_stabilized: false,
        sleeper_sentry_disabled: false,
        sleeper_gas_cloud_active: false,
        cleared: false,
        created_unix: now,
        lifetime_seconds: 86_400,
    });

    // 3. Data, Relic, Ghost & Sleeper Cache Signatures
    let guarded = wh_class.is_some() || true_sec <= 0.0;
    let sig_specs: [(String, u32, &str, &str, &str, [f64; 3], i64, TypeId, bool); 4] = [
        (
            format!("DAT-{solar_system_id}"),
            if guarded { 3729 } else { 1133 },
            if guarded { "Unsecured Frontier Enclave Relay" } else { "Digital Plexus" },
            "Data",
            "Data",
            [-32_000_000.0, 28_000_000.0, -10_000_000.0],
            1_870_000_000,
            21_740,
            guarded,
        ),
        (
            format!("REL-{solar_system_id}"),
            if guarded { 3722 } else { 1286 },
            if guarded { "Forgotten Core Data Field" } else { "Ancient Ruins" },
            "Relic",
            "Relic",
            [-38_000_000.0, -30_000_000.0, 22_000_000.0],
            1_871_000_000,
            21_741,
            guarded,
        ),
        (
            format!("GHO-{solar_system_id}"),
            if wh_class.is_some() { 5536 } else if true_sec <= 0.0 { 5535 } else if true_sec < 0.5 { 5534 } else { 5533 },
            "Standard Angel Covert Research Facility",
            "Data",
            "Ghost",
            [52_000_000.0, 42_000_000.0, -18_000_000.0],
            1_872_000_000,
            21_740,
            false,
        ),
        (
            format!("SLC-{solar_system_id}"),
            if true_sec <= 0.0 { 5632 } else if true_sec < 0.5 { 5609 } else { 5604 },
            "Standard Sleeper Cache",
            "Data",
            "SleeperCache",
            [-58_000_000.0, 14_000_000.0, 35_000_000.0],
            1_873_000_000,
            21_740,
            true,
        ),
    ];

    for (sig_id, did, fallback_name, grp, cat, pos, base_can_id, can_tid, has_guards) in sig_specs {
        let name = ctx
            .sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .unwrap_or_else(|| fallback_name.to_string());
        let mut can_ids = Vec::new();
        for c_idx in 0..3_i64 {
            let cid = base_can_id + (solar_system_id as i64 % 100_000) * 100 + c_idx;
            can_ids.push(cid);
            cans_to_insert.push(make_can_record(cid, can_tid, solar_system_id));
        }
        let guard_balls = if has_guards {
            build_wave_npc_balls(solar_system_id, did, 1, pos, wh_class.or(Some(3)))
        } else {
            Vec::new()
        };
        let guard_ids: Vec<i64> = guard_balls.iter().map(|b| b.id).collect();
        balls_to_spawn.extend(guard_balls);

        new_sigs.push(make_sig_record(sig_id.clone(), solar_system_id, grp, name.clone(), pos, now));
        sites.push(PveSiteInstance {
            site_id: sig_id,
            dungeon_id: did,
            dungeon_name: name,
            solar_system_id,
            site_category: cat.into(),
            ded_rating: None,
            wormhole_class: wh_class,
            position: pos,
            probe_required: true,
            signal_strength: 0.0,
            current_room: 1,
            total_rooms: if cat == "SleeperCache" { 3 } else { 1 },
            current_wave: 1,
            total_waves: if cat == "SleeperCache" { 2 } else { 1 },
            active_npc_ball_ids: guard_ids,
            hackable_can_ids: can_ids,
            npc_guards_spawned: has_guards,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 90,
            ghost_detonated: false,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: cat == "SleeperCache",
            cleared: false,
            created_unix: now,
            lifetime_seconds: 86_400,
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for ball in balls_to_spawn {
        let _ = sys_handle.spawn_ball(ball).await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let sys_anoms = snap.cosmic_anomalies.entry(solar_system_id).or_default();
        for a in new_anoms {
            if !sys_anoms.iter().any(|x| x.anomaly_id == a.anomaly_id) {
                sys_anoms.push(a);
            }
        }
        let sys_sigs = snap.cosmic_signatures.entry(solar_system_id).or_default();
        for s in new_sigs {
            if !sys_sigs.iter().any(|x| x.signature_id == s.signature_id) {
                sys_sigs.push(s);
            }
        }
        for item in cans_to_insert {
            snap.items.insert(item.item_id, item);
        }
    });

    with_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.insert(solar_system_id, true);
        reg.sites_by_system.insert(solar_system_id, sites);
    });
}

pub fn build_wave_npc_balls(
    solar_system_id: SolarSystemId,
    dungeon_id: u32,
    wave_number: u8,
    center: [f64; 3],
    wormhole_class: Option<u8>,
) -> Vec<Ball> {
    let npc_type_id: TypeId = match wormhole_class {
        Some(1 | 2) => 30_192,
        Some(3 | 4) => 30_195,
        Some(_) => 30_198,
        None => 10_962,
    };
    let count = (wave_number as usize).clamp(2, 4);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let ball_id = 1_860_000_000
            + (solar_system_id as i64 % 10_000) * 100_000
            + (dungeon_id as i64 % 1_000) * 100
            + (wave_number as i64) * 10
            + (i as i64)
            + 1;
        out.push(Ball {
            id: ball_id,
            type_id: npc_type_id,
            position: DVec3::new(
                center[0] + (i as f64) * 2_500.0,
                center[1] + (wave_number as f64) * 1_500.0,
                center[2],
            ),
            velocity: DVec3::ZERO,
            max_velocity: 420.0,
            inertia: 1.0,
            mass: 2_500_000.0,
            radius: 180.0,
            speed_fraction: 0.75,
            mode: MovementMode::Stop,
        });
    }
    out
}

// parity: feat:pve.combat-anomalies, feat:pve.ded-complexes, feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites
pub async fn advance_site_wave_or_complete(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> Result<PyRep, BridgeRpcError> {
    let char_id = session.character_id.unwrap_or(90_000_001);
    if session.solar_system_id != Some(solar_system_id) {
        return Err(BridgeRpcError {
            code: "NotInSiteSystem".into(),
            message: format!("Caller must be in solar system {solar_system_id} to advance site {site_id}"),
        });
    }
    ensure_system_pve_sites_seeded(ctx, solar_system_id).await;

    let (current_balls, is_sleeper_cache_stabilized) = with_pve_exploration_reg(ctx, |reg| {
        let sites = reg.sites_by_system.entry(solar_system_id).or_default();
        let Some(site) = sites
            .iter()
            .find(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
        else {
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {site_id} not found in system {solar_system_id}"),
            });
        };
        if site.cleared {
            return Err(BridgeRpcError {
                code: "SiteAlreadyCleared".into(),
                message: format!("PvE site {site_id} is already cleared"),
            });
        }
        Ok((
            site.active_npc_ball_ids.clone(),
            site.site_category == "SleeperCache" && site.sleeper_rift_stabilized,
        ))
    })?;

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if !is_sleeper_cache_stabilized {
        for &bid in &current_balls {
            if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                return Err(BridgeRpcError {
                    code: "WaveNpcsRemaining".into(),
                    message: format!("Cannot advance site {site_id}: wave NPC ball {bid} has not been destroyed"),
                });
            }
        }
    }

    let (
        dungeon_id,
        dungeon_name,
        site_category,
        ded_rating,
        wh_class,
        pos,
        new_wave,
        total_waves,
        old_balls,
        completed_now,
    ) = with_pve_exploration_reg(ctx, |reg| {
        let sites = reg.sites_by_system.entry(solar_system_id).or_default();
        let Some(site) = sites
            .iter_mut()
            .find(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
        else {
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {site_id} not found in system {solar_system_id}"),
            });
        };
        let old = std::mem::take(&mut site.active_npc_ball_ids);
        let done = site.current_wave >= site.total_waves;
        if done {
            site.cleared = true;
        } else {
            site.current_wave += 1;
        }
        Ok((
            site.dungeon_id,
            site.dungeon_name.clone(),
            site.site_category.clone(),
            site.ded_rating,
            site.wormhole_class,
            site.position,
            site.current_wave,
            site.total_waves,
            old,
            done,
        ))
    })?;

    for bid in old_balls {
        let _ = sys_handle.remove_ball(bid).await;
    }

    let mut spawned_npc_ids = Vec::new();
    let mut dropped_loot_items = Vec::new();
    let mut escalation_created: Option<EscalatingPathEntry> = None;

    if !completed_now {
        let next_balls = build_wave_npc_balls(solar_system_id, dungeon_id, new_wave, pos, wh_class);
        for b in next_balls {
            spawned_npc_ids.push(b.id);
            let _ = sys_handle.spawn_ball(b).await;
        }
        with_pve_exploration_reg(ctx, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id || s.dungeon_id == dungeon_id))
            {
                site.active_npc_ball_ids = spawned_npc_ids.clone();
            }
        });
    } else {
        let loot_location_id = session.ship_id.unwrap_or(solar_system_id as i64);
        let mut loot_specs: Vec<(TypeId, i64, String)> = Vec::new();

        if let Some(rating) = ded_rating {
            let ope_tier = (rating as u32 * 2).clamp(1, 23);
            let ope_type_id = 19_399 + ope_tier;
            loot_specs.push((ope_type_id, 1, format!("{ope_tier}th Tier Overseer's Personal Effects")));
        }
        if let Some(wh_c) = wh_class {
            let (blue_tid, blue_qty, blue_name) = match wh_c {
                1 | 2 => (30_744, (wh_c as i64) * 4, "Neural Network Analyzer"),
                3 | 4 => (30_745, (wh_c as i64) * 3, "Sleeper Data Library"),
                _ => (30_746, (wh_c as i64) * 3, "Ancient Coordinates Database"),
            };
            loot_specs.push((blue_tid, blue_qty, blue_name.into()));
        }

        with_pve_exploration_reg(ctx, |reg| {
            let created_ts = now_sec(reg);
            for (l_tid, l_qty, l_name) in &loot_specs {
                let item_id = 1_880_000_000 + next_pve_seq(reg);
                dropped_loot_items.push((item_id, *l_tid, *l_qty, l_name.clone()));
            }
            if matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature" | "DED") {
                let dest_sys_id = find_escalation_destination_system(ctx, solar_system_id);
                let (exp_did, exp_default_name) = match dungeon_id {
                    2132 | 1214 | 1228 => (1422_u32, "Chasing the Dragon"),
                    2115 => (2525_u32, "Angel Domination Fleet Staging Point 2."),
                    2311 => (2396_u32, "Angel Cartel Naval Shipyard"),
                    _ => (2403_u32, "The Maze"),
                };
                let exp_name = ctx
                    .sde
                    .dungeons
                    .get(&exp_did)
                    .map(|d| d.name.trim().to_string())
                    .unwrap_or_else(|| exp_default_name.to_string());
                let inst_id = 910_000 + next_pve_seq(reg);
                let entry = EscalatingPathEntry {
                    instance_id: inst_id,
                    character_id: char_id,
                    source_dungeon_id: dungeon_id,
                    expedition_dungeon_id: exp_did,
                    expedition_name: exp_name.clone(),
                    destination_solar_system_id: dest_sys_id,
                    step_index: 1,
                    total_steps: 4,
                    journal_text: format!(
                        "Logs recovered from {dungeon_name} point to secret pirate coordinates for '{exp_name}' in solar system {dest_sys_id}."
                    ),
                    created_at_sec: created_ts,
                    expires_at_sec: created_ts + 86_400,
                    dungeons_entered: vec![dungeon_id],
                };
                reg.escalations_by_char.entry(char_id).or_default().push(entry.clone());
                escalation_created = Some(entry);
            }
        });

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                if let Some(a) = anoms.iter_mut().find(|a| a.anomaly_id == site_id) {
                    a.cleared = true;
                }
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                if let Some(s) = sigs.iter_mut().find(|s| s.signature_id == site_id) {
                    s.collapsed = true;
                }
            }
            if let Some(ref esc) = escalation_created {
                snap.next_entity_id += 1;
                let bm_id = snap.next_entity_id;
                snap.bookmarks.insert(
                    bm_id,
                    eve_db::BookmarkRecord {
                        bookmark_id: bm_id,
                        owner_id: char_id,
                        label: format!("Escalation: {}", esc.expedition_name),
                        solar_system_id: esc.destination_solar_system_id,
                        x: pos[0],
                        y: pos[1],
                        z: pos[2],
                        item_id: Some(esc.instance_id),
                        memo: esc.journal_text.clone(),
                        folder_id: None,
                        is_shared: false,
                    },
                );
            }
            for (item_id, l_tid, l_qty, _l_name) in &dropped_loot_items {
                snap.items.insert(
                    *item_id,
                    InventoryItemRecord {
                        item_id: *item_id,
                        type_id: *l_tid,
                        owner_id: char_id,
                        location_id: loot_location_id,
                        flag: InventoryFlag::Cargo,
                        quantity: *l_qty as i32,
                        singleton: false,
                        ..Default::default()
                    },
                );
            }
        });

        session.push_notification("OnCosmicAnomalyRemoved", vec![PyRep::String(site_id.to_string())]);
        if let Some(esc) = &escalation_created {
            session.push_notification(
                "OnEscalatingPathMessage",
                vec![
                    PyRep::Integer(esc.instance_id),
                    PyRep::keyval([
                        ("instanceID", PyRep::Integer(esc.instance_id)),
                        ("dungeonID", PyRep::Integer(esc.expedition_dungeon_id as i64)),
                        ("dungeonName", PyRep::String(esc.expedition_name.clone())),
                        ("destinationSolarSystemID", PyRep::Integer(esc.destination_solar_system_id as i64)),
                        ("journalText", PyRep::String(esc.journal_text.clone())),
                        ("expiresAt", PyRep::Integer(esc.expires_at_sec as i64)),
                    ]),
                ],
            );
        }
    }

    let loot_py: Vec<PyRep> = dropped_loot_items
        .iter()
        .map(|(iid, tid, qty, name)| {
            PyRep::keyval([
                ("itemID", PyRep::Integer(*iid)),
                ("typeID", PyRep::Integer(*tid as i64)),
                ("quantity", PyRep::Integer(*qty)),
                ("typeName", PyRep::String(name.clone())),
            ])
        })
        .collect();

    Ok(PyRep::keyval([
        ("siteID", PyRep::String(site_id.to_string())),
        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
        ("dungeonName", PyRep::String(dungeon_name)),
        ("currentWave", PyRep::Integer(new_wave as i64)),
        ("totalWaves", PyRep::Integer(total_waves as i64)),
        ("completed", PyRep::Bool(completed_now)),
        ("activeNpcBallIDs", PyRep::List(spawned_npc_ids.into_iter().map(PyRep::Integer).collect())),
        ("droppedLoot", PyRep::List(loot_py)),
        ("escalated", PyRep::Bool(escalation_created.is_some())),
        (
            "escalationInstanceID",
            escalation_created.as_ref().map(|e| PyRep::Integer(e.instance_id)).unwrap_or(PyRep::None),
        ),
    ]))
}

fn find_escalation_destination_system(
    ctx: &ServerContext,
    origin_system_id: SolarSystemId,
) -> SolarSystemId {
    for jump in ctx.sde.jumps.values() {
        if jump.from_solar_system_id == origin_system_id {
            return jump.to_solar_system_id;
        }
    }
    origin_system_id.saturating_add(1)
}

pub async fn handle_pve_exploration(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = handle_pve_hacking(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }

    match (service, method) {
        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites
        ("scanSvc", "GetCombatAnomalyInstances") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as SolarSystemId)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;

            let list = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                reg.sites_by_system
                    .get(&sys_id)
                    .map(|sites| {
                        sites
                            .iter()
                            .filter(|s| {
                                !s.probe_required
                                    && !s.cleared
                                    && (s.lifetime_seconds == 0 || now < s.created_unix.saturating_add(s.lifetime_seconds))
                            })
                            .map(|s| {
                                PyRep::keyval([
                                    ("siteID", PyRep::String(s.site_id.clone())),
                                    ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                                    ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                                    ("siteCategory", PyRep::String(s.site_category.clone())),
                                    (
                                        "wormholeClass",
                                        s.wormhole_class.map(|c| PyRep::Integer(c as i64)).unwrap_or(PyRep::None),
                                    ),
                                    ("currentWave", PyRep::Integer(s.current_wave as i64)),
                                    ("totalWaves", PyRep::Integer(s.total_waves as i64)),
                                    (
                                        "activeNpcBallIDs",
                                        PyRep::List(s.active_npc_ball_ids.iter().copied().map(PyRep::Integer).collect()),
                                    ),
                                    ("probeRequired", PyRep::Bool(false)),
                                    ("overviewVisible", PyRep::Bool(true)),
                                ])
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites, feat:pve.escalations, feat:pve.expeditions
        ("scanSvc", "EnterCombatAnomaly") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let site_id = args
                .first()
                .and_then(|a| a.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("PVE-ANOM-{sys_id}-1"));
            let advance_wave = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
            if advance_wave {
                return Some(advance_site_wave_or_complete(ctx, session, sys_id, &site_id).await);
            }
            let res = with_pve_exploration_reg(ctx, |reg| {
                let Some(site) = reg
                    .sites_by_system
                    .get(&sys_id)
                    .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                else {
                    return Err(BridgeRpcError {
                        code: "AnomalyNotFound".into(),
                        message: format!("Combat anomaly {site_id} not found"),
                    });
                };
                Ok(PyRep::keyval([
                    ("siteID", PyRep::String(site.site_id.clone())),
                    ("dungeonID", PyRep::Integer(site.dungeon_id as i64)),
                    ("dungeonName", PyRep::String(site.dungeon_name.clone())),
                    ("currentWave", PyRep::Integer(site.current_wave as i64)),
                    ("totalWaves", PyRep::Integer(site.total_waves as i64)),
                    (
                        "activeNpcBallIDs",
                        PyRep::List(site.active_npc_ball_ids.iter().copied().map(PyRep::Integer).collect()),
                    ),
                ]))
            });
            Some(res)
        }

        // parity: feat:pve.combat-signatures, feat:pve.data-sites, feat:pve.relic-sites, feat:pve.ghost-sites, feat:pve.sleeper-caches
        ("scanSvc", "GetSignatureInstances") | ("scanSvc", "GetAllSignatures") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as SolarSystemId)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;

            let sigs = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                reg.sites_by_system
                    .get(&sys_id)
                    .map(|sites| {
                        sites
                            .iter()
                            .filter(|s| {
                                s.probe_required
                                    && !s.cleared
                                    && (s.lifetime_seconds == 0 || now < s.created_unix.saturating_add(s.lifetime_seconds))
                            })
                            .map(format_signature_py)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(sigs)))
        }

        // parity: feat:pve.combat-signatures
        ("scanSvc", "ScanSignature") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let sig_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let probe_strength = args.get(1).and_then(|a| a.as_f64()).unwrap_or(1.0).clamp(0.0, 2.0);

            let scan_res = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                let Some(site) = reg
                    .sites_by_system
                    .get_mut(&sys_id)
                    .and_then(|sites| sites.iter_mut().find(|s| s.site_id == sig_id))
                else {
                    return Err(BridgeRpcError {
                        code: "SignatureNotFound".into(),
                        message: format!("Cosmic signature {sig_id} not found"),
                    });
                };
                site.signal_strength = (site.signal_strength + probe_strength).clamp(0.0, 1.0);
                if site.signal_strength >= 1.0 && site.site_category == "Ghost" && site.ghost_timer_started_at.is_none() {
                    site.ghost_timer_started_at = Some(now);
                }
                Ok((site.site_id.clone(), site.signal_strength, format_signature_py(site)))
            });
            let (sid, strength, py_rep) = match scan_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                    if let Some(s) = sigs.iter_mut().find(|x| x.signature_id == sid) {
                        s.signal_strength = strength;
                    }
                }
            });
            Some(Ok(py_rep))
        }

        // parity: feat:pve.ded-complexes
        ("keeper", "GetDungeonShipRestrictions") | ("keeper", "CanEnterDungeonInCurrentShip") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ded_rating = args.first().and_then(|a| a.as_i64()).map(|v| v.clamp(1, 10) as u8).unwrap_or(3);
            let (max_mass, label, allowed_groups) = ded_ship_restriction_for_rating(ded_rating);
            let (did, dname) = ded_complex_for_rating(ded_rating);

            let (ship_type_id, ship_group_id, ship_mass) = session
                .ship_id
                .and_then(|sid| {
                    ctx.db.with_snapshot(|snap| {
                        snap.items.get(&sid).map(|item| {
                            let t = ctx.sde.types.get(&item.type_id);
                            (item.type_id, t.map(|x| x.group_id).unwrap_or(25), t.map(|x| x.mass).unwrap_or(1_200_000.0))
                        })
                    })
                })
                .unwrap_or((587, 25, 1_067_000.0));

            let can_enter = allowed_groups.contains(&ship_group_id) && ship_mass <= max_mass;
            Some(Ok(PyRep::keyval([
                ("dungeonID", PyRep::Integer(did as i64)),
                ("dungeonName", PyRep::String(dname.into())),
                ("dedRating", PyRep::Integer(ded_rating as i64)),
                ("maxShipMass", PyRep::Real(max_mass)),
                ("restrictionDescription", PyRep::String(label.into())),
                ("allowedGroupIDs", PyRep::List(allowed_groups.iter().map(|&g| PyRep::Integer(g as i64)).collect())),
                ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                ("shipGroupID", PyRep::Integer(ship_group_id as i64)),
                ("canEnter", PyRep::Bool(can_enter)),
            ])))
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "EnterNextDungeonRoom") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(instance_id) = args.first().and_then(|a| a.as_i64()) {
                let fleet_count = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 10) as usize;
                return Some(
                    crate::services::pve_special_sites::complete_special_dungeon_instance(
                        ctx,
                        session,
                        instance_id,
                        fleet_count,
                    )
                    .await,
                );
            }
            let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            if site_id.starts_with("INC-") {
                return crate::services::pve_special_sites::handle_pve_special_sites(
                    ctx,
                    session,
                    "incursion",
                    "UpdateIncursionLPLog",
                    args,
                    kwargs,
                )
                .await;
            }
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let ded_rating_override = args.get(1).and_then(|a| a.as_i64()).map(|v| v as u8);

            let (rating_to_check, rift_unstable) = with_pve_exploration_reg(ctx, |reg| {
                let site = reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id));
                let r = ded_rating_override.or_else(|| site.and_then(|s| s.ded_rating));
                let unstable = site.is_some_and(|s| s.site_category == "SleeperCache" && !s.sleeper_rift_stabilized);
                (r, unstable)
            });

            if let Some(rating) = rating_to_check {
                let (max_mass, label, allowed_groups) = ded_ship_restriction_for_rating(rating);
                if let Some(sid) = session.ship_id {
                    let (grp, mass) = ctx.db.with_snapshot(|snap| {
                        snap.items
                            .get(&sid)
                            .and_then(|item| ctx.sde.types.get(&item.type_id))
                            .map(|t| (t.group_id, t.mass))
                            .unwrap_or((25, 1_000_000.0))
                    });
                    if !allowed_groups.contains(&grp) || mass > max_mass {
                        return Some(Err(BridgeRpcError {
                            code: "ShipSizeRestricted".into(),
                            message: format!("Acceleration gate rejects hull group {grp} (mass {mass} kg): {label}"),
                        }));
                    }
                }
            }
            if rift_unstable {
                return Some(Err(BridgeRpcError {
                    code: "RiftPassageUnstable".into(),
                    message: "Must hack the Spatial Rift Generator Control before traversing the Sleeper Cache passage".into(),
                }));
            }

            Some(advance_site_wave_or_complete(ctx, session, sys_id, &site_id).await)
        }

        // parity: feat:pve.sleeper-caches, feat:pve.ded-complexes
        ("keeper", "GetDungeonInfo") | ("keeper", "GetDungeonObjectivesData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let res = with_pve_exploration_reg(ctx, |reg| {
                let Some(site) = reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)) else {
                    return Err(BridgeRpcError {
                        code: "DungeonNotFound".into(),
                        message: format!("Dungeon site {site_id} not found"),
                    });
                };
                Ok(PyRep::keyval([
                    ("siteID", PyRep::String(site.site_id.clone())),
                    ("dungeonID", PyRep::Integer(site.dungeon_id as i64)),
                    ("dungeonName", PyRep::String(site.dungeon_name.clone())),
                    ("siteCategory", PyRep::String(site.site_category.clone())),
                    ("currentRoom", PyRep::Integer(site.current_room as i64)),
                    ("totalRooms", PyRep::Integer(site.total_rooms as i64)),
                    ("currentWave", PyRep::Integer(site.current_wave as i64)),
                    ("totalWaves", PyRep::Integer(site.total_waves as i64)),
                    ("sleeperRiftStabilized", PyRep::Bool(site.sleeper_rift_stabilized)),
                    ("sleeperSentryDisabled", PyRep::Bool(site.sleeper_sentry_disabled)),
                    ("sleeperGasCloudActive", PyRep::Bool(site.sleeper_gas_cloud_active)),
                    ("cleared", PyRep::Bool(site.cleared)),
                ]))
            });
            Some(res)
        }

        // parity: feat:pve.escalations, feat:pve.expeditions
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails")
        | ("dungeonExplorationMgr", "GetEscalations") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entries = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                reg.escalations_by_char
                    .get(&char_id)
                    .map(|list| {
                        list.iter()
                            .filter(|e| now < e.expires_at_sec)
                            .map(|e| {
                                PyRep::keyval([
                                    ("instanceID", PyRep::Integer(e.instance_id)),
                                    ("sourceDungeonID", PyRep::Integer(e.source_dungeon_id as i64)),
                                    ("dungeonID", PyRep::Integer(e.expedition_dungeon_id as i64)),
                                    ("dungeonName", PyRep::String(e.expedition_name.clone())),
                                    ("destinationSolarSystemID", PyRep::Integer(e.destination_solar_system_id as i64)),
                                    ("stepIndex", PyRep::Integer(e.step_index as i64)),
                                    ("totalSteps", PyRep::Integer(e.total_steps as i64)),
                                    ("journalText", PyRep::String(e.journal_text.clone())),
                                    ("createdAt", PyRep::Integer(e.created_at_sec as i64)),
                                    ("expiresAt", PyRep::Integer(e.expires_at_sec as i64)),
                                ])
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(entries)))
        }

        // parity: feat:pve.escalations, feat:pve.expeditions
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entered = with_pve_exploration_reg(ctx, |reg| {
                reg.escalations_by_char
                    .get(&char_id)
                    .map(|list| {
                        list.iter()
                            .flat_map(|e| e.dungeons_entered.iter().copied())
                            .map(|d| PyRep::Integer(d as i64))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(entered)))
        }

        _ => None,
    }
}

fn format_signature_py(s: &PveSiteInstance) -> PyRep {
    let scanned_100 = s.signal_strength >= 1.0;
    PyRep::keyval([
        ("signatureID", PyRep::String(s.site_id.clone())),
        ("dungeonID", if scanned_100 { PyRep::Integer(s.dungeon_id as i64) } else { PyRep::None }),
        (
            "siteName",
            if scanned_100 {
                PyRep::String(s.dungeon_name.clone())
            } else {
                PyRep::String("Unidentified Cosmic Signature".into())
            },
        ),
        (
            "siteCategory",
            if scanned_100 {
                PyRep::String(s.site_category.clone())
            } else {
                PyRep::String("Unknown".into())
            },
        ),
        (
            "dedRating",
            if scanned_100 {
                s.ded_rating.map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::None)
            } else {
                PyRep::None
            },
        ),
        ("signalStrength", PyRep::Real(s.signal_strength)),
        ("overviewVisible", PyRep::Bool(scanned_100)),
        ("warpable", PyRep::Bool(scanned_100)),
        (
            "hackableContainerIDs",
            if scanned_100 {
                PyRep::List(s.hackable_can_ids.iter().copied().map(PyRep::Integer).collect())
            } else {
                PyRep::List(vec![])
            },
        ),
        ("npcGuardsSpawned", PyRep::Bool(s.npc_guards_spawned)),
    ])
}
