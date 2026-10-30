//! `pve_abyssal_hazards` — Abyssal Deadspace weather modifiers, spatial hazard clouds,
//! automated pylons, filament activation readiness/proximity checks, and home-station helpers.

#![forbid(unsafe_code)]

pub use crate::services::pve_abyssal_pylons::*;

use crate::router::ServerContext;
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pve_special_sites_abyssal::AbyssalWeatherKind;
use eve_proto::BridgeRpcError;
use eve_sde::{AttributeId, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;

type CharacterId = i64;
type ItemId = i64;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

fn err_map_disabled_in_abyssal() -> BridgeRpcError {
    rpc_err(
        "MapDisabledInAbyssalSpace",
        "Star map queries are disabled inside Abyssal Deadspace",
    )
}

/// Spatial hazard cloud types in Abyssal pockets (SDE group 1971).
pub const TACHYON_CLOUD_TYPE_ID: TypeId = 80_876; // Small Tachyon Cloud (SDE group 1971)
pub const PROVING_TACHYON_CLOUD_TYPE_ID: TypeId = 64_896; // Large Proving Tachyon Cloud
pub const BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID: TypeId = 47_439; // Small Bioluminescence Cloud
pub const BIOLUMINESCENT_CLOUD_TYPE_ID: TypeId = 47_440; // Medium Bioluminescence Cloud
pub const BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID: TypeId = 47_441; // Large Bioluminescence Cloud
pub const FILAMENT_CLOUD_SMALL_TYPE_ID: TypeId = 47_620; // Small Filament Cloud
pub const FILAMENT_CLOUD_TYPE_ID: TypeId = 47_472; // Medium Filament Cloud
pub const FILAMENT_CLOUD_LARGE_TYPE_ID: TypeId = 47_473; // Large Filament Cloud
pub const SPATIAL_CLOUD_RADIUS_M: f64 = 15_000.0;
pub const ABYSSAL_ROOM_BOUNDARY_RADIUS_M: f64 = 75_000.0;

/// Automated pylon types in Abyssal pockets (SDE group 1981).
pub const MULTIBODY_TRACKING_PYLON_TYPE_ID: TypeId = 47_469; // Short-Range Multibody Tracking Pylon
pub const MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID: TypeId = 47_470; // Medium-Range Multibody Tracking Pylon
pub const AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID: TypeId = 47_437; // Short-Range Deviant Automata Suppressor
pub const AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID: TypeId = 47_438; // Medium-Range Deviant Automata Suppressor
pub const AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID: TypeId = 48_254;
pub const ABYSSAL_TRACE_TYPE_ID: TypeId = 47_458; // Abyssal Trace (SDE group 1991)
pub const FILAMENT_PROXIMITY_RESTRICTION_RANGE_M: f64 = 1_000_000.0; // 1,000 km

/// Resolves a character's home station and solar system dynamically from persisted character/corp records and SDE.
pub fn resolve_character_home_station_and_system(
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    char_id: CharacterId,
) -> (i64, SolarSystemId, u32, u32) {
    let ch_opt = snap.characters.get(&char_id);
    let cand_station = ch_opt
        .map(|c| c.home_station_id)
        .filter(|&sid| sid > 0 && sde.stations.contains_key(&sid))
        .or_else(|| {
            ch_opt
                .and_then(|c| snap.corporations.get(&c.corporation_id))
                .and_then(|corp| corp.hq_station_id)
                .filter(|&sid| sid > 0 && sde.stations.contains_key(&sid))
        })
        .or_else(|| sde.stations.keys().copied().min())
        .unwrap_or(0);
    let home_sys_id = sde
        .stations
        .get(&cand_station)
        .map(|st| st.solar_system_id)
        .or_else(|| {
            ch_opt
                .map(|c| c.solar_system_id)
                .filter(|&sys| !(32_000_000..33_000_000).contains(&sys) && sde.solar_systems.contains_key(&sys))
        })
        .or_else(|| sde.solar_systems.keys().copied().min())
        .unwrap_or(0);
    let (const_id, reg_id) = sde
        .solar_systems
        .get(&home_sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));
    (cand_station, home_sys_id, const_id, reg_id)
}

/// Validates allowed Abyssal subcapital hull and returns `(ship_type_id, ship_group_id, max_ships)`.
pub fn validate_abyssal_hull_and_capacity(
    ctx: &ServerContext,
    ship_id: ItemId,
) -> Result<(TypeId, u32, u8), BridgeRpcError> {
    let (ship_type_id, ship_group_id) = ctx.db.with_snapshot(|snap| {
        let tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        let gid = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(0);
        (tid, gid)
    });
    let is_allowed_hull = matches!(
        ship_group_id,
        25 | 26 | 324 | 358 | 420 | 541 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 1283 | 1305 | 1527 | 1534 | 1972
    );
    if !is_allowed_hull {
        return Err(rpc_err(
            "DisallowedShipType",
            format!("Ship type {ship_type_id} (group {ship_group_id}) is not permitted to enter Abyssal Deadspace"),
        ));
    }
    let max_ships: u8 = if matches!(ship_group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527) {
        3
    } else if matches!(ship_group_id, 420 | 541 | 1305 | 1534) {
        2
    } else {
        1
    };
    Ok((ship_type_id, ship_group_id, max_ships))
}

/// Evaluates high-sec Abyssal filament tier eligibility and whether a 15-minute Suspect flag applies.
/// - `1.0` & `0.9` (`>= 0.85`): T0 only (`Ok(false)`), T1+ rejected (`FilamentDisallowedInHighSec`).
/// - `0.8` (`0.75..0.85`): T0..=T3 clean (`Ok(false)`), T4..=T6 Suspect (`Ok(true)`).
/// - `0.7` (`0.65..0.75`): T0..=T4 clean (`Ok(false)`), T5..=T6 Suspect (`Ok(true)`).
/// - `0.6` (`0.55..0.65`): T0..=T5 clean (`Ok(false)`), T6 Suspect (`Ok(true)`).
/// - `0.5` & low/null/WH (`< 0.55`): T0..=T6 clean (`Ok(false)`).
pub fn evaluate_highsec_abyssal_tier_policy(origin_sec: f64, tier: u8) -> Result<bool, BridgeRpcError> {
    if origin_sec >= 0.85 {
        if tier >= 1 {
            return Err(rpc_err(
                "FilamentDisallowedInHighSec",
                "Only Tranquil (T0) Abyssal filaments may be activated in 0.9 - 1.0 high-security systems",
            ));
        }
        return Ok(false);
    }
    if origin_sec >= 0.75 {
        return Ok(tier >= 4);
    }
    if origin_sec >= 0.65 {
        return Ok(tier >= 5);
    }
    if origin_sec >= 0.55 {
        return Ok(tier >= 6);
    }
    Ok(false)
}

/// Rejects filament activation inside Abyssal Deadspace (`32_000_000..=32_999_999`) or an active DED/deadspace pocket.
pub async fn check_not_in_deadspace_pocket(
    ctx: &ServerContext,
    cur_sys: SolarSystemId,
    ship_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    if (32_000_000..=32_999_999).contains(&cur_sys) {
        return Err(rpc_err(
            "ShipInDungeon",
            "Cannot activate filament while inside Abyssal or Void deadspace",
        ));
    }
    let near_deadspace_gate = crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&cur_sys).is_some_and(|sites| {
            sites.iter().any(|s| {
                (s.site_category == "Combat" || s.ded_rating.is_some())
                    && s.gate_ball_id.is_some()
                    && DVec3::from_array(s.position).distance(ship_pos) < 250_000.0
            })
        })
    });
    if near_deadspace_gate {
        return Err(rpc_err(
            "ShipInDungeon",
            "Cannot activate filament while inside a deadspace pocket",
        ));
    }
    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(balls) = sys_handle.list_balls().await {
        for b in balls {
            let is_accel_gate = ctx
                .sde
                .types
                .get(&b.type_id)
                .is_some_and(|t| t.group_id == 366);
            if is_accel_gate && b.position.distance(ship_pos) < 250_000.0 {
                return Err(rpc_err(
                    "ShipInDungeon",
                    "Cannot activate filament near a deadspace acceleration gate",
                ));
            }
        }
    }
    Ok(())
}

/// Returns `true` if `sys_id` is an Abyssal or Void deadspace system (`32_000_000..=32_999_999`).
pub fn is_abyssal_system(sys_id: u32) -> bool {
    (32_000_000..=32_999_999).contains(&sys_id)
}

/// Returns `true` if `chan_id` targets a disabled Abyssal Deadspace local chat channel.
pub fn is_abyssal_local_channel_disabled(chan_id: &str, session_sys: Option<u32>) -> bool {
    let rest_opt = chan_id
        .strip_prefix("solarsystemid2:")
        .or_else(|| chan_id.strip_prefix("solarsystemid2_"))
        .or_else(|| chan_id.strip_prefix("solarsystemid_"));
    if let Some(rest) = rest_opt {
        if let Ok(sid) = rest.trim().parse::<u32>() {
            if is_abyssal_system(sid) {
                return true;
            }
        }
    }
    session_sys.is_some_and(is_abyssal_system)
        && (chan_id.eq_ignore_ascii_case("local")
            || chan_id.starts_with("solarsystemid2:")
            || chan_id.starts_with("solarsystemid2_")
            || chan_id.starts_with("solarsystemid_"))
}

/// Blocks restricted RPCs (star map, D-scan, Local chat) while a pilot is inside Abyssal Deadspace.
pub fn check_abyssal_in_pocket_rpc_restrictions(
    session: &eve_machonet::SessionState,
    service: &str,
    method: &str,
    args: &[eve_proto::PyRep],
) -> Option<Result<eve_proto::PyRep, BridgeRpcError>> {
    if !session.solar_system_id.is_some_and(is_abyssal_system) {
        return None;
    }
    if service == "map" {
        return Some(Err(err_map_disabled_in_abyssal()));
    }
    if matches!(service, "scanMgr" | "scanSvc") && matches!(method, "DirectionalScan" | "ConeScan") {
        return Some(Err(BridgeRpcError::user_error("CannotDscanInThisSystem")));
    }
    let is_lsc_method = (service == "LSC" || service.eq_ignore_ascii_case("lsc"))
        && matches!(method, "JoinChannel" | "SendMessage");
    let is_xmpp_method = service == "XmppChatMgr"
        && matches!(method, "AddCharacterToChannel" | "SendMessage");
    if is_lsc_method || is_xmpp_method {
        let chan_opt = match args.first() {
            Some(eve_proto::PyRep::String(s)) => Some(s.as_str()),
            Some(eve_proto::PyRep::List(l) | eve_proto::PyRep::Tuple(l)) => l.first().and_then(|x| x.as_str()),
            _ => None,
        };
        if chan_opt.is_some_and(|c| is_abyssal_local_channel_disabled(c, session.solar_system_id)) {
            return Some(Err(rpc_err(
                "LocalChannelDisabledInAbyssalSpace",
                "Local chat channel is disabled inside Abyssal Deadspace",
            )));
        }
    }
    None
}

/// Enforces the 1,000 km (`1_000_000.0 m`) filament activation proximity rule against stargates, stations,
/// structures, celestials, mobile depots/deployables, and existing Abyssal traces.
pub async fn check_filament_1000km_proximity(
    ctx: &ServerContext,
    cur_sys: SolarSystemId,
    ship_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    if let Some(sys_rec) = ctx.sde.solar_systems.get(&cur_sys) {
        for st in &sys_rec.stations {
            let d = st.position.distance(ship_pos);
            if d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M {
                return Err(rpc_err(
                    "ProximityRestriction",
                    format!("Cannot activate filament within 1,000 km of station {} ({d:.0} m)", st.station_id),
                ));
            }
        }
    }
    for j in ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == cur_sys) {
        let d = j.position.distance(ship_pos);
        if d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M {
            return Err(rpc_err(
                "ProximityRestriction",
                format!("Cannot activate filament within 1,000 km of stargate {} ({d:.0} m)", j.stargate_id),
            ));
        }
    }
    let too_close_struct = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|s| s.solar_system_id == cur_sys && s.state != "Destroyed")
            .find_map(|s| {
                let d = DVec3::from_array(s.position).distance(ship_pos);
                (d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M).then_some((s.structure_id, d))
            })
    });
    if let Some((sid, d)) = too_close_struct {
        return Err(rpc_err(
            "ProximityRestriction",
            format!("Cannot activate filament within 1,000 km of structure {sid} ({d:.0} m)"),
        ));
    }
    let too_close_trace = with_pve_special_reg(ctx, |reg| {
        reg.abyssal_traces
            .values()
            .filter(|t| t.origin_solar_system_id == cur_sys)
            .find_map(|t| {
                let d = DVec3::from_array(t.position).distance(ship_pos);
                (d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M).then_some((t.trace_ball_id, d))
            })
    });
    if let Some((tid, d)) = too_close_trace {
        return Err(rpc_err(
            "ProximityRestriction",
            format!("Cannot activate filament within 1,000 km of Abyssal trace {tid} ({d:.0} m)"),
        ));
    }
    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(balls) = sys_handle.list_balls().await {
        for b in balls {
            let is_restricted_obj = b.type_id == ABYSSAL_TRACE_TYPE_ID
                || crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, b.type_id)
                || ctx.sde.types.get(&b.type_id).is_some_and(|t| {
                    matches!(t.category_id, 3 | 22 | 65)
                        || matches!(t.group_id, 10 | 1246 | 1991)
                });
            if is_restricted_obj {
                let d = b.position.distance(ship_pos);
                if d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M {
                    return Err(rpc_err(
                        "ProximityRestriction",
                        format!("Cannot activate filament within 1,000 km of object {} ({d:.0} m)", b.id),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validates hull, readiness (no PvP/weapon/logoff timer, no warp/cloak/tether/scramble/invuln), deadspace/1,000 km proximity,
/// `InventoryFlag::Cargo`, high-sec tier/suspect rules, and hull-class filament count (1 Cruiser, 2 Destroyer, 3 Frigate), then consumes the filaments.
pub async fn validate_and_consume_abyssal_filaments(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    cur_sys: SolarSystemId,
    filament_arg: i64,
) -> Result<(ItemId, TypeId, u8, u8, f64, DVec3), BridgeRpcError> {
    if filament_arg <= 0 {
        return Err(rpc_err(
            "MissingRequiredArgument",
            "Filament activation requires a positive filament itemID or typeID",
        ));
    }
    let (_ship_tid, _ship_gid, max_ships) = validate_abyssal_hull_and_capacity(ctx, ship_id)?;
    let required_filaments = max_ships as i32;

    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, cur_sys)?;

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            return Err(rpc_err("ShipIsCloaked", "Cannot activate filament while cloaked"));
        }
        if cs.is_tethered {
            return Err(rpc_err("ShipIsTethered", "Cannot activate filament while tethered"));
        }
    }
    let ship_pos = if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ball.position
    } else {
        let p = ctx.db.with_snapshot(|snap| snap.item_positions.get(&ship_id).copied().unwrap_or([0.0, 0.0, 0.0]));
        DVec3::from_array(p)
    };

    check_not_in_deadspace_pocket(ctx, cur_sys, ship_pos).await?;
    check_filament_1000km_proximity(ctx, cur_sys, ship_pos).await?;

    let now_unix = ctx.now_unix();
    let (iid, filament_type_id, tier, origin_sec, gives_suspect) = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && (i.item_id == filament_arg || i.type_id as i64 == filament_arg)
                    && i.quantity >= 1
            })
            .map(|i| (i.item_id, i.type_id, i.quantity));
        let Some((iid, tid, qty)) = found else {
            return Err(rpc_err("ItemNotFound", format!("Abyssal filament {filament_arg} not found in ship cargo")));
        };
        if qty < required_filaments {
            return Err(rpc_err(
                "InsufficientFilamentQuantity",
                format!("Entering Abyssal Deadspace in this ship class requires {required_filaments} filaments, but only {qty} found in cargo"),
            ));
        }
        let Some((tier, _)) = crate::services::pve_special_sites_abyssal::classify_abyssal_filament(tid) else {
            return Err(rpc_err("InvalidAbyssalFilament", format!("Type {tid} is not a valid Abyssal Deadspace filament")));
        };
        let origin_sec = ctx.sde.solar_systems.get(&cur_sys).map(|s| s.security_status).unwrap_or(0.9);
        let gives_suspect = evaluate_highsec_abyssal_tier_policy(origin_sec, tier)?;
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > required_filaments {
                item.quantity -= required_filaments;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok((iid, tid, tier, origin_sec, gives_suspect))
    })?;

    if gives_suspect {
        ctx.crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .trigger_suspect_flag(char_id, now_unix);
    }

    Ok((iid, filament_type_id, tier, max_ships, origin_sec, ship_pos))
}

/// Rolls the Abyssal weather penalty percentage (`30%` or `50%` for `T0..=T3`, `50%` or `70%` for `T4..=T6`).
pub fn roll_abyssal_weather_penalty_pct(ctx: &ServerContext, tier: u8) -> f64 {
    let high_roll = ctx.mechanic_rng.roll(0.5);
    match tier {
        0..=3 => {
            if high_roll {
                50.0
            } else {
                30.0
            }
        }
        _ => {
            if high_roll {
                70.0
            } else {
                50.0
            }
        }
    }
}

/// Applies V24 SDE Abyssal weather `dbuffCollections` (`90..=100`, `PostPercent`) to a `CombatState` and velocity/optimal.
pub fn apply_abyssal_weather_to_combat_and_nav(
    weather: AbyssalWeatherKind,
    penalty_pct: f64,
    bonus_pct: f64,
    cs: &mut eve_destiny::CombatState,
    max_velocity: &mut f64,
    optimal_range: &mut f64,
) {
    let pen = penalty_pct / 100.0;
    let bon = bonus_pct / 100.0;
    let pen_res = |r: f64| (r * (1.0 + pen)).clamp(0.0, 1.0);
    match weather {
        AbyssalWeatherKind::Electrical => {
            cs.shield_resonance[0] = pen_res(cs.shield_resonance[0]);
            cs.armor_resonance[0] = pen_res(cs.armor_resonance[0]);
            cs.structure_resonance[0] = pen_res(cs.structure_resonance[0]);
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms * (1.0 - bon)).max(1_000.0);
        }
        AbyssalWeatherKind::Dark => {
            *max_velocity *= 1.0 + bon;
            *optimal_range *= (1.0 - pen).max(0.1);
        }
        AbyssalWeatherKind::Exotic => {
            cs.shield_resonance[2] = pen_res(cs.shield_resonance[2]);
            cs.armor_resonance[2] = pen_res(cs.armor_resonance[2]);
            cs.structure_resonance[2] = pen_res(cs.structure_resonance[2]);
        }
        AbyssalWeatherKind::Firestorm => {
            cs.max_armor_hp *= 1.0 + bon;
            cs.armor_hp *= 1.0 + bon;
            cs.shield_resonance[1] = pen_res(cs.shield_resonance[1]);
            cs.armor_resonance[1] = pen_res(cs.armor_resonance[1]);
            cs.structure_resonance[1] = pen_res(cs.structure_resonance[1]);
        }
        AbyssalWeatherKind::Gamma => {
            cs.max_shield_hp *= 1.0 + bon;
            cs.shield_hp *= 1.0 + bon;
            cs.shield_resonance[3] = pen_res(cs.shield_resonance[3]);
            cs.armor_resonance[3] = pen_res(cs.armor_resonance[3]);
            cs.structure_resonance[3] = pen_res(cs.structure_resonance[3]);
        }
    }
}

pub(crate) fn is_gunnery_turret_type(t: &eve_sde::InvType) -> bool {
    matches!(t.group_id, 53 | 55 | 74 | 1986)
        || [182_u16, 183, 184, 1285, 1289, 1290]
            .iter()
            .any(|aid| t.default_attributes.get(aid).copied() == Some(3300.0))
}

pub(crate) fn dark_weather_midpocket_turret_mult(
    ctx: &ServerContext,
    char_id: CharacterId,
    sys_id: SolarSystemId,
    is_missile_launcher: bool,
    module_id: ItemId,
    mod_type: &eve_sde::InvType,
) -> f64 {
    if is_missile_launcher || !is_abyssal_system(sys_id) || !is_gunnery_turret_type(mod_type) {
        return 1.0;
    }
    let already_applied = ctx.db.with_snapshot(|s| {
        s.items
            .get(&module_id)
            .is_some_and(|it| it.dynamic_attributes.contains_key(&9954))
    });
    if already_applied {
        return 1.0;
    }
    with_pve_special_reg(ctx, |reg| {
        reg.abyssal_pockets
            .get(&char_id)
            .filter(|p| !p.completed && !p.collapsed_destroyed && p.weather == AbyssalWeatherKind::Dark)
            .map(|p| (1.0 - p.weather_penalty_pct / 100.0).max(0.1))
    })
    .unwrap_or(1.0)
}

/// Applies V24 SDE Abyssal weather dogma modifiers (`dbuffCollections` `90..=100`) to `ship_id` (and fitted Gunnery
/// turrets under Dark weather `dbuff 97`), invalidating cached ship fittings and returning saved pre-weather ship attrs.
pub fn apply_abyssal_weather_dogma_modifiers_to_ship(
    ctx: &ServerContext,
    ship_id: ItemId,
    weather: AbyssalWeatherKind,
    penalty_pct: f64,
    bonus_pct: f64,
) -> HashMap<AttributeId, f64> {
    let pen = penalty_pct / 100.0;
    let bon = bonus_pct / 100.0;
    let range_mult = (1.0 - pen).max(0.1);
    let saved = ctx.db.with_mut_snapshot(|snap| {
        let mut saved = HashMap::new();
        let Some(ship_type_id) = snap.items.get(&ship_id).map(|s| s.type_id) else {
            return saved;
        };
        let sde_defaults = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            let mut modify_attr = |aid: AttributeId, fallback: f64, mult: f64, is_res_pen: bool| {
                let dyn_prev = ship.dynamic_attributes.get(&aid).copied();
                let orig = dyn_prev
                    .or_else(|| sde_defaults.get(&aid).copied())
                    .unwrap_or(fallback);
                saved.entry(aid).or_insert(dyn_prev.unwrap_or(-1.0e300));
                let new_val = if is_res_pen {
                    (orig * (1.0 + pen)).clamp(0.0, 1.0)
                } else {
                    orig * mult
                };
                ship.dynamic_attributes.insert(aid, new_val);
            };
            match weather {
                AbyssalWeatherKind::Electrical => {
                    modify_attr(271, 1.0, 1.0, true); // shieldEmDamageResonance
                    modify_attr(267, 0.5, 1.0, true); // armorEmDamageResonance
                    modify_attr(113, 0.67, 1.0, true); // emDamageResonance
                    modify_attr(55, 250_000.0, (1.0 - bon).max(0.1), false); // rechargeRate
                }
                AbyssalWeatherKind::Dark => {
                    modify_attr(37, 250.0, 1.0 + bon, false); // maxVelocity
                    modify_attr(54, 15_000.0, range_mult, false); // maxRange
                    modify_attr(158, 10_000.0, range_mult, false); // falloff
                }
                AbyssalWeatherKind::Exotic => {
                    modify_attr(273, 0.6, 1.0, true); // shieldKineticDamageResonance
                    modify_attr(269, 0.65, 1.0, true); // armorKineticDamageResonance
                    modify_attr(109, 0.67, 1.0, true); // kineticDamageResonance
                    modify_attr(564, 250.0, 1.0 + bon, false); // scanResolution
                }
                AbyssalWeatherKind::Firestorm => {
                    modify_attr(265, 1_500.0, 1.0 + bon, false); // armorHP
                    modify_attr(274, 0.8, 1.0, true); // shieldThermalDamageResonance
                    modify_attr(270, 0.65, 1.0, true); // armorThermalDamageResonance
                    modify_attr(110, 0.67, 1.0, true); // thermalDamageResonance
                }
                AbyssalWeatherKind::Gamma => {
                    modify_attr(263, 1_500.0, 1.0 + bon, false); // shieldCapacity
                    modify_attr(272, 0.5, 1.0, true); // shieldExplosiveDamageResonance
                    modify_attr(268, 0.9, 1.0, true); // armorExplosiveDamageResonance
                    modify_attr(111, 0.67, 1.0, true); // explosiveDamageResonance
                }
            }
        }
        if weather == AbyssalWeatherKind::Dark {
            let mod_ids: Vec<(ItemId, TypeId)> = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == ship_id
                        && (11..=34).contains(&(it.flag as u16))
                        && ctx.sde.types.get(&it.type_id).is_some_and(is_gunnery_turret_type)
                })
                .map(|it| (it.item_id, it.type_id))
                .collect();
            for (mid, mtid) in mod_ids {
                let m_defs = ctx
                    .sde
                    .types
                    .get(&mtid)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                if let Some(m_item) = snap.items.get_mut(&mid) {
                    for (aid, backup_key, def_v) in [(54_u16, 9954_u16, 5_000.0), (158_u16, 9958_u16, 5_000.0)] {
                        let prev_dyn = m_item.dynamic_attributes.get(&aid).copied();
                        let base_v = if let Some(&saved_v) = m_item.dynamic_attributes.get(&backup_key) {
                            if saved_v <= -1.0e299 {
                                m_defs.get(&aid).copied().unwrap_or(def_v)
                            } else {
                                saved_v
                            }
                        } else {
                            m_item.dynamic_attributes.insert(backup_key, prev_dyn.unwrap_or(-1.0e300));
                            prev_dyn.or_else(|| m_defs.get(&aid).copied()).unwrap_or(def_v)
                        };
                        m_item.dynamic_attributes.insert(aid, base_v * range_mult);
                    }
                }
            }
        }
        saved
    });
    ctx.invalidate_ship_fitting(ship_id);
    saved
}

pub fn restore_abyssal_weather_dogma_modifiers(
    ctx: &ServerContext,
    ship_id: ItemId,
    saved_attrs: &HashMap<AttributeId, f64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if !saved_attrs.is_empty() {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                let sde_defaults = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                for (&attr_id, &val) in saved_attrs {
                    if val <= -1.0e299 || sde_defaults.get(&attr_id).is_some_and(|&def| (def - val).abs() < 1e-9) {
                        ship.dynamic_attributes.remove(&attr_id);
                    } else {
                        ship.dynamic_attributes.insert(attr_id, val);
                    }
                }
                for temp_key in [9937_u16, 9952, 9960, 9961, 9962] {
                    ship.dynamic_attributes.remove(&temp_key);
                }
            }
        }
        let mod_ids: Vec<(ItemId, TypeId)> = snap
            .items
            .values()
            .filter(|it| {
                it.location_id == ship_id
                    && (it.dynamic_attributes.contains_key(&9954)
                        || it.dynamic_attributes.contains_key(&9958))
            })
            .map(|it| (it.item_id, it.type_id))
            .collect();
        for (mid, mtid) in mod_ids {
            let m_defs = ctx
                .sde
                .types
                .get(&mtid)
                .map(|t| t.default_attributes.clone())
                .unwrap_or_default();
            if let Some(m_item) = snap.items.get_mut(&mid) {
                for (aid, backup_key) in [(54_u16, 9954_u16), (158_u16, 9958_u16)] {
                    if let Some(prev) = m_item.dynamic_attributes.remove(&backup_key) {
                        if prev <= -1.0e299 || m_defs.get(&aid).is_some_and(|&def| (def - prev).abs() < 1e-9) {
                            m_item.dynamic_attributes.remove(&aid);
                        } else {
                            m_item.dynamic_attributes.insert(aid, prev);
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

pub(crate) fn is_spatial_hazard_cloud_type(tid: TypeId) -> bool {
    matches!(
        tid,
        TACHYON_CLOUD_TYPE_ID
            | PROVING_TACHYON_CLOUD_TYPE_ID
            | BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID
            | BIOLUMINESCENT_CLOUD_TYPE_ID
            | BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID
            | FILAMENT_CLOUD_SMALL_TYPE_ID
            | FILAMENT_CLOUD_TYPE_ID
            | FILAMENT_CLOUD_LARGE_TYPE_ID
    )
}

/// Returns the V24 SDE radius (`attr 54`, fallback `5_000` / `15_000` / `40_000` m) for a spatial hazard cloud type.
pub fn spatial_cloud_radius_m(
    sde_opt: Option<&eve_sde::SdeRegistry>,
    cloud_tid: TypeId,
    fallback_radius: f64,
) -> f64 {
    if let Some(r) = sde_opt
        .and_then(|s| s.types.get(&cloud_tid))
        .and_then(|t| t.default_attributes.get(&54).copied())
        .filter(|&r| r > 0.0)
    {
        return r;
    }
    match cloud_tid {
        BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID | FILAMENT_CLOUD_SMALL_TYPE_ID => 5_000.0,
        TACHYON_CLOUD_TYPE_ID | BIOLUMINESCENT_CLOUD_TYPE_ID | FILAMENT_CLOUD_TYPE_ID => 15_000.0,
        PROVING_TACHYON_CLOUD_TYPE_ID | BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID | FILAMENT_CLOUD_LARGE_TYPE_ID => 40_000.0,
        _ => fallback_radius.max(SPATIAL_CLOUD_RADIUS_M),
    }
}

pub(crate) fn cloud_modifiers_for_type(cloud_tid: Option<TypeId>) -> (f64, f64, f64, f64, f64, Option<&'static str>) {
    match cloud_tid {
        Some(TACHYON_CLOUD_TYPE_ID | PROVING_TACHYON_CLOUD_TYPE_ID) => {
            (4.0, 4.0, 0.5, 1.0, 1.0, Some("TachyonCloud"))
        }
        Some(
            BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID
            | BIOLUMINESCENT_CLOUD_TYPE_ID
            | BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID,
        ) => (1.0, 4.0, 1.0, 1.0, 1.0, Some("BioluminescentCloud")),
        Some(FILAMENT_CLOUD_SMALL_TYPE_ID) => (1.0, 1.0, 1.0, 0.5, 1.0, Some("FilamentCloud")),
        Some(FILAMENT_CLOUD_TYPE_ID) => (1.0, 1.0, 1.0, 0.6, 1.0, Some("FilamentCloud")),
        Some(FILAMENT_CLOUD_LARGE_TYPE_ID) => (1.0, 1.0, 1.0, 0.7, 1.0, Some("FilamentCloud")),
        _ => (1.0, 1.0, 1.0, 1.0, 1.0, None),
    }
}

/// Evaluates spatial hazard clouds for `ship_id` in `sys_id` and updates ball & `CombatState` modifiers.
pub async fn evaluate_abyssal_spatial_clouds_for_ship(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_id: ItemId,
) -> Option<&'static str> {
    if sys_id == crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID {
        return None;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten()?;
    let is_excluded_ship = ctx.db.with_snapshot(|snap| {
        let pvp = crate::services::pvp_filament_mgr::load_pvp_store(snap);
        let in_pvp = pvp.active_matches.values().any(|m| {
            let elim = pvp.match_eliminated.get(&m.match_id);
            crate::services::pvp_filament_arena::effective_match_teams(&pvp, m)
                .into_iter()
                .flatten()
                .any(|cid| {
                    !m.exited_pilots.contains(&cid)
                        && !elim.is_some_and(|e| e.contains(&cid))
                        && snap.characters.get(&cid).is_some_and(|c| {
                            c.active_ship_id == ship_id
                                && c.solar_system_id == m.arena_solar_system_id
                        })
                })
        });
        in_pvp
    }) || with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions
            .values()
            .any(|vs| vs.ship_id == ship_id)
    });
    if is_excluded_ship {
        return None;
    }
    let (ship_pocket, weather_vel_mult, weather_cap_mult) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.values().find(|p| {
            !p.completed
                && !p.collapsed_destroyed
                && (p.fleet_ship_ids.contains(&ship_id) || p.ship_id == ship_id)
        });
        let pid = p.map(|p| p.pocket_id);
        let v_mult = p
            .map(|p| {
                if p.weather == AbyssalWeatherKind::Dark {
                    1.0 + (p.weather_bonus_pct / 100.0)
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0);
        let c_mult = p
            .map(|p| {
                if p.weather == AbyssalWeatherKind::Electrical {
                    (1.0 - p.weather_bonus_pct / 100.0).max(0.1)
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0);
        (pid, v_mult, c_mult)
    });
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let mut active_cloud: Option<TypeId> = None;
    for b in &balls {
        let cloud_pocket = (b.id >= 1_898_000_000).then_some((b.id - 1_898_000_000) / 100);
        let radius = spatial_cloud_radius_m(Some(&ctx.sde), b.type_id, b.radius);
        if (cloud_pocket.is_none() || cloud_pocket == ship_pocket)
            && is_spatial_hazard_cloud_type(b.type_id)
            && ship_ball.position.distance(b.position) < radius
        {
            active_cloud = Some(b.type_id);
            break;
        }
    }

    let (vel_mult, sig_mult, inertia_mult, boost_mult, cap_rech_mult, label) =
        cloud_modifiers_for_type(active_cloud);
    let (char_id, ship_type, has_dyn_vel, has_dyn_cap) = ctx.db.with_snapshot(|snap| {
        let ship_item = snap.items.get(&ship_id);
        let stype = ship_item.map(|i| i.type_id).unwrap_or(ship_ball.type_id);
        let cid = ship_item
            .map(|i| i.owner_id)
            .filter(|&o| o > 0)
            .or_else(|| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == ship_id)
                    .map(|c| c.character_id)
            })
            .unwrap_or(0);
        let dyn_v = ship_item.is_some_and(|i| i.dynamic_attributes.contains_key(&37));
        let dyn_c = ship_item.is_some_and(|i| i.dynamic_attributes.contains_key(&55));
        (cid, stype, dyn_v, dyn_c)
    });
    let fitted_ball = crate::services::pve_abyssal_pylons::build_fitted_ship_space_ball(
        ctx,
        char_id,
        ship_id,
        ship_type,
        ship_ball.position,
    );
    let fitted_cs = if char_id > 0 {
        ctx.build_ship_combat_state(char_id, ship_id, ship_type)
    } else {
        ctx.build_type_combat_state(ship_type)
    };
    let eff_weather_vel = if has_dyn_vel { 1.0 } else { weather_vel_mult };
    let eff_weather_cap = if has_dyn_cap { 1.0 } else { weather_cap_mult };

    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.max_velocity = fitted_ball.max_velocity * eff_weather_vel * vel_mult;
            b.inertia = fitted_ball.inertia * inertia_mult;
        }
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.signature_radius = fitted_cs.signature_radius * sig_mult;
            cs.shield_boost_multiplier = boost_mult;
            cs.capacitor_recharge_time_ms =
                (fitted_cs.capacitor_recharge_time_ms * eff_weather_cap * cap_rech_mult)
                    .max(1_000.0);
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut active_clouds: HashMap<i64, TypeId> = snap
            .service_state_blobs
            .get("abyssal_active_clouds")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if let Some(cid) = active_cloud {
            active_clouds.insert(ship_id, cid);
        } else {
            active_clouds.remove(&ship_id);
        }
        if let Ok(json) = serde_json::to_string(&active_clouds) {
            snap.service_state_blobs
                .insert("abyssal_active_clouds".into(), json);
        }
    });
    label
}
