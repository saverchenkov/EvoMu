//! `pve_abyssal_hazards` — Abyssal Deadspace weather modifiers, spatial hazard clouds,
//! automated pylons, filament activation readiness/proximity checks, and home-station helpers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pve_special_sites_abyssal::AbyssalWeatherKind;
use eve_proto::BridgeRpcError;
use eve_sde::{AttributeId, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

type CharacterId = i64;
type ItemId = i64;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Spatial hazard cloud types in Abyssal pockets (SDE group 1971).
pub const TACHYON_CLOUD_TYPE_ID: TypeId = 80_876; // Small Tachyon Cloud (SDE group 1971)
pub const PROVING_TACHYON_CLOUD_TYPE_ID: TypeId = 64_896; // Large Proving Tachyon Cloud
pub const BIOLUMINESCENT_CLOUD_TYPE_ID: TypeId = 47_440; // Medium Bioluminescence Cloud
pub const FILAMENT_CLOUD_TYPE_ID: TypeId = 47_472; // Medium Filament Cloud
pub const SPATIAL_CLOUD_RADIUS_M: f64 = 15_000.0;

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
        25 | 26 | 324 | 358 | 420 | 541 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 1305 | 1527 | 1534 | 1972
    );
    if !is_allowed_hull {
        return Err(rpc_err(
            "DisallowedShipType",
            format!("Ship type {ship_type_id} (group {ship_group_id}) is not permitted to enter Abyssal Deadspace"),
        ));
    }
    let max_ships: u8 = if matches!(ship_group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1527) {
        3
    } else if matches!(ship_group_id, 420 | 541 | 1305 | 1534) {
        2
    } else {
        1
    };
    Ok((ship_type_id, ship_group_id, max_ships))
}

/// Enforces the 1,000 km (`1_000_000.0 m`) filament activation proximity rule against stargates, stations, and structures.
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
                    "FilamentProximityRestriction",
                    format!("Cannot activate filament within 1,000 km of station {} ({d:.0} m)", st.station_id),
                ));
            }
        }
    }
    for j in ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == cur_sys) {
        let d = j.position.distance(ship_pos);
        if d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M {
            return Err(rpc_err(
                "FilamentProximityRestriction",
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
            "FilamentProximityRestriction",
            format!("Cannot activate filament within 1,000 km of structure {sid} ({d:.0} m)"),
        ));
    }
    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(balls) = sys_handle.list_balls().await {
        for b in balls {
            let is_restricted_celestial = ctx.sde.types.get(&b.type_id).is_some_and(|t| {
                matches!(t.category_id, 3 | 65) || t.group_id == 10
            });
            if is_restricted_celestial {
                let d = b.position.distance(ship_pos);
                if d < FILAMENT_PROXIMITY_RESTRICTION_RANGE_M {
                    return Err(rpc_err(
                        "FilamentProximityRestriction",
                        format!("Cannot activate filament within 1,000 km of celestial {} ({d:.0} m)", b.id),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validates hull, readiness (no PvP/weapon timer, no warp/cloak/tether/scramble), 1,000 km proximity,
/// `InventoryFlag::Cargo`, and hull-class filament count (1 Cruiser, 2 Destroyer, 3 Frigate), then consumes the filaments.
pub async fn validate_and_consume_abyssal_filaments(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    cur_sys: SolarSystemId,
    filament_arg: i64,
) -> Result<(ItemId, TypeId, u8, u8, f64, DVec3), BridgeRpcError> {
    let (_ship_tid, _ship_gid, max_ships) = validate_abyssal_hull_and_capacity(ctx, ship_id)?;
    let required_filaments = max_ships as i32;

    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, cur_sys)?;

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            return Err(rpc_err("CannotActivateFilamentCloaked", "Cannot activate filament while cloaked"));
        }
        if cs.is_tethered {
            return Err(rpc_err("CannotActivateFilamentTethered", "Cannot activate filament while tethered"));
        }
    }
    let ship_pos = if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ball.position
    } else {
        let p = ctx.db.with_snapshot(|snap| snap.item_positions.get(&ship_id).copied().unwrap_or([0.0, 0.0, 0.0]));
        DVec3::from_array(p)
    };

    check_filament_1000km_proximity(ctx, cur_sys, ship_pos).await?;

    let now_unix = ctx.now_unix();
    let (iid, filament_type_id, tier, origin_sec) = ctx.db.try_with_mut_snapshot(|snap| {
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
        if tier >= 4 && origin_sec >= 0.75 {
            return Err(rpc_err(
                "FilamentDisallowedInHighSec",
                "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems",
            ));
        }
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > required_filaments {
                item.quantity -= required_filaments;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok((iid, tid, tier, origin_sec))
    })?;

    if tier >= 4 && (0.45..0.75).contains(&origin_sec) {
        ctx.crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .trigger_suspect_flag(char_id, now_unix);
    }

    Ok((iid, filament_type_id, tier, max_ships, origin_sec, ship_pos))
}

/// Applies Abyssal weather dogma modifiers to a `CombatState` and velocity/optimal.
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
    match weather {
        AbyssalWeatherKind::Electrical => {
            let shield_em = (1.0 - cs.shield_resonance[0]).max(0.0);
            cs.shield_resonance[0] = 1.0 - (shield_em * (1.0 - pen)).clamp(0.0, 0.99);
            let armor_em = (1.0 - cs.armor_resonance[0]).max(0.0);
            cs.armor_resonance[0] = 1.0 - (armor_em + (1.0 - armor_em) * bon).clamp(0.0, 0.99);
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms * (1.0 - bon)).max(1_000.0);
        }
        AbyssalWeatherKind::Dark => {
            *max_velocity *= 1.0 + bon;
            *optimal_range *= (1.0 - pen).max(0.1);
        }
        AbyssalWeatherKind::Exotic => {
            let shield_kin = (1.0 - cs.shield_resonance[2]).max(0.0);
            cs.shield_resonance[2] = 1.0 - (shield_kin * (1.0 - pen)).clamp(0.0, 0.99);
            let armor_kin = (1.0 - cs.armor_resonance[2]).max(0.0);
            cs.armor_resonance[2] = 1.0 - (armor_kin + (1.0 - armor_kin) * bon).clamp(0.0, 0.99);
        }
        AbyssalWeatherKind::Firestorm => {
            cs.max_armor_hp *= 1.0 + bon;
            cs.armor_hp *= 1.0 + bon;
            let shield_therm = (1.0 - cs.shield_resonance[1]).max(0.0);
            cs.shield_resonance[1] = 1.0 - (shield_therm * (1.0 - pen)).clamp(0.0, 0.99);
            let armor_therm = (1.0 - cs.armor_resonance[1]).max(0.0);
            cs.armor_resonance[1] = 1.0 - (armor_therm * (1.0 - pen)).clamp(0.0, 0.99);
            let hull_therm = (1.0 - cs.structure_resonance[1]).max(0.0);
            cs.structure_resonance[1] = 1.0 - (hull_therm * (1.0 - pen)).clamp(0.0, 0.99);
        }
        AbyssalWeatherKind::Gamma => {
            cs.max_shield_hp *= 1.0 + bon;
            cs.shield_hp *= 1.0 + bon;
            let shield_exp = (1.0 - cs.shield_resonance[3]).max(0.0);
            cs.shield_resonance[3] = 1.0 - (shield_exp * (1.0 - pen)).clamp(0.0, 0.99);
            let armor_exp = (1.0 - cs.armor_resonance[3]).max(0.0);
            cs.armor_resonance[3] = 1.0 - (armor_exp * (1.0 - pen)).clamp(0.0, 0.99);
            let hull_exp = (1.0 - cs.structure_resonance[3]).max(0.0);
            cs.structure_resonance[3] = 1.0 - (hull_exp * (1.0 - pen)).clamp(0.0, 0.99);
        }
    }
}

/// Applies Abyssal weather dogma attributes to a player ship in `snap.items`, writing the modified values
/// into `ship.dynamic_attributes` and returning the saved pre-weather attribute values.
pub fn apply_abyssal_weather_dogma_modifiers_to_ship(
    ctx: &ServerContext,
    ship_id: ItemId,
    weather: AbyssalWeatherKind,
    penalty_pct: f64,
    bonus_pct: f64,
) -> HashMap<AttributeId, f64> {
    let pen = penalty_pct / 100.0;
    let bon = bonus_pct / 100.0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut saved = HashMap::new();
        let Some(ship) = snap.items.get_mut(&ship_id) else {
            return saved;
        };
        let sde_defaults = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();
        let mut modify_attr = |aid: AttributeId, fallback: f64, mult: f64, res_mode: Option<bool>| {
            let dyn_prev = ship.dynamic_attributes.get(&aid).copied();
            let orig = dyn_prev
                .or_else(|| sde_defaults.get(&aid).copied())
                .unwrap_or(fallback);
            saved.entry(aid).or_insert(dyn_prev.unwrap_or(-1.0e300));
            let new_val = match res_mode {
                Some(true) => {
                    let r = (1.0 - orig).clamp(0.0, 0.99);
                    1.0 - (r * (1.0 - pen)).clamp(0.0, 0.99)
                }
                Some(false) => {
                    let r = (1.0 - orig).clamp(0.0, 0.99);
                    1.0 - (r + (1.0 - r) * bon).clamp(0.0, 0.99)
                }
                None => orig * mult,
            };
            ship.dynamic_attributes.insert(aid, new_val);
        };
        match weather {
            AbyssalWeatherKind::Electrical => {
                modify_attr(271, 1.0, 1.0, Some(true)); // shieldEmDamageResonance
                modify_attr(267, 0.5, 1.0, Some(false)); // armorEmDamageResonance
                modify_attr(55, 250_000.0, (1.0 - bon).max(0.1), None); // rechargeRate
            }
            AbyssalWeatherKind::Dark => {
                modify_attr(37, 250.0, 1.0 + bon, None); // maxVelocity
                modify_attr(54, 15_000.0, (1.0 - pen).max(0.1), None); // maxRange
                modify_attr(158, 10_000.0, (1.0 - pen).max(0.1), None); // falloff
            }
            AbyssalWeatherKind::Exotic => {
                modify_attr(273, 0.6, 1.0, Some(true)); // shieldKineticDamageResonance
                modify_attr(269, 0.65, 1.0, Some(false)); // armorKineticDamageResonance
                modify_attr(564, 250.0, 1.0 + bon, None); // scanResolution
            }
            AbyssalWeatherKind::Firestorm => {
                modify_attr(265, 1_500.0, 1.0 + bon, None); // armorHP
                modify_attr(274, 0.8, 1.0, Some(true)); // shieldThermalDamageResonance
                modify_attr(270, 0.65, 1.0, Some(true)); // armorThermalDamageResonance
                modify_attr(110, 0.67, 1.0, Some(true)); // structureThermalDamageResonance
            }
            AbyssalWeatherKind::Gamma => {
                modify_attr(263, 1_500.0, 1.0 + bon, None); // shieldCapacity
                modify_attr(272, 0.5, 1.0, Some(true)); // shieldExplosiveDamageResonance
                modify_attr(268, 0.9, 1.0, Some(true)); // armorExplosiveDamageResonance
                modify_attr(111, 0.67, 1.0, Some(true)); // structureExplosiveDamageResonance
            }
        }
        saved
    })
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
    });
}

fn cloud_modifiers_for_type(cloud_tid: Option<TypeId>) -> (f64, f64, f64, f64, f64, Option<&'static str>) {
    match cloud_tid {
        Some(TACHYON_CLOUD_TYPE_ID | PROVING_TACHYON_CLOUD_TYPE_ID) => {
            (4.0, 0.5, 4.0, 1.0, 1.0, Some("TachyonCloud"))
        }
        Some(BIOLUMINESCENT_CLOUD_TYPE_ID) => {
            (1.0, 4.0, 1.0, 1.0, 1.0, Some("BioluminescentCloud"))
        }
        Some(FILAMENT_CLOUD_TYPE_ID) => {
            (1.0, 1.25, 1.0, 0.6, 1.5, Some("FilamentCloud"))
        }
        _ => (1.0, 1.0, 1.0, 1.0, 1.0, None),
    }
}

/// Evaluates spatial hazard clouds for `ship_id` in `sys_id` and updates ball & `CombatState` modifiers.
pub async fn evaluate_abyssal_spatial_clouds_for_ship(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_id: ItemId,
) -> Option<&'static str> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten()?;
    let is_arena_ship = ctx.db.with_snapshot(|snap| {
        let pvp = crate::services::pvp_filament_mgr::load_pvp_store(snap);
        pvp.active_matches.values().filter(|m| m.active).any(|m| {
            m.team_a.iter().chain(m.team_b.iter()).any(|&cid| {
                snap.characters.get(&cid).is_some_and(|c| c.active_ship_id == ship_id)
            })
        })
    });
    if is_arena_ship {
        return None;
    }
    let (ship_pocket, weather_vel_mult, weather_cap_mult) = with_pve_special_reg(ctx, |reg| {
        let p = reg
            .abyssal_pockets
            .values()
            .find(|p| !p.completed && !p.collapsed_destroyed && (p.fleet_ship_ids.contains(&ship_id) || p.ship_id == ship_id));
        let pid = p.map(|p| p.pocket_id);
        let v_mult = p
            .map(|p| if p.weather == AbyssalWeatherKind::Dark { 1.0 + (p.weather_bonus_pct / 100.0) } else { 1.0 })
            .unwrap_or(1.0);
        let c_mult = p
            .map(|p| if p.weather == AbyssalWeatherKind::Electrical { (1.0 - p.weather_bonus_pct / 100.0).max(0.1) } else { 1.0 })
            .unwrap_or(1.0);
        (pid, v_mult, c_mult)
    });
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let mut active_cloud: Option<TypeId> = None;
    for b in &balls {
        let cloud_pocket = (b.id >= 1_898_000_000).then_some((b.id - 1_898_000_000) / 100);
        if (cloud_pocket.is_none() || cloud_pocket == ship_pocket)
            && matches!(
                b.type_id,
                TACHYON_CLOUD_TYPE_ID | PROVING_TACHYON_CLOUD_TYPE_ID | BIOLUMINESCENT_CLOUD_TYPE_ID | FILAMENT_CLOUD_TYPE_ID
            )
            && ship_ball.position.distance(b.position) < b.radius.max(SPATIAL_CLOUD_RADIUS_M)
        {
            active_cloud = Some(b.type_id);
            break;
        }
    }

    let (vel_mult, sig_mult, inertia_mult, boost_mult, cap_rech_mult, label) = cloud_modifiers_for_type(active_cloud);
    let (default_vel, default_sig, default_cap_rech) = ctx.db.with_snapshot(|snap| {
        let ship_type = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(ship_ball.type_id);
        let sde_t = ctx.sde.types.get(&ship_type);
        let dv = sde_t.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(250.0);
        let ds = sde_t.and_then(|t| t.default_attributes.get(&552).copied()).unwrap_or(125.0);
        let dc = sde_t.and_then(|t| t.default_attributes.get(&55).copied()).unwrap_or(120_000.0);
        (dv, ds, dc)
    });

    ship_ball.max_velocity = default_vel * weather_vel_mult * vel_mult;
    ship_ball.inertia = 0.5 * inertia_mult;
    let _ = sys_handle.spawn_ball(ship_ball).await;
    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
        cs.signature_radius = default_sig * sig_mult;
        cs.shield_boost_multiplier = boost_mult;
        cs.capacitor_recharge_time_ms = (default_cap_rech * weather_cap_mult * cap_rech_mult).max(1_000.0);
        let _ = sys_handle.set_combat_state(ship_id, cs).await;
    }
    label
}

/// Pulses automated pylons in `sys_id`, applying Automata Suppressor AoE damage and Multibody Tracking Pylon `tracking_speed_multiplier`.
pub async fn pulse_abyssal_automated_pylons(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (usize, usize) {
    let pocket_by_ship: HashMap<i64, i64> = with_pve_special_reg(ctx, |reg| {
        let mut map = HashMap::new();
        for p in reg.abyssal_pockets.values().filter(|p| !p.completed && !p.collapsed_destroyed) {
            map.insert(p.ship_id, p.pocket_id);
            for &sid in &p.fleet_ship_ids {
                map.insert(sid, p.pocket_id);
            }
        }
        map
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let mut suppressors: Vec<(Option<i64>, DVec3, f64)> = Vec::new();
    let mut tracking_pylons: Vec<(Option<i64>, DVec3, f64)> = Vec::new();
    for b in &balls {
        let is_alive = sys_handle
            .get_combat_state(b.id)
            .await
            .ok()
            .flatten()
            .map(|cs| cs.structure_hp > 0.0)
            .unwrap_or(true);
        if !is_alive {
            continue;
        }
        let py_pocket = (b.id >= 1_899_000_000).then_some((b.id - 1_899_000_000) / 100);
        match b.type_id {
            AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID => suppressors.push((py_pocket, b.position, 15_000.0)),
            AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID => suppressors.push((py_pocket, b.position, 25_000.0)),
            AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID => suppressors.push((py_pocket, b.position, 40_000.0)),
            MULTIBODY_TRACKING_PYLON_TYPE_ID | MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID => {
                tracking_pylons.push((py_pocket, b.position, b.radius.max(20_000.0)));
            }
            _ => {}
        }
    }

    let mut damaged_count = 0usize;
    let mut tracking_buffed_count = 0usize;
    for b in &balls {
        let Some(t) = ctx.sde.types.get(&b.type_id) else { continue };
        let b_pocket = pocket_by_ship
            .get(&b.id)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&b.id).then_some(b.id / 100));
        let is_missile_or_drone = matches!(t.category_id, 8 | 18);
        let is_rogue_drone_frigate = matches!(t.group_id, 1997 | 4037 | 297);
        if is_missile_or_drone || is_rogue_drone_frigate {
            for &(spocket, spos, srange) in &suppressors {
                if (spocket.is_none() || b_pocket.is_none() || spocket == b_pocket) && b.position.distance(spos) <= srange {
                    let dmg = eve_destiny::DamageVector { em: 0.0, thermal: 0.0, kinetic: 0.0, explosive: 250.0 };
                    let _ = sys_handle.apply_damage(b.id, dmg).await;
                    damaged_count += 1;
                }
            }
        }

        let is_ship_or_sentry = t.category_id == 6 || t.group_id == 1982 || t.group_id == 640;
        if is_ship_or_sentry {
            let in_tracking_pylon = tracking_pylons
                .iter()
                .any(|&(ppocket, ppos, prange)| (ppocket.is_none() || b_pocket.is_none() || ppocket == b_pocket) && b.position.distance(ppos) <= prange);
            let desired_mult = if in_tracking_pylon { 1.8 } else { 1.0 };
            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(b.id).await {
                if (cs.tracking_speed_multiplier - desired_mult).abs() > 1e-6 {
                    cs.tracking_speed_multiplier = desired_mult;
                    let _ = sys_handle.set_combat_state(b.id, cs).await;
                }
            }
            if in_tracking_pylon {
                tracking_buffed_count += 1;
            }
        }
    }
    (damaged_count, tracking_buffed_count)
}

/// Real-time 1 Hz tick evaluation of Abyssal hazard clouds and automated pylons in `solar_system_id == 32_000_001`.
pub fn step_abyssal_hazards_and_pylons_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    solar_system_id: eve_sde::SolarSystemId,
) {
    if !(32_000_001..=32_999_999).contains(&solar_system_id) {
        return;
    }
    let mut pocket_by_ship: HashMap<i64, i64> = HashMap::new();
    let mut arena_ships: HashSet<i64> = HashSet::new();
    let mut active_clouds: HashMap<i64, TypeId> = HashMap::new();
    if let Some(db) = db_opt {
        let _ = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
            if let Some(blob) = snap.service_state_blobs.get("incursions_state") {
                if let Ok(reg) = serde_json::from_str::<crate::services::pve_special_sites::PveSpecialSitesRegistry>(blob) {
                    for p in reg.abyssal_pockets.values().filter(|p| !p.completed && !p.collapsed_destroyed) {
                        pocket_by_ship.insert(p.ship_id, p.pocket_id);
                        for &sid in &p.fleet_ship_ids {
                            pocket_by_ship.insert(sid, p.pocket_id);
                        }
                    }
                }
            }
            let pvp_store = crate::services::pvp_filament_mgr::load_pvp_store(snap);
            for m in pvp_store.active_matches.values().filter(|m| m.active) {
                for &cid in m.team_a.iter().chain(m.team_b.iter()) {
                    if let Some(ch) = snap.characters.get(&cid) {
                        arena_ships.insert(ch.active_ship_id);
                    }
                }
            }
            if let Some(cblob) = snap.service_state_blobs.get("abyssal_active_clouds") {
                active_clouds = serde_json::from_str(cblob).unwrap_or_default();
            }
            Ok(())
        });
    }

    let mut suppressors: Vec<(Option<i64>, DVec3, f64)> = Vec::new();
    let mut tracking_pylons: Vec<(Option<i64>, DVec3, f64)> = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if !ballpark.combat_states.get(&bid).map(|cs| cs.structure_hp > 0.0).unwrap_or(true) {
            continue;
        }
        let py_pocket = (bid >= 1_899_000_000).then_some((bid - 1_899_000_000) / 100);
        if matches!(
            b.type_id,
            AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID | AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID | AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID
        ) {
            let s_range = sde_opt
                .and_then(|s| s.types.get(&b.type_id))
                .and_then(|t| t.default_attributes.get(&54).copied())
                .filter(|&r| r > 0.0)
                .unwrap_or(b.radius.max(15_000.0));
            suppressors.push((py_pocket, b.position, s_range));
        } else if matches!(b.type_id, MULTIBODY_TRACKING_PYLON_TYPE_ID | MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID) {
            tracking_pylons.push((py_pocket, b.position, b.radius.max(20_000.0)));
        }
    }

    if !suppressors.is_empty() {
        let mut damages: Vec<(i64, eve_destiny::DamageVector)> = Vec::new();
        for (&bid, b) in &ballpark.balls {
            if arena_ships.contains(&bid) {
                continue;
            }
            let b_pocket = pocket_by_ship
                .get(&bid)
                .copied()
                .or_else(|| (60_000_000..100_000_000).contains(&bid).then_some(bid / 100));
            let sde_type = sde_opt.and_then(|sde| sde.types.get(&b.type_id));
            let is_missile_or_drone = sde_type.is_some_and(|t| matches!(t.category_id, 8 | 18));
            let is_rogue_drone_frigate = sde_type.is_some_and(|t| matches!(t.group_id, 1997 | 4037 | 297));
            if is_missile_or_drone || is_rogue_drone_frigate {
                for &(spocket, spos, srange) in &suppressors {
                    if (spocket.is_none() || b_pocket.is_none() || spocket == b_pocket) && b.position.distance(spos) <= srange {
                        damages.push((
                            bid,
                            eve_destiny::DamageVector { em: 0.0, thermal: 0.0, kinetic: 0.0, explosive: 250.0 },
                        ));
                        break;
                    }
                }
            }
        }
        for (target_id, dmg) in damages {
            let _ = ballpark.apply_damage_to_ball(target_id, dmg);
        }
    }

    // Apply or clear Multibody Tracking Pylon tracking_speed_multiplier on ships and NPCs
    let ship_ids: Vec<i64> = ballpark
        .balls
        .keys()
        .copied()
        .filter(|&id| ballpark.combat_states.contains_key(&id) && !ballpark.is_static_ball(id) && !arena_ships.contains(&id))
        .collect();

    for &sid in &ship_ids {
        let Some(b) = ballpark.balls.get(&sid) else { continue };
        let is_ship_or_sentry = sde_opt
            .and_then(|s| s.types.get(&b.type_id))
            .map(|t| t.category_id == 6 || t.group_id == 1982 || t.group_id == 640)
            .unwrap_or(true);
        if !is_ship_or_sentry {
            continue;
        }
        let s_pocket = pocket_by_ship
            .get(&sid)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&sid).then_some(sid / 100));
        let in_tracking = tracking_pylons
            .iter()
            .any(|&(ppocket, ppos, prange)| (ppocket.is_none() || s_pocket.is_none() || ppocket == s_pocket) && b.position.distance(ppos) <= prange);
        let desired = if in_tracking { 1.8 } else { 1.0 };
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.tracking_speed_multiplier = desired;
        }
    }

    let mut clouds: Vec<(Option<i64>, TypeId, DVec3, f64)> = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if matches!(
            b.type_id,
            TACHYON_CLOUD_TYPE_ID | PROVING_TACHYON_CLOUD_TYPE_ID | BIOLUMINESCENT_CLOUD_TYPE_ID | FILAMENT_CLOUD_TYPE_ID
        ) {
            let c_pocket = (bid >= 1_898_000_000).then_some((bid - 1_898_000_000) / 100);
            let c_range = sde_opt
                .and_then(|s| s.types.get(&b.type_id))
                .and_then(|t| t.default_attributes.get(&54).copied())
                .filter(|&r| r > 0.0)
                .unwrap_or(b.radius.max(SPATIAL_CLOUD_RADIUS_M));
            clouds.push((c_pocket, b.type_id, b.position, c_range));
        }
    }

    if clouds.is_empty() && active_clouds.is_empty() {
        return;
    }

    active_clouds.retain(|bid, _| ballpark.balls.contains_key(bid));

    for sid in ship_ids {
        let Some(b) = ballpark.balls.get_mut(&sid) else { continue };
        let pos = b.position;
        let s_pocket = pocket_by_ship
            .get(&sid)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&sid).then_some(sid / 100));
        let current_cloud = clouds
            .iter()
            .find(|(cpocket, _, cpos, cradius)| (cpocket.is_none() || s_pocket.is_none() || *cpocket == s_pocket) && pos.distance(*cpos) < *cradius)
            .map(|(_, tid, _, _)| *tid);

        let prev_cloud = active_clouds.get(&sid).copied();
        if prev_cloud == current_cloud {
            continue;
        }

        let (prev_v, prev_sig, prev_in, _prev_b, prev_cap, _) = cloud_modifiers_for_type(prev_cloud);
        b.max_velocity /= prev_v;
        b.inertia /= prev_in;
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.signature_radius /= prev_sig;
            cs.shield_boost_multiplier = 1.0;
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms / prev_cap).max(1_000.0);
        }

        let (cur_v, cur_sig, cur_in, cur_b, cur_cap, _) = cloud_modifiers_for_type(current_cloud);
        b.max_velocity *= cur_v;
        b.inertia *= cur_in;
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.signature_radius *= cur_sig;
            cs.shield_boost_multiplier = cur_b;
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms * cur_cap).max(1_000.0);
        }
        if let Some(cid) = current_cloud {
            active_clouds.insert(sid, cid);
        } else {
            active_clouds.remove(&sid);
        }
    }

    if let Some(db) = db_opt {
        if let Ok(json) = serde_json::to_string(&active_clouds) {
            let _ = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
                snap.service_state_blobs.insert("abyssal_active_clouds".into(), json);
                Ok(())
            });
        }
    }
}
