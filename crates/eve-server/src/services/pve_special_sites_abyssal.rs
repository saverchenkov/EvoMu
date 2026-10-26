//! `pve_special_sites_abyssal` — Abyssal Deadspace filaments, pockets, weather, and lifecycle.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::{
    next_spec_seq, spec_now_sec, with_pve_special_reg,
};
use glam::DVec3;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AttributeId, SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

type CharacterId = i64;
type ItemId = i64;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

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

pub async fn ensure_abyssal_room_balls_for_ctx(ctx: &ServerContext, char_id: CharacterId) {
    let info = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id)?;
        if p.completed || p.collapsed_destroyed { return None; }
        let r_idx = (p.current_room.saturating_sub(1) as usize).min(2);
        Some((p.abyssal_solar_system_id, p.pocket_id, p.current_room, p.rooms[r_idx].clone(), (p.tier, p.filament_type_id, p.npc_difficulty_mult)))
    });
    if let Some((sys_id, pid, room_num, room, (tier, filament, diff))) = info {
        let marker_id = 1_895_000_000 + pid * 10 + (room_num as i64);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if matches!(sys_handle.get_ball_snapshot(marker_id).await, Ok(None)) {
            let z = (room_num as f64 - 1.0) * 50_000.0;
            let npcs: &[i64] = if room.cleared { &[] } else { &room.npc_ball_ids };
            spawn_abyssal_room_contents(ctx, &sys_handle, pid, room_num, npcs, room.cache_item_id, tier, filament, diff, z).await;
        }
    }
}

/// Abyssal Spaceship Entities (SDE group 1982): Starving Damavik, Anchoring Damavik, Striking Damavik,
/// Starving Vedmak, Starving Leshak. DERIVED: per-room roster rotation through this list (no CCP spawn table).
pub const ABYSSAL_ROOM_NPC_TYPES: [TypeId; 5] = [48090, 48089, 48092, 48087, 48125];
/// `Triglavian Bioadaptive Cache` (SDE type 47951, group 2009).
pub const BIOADAPTIVE_CACHE_TYPE_ID: TypeId = 47951;
/// `Triglavian Survey Database` (SDE type 48121).
pub const TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID: TypeId = 48121;
/// `Transfer Conduit (Triglavian)` (47685) for rooms 1-2, `Origin Conduit (Triglavian)` (47686) for room 3.
pub fn abyssal_room_gate_type(room: u8) -> TypeId { if room >= 3 { 47686 } else { 47685 } }

/// Spawns one Abyssal room: its exit conduit marker, the room NPCs with their real SDE type and
/// `CombatState` (so they are destroyable through the damage pipeline), and a Triglavian Bioadaptive
/// Cache in space whose loot is persisted in `snap.items` (cache item + contained loot stacks).
#[allow(clippy::too_many_arguments)]
pub async fn spawn_abyssal_room_contents(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    pocket_id: i64,
    room: u8,
    npc_ids: &[i64],
    cache_item_id: ItemId,
    tier: u8,
    filament_type_id: TypeId,
    difficulty_mult: f64,
    z: f64,
) {
    let marker_id = 1_895_000_000 + pocket_id * 10 + (room as i64);
    let _ = sys_handle.spawn_ball(make_site_npc_ball(marker_id, abyssal_room_gate_type(room), DVec3::new(0.0, 0.0, z))).await;
    let mult = difficulty_mult.max(1.0);
    for (idx, &bid) in npc_ids.iter().enumerate() {
        let tid = ABYSSAL_ROOM_NPC_TYPES[(room as usize - 1 + idx) % ABYSSAL_ROOM_NPC_TYPES.len()];
        let pos = DVec3::new((idx as f64) * 3_000.0, 5_000.0, z);
        // Pocket tier difficulty (`AbyssalPocketSession::npc_difficulty_mult`) scales SDE hit points.
        let mut cs = ctx.build_type_combat_state(tid);
        (cs.max_shield_hp, cs.max_armor_hp, cs.max_structure_hp) = (cs.max_shield_hp * mult, cs.max_armor_hp * mult, cs.max_structure_hp * mult);
        (cs.shield_hp, cs.armor_hp, cs.structure_hp) = (cs.max_shield_hp, cs.max_armor_hp, cs.max_structure_hp);
        let _ = sys_handle.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
        register_abyssal_room_npc(ctx, bid, tid, sys_handle.solar_system_id, pocket_id, pos, mult);
    }
    let cache_pos = DVec3::new(0.0, -5_000.0, z);
    let cache_ball = make_site_npc_ball(cache_item_id, BIOADAPTIVE_CACHE_TYPE_ID, cache_pos);
    let _ = sys_handle.spawn_ball_with_combat(cache_ball, ctx.build_type_combat_state(BIOADAPTIVE_CACHE_TYPE_ID)).await;
    let sys_id = sys_handle.solar_system_id as i64;
    ctx.db.with_mut_snapshot(|snap| {
        if snap.items.contains_key(&cache_item_id) {
            return;
        }
        let rec = |item_id, type_id, location_id, quantity, singleton| eve_db::InventoryItemRecord {
            item_id, type_id, owner_id: 500_026, location_id, flag: eve_db::InventoryFlag::None,
            quantity, singleton, version: 1, ..Default::default()
        };
        snap.items.insert(cache_item_id, rec(cache_item_id, BIOADAPTIVE_CACHE_TYPE_ID, sys_id, 1, true));
        snap.item_positions.insert(cache_item_id, [cache_pos.x, cache_pos.y, cache_pos.z]);
        // DERIVED: loot table — (1 + tier) Survey Databases and one filament of the activated type per cache.
        for (type_id, qty) in [(TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, 1 + tier as i32), (filament_type_id, 1)] {
            snap.next_entity_id += 1;
            let loot_id = snap.next_entity_id;
            snap.items.insert(loot_id, rec(loot_id, type_id, cache_item_id, qty, false));
        }
    });
}

/// Registers an Abyssal room NPC with the NPC AI runtime (`rt.npc_entities`) as a Triglavian Collective
/// (`500_026`) combatant so `step_extended_npc_ai_sync` acquires and attacks pilots in the pocket.
fn register_abyssal_room_npc(ctx: &ServerContext, bid: i64, tid: TypeId, sys_id: SolarSystemId, pocket_id: i64, pos: DVec3, mult: f64) {
    let attr = |aid: u16| ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&aid).copied());
    let dmg_mult = attr(64).unwrap_or(1.0).max(1.0) * mult;
    let (em, ex, ki, th) = (attr(114).unwrap_or(0.0), attr(116).unwrap_or(0.0), attr(117).unwrap_or(0.0), attr(118).unwrap_or(0.0));
    let damage_vector = if em + ex + ki + th > 0.0 {
        eve_destiny::DamageVector { em: em * dmg_mult, explosive: ex * dmg_mult, kinetic: ki * dmg_mult, thermal: th * dmg_mult }
    } else {
        // DERIVED: Triglavian exotic-plasma style thermal/explosive profile when the SDE type has no damage attrs.
        eve_destiny::DamageVector { em: 0.0, explosive: 75.0, kinetic: 0.0, thermal: 75.0 }
    };
    let optimal_range = attr(54).unwrap_or(15_000.0).max(5_000.0);
    let entity = crate::services::combat_crimewatch::ExtendedNpcEntity {
        ball_id: bid, type_id: tid, faction_id: 500_026, archetype: "Triglavian".into(), solar_system_id: sys_id,
        spawn_group_id: pocket_id as u32, target_id: None, aggression_threat: HashMap::new(), threat_table: HashMap::new(),
        bounty_isk: attr(481).unwrap_or(0.0), optimal_range, orbit_range: (optimal_range * 0.6).clamp(2_500.0, 20_000.0),
        damage_vector, entropic_cycles: 0, drifter_overshield_hp: 0.0, drifter_doomsday_fired: false,
        web_speed_factor: attr(20).unwrap_or(0.0), web_range: attr(580).unwrap_or(0.0),
        scram_strength: attr(105).unwrap_or(0.0), scram_range: attr(582).unwrap_or(0.0), neut_amount: attr(97).unwrap_or(0.0),
        ecm_jammed: false, remote_shield_rep: 0.0, remote_armor_rep: 0.0, remote_rep_range: 0.0, tag_drop_type_id: None,
        spawn_position: pos, sleeper_wh_class: 0, custom_cargo_loot: None,
    };
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.insert(bid, entity);
    });
}

/// `Triglavian Bioadaptive Cache Wreck` (SDE type 48120).
pub const BIOADAPTIVE_CACHE_WRECK_TYPE_ID: TypeId = 48120;

/// Death-hook handler: when an Abyssal room's Bioadaptive Cache is destroyed, spawn its wreck in space and
/// move the cache's persisted loot (`location_id == cache_id`) into the wreck so the pilot can loot it.
pub async fn on_abyssal_cache_destroyed_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    ball_id: i64,
    killer_char_id: Option<CharacterId>,
) {
    let is_cache = db.with_snapshot(|snap| {
        snap.items.get(&ball_id).is_some_and(|i| i.type_id == BIOADAPTIVE_CACHE_TYPE_ID && i.location_id == solar_system_id as i64)
    });
    if !is_cache {
        return;
    }
    let (wreck_id, pos) = db.with_mut_snapshot(|snap| {
        let pos = snap.item_positions.remove(&ball_id).unwrap_or([0.0; 3]);
        snap.items.remove(&ball_id);
        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;
        snap.items.insert(wreck_id, eve_db::InventoryItemRecord {
            item_id: wreck_id, type_id: BIOADAPTIVE_CACHE_WRECK_TYPE_ID, owner_id: killer_char_id.unwrap_or(500_026),
            location_id: solar_system_id as i64, flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true,
            version: 1, ..Default::default()
        });
        snap.item_positions.insert(wreck_id, pos);
        for it in snap.items.values_mut().filter(|i| i.location_id == ball_id) {
            it.location_id = wreck_id;
            it.version += 1;
        }
        (wreck_id, pos)
    });
    let _ = systems
        .get_or_spawn(solar_system_id)
        .spawn_ball(make_site_npc_ball(wreck_id, BIOADAPTIVE_CACHE_WRECK_TYPE_ID, DVec3::new(pos[0], pos[1], pos[2])))
        .await;
}

pub fn restore_abyssal_weather_dogma_modifiers(
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
    let (pocket_id, room1_npcs, r1_cache) = with_pve_special_reg(ctx, |reg| {
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
        let r1_cache = rooms[0].cache_item_id;
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
        (pid, r1_npcs, r1_cache)
    });
    spawn_abyssal_room_contents(ctx, &sys_handle, pocket_id, 1, &room1_npcs, r1_cache, tier, filament_type_id, diff_mult, 0.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = abyssal_sys_id; ch.active_ship_id = ship_id; }
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

/// Destroys the character's ship, fitted/cargo items, and active implants on Abyssal pocket 20-minute
/// collapse, cleans up the pocket's Bioadaptive Caches, markers, and NPCs, records both ship and
/// capsule (`670`) killmails, and respawns the character in a starter corvette (`606`) at their home
/// station (`feat:pve.abyssal-pocket`, `feat:lifecycle.abyssal-disconnect`, `feat:lifecycle.abyssal-restart`).
pub fn collapse_abyssal_pocket_and_pod_pilot(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    abyssal_sys: SolarSystemId,
) -> SolarSystemId {
    collapse_abyssal_pocket_and_pod_pilot_at(ctx, char_id, ship_id, abyssal_sys, ctx.now_unix())
}

pub fn collapse_abyssal_pocket_and_pod_pilot_at(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    abyssal_sys: SolarSystemId,
    now_unix: u64,
) -> SolarSystemId {
    // 1. Collect pocket ball/cache/NPC IDs inside `with_pve_special_reg` without calling `ctx.systems`
    //    while holding the DB snapshot lock (`Systems -> Ballpark -> Database Snapshot`).
    let pocket_cleanup = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get_mut(&char_id)?;
        p.collapsed_destroyed = true;
        p.completed = true;
        let pid = p.pocket_id;
        let markers = [
            1_895_000_000 + pid * 10 + 1,
            1_895_000_000 + pid * 10 + 2,
            1_895_000_000 + pid * 10 + 3,
        ];
        let caches = [
            1_896_000_000 + pid * 10 + 1,
            1_896_000_000 + pid * 10 + 2,
            1_896_000_000 + pid * 10 + 3,
        ];
        let npcs: Vec<i64> = p
            .rooms
            .iter()
            .flat_map(|r| r.npc_ball_ids.iter().copied())
            .collect();
        Some((p.abyssal_solar_system_id, markers, caches, npcs))
    });

    // 2. Despawn ship, room markers, Bioadaptive Caches, and NPCs outside any DB snapshot lock.
    let eff_pocket_sys = pocket_cleanup.as_ref().map(|c| c.0).unwrap_or(abyssal_sys);
    let handle = ctx.systems.get_or_spawn(eff_pocket_sys);
    handle.try_remove_ball(ship_id);
    if let Some((_, ref markers, ref caches, ref npcs)) = pocket_cleanup {
        for &mid in markers {
            handle.try_remove_ball(mid);
        }
        for &cid in caches {
            handle.try_remove_ball(cid);
        }
        for &nid in npcs {
            handle.try_remove_ball(nid);
        }
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for nid in npcs {
                rt.npc_entities.remove(nid);
            }
        });
    }
    ctx.invalidate_ship_fitting(ship_id);

    // 3. Remove Bioadaptive Cache items + child loot, destroy ship + modules/cargo + active implants,
    //    record ship & capsule (670) killmails, and respawn pilot in a starter corvette (606) at home station.
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, snap| {
        st.abyssal_sessions.remove(&char_id);
        if let Some((_, _, ref caches, _)) = pocket_cleanup {
            for &cid in caches {
                snap.items.remove(&cid);
                snap.item_positions.remove(&cid);
            }
            snap.items.retain(|_, item| !caches.contains(&item.location_id));
        }

        let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
        let base_price = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| t.base_price)
            .unwrap_or(250_000.0);

        snap.items.remove(&ship_id);
        snap.item_positions.remove(&ship_id);
        snap.module_online_states.remove(&ship_id);
        st.ship_combat_hp.remove(&ship_id);
        snap.items.retain(|_, item| {
            item.location_id != ship_id
                && !(item.owner_id == char_id && item.flag == eve_db::InventoryFlag::Implant)
        });
        snap.active_implants.remove(&char_id);

        let home_station_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.home_station_id)
            .unwrap_or(60_003_760);
        let home_sys_id = ctx
            .sde
            .stations
            .get(&home_station_id)
            .map(|s| s.solar_system_id)
            .unwrap_or(30_000_142);
        let (const_id, reg_id) = ctx
            .sde
            .solar_systems
            .get(&home_sys_id)
            .map(|s| (s.constellation_id, s.region_id))
            .unwrap_or((20_000_020, 10_000_002));

        snap.next_entity_id += 1;
        let corvette_id = snap.next_entity_id;
        snap.items.insert(
            corvette_id,
            eve_db::InventoryItemRecord {
                item_id: corvette_id,
                type_id: 606, // Velator rookie corvette
                owner_id: char_id,
                location_id: home_station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = corvette_id;
            ch.station_id = Some(home_station_id);
            ch.structure_id = None;
            ch.solar_system_id = home_sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.version += 1;
            ch.system_epoch += 1;
        }

        snap.next_entity_id += 1;
        let ship_kid = snap.next_entity_id;
        snap.record_killmail(
            eve_db::KillmailRecord {
                kill_id: ship_kid,
                victim_character_id: char_id,
                attacker_character_id: 0,
                attacker_corp_id: 1_000_298,
                solar_system_id: eff_pocket_sys,
                ship_type_id,
                total_value_isk: base_price,
                ..Default::default()
            },
            now_unix,
        );
        snap.next_entity_id += 1;
        let pod_kid = snap.next_entity_id;
        snap.record_killmail(
            eve_db::KillmailRecord {
                kill_id: pod_kid,
                victim_character_id: char_id,
                attacker_character_id: 0,
                attacker_corp_id: 1_000_298,
                solar_system_id: eff_pocket_sys,
                ship_type_id: 670, // Capsule
                total_value_isk: 1_000.0,
                ..Default::default()
            },
            now_unix,
        );
        home_sys_id
    })
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

pub enum AbyssalLoginAction {
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
