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
