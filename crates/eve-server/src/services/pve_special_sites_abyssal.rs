//! `pve_special_sites_abyssal` — Abyssal Deadspace filaments, pockets, room spawning,
//! proactive conduit unlock on NPC clear, and 20-minute collapse lifecycle.

#![forbid(unsafe_code)]

pub use crate::services::pve_abyssal_hazards::*;

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::{
    next_spec_seq, spec_now_sec, with_pve_special_reg, with_pve_special_reg_db,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AttributeId, SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

type CharacterId = i64;
type ItemId = i64;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbyssalRoomRecord {
    pub room_index: u8,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub npc_ball_ids: Vec<i64>,
    pub cache_item_id: ItemId,
    pub cleared: bool,
    #[serde(default)]
    pub gate_unlocked_at_sec: Option<u64>,
}

fn default_one() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbyssalPocketSession {
    pub pocket_id: i64,
    pub character_id: CharacterId,
    pub ship_id: ItemId,
    pub origin_solar_system_id: SolarSystemId,
    pub abyssal_solar_system_id: SolarSystemId,
    #[serde(default)]
    pub origin_position: [f64; 3],
    #[serde(default)]
    pub grid_offset: [f64; 3],
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
    #[serde(default)]
    pub trace_ball_id: i64,
    #[serde(default)]
    pub fleet_ship_ids: Vec<ItemId>,
    #[serde(default)]
    pub ship_room: HashMap<ItemId, u8>,
    #[serde(default = "default_one")]
    pub max_ships: u8,
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
    #[serde(default)]
    pub max_ships: u8,
    #[serde(default)]
    pub entered_char_ids: Vec<CharacterId>,
    #[serde(default)]
    pub pocket_id: Option<i64>,
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

pub fn pocket_grid_offset(ctx: &ServerContext, pocket_id: i64) -> [f64; 3] {
    with_pve_special_reg(ctx, |reg| {
        reg.abyssal_pockets
            .values()
            .find(|p| p.pocket_id == pocket_id)
            .map(|p| p.grid_offset)
            .unwrap_or([0.0, 0.0, 0.0])
    })
}

pub async fn ensure_abyssal_room_balls_for_ctx(ctx: &ServerContext, char_id: CharacterId) {
    let info = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id)?;
        if p.completed || p.collapsed_destroyed {
            return None;
        }
        let r_idx = (p.current_room.saturating_sub(1) as usize).min(2);
        Some((
            p.abyssal_solar_system_id,
            p.pocket_id,
            p.current_room,
            p.rooms[r_idx].clone(),
            (p.tier, p.filament_type_id, p.npc_difficulty_mult, p.max_ships.max(1)),
        ))
    });
    if let Some((sys_id, pid, room_num, room, (tier, filament, diff, max_ships))) = info {
        let marker_id = 1_895_000_000 + pid * 10 + (room_num as i64);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if matches!(sys_handle.get_ball_snapshot(marker_id).await, Ok(None)) {
            let z = (room_num as f64 - 1.0) * 50_000.0;
            let npcs: &[i64] = if room.cleared { &[] } else { &room.npc_ball_ids };
            spawn_abyssal_room_contents(
                ctx,
                &sys_handle,
                pid,
                room_num,
                npcs,
                room.cache_item_id,
                tier,
                filament,
                diff,
                z,
                max_ships,
            )
            .await;
        }
    }
}

pub const ABYSSAL_ROOM_NPC_TYPES: [TypeId; 5] = [48090, 48089, 48092, 48087, 48125];
pub const UNENTERED_GATE_LIFETIME_SEC: u64 = 1_800;
pub const BIOADAPTIVE_CACHE_TYPE_ID: TypeId = 47951;
pub const EXTRACTION_SUBNODE_TYPE_ID: TypeId = 49662;
pub const TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID: TypeId = 48121;
pub const ZERO_POINT_CONDENSATE_TYPE_ID: TypeId = 48112;
pub const BIOADAPTIVE_CACHE_WRECK_TYPE_ID: TypeId = 48120;

pub fn abyssal_mutaplasmid_for_tier(tier: u8) -> TypeId {
    match tier {
        0..=2 => 47_699,
        3..=4 => 47_701,
        _ => 47_700,
    }
}

pub fn abyssal_room_gate_type(room: u8) -> TypeId {
    if room >= 3 {
        47686
    } else {
        47685
    }
}

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
    max_ships: u8,
) {
    let base = DVec3::from_array(pocket_grid_offset(ctx, pocket_id));
    let marker_id = 1_895_000_000 + pocket_id * 10 + (room as i64);
    let mut conduit_cs = ctx.build_type_combat_state(abyssal_room_gate_type(room));
    conduit_cs.undock_invulnerable_remaining_ticks = 999_999;
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_site_npc_ball(
                marker_id,
                abyssal_room_gate_type(room),
                base + DVec3::new(0.0, 0.0, z),
            ),
            conduit_cs,
        )
        .await;
    let _ = sys_handle
        .emit_destiny_event(
            marker_id,
            eve_destiny::DestinyEvent::SetBallInteractive {
                ball_id: marker_id,
                is_interactive: npc_ids.is_empty(),
            },
        )
        .await;

    let weather = classify_abyssal_filament(filament_type_id)
        .map(|(_, w)| w)
        .unwrap_or(AbyssalWeatherKind::Electrical);
    let penalty_pct = with_pve_special_reg(ctx, |reg| {
        reg.abyssal_pockets
            .values()
            .find(|p| p.pocket_id == pocket_id)
            .map(|p| p.weather_penalty_pct)
    })
    .unwrap_or_else(|| roll_abyssal_weather_penalty_pct(ctx, tier));
    let bonus_pct = 50.0;
    let mult = difficulty_mult.max(1.0);
    for (idx, &bid) in npc_ids.iter().enumerate() {
        let tid = ABYSSAL_ROOM_NPC_TYPES[(room as usize - 1 + idx) % ABYSSAL_ROOM_NPC_TYPES.len()];
        let pos = base + DVec3::new((idx as f64) * 3_000.0, 5_000.0, z);
        let mut cs = ctx.build_type_combat_state(tid);
        (cs.max_shield_hp, cs.max_armor_hp, cs.max_structure_hp) = (
            cs.max_shield_hp * mult,
            cs.max_armor_hp * mult,
            cs.max_structure_hp * mult,
        );
        (cs.shield_hp, cs.armor_hp, cs.structure_hp) =
            (cs.max_shield_hp, cs.max_armor_hp, cs.max_structure_hp);
        let mut ball = make_site_npc_ball(bid, tid, pos);
        ball.max_velocity = ctx
            .sde
            .types
            .get(&tid)
            .and_then(|t| t.default_attributes.get(&37).copied())
            .filter(|&v| v > 0.0)
            .unwrap_or(380.0);
        let mut opt_range = ctx
            .sde
            .types
            .get(&tid)
            .and_then(|t| t.default_attributes.get(&54).copied())
            .filter(|&r| r > 0.0)
            .unwrap_or(15_000.0)
            .max(5_000.0);
        apply_abyssal_weather_to_combat_and_nav(
            weather,
            penalty_pct,
            bonus_pct,
            &mut cs,
            &mut ball.max_velocity,
            &mut opt_range,
        );
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
        register_abyssal_room_npc(
            ctx,
            bid,
            tid,
            sys_handle.solar_system_id,
            pocket_id,
            pos,
            mult,
            opt_range,
        );
    }

    let cloud_base = 1_898_000_000 + pocket_id * 100 + (room as i64) * 10;
    for (offset_id, cloud_tid, cpos) in [
        (1_i64, TACHYON_CLOUD_TYPE_ID, base + DVec3::new(15_000.0, 0.0, z)),
        (2_i64, BIOLUMINESCENT_CLOUD_TYPE_ID, base + DVec3::new(-15_000.0, 0.0, z)),
        (3_i64, FILAMENT_CLOUD_TYPE_ID, base + DVec3::new(0.0, 15_000.0, z)),
    ] {
        let mut cball = make_site_npc_ball(cloud_base + offset_id, cloud_tid, cpos);
        cball.radius = spatial_cloud_radius_m(Some(&ctx.sde), cloud_tid, SPATIAL_CLOUD_RADIUS_M);
        let _ = sys_handle.spawn_ball(cball).await;
    }

    let pylon_base = 1_899_000_000 + pocket_id * 100 + (room as i64) * 10;
    for (offset_id, pylon_tid, def_range, ppos) in [
        (1_i64, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 15_000.0, base + DVec3::new(20_000.0, 20_000.0, z)),
        (2_i64, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 40_000.0, base + DVec3::new(-20_000.0, 20_000.0, z)),
        (3_i64, AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 40_000.0, base + DVec3::new(20_000.0, -20_000.0, z)),
        (4_i64, MULTIBODY_TRACKING_PYLON_TYPE_ID, 15_000.0, base + DVec3::new(-20_000.0, -20_000.0, z)),
        (5_i64, MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 40_000.0, base + DVec3::new(-30_000.0, -30_000.0, z)),
    ] {
        let mut pball = make_site_npc_ball(pylon_base + offset_id, pylon_tid, ppos);
        pball.radius = ctx
            .sde
            .types
            .get(&pylon_tid)
            .and_then(|t| t.default_attributes.get(&54).copied())
            .filter(|&r| r > 0.0)
            .unwrap_or(def_range);
        let _ = sys_handle
            .spawn_ball_with_combat(pball, ctx.build_type_combat_state(pylon_tid))
            .await;
    }

    let cache_pos = base + DVec3::new(0.0, -5_000.0, z);
    let cache_ball = make_site_npc_ball(cache_item_id, BIOADAPTIVE_CACHE_TYPE_ID, cache_pos);
    let cache_cs = ctx.build_type_combat_state(BIOADAPTIVE_CACHE_TYPE_ID);
    let _ = sys_handle.spawn_ball_with_combat(cache_ball, cache_cs.clone()).await;

    let subnode_id = cache_item_id + 500_000;
    let subnode_pos = base + DVec3::new(5_000.0, -5_000.0, z);
    if tier >= 1 {
        let subnode_ball = make_site_npc_ball(subnode_id, EXTRACTION_SUBNODE_TYPE_ID, subnode_pos);
        let _ = sys_handle.spawn_ball_with_combat(subnode_ball, cache_cs).await;
    }

    let sys_id = sys_handle.solar_system_id as i64;
    let mutaplasmid_tid = abyssal_mutaplasmid_for_tier(tier);
    let condensate_qty = 5 + (tier as i32) * 5;
    let loot_mult = i32::from(max_ships.max(1));
    let subnode_has_loot = tier >= 1 && ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5) >= 0.35;
    ctx.db.with_mut_snapshot(|snap| {
        if snap.items.contains_key(&cache_item_id) {
            return;
        }
        let rec = |item_id, type_id, location_id, quantity, singleton| eve_db::InventoryItemRecord {
            item_id,
            type_id,
            owner_id: 500_026,
            location_id,
            flag: eve_db::InventoryFlag::None,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(cache_item_id, rec(cache_item_id, BIOADAPTIVE_CACHE_TYPE_ID, sys_id, 1, true));
        snap.item_positions.insert(cache_item_id, [cache_pos.x, cache_pos.y, cache_pos.z]);
        for (type_id, qty) in [
            (TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, (1 + tier as i32) * loot_mult),
            (filament_type_id, loot_mult),
            (mutaplasmid_tid, loot_mult),
            (ZERO_POINT_CONDENSATE_TYPE_ID, condensate_qty * loot_mult),
        ] {
            snap.next_entity_id += 1;
            let loot_id = snap.next_entity_id;
            snap.items.insert(loot_id, rec(loot_id, type_id, cache_item_id, qty, false));
        }

        if tier >= 1 {
            snap.items.insert(subnode_id, rec(subnode_id, EXTRACTION_SUBNODE_TYPE_ID, sys_id, 1, true));
            snap.item_positions.insert(subnode_id, [subnode_pos.x, subnode_pos.y, subnode_pos.z]);
            if subnode_has_loot {
                for (type_id, qty) in [
                    (TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, (1 + tier as i32) * loot_mult),
                    (ZERO_POINT_CONDENSATE_TYPE_ID, (condensate_qty / 2).max(1) * loot_mult),
                ] {
                    snap.next_entity_id += 1;
                    let loot_id = snap.next_entity_id;
                    snap.items.insert(loot_id, rec(loot_id, type_id, subnode_id, qty, false));
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn register_abyssal_room_npc(
    ctx: &ServerContext,
    bid: i64,
    tid: TypeId,
    sys_id: SolarSystemId,
    pocket_id: i64,
    pos: DVec3,
    mult: f64,
    optimal_range: f64,
) {
    let attr = |aid: u16| ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&aid).copied());
    let dmg_mult = attr(64).unwrap_or(1.0).max(1.0) * mult;
    let (em, ex, ki, th) = (
        attr(114).unwrap_or(0.0),
        attr(116).unwrap_or(0.0),
        attr(117).unwrap_or(0.0),
        attr(118).unwrap_or(0.0),
    );
    let damage_vector = if em + ex + ki + th > 0.0 {
        eve_destiny::DamageVector {
            em: em * dmg_mult,
            explosive: ex * dmg_mult,
            kinetic: ki * dmg_mult,
            thermal: th * dmg_mult,
        }
    } else {
        eve_destiny::DamageVector {
            em: 0.0,
            explosive: 75.0,
            kinetic: 0.0,
            thermal: 75.0,
        }
    };
    let entity = crate::services::combat_crimewatch::ExtendedNpcEntity {
        ball_id: bid,
        type_id: tid,
        faction_id: 500_026,
        archetype: "Triglavian".into(),
        solar_system_id: sys_id,
        spawn_group_id: pocket_id as u32,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: attr(481).unwrap_or(0.0),
        optimal_range,
        orbit_range: (optimal_range * 0.6).clamp(2_500.0, 20_000.0),
        damage_vector,
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: attr(20).unwrap_or(0.0),
        web_range: attr(580).unwrap_or(0.0),
        scram_strength: attr(105).unwrap_or(0.0),
        scram_range: attr(582).unwrap_or(0.0),
        neut_amount: attr(97).unwrap_or(0.0),
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: pos,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    };
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.insert(bid, entity);
    });
}

/// Proactively unlocks the Abyssal room conduit immediately when the final room NPC is destroyed on a ballpark tick.
pub fn on_abyssal_room_npc_destroyed_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
) -> bool {
    if !(32_000_000..=32_999_999).contains(&solar_system_id) {
        return false;
    }
    let now_sec = db.clock().now_unix_secs();
    let unlocked = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut target: Option<(i64, usize)> = None;
        for p in reg.abyssal_pockets.values().filter(|p| !p.completed && !p.collapsed_destroyed) {
            for (r_idx, r) in p.rooms.iter().enumerate() {
                if !r.cleared && r.npc_ball_ids.contains(&npc_ball_id) {
                    let all_gone = r
                        .npc_ball_ids
                        .iter()
                        .all(|&bid| bid == npc_ball_id || !ballpark.balls.contains_key(&bid));
                    if all_gone {
                        target = Some((p.pocket_id, r_idx));
                        break;
                    }
                }
            }
            if target.is_some() {
                break;
            }
        }
        let Some((pid, r_idx)) = target else {
            return None;
        };
        for p in reg.abyssal_pockets.values_mut().filter(|p| p.pocket_id == pid) {
            p.rooms[r_idx].cleared = true;
            p.rooms[r_idx].gate_unlocked_at_sec = Some(now_sec);
        }
        Some(1_895_000_000 + pid * 10 + (r_idx as i64 + 1))
    });
    if let Some(gate_id) = unlocked {
        ballpark.set_ball_interactive(gate_id, true);
        ballpark.emit_client_controller_event(gate_id, None, "GateActivate", 1, None);
        return true;
    }
    false
}

/// Proactively unlocks the Abyssal room conduit immediately when the final room NPC is destroyed via async damage/death hooks.
pub async fn on_abyssal_room_npc_destroyed_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
) -> bool {
    if !(32_000_000..=32_999_999).contains(&solar_system_id) {
        return false;
    }
    let now_sec = db.clock().now_unix_secs();
    let cand = with_pve_special_reg_db(db, now_sec, |reg| {
        for p in reg.abyssal_pockets.values().filter(|p| !p.completed && !p.collapsed_destroyed) {
            for (r_idx, r) in p.rooms.iter().enumerate() {
                if !r.cleared && r.npc_ball_ids.contains(&npc_ball_id) {
                    return Some((p.pocket_id, r_idx, r.npc_ball_ids.clone()));
                }
            }
        }
        None
    });
    let Some((pid, r_idx, npc_ids)) = cand else {
        return false;
    };
    let sys_handle = systems.get_or_spawn(solar_system_id);
    for &bid in &npc_ids {
        if bid != npc_ball_id && sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
            return false;
        }
    }
    let newly_unlocked = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut changed = false;
        for p in reg.abyssal_pockets.values_mut().filter(|p| p.pocket_id == pid) {
            if !p.rooms[r_idx].cleared {
                p.rooms[r_idx].cleared = true;
                p.rooms[r_idx].gate_unlocked_at_sec = Some(now_sec);
                changed = true;
            }
        }
        changed
    });
    if newly_unlocked {
        let gate_id = 1_895_000_000 + pid * 10 + (r_idx as i64 + 1);
        let _ = sys_handle
            .emit_destiny_event(
                gate_id,
                eve_destiny::DestinyEvent::SetBallInteractive {
                    ball_id: gate_id,
                    is_interactive: true,
                },
            )
            .await;
        let _ = sys_handle
            .emit_destiny_event(
                gate_id,
                eve_destiny::DestinyEvent::OnClientControllerEvent {
                    ball_id: gate_id,
                    target_id: None,
                    event_name: "GateActivate".into(),
                    value: 1,
                    delay: None,
                },
            )
            .await;
    }
    newly_unlocked
}

pub async fn on_abyssal_cache_destroyed_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    ball_id: i64,
    killer_char_id: Option<CharacterId>,
) {
    let is_cache = db.with_snapshot(|snap| {
        snap.items.get(&ball_id).is_some_and(|i| {
            matches!(i.type_id, BIOADAPTIVE_CACHE_TYPE_ID | EXTRACTION_SUBNODE_TYPE_ID | 49_661)
                && i.location_id == solar_system_id as i64
        })
    });
    if !is_cache {
        return;
    }
    let (wreck_id, pos) = db.with_mut_snapshot(|snap| {
        let pos = snap.item_positions.remove(&ball_id).unwrap_or([0.0; 3]);
        snap.items.remove(&ball_id);
        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;
        snap.items.insert(
            wreck_id,
            eve_db::InventoryItemRecord {
                item_id: wreck_id,
                type_id: BIOADAPTIVE_CACHE_WRECK_TYPE_ID,
                owner_id: killer_char_id.unwrap_or(500_026),
                location_id: solar_system_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(wreck_id, pos);
        for it in snap.items.values_mut().filter(|i| i.location_id == ball_id) {
            it.location_id = wreck_id;
            it.version += 1;
        }
        (wreck_id, pos)
    });
    let _ = systems
        .get_or_spawn(solar_system_id)
        .spawn_ball(make_site_npc_ball(
            wreck_id,
            BIOADAPTIVE_CACHE_WRECK_TYPE_ID,
            DVec3::new(pos[0], pos[1], pos[2]),
        ))
        .await;
}

pub async fn enter_abyssal_pocket_for_character(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: CharacterId,
    filament_type_id: TypeId,
    tier: u8,
    origin_sys: SolarSystemId,
) -> (i64, Vec<i64>) {
    enter_abyssal_pocket_with_trace(ctx, session, char_id, filament_type_id, tier, origin_sys, None)
        .await
}

pub async fn enter_abyssal_pocket_with_trace(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: CharacterId,
    filament_type_id: TypeId,
    tier: u8,
    origin_sys: SolarSystemId,
    trace_ball_id: Option<i64>,
) -> (i64, Vec<i64>) {
    let ship_id = session.ship_id.unwrap_or(100_000_002);
    let ship_max_ships = validate_abyssal_hull_and_capacity(ctx, ship_id)
        .map(|(_, _, m)| m)
        .unwrap_or(1);
    let weather = classify_abyssal_filament(filament_type_id)
        .map(|(_, w)| w)
        .unwrap_or(AbyssalWeatherKind::Electrical);
    let (existing_penalty, trace_max_ships) = with_pve_special_reg(ctx, |reg| {
        let trace_opt = if let Some(tid) = trace_ball_id {
            reg.abyssal_traces.get(&tid)
        } else {
            reg.abyssal_traces
                .values()
                .filter(|t| t.entered_char_ids.contains(&char_id))
                .max_by_key(|t| t.trace_ball_id)
        };
        let pen = trace_opt
            .and_then(|t| t.pocket_id)
            .and_then(|pid| {
                reg.abyssal_pockets
                    .values()
                    .find(|p| p.pocket_id == pid && !p.completed && !p.collapsed_destroyed)
                    .map(|p| p.weather_penalty_pct)
            });
        let tmax = trace_opt.map(|t| t.max_ships).unwrap_or(0);
        (pen, tmax)
    });
    let eff_max_ships = ship_max_ships.max(trace_max_ships).max(1);
    let penalty_pct = existing_penalty.unwrap_or_else(|| roll_abyssal_weather_penalty_pct(ctx, tier));
    let bonus_pct = 50.0;
    let diff_mult = 1.0 + (tier as f64) * 1.25;
    let saved_attrs = apply_abyssal_weather_dogma_modifiers_to_ship(
        ctx,
        ship_id,
        weather,
        penalty_pct,
        bonus_pct,
    );
    let origin_pos = ctx.db.with_snapshot(|snap| {
        snap.item_positions
            .get(&ship_id)
            .copied()
            .unwrap_or([145_000.0, -75_000.0, 42_000.0])
    });
    let abyssal_sys_id: SolarSystemId = 32_000_001;
    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys_id);
    let (pocket_id, room1_npcs, r1_cache, already_spawned, started_at, collapse_at, grid_offset) =
        with_pve_special_reg(ctx, |reg| {
            let target_pid = if let Some(tid) = trace_ball_id {
                reg.abyssal_traces.get(&tid).and_then(|t| t.pocket_id)
            } else {
                reg.abyssal_traces
                    .values()
                    .filter(|t| t.entered_char_ids.contains(&char_id))
                    .max_by_key(|t| t.trace_ball_id)
                    .and_then(|t| t.pocket_id)
            };
            if let Some(existing) = target_pid.and_then(|pid| {
                reg.abyssal_pockets
                    .values()
                    .find(|p| p.pocket_id == pid && !p.completed && !p.collapsed_destroyed)
                    .cloned()
            })
            {
                let pid = existing.pocket_id;
                let r1_npcs = existing.rooms[0].npc_ball_ids.clone();
                let r1_cache = existing.rooms[0].cache_item_id;
                let started = existing.started_at_sec;
                let collapse = existing.collapse_at_sec;
                let goffset = existing.grid_offset;
                let mut joined = existing;
                joined.character_id = char_id;
                joined.ship_id = ship_id;
                joined.current_room = 1;
                joined.origin_position = origin_pos;
                joined.saved_ship_attrs = saved_attrs.clone();
                if let Some(tid) = trace_ball_id {
                    joined.trace_ball_id = tid;
                }
                if !joined.fleet_ship_ids.contains(&ship_id) {
                    joined.fleet_ship_ids.push(ship_id);
                }
                joined.ship_room.insert(ship_id, 1);
                for p in reg.abyssal_pockets.values_mut().filter(|p| p.pocket_id == pid) {
                    if !p.fleet_ship_ids.contains(&ship_id) {
                        p.fleet_ship_ids.push(ship_id);
                    }
                    p.ship_room.insert(ship_id, 1);
                }
                reg.abyssal_pockets.insert(char_id, joined);
                return (pid, r1_npcs, r1_cache, true, started, collapse, goffset);
            }

            let mut used_slots: HashSet<i64> = HashSet::new();
            for active_p in reg
                .abyssal_pockets
                .values()
                .filter(|p| !p.completed && !p.collapsed_destroyed)
            {
                let slot = (active_p.grid_offset[1] / 10_000_000.0).round() as i64;
                used_slots.insert(slot);
            }
            let slot = (0_i64..10_000).find(|s| !used_slots.contains(s)).unwrap_or(0);
            let goffset = [0.0, (slot as f64) * 10_000_000.0, 0.0];

            let pid = 600_000 + next_spec_seq(reg);
            let now = ctx.now_unix();
            let collapse = now + 1_200;
            let mut rooms_vec = Vec::with_capacity(3);
            for (r_idx, did, r_name) in [
                (1_u8, 5810_u32, "Canyon"),
                (2, 6047, "Rise"),
                (3, 6048, "Forest Opening"),
            ] {
                let npc_count = (2 + (tier as usize / 2)).clamp(2, 5);
                let npc_ids: Vec<i64> = (0..npc_count)
                    .map(|n| pid * 100 + (r_idx as i64) * 10 + (n as i64) + 1)
                    .collect();
                rooms_vec.push(AbyssalRoomRecord {
                    room_index: r_idx,
                    dungeon_id: did,
                    dungeon_name: r_name.into(),
                    npc_ball_ids: npc_ids,
                    cache_item_id: 1_896_000_000 + pid * 10 + (r_idx as i64),
                    cleared: false,
                    gate_unlocked_at_sec: None,
                });
            }
            let rooms: [AbyssalRoomRecord; 3] = rooms_vec.try_into().unwrap();
            let r1_npcs = rooms[0].npc_ball_ids.clone();
            let r1_cache = rooms[0].cache_item_id;
            let trace_id = if let Some(tid) = trace_ball_id {
                if let Some(t) = reg.abyssal_traces.get_mut(&tid) {
                    t.pocket_id = Some(pid);
                    t.expires_at_sec = collapse;
                }
                tid
            } else {
                reg.abyssal_traces
                    .values_mut()
                    .filter(|t| t.pocket_id.is_none() && t.entered_char_ids.contains(&char_id))
                    .max_by_key(|t| t.trace_ball_id)
                    .map(|t| {
                        t.pocket_id = Some(pid);
                        t.expires_at_sec = collapse;
                        t.trace_ball_id
                    })
                    .unwrap_or(0)
            };
            reg.abyssal_pockets.insert(
                char_id,
                AbyssalPocketSession {
                    pocket_id: pid,
                    character_id: char_id,
                    ship_id,
                    origin_solar_system_id: origin_sys,
                    origin_position: origin_pos,
                    grid_offset: goffset,
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
                    trace_ball_id: trace_id,
                    fleet_ship_ids: vec![ship_id],
                    ship_room: HashMap::from([(ship_id, 1)]),
                    max_ships: eff_max_ships,
                },
            );
            (pid, r1_npcs, r1_cache, false, now, collapse, goffset)
        });
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        abyssal_sys_id,
        origin_sys,
        started_at,
        collapse_at,
    );
    if !already_spawned {
        spawn_abyssal_room_contents(
            ctx,
            &sys_handle,
            pocket_id,
            1,
            &room1_npcs,
            r1_cache,
            tier,
            filament_type_id,
            diff_mult,
            0.0,
            eff_max_ships,
        )
        .await;
    } else {
        let r1_gate = 1_895_000_000 + pocket_id * 10 + 1;
        if sys_handle.get_ball_snapshot(r1_gate).await.ok().flatten().is_none() {
            let mut conduit_cs = ctx.build_type_combat_state(abyssal_room_gate_type(1));
            conduit_cs.undock_invulnerable_remaining_ticks = 999_999;
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(
                        r1_gate,
                        abyssal_room_gate_type(1),
                        DVec3::from_array(grid_offset),
                    ),
                    conduit_cs,
                )
                .await;
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = abyssal_sys_id;
            ch.active_ship_id = ship_id;
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = abyssal_sys_id as i64;
        }
        let old_chan_id = format!("solarsystemid2:{origin_sys}");
        if let Some(chan) = snap.chat_channels.get_mut(&old_chan_id) {
            chan.members.remove(&char_id);
        }
        snap.chat_channels.remove(&format!("solarsystemid2:{abyssal_sys_id}"));
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
    let pocket_cleanup = with_pve_special_reg(ctx, |reg| {
        let (pid, pocket_sys, origin_sys, bound_tid, npcs) = {
            let p = reg.abyssal_pockets.get_mut(&char_id)?;
            p.collapsed_destroyed = true;
            p.completed = true;
            let npcs: Vec<i64> = p
                .rooms
                .iter()
                .flat_map(|r| r.npc_ball_ids.iter().copied())
                .collect();
            (
                p.pocket_id,
                p.abyssal_solar_system_id,
                p.origin_solar_system_id,
                p.trace_ball_id,
                npcs,
            )
        };
        let any_other_alive = reg
            .abyssal_pockets
            .values()
            .any(|other| other.pocket_id == pid && !other.completed && !other.collapsed_destroyed);
        let mut trace_ids = Vec::new();
        if !any_other_alive {
            if bound_tid > 0 {
                trace_ids.push(bound_tid);
            }
            reg.abyssal_traces.retain(|&tid, t| {
                if t.pocket_id == Some(pid) || (bound_tid > 0 && tid == bound_tid) {
                    if !trace_ids.contains(&tid) {
                        trace_ids.push(tid);
                    }
                    false
                } else {
                    true
                }
            });
        }
        let markers = [
            1_895_000_000 + pid * 10 + 1,
            1_895_000_000 + pid * 10 + 2,
            1_895_000_000 + pid * 10 + 3,
        ];
        let caches = [
            1_896_000_000 + pid * 10 + 1,
            1_896_000_000 + pid * 10 + 2,
            1_896_000_000 + pid * 10 + 3,
            1_896_500_000 + pid * 10 + 1,
            1_896_500_000 + pid * 10 + 2,
            1_896_500_000 + pid * 10 + 3,
        ];
        let mut env_balls = Vec::new();
        for r in 1..=3_i64 {
            for c in 1..=3_i64 {
                env_balls.push(1_898_000_000 + pid * 100 + r * 10 + c);
            }
            for py in 1..=5_i64 {
                env_balls.push(1_899_000_000 + pid * 100 + r * 10 + py);
            }
        }
        Some((
            pocket_sys,
            origin_sys,
            any_other_alive,
            trace_ids,
            markers,
            caches,
            env_balls,
            npcs,
        ))
    });

    let eff_pocket_sys = pocket_cleanup.as_ref().map(|c| c.0).unwrap_or(abyssal_sys);
    let handle = ctx.systems.get_or_spawn(eff_pocket_sys);
    handle.try_remove_ball(ship_id);
    if let Some((_, origin_sys, any_other_alive, ref trace_ids, ref markers, ref caches, ref env_balls, ref npcs)) =
        pocket_cleanup
    {
        let origin_handle = ctx.systems.get_or_spawn(origin_sys);
        for &tid in trace_ids {
            origin_handle.try_remove_ball(tid);
        }
        if !any_other_alive {
            for &mid in markers {
                handle.try_remove_ball(mid);
            }
            for &cid in caches {
                handle.try_remove_ball(cid);
            }
            for &eid in env_balls {
                handle.try_remove_ball(eid);
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
    }
    ctx.invalidate_ship_fitting(ship_id);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.remove(&ship_id);
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
        st.insurance_contracts.remove(&ship_id);
    });

    let (home_sys_id, _home_station_id, _corvette_id, cleared_zarzakh_gate) =
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, snap| {
            st.abyssal_sessions.remove(&char_id);
            if let Some((_, _, false, _, _, ref caches, _, _)) = pocket_cleanup {
                for &cid in caches {
                    snap.items.remove(&cid);
                    snap.item_positions.remove(&cid);
                }
                snap.items.retain(|_, item| !caches.contains(&item.location_id));
            }

            let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
            let death_pos = snap.item_positions.remove(&ship_id).unwrap_or([0.0, 0.0, 0.0]);
            let base_price = ctx
                .sde
                .types
                .get(&ship_type_id)
                .map(|t| t.base_price)
                .unwrap_or(250_000.0);

            snap.items.remove(&ship_id);
            snap.module_online_states.remove(&ship_id);
            st.ship_combat_hp.remove(&ship_id);
            snap.items.retain(|_, item| item.location_id != ship_id);
            let cleared_lock = snap.zarzakh_gate_locks.remove(&char_id).map(|(gid, _)| gid);

            if ship_type_id != 670 {
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
            }
            let (home_station_id, home_sys_id, corvette_id, _corpse_id, _pod_kid) =
                destroy_pod_and_respawn_faction_corvette_in_snap(
                    snap,
                    Some(&ctx.sde),
                    char_id,
                    eff_pocket_sys,
                    death_pos,
                    now_unix,
                );
            (home_sys_id, home_station_id, corvette_id, cleared_lock)
        });

    if let Some(locked_gate_id) = cleared_zarzakh_gate {
        crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
            &ctx.quasar_gateway,
            char_id,
            30_100_000,
            locked_gate_id,
            now_unix,
        );
    }
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        home_sys_id,
        0,
        "podding",
    );
    crate::services::map_service::record_solar_system_visit(ctx, char_id, home_sys_id);
    home_sys_id
}

pub fn evacuate_abyssal_pockets_for_downtime(ctx: &ServerContext) -> i64 {
    let evacuated: Vec<(CharacterId, ItemId, SolarSystemId, SolarSystemId, [f64; 3], HashMap<AttributeId, f64>)> =
        with_pve_special_reg(ctx, |reg| {
            let mut out = Vec::new();
            for p in reg.abyssal_pockets.values_mut() {
                if !p.completed && !p.collapsed_destroyed {
                    p.completed = true;
                    out.push((
                        p.character_id,
                        p.ship_id,
                        p.abyssal_solar_system_id,
                        p.origin_solar_system_id,
                        p.origin_position,
                        p.saved_ship_attrs.clone(),
                    ));
                }
            }
            out
        });
    for (cid, sid, abyss_sys, origin_sys, origin_pos, saved_attrs) in &evacuated {
        restore_abyssal_weather_dogma_modifiers(ctx, *sid, saved_attrs);
        crate::sp6_lifecycle::clear_abyssal_session(ctx, *cid);
        let evacuated_pos = random_offset_around_trace_origin(ctx, *origin_pos);
        let abyss_handle = ctx.systems.get_or_spawn(*abyss_sys);
        abyss_handle.try_remove_ball(*sid);
        let mut destroyed_drones: Vec<ItemId> = Vec::new();
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.retain(|&did, d| {
                if d.owner_char_id == *cid || d.parent_ship_id == *sid {
                    destroyed_drones.push(did);
                    false
                } else {
                    true
                }
            });
        });
        for did in &destroyed_drones {
            abyss_handle.try_remove_ball(*did);
        }
        ctx.db.with_mut_snapshot(|snap| {
            for did in &destroyed_drones {
                snap.items.remove(did);
                snap.item_positions.remove(did);
            }
            if let Some(ch) = snap.characters.get_mut(cid) {
                ch.solar_system_id = *origin_sys;
            }
            if let Some(sh) = snap.items.get_mut(sid) {
                sh.location_id = *origin_sys as i64;
            }
            snap.item_positions.insert(*sid, evacuated_pos);
        });
        ctx.sync_character_local_chat(*cid, Some(*abyss_sys), *origin_sys);
    }
    evacuated.len() as i64
}

pub enum AbyssalLoginAction {
    Active(SolarSystemId),
    CollapsedDestroy { ship_id: ItemId, abyssal_sys: SolarSystemId },
    Evacuated {
        origin_sys: SolarSystemId,
        origin_pos: [f64; 3],
        saved_attrs: HashMap<AttributeId, f64>,
    },
}

pub fn reconcile_abyssal_login(
    ctx: &ServerContext,
    char_id: CharacterId,
    cur_sys: SolarSystemId,
    ship_id: ItemId,
) -> SolarSystemId {
    if !(32_000_000..33_000_000).contains(&cur_sys) {
        return cur_sys;
    }
    let has_sp6_only = with_pve_special_reg(ctx, |reg| !reg.abyssal_pockets.contains_key(&char_id))
        && crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.abyssal_sessions.contains_key(&char_id));
    if has_sp6_only {
        return cur_sys;
    }
    let fallback_sys = ctx.db.with_snapshot(|snap| {
        resolve_character_home_station_and_system(snap, &ctx.sde, char_id).1
    });
    let action = with_pve_special_reg(ctx, |reg| {
        let now = spec_now_sec(reg);
        if let Some(p) = reg.abyssal_pockets.get_mut(&char_id) {
            if !p.completed && !p.collapsed_destroyed && now < p.collapse_at_sec {
                return AbyssalLoginAction::Active(p.abyssal_solar_system_id);
            }
            if !p.completed && (p.collapsed_destroyed || now >= p.collapse_at_sec) {
                p.collapsed_destroyed = true;
                p.completed = true;
                return AbyssalLoginAction::CollapsedDestroy {
                    ship_id: p.ship_id,
                    abyssal_sys: p.abyssal_solar_system_id,
                };
            }
            return AbyssalLoginAction::Evacuated {
                origin_sys: p.origin_solar_system_id,
                origin_pos: p.origin_position,
                saved_attrs: p.saved_ship_attrs.clone(),
            };
        }
        AbyssalLoginAction::Evacuated {
            origin_sys: fallback_sys,
            origin_pos: [145_000.0, -75_000.0, 42_000.0],
            saved_attrs: HashMap::new(),
        }
    });
    match action {
        AbyssalLoginAction::Active(abyssal_sys) => abyssal_sys,
        AbyssalLoginAction::CollapsedDestroy { ship_id: sid, abyssal_sys } => {
            collapse_abyssal_pocket_and_pod_pilot(ctx, char_id, sid, abyssal_sys)
        }
        AbyssalLoginAction::Evacuated {
            origin_sys,
            origin_pos,
            saved_attrs,
        } => {
            if !saved_attrs.is_empty() {
                restore_abyssal_weather_dogma_modifiers(ctx, ship_id, &saved_attrs);
            }
            let rand_pos = random_offset_around_trace_origin(ctx, origin_pos);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = origin_sys;
                }
                if let Some(sh) = snap.items.get_mut(&ship_id) {
                    if sh.location_id == cur_sys as i64 {
                        sh.location_id = origin_sys as i64;
                    }
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
    let cur_sys = session
        .solar_system_id
        .ok_or_else(|| rpc_err("NoSolarSystem", "Session has no active solar system"))?;
    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, cur_sys)?;
    let filament_arg = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

    let (_iid, filament_type_id, tier, max_ships, _origin_sec, ship_pos) =
        validate_and_consume_abyssal_filaments(ctx, char_id, ship_id, cur_sys, filament_arg).await?;

    let now_unix = ctx.now_unix();
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
                expires_at_sec: now_unix + UNENTERED_GATE_LIFETIME_SEC,
                max_ships,
                entered_char_ids: Vec::new(),
                pocket_id: None,
            },
        );
        tid
    });

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    let _ = sys_handle
        .spawn_ball(make_site_npc_ball(
            trace_ball_id,
            ABYSSAL_TRACE_TYPE_ID,
            trace_pos,
        ))
        .await;

    Ok(PyRep::keyval([
        ("traceBallID", PyRep::Integer(trace_ball_id)),
        ("solarSystemID", PyRep::Integer(cur_sys as i64)),
        ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
        ("maxShips", PyRep::Integer(max_ships as i64)),
    ]))
}
