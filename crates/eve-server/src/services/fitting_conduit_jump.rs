//! `fitting_conduit_jump` — Jump Portal Generators (`beyonce.CmdBridgeToFleetModuleBeacon`,
//! `beyonce.CmdBridgeToFleetDeployableBeacon`, `beyonce.CmdJumpThroughFleet`), Conduit Jumps
//! (`beyonce.CmdGroupJumpToFleetModuleBeacon`, `beyonce.CmdGroupJumpToFleetDeployableBeacon`),
//! and `beyonce` cyno/portal RPC dispatch.
//!
// parity: rpc:beyonce.CmdBridgeToFleetDeployableBeacon, rpc:beyonce.CmdGroupJumpToFleetDeployableBeacon
// parity: rpc:beyonce.CmdGroupJumpToFleetModuleBeacon, feat:modules.jump-portal-bridge-rules, feat:navigation.conduit-jump

use super::*;
use crate::router::notify_registry::{
    OnBridgeModeChange, OnGroupJumpAnchorActivated, OnGroupJumpedPassenger, OnSessionChanged,
};
use eve_destiny::MovementMode;

pub(super) fn hull_allowed(ctx: &ServerContext, mod_type: &eve_sde::InvType, ship_type: &eve_sde::InvType) -> bool {
    let mut groups = Vec::new();
    let mut types = Vec::new();
    for (aid, &val) in &mod_type.default_attributes {
        if val <= 0.0 {
            continue;
        }
        let Some(name) = ctx.sde.attributes.get(aid).map(|a| a.name.as_str()) else {
            continue;
        };
        if name.starts_with("canFitShipGroup") {
            groups.push(val as u32);
        } else if name.starts_with("canFitShipType") {
            types.push(val as TypeId);
        }
    }
    (groups.is_empty() && types.is_empty())
        || groups.contains(&ship_type.group_id)
        || types.contains(&ship_type.type_id)
}

pub(super) fn ship_accepts_beacon(ctx: &ServerContext, ship_type: &eve_sde::InvType, beacon_type_id: TypeId) -> bool {
    let list_id = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_TARGET_BEACON_TYPELIST) as u32;
    let (Some(list), Some(beacon)) = (ctx.sde.type_lists.get(&list_id), ctx.sde.types.get(&beacon_type_id)) else {
        return false;
    };
    list.contains(beacon)
}

pub(super) fn csprng_unit_f64(ctx: &ServerContext) -> f64 {
    if let Some(u) = ctx.mechanic_rng.try_unit_f64() {
        return u;
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut z = seq
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(ctx.now_blueos_filetime_ticks() as u64);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
}

pub(super) fn random_arrival_position(ctx: &ServerContext, beacon_pos: DVec3) -> DVec3 {
    let u1 = csprng_unit_f64(ctx);
    let u2 = csprng_unit_f64(ctx);
    let z = u1 * 2.0 - 1.0;
    let theta = u2 * std::f64::consts::TAU;
    let r_xy = (1.0 - z * z).max(0.0).sqrt();
    beacon_pos + DVec3::new(r_xy * theta.cos(), z, r_xy * theta.sin()) * 5_000.0
}

pub(super) fn star_arrival_position(ctx: &ServerContext, dest_sys: u32) -> DVec3 {
    let star_radius = ctx
        .sde
        .celestials_in_system(dest_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.radius)
        .unwrap_or(65_000_000.0)
        .max(1_000_000.0);
    random_arrival_position(ctx, DVec3::new(star_radius + 25_000.0, 0.0, 0.0))
}

pub(super) fn is_covert_ops_cloak_capable(ctx: &ServerContext, ship_type: &eve_sde::InvType) -> bool {
    if let Some(cct) = ctx.sde.types.get(&11578) {
        if hull_allowed(ctx, cct, ship_type) {
            return true;
        }
    }
    matches!(ship_type.group_id, 830 | 833 | 834 | 963 | 1202 | 5087)
        || matches!(
            ship_type.type_id,
            32790 | 33468 | 33470 | 33553 | 33697 | 34590 | 42245 | 77114 | 85086 | 85087 | 89607
        )
}

#[allow(dead_code)]
pub fn sweep_expired_cyno_beacons(ctx: &ServerContext) {
    let (now, ctx_k) = (ctx.now_unix_secs(), ctx_key(ctx));
    let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
    let mut expired_beacons = Vec::new();
    st.beacons.retain(|&(k, _), b| {
        if k == ctx_k && b.expires_at_unix <= now {
            expired_beacons.push((b.char_id, b.solar_system_id, b.beacon_id, b.beacon_type_id));
            false
        } else {
            true
        }
    });
    let mut expired_bridges = Vec::new();
    st.bridges.retain(|&(k, _), br| {
        if k == ctx_k && br.expires_at_unix <= now {
            expired_bridges.push((br.origin_system_id, br.bridger_ship_id, br.portal_module_id, br.portal_ball_id));
            false
        } else {
            true
        }
    });
    drop(st);
    cleanup_expired_beacons(ctx, expired_beacons);
    for (sys_id, bridger_ship_id, portal_mod_id, ball_id) in expired_bridges {
        let sys = ctx.systems.get_or_spawn(sys_id);
        if ball_id != 0 {
            sys.try_remove_ball(ball_id);
        }
        if portal_mod_id != 0 {
            sys.try_remove_module_cycle(bridger_ship_id, portal_mod_id);
        }
    }
}

fn ship_has_fitted_covert_cloak(
    snap: &eve_db::WorldStateSnapshot,
    ctx: &ServerContext,
    ship_id: i64,
) -> bool {
    snap.items.values().any(|i| {
        i.location_id == ship_id
            && matches!(i.flag as u16, 11..=34 | 125..=132)
            && ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                t.group_id == 330
                    && (t.type_id == 11578
                        || t.type_id == 20563
                        || (type_attr(ctx, t, 1298) > 0.0 && (type_attr(ctx, t, 306) - 1.0).abs() < 1e-6))
            })
    })
}

/// `beyonce.CmdBridgeToFleetModuleBeacon` / `beyonce.CmdBridgeToFleetDeployableBeacon`: opens a jump portal.
// parity: feat:ships.titan-jump-portal-bridge, feat:modules.jump-portal-bridge-rules
pub(super) async fn bridge_to_fleet_beacon(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<PyRep, BridgeRpcError> {
    let rpc = crate::router::RpcArgs::positional(args);
    let cyno_char = rpc.req_positive_i64(0, "cynoCharID")?;
    let target_sys = rpc.opt_u32(1, "targetSystemID")?.unwrap_or(0);
    let beacon_id = rpc.opt_i64(2, "beaconID")?.unwrap_or(0);
    if (31_000_000..32_000_000).contains(&target_sys) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {target_sys}"),
        ));
    }
    if (32_000_000..=32_999_999).contains(&target_sys) {
        return Err(err(
            "JumpDriveForbiddenInAbyssalSpace",
            format!("Jump drives cannot target Abyssal Deadspace system {target_sys}"),
        ));
    }
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session, true).await?;
    if sys_id == 30_100_000 || target_sys == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Jump portals cannot operate in or into Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&target_sys).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents bridging into Pochven system {target_sys}"),
        ));
    }
    if target_sys != 0 {
        let (vals, _) = evaluated_attrs(ctx, char_id, ship_id, &[ATTR_JUMP_DRIVE_RANGE], None);
        let dist = distance_ly(ctx, sys_id, target_sys);
        if dist > vals[0] {
            return Err(err(
                "JumpDistanceOutOfRange",
                format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0]),
            ));
        }
    }
    let beacon = resolve_fleet_beacon(ctx, char_id, cyno_char, beacon_id, target_sys, is_deployable).await?;
    bridge_to_resolved_beacon(ctx, session, beacon, None).await
}

/// Opens an active jump portal (`ActiveJumpBridge`) toward an already-resolved fleet, deployable,
/// or Pharolux structure beacon.
pub(crate) async fn bridge_to_resolved_beacon(
    ctx: &ServerContext,
    session: &SessionState,
    beacon: FleetModuleBeacon,
    structure_id_opt: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session, true).await?;
    if sys_id == 30_100_000 || beacon.solar_system_id == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Jump portals cannot operate in or into Zarzakh"));
    }
    let dest_sec = ctx
        .sde
        .solar_systems
        .get(&beacon.solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if dest_sec >= 0.45 {
        return Err(err(
            "CannotJumpToHighSec",
            format!("Jump portals cannot target high-security system {}", beacon.solar_system_id),
        ));
    }
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id),
        ));
    }
    if (32_000_000..=32_999_999).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInAbyssalSpace",
            format!("Jump portals cannot target Abyssal Deadspace system {}", beacon.solar_system_id),
        ));
    }
    if ctx.sde.solar_systems.get(&beacon.solar_system_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents bridging into Pochven system {}", beacon.solar_system_id),
        ));
    }
    check_cyno_jammer_for_beacon(ctx, &beacon)?;
    let (ship_type_id, portal) = ctx.db.with_snapshot(|s| {
        let st = s.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        let portal = s
            .items
            .values()
            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34))
            .filter(|i| s.module_online_states.get(&i.item_id).copied().unwrap_or(true))
            .find(|i| {
                ctx.sde
                    .types
                    .get(&i.type_id)
                    .is_some_and(|t| type_attr(ctx, t, ATTR_ENABLE_OPEN_JUMP_PORTAL) > 0.0)
            })
            .map(|i| (i.item_id, i.type_id));
        (st, portal)
    });
    let (portal_id, portal_tid) =
        portal.ok_or_else(|| err("NoJumpPortalGenerator", "No online jump portal generator fitted"))?;
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    let portal_type = ctx.sde.types.get(&portal_tid).ok_or_else(|| err("ModuleNotFound", "Portal type unknown"))?;
    if !hull_allowed(ctx, portal_type, ship_type) {
        return Err(err(
            "ShipCannotBridgeToBeacon",
            format!("{} cannot operate {}", ship_type.name, portal_type.name),
        ));
    }
    let is_covert_portal = portal_tid == 28652
        || type_attr(ctx, portal_type, ATTR_IS_COVERT) > 0.0
        || type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_PASSENGER_REQUIRED_ATTR) as AttributeId == 3320;
    let eff_beacon_type = effective_beacon_field_type(ctx, beacon.beacon_type_id);
    let is_industrial_beacon = ctx
        .sde
        .types
        .get(&eff_beacon_type)
        .is_some_and(|t| type_attr(ctx, t, ATTR_IS_INDUSTRIAL_CYNO) > 0.0);
    let portal_matches_beacon = if is_covert_portal {
        is_covert_beacon_type(eff_beacon_type) || is_industrial_beacon
    } else {
        !is_covert_beacon_type(eff_beacon_type)
    };
    let accepts_beacon = ship_accepts_beacon(ctx, ship_type, eff_beacon_type) && portal_matches_beacon;
    if !accepts_beacon {
        return Err(err(
            "ShipCannotBridgeToBeacon",
            format!("{} cannot bridge to beacon type {}", ship_type.name, beacon.beacon_type_id),
        ));
    }
    let (vals, mod_vals) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[ATTR_JUMP_DRIVE_RANGE],
        Some((portal_id, &[ATTR_CONSUMPTION_QUANTITY, ATTR_JUMP_PORTAL_DURATION])),
    );
    let dist = distance_ly(ctx, sys_id, beacon.solar_system_id);
    if dist > vals[0] {
        return Err(err(
            "JumpDistanceOutOfRange",
            format!("Distance {dist:.3} LY exceeds portal range {:.3} LY", vals[0]),
        ));
    }
    let act_type = type_attr(ctx, portal_type, ATTR_CONSUMPTION_TYPE) as TypeId;
    let jpg_lvl = ctx.db.with_snapshot(|s| {
        s.skills
            .get(&char_id)
            .and_then(|sk| sk.get(&24562))
            .map(|r| r.level)
            .unwrap_or(0)
    });
    let raw_act_qty = type_attr(ctx, portal_type, ATTR_CONSUMPTION_QUANTITY);
    let act_qty = if mod_vals[0] > 0.0 && (mod_vals[0] - raw_act_qty).abs() > 1e-3 {
        mod_vals[0].ceil() as i64
    } else if jpg_lvl > 0 && raw_act_qty > 0.0 {
        (raw_act_qty * (1.0 - 0.10 * (jpg_lvl.min(5) as f64))).ceil() as i64
    } else {
        raw_act_qty.ceil() as i64
    };
    let dur_ms = if mod_vals[1] > 0.0 {
        mod_vals[1]
    } else {
        type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_DURATION)
    };
    let duration_s = dur_ms / 1000.0;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let now_reserve = ctx.now_unix_secs();
    {
        let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
        super::fitting_cyno_portal::purge_expired_bridges(&mut st, ctx_key(ctx), now_reserve);
        if st.bridges.contains_key(&(ctx_key(ctx), ship_id)) {
            return Err(err(
                "JumpBridgeAlreadyActive",
                "A jump portal bridge is already active on this ship",
            ));
        }
        st.bridges.insert(
            (ctx_key(ctx), ship_id),
            ActiveJumpBridge {
                bridger_char_id: char_id,
                bridger_ship_id: ship_id,
                portal_module_id: portal_id,
                portal_module_type_id: portal_tid,
                origin_system_id: sys_id,
                beacon: beacon.clone(),
                distance_ly: dist,
                expires_at_unix: now_reserve + duration_s.round() as u64 + 60,
                portal_ball_id: -ship_id,
            },
        );
    }
    let clear_bridge_claim = || {
        let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
        if st.bridges.get(&(ctx_key(ctx), ship_id)).is_some_and(|b| b.portal_ball_id == -ship_id) {
            st.bridges.remove(&(ctx_key(ctx), ship_id));
        }
    };
    if let Err(e) = sys.add_module_cycle(self_immobilizing_cycle(
        ship_id,
        portal_id,
        portal_tid,
        "jumpPortalGeneration",
        duration_s,
    ))
    .await
    {
        clear_bridge_claim();
        return Err(err("ModuleActivationFailed", e));
    }
    if act_type != 0 && act_qty > 0 {
        if let Err(e) = consume_or_err(ctx, ship_id, act_type, act_qty, |have| {
            format!("Portal activation requires {act_qty} x type {act_type}, ship holds {have}")
        }) {
            clear_bridge_claim();
            let _ = sys.remove_module_cycle(ship_id, portal_id).await;
            return Err(e);
        }
    }
    let portal_ball_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let bridger_pos = sys.get_ball_position_sync(ship_id).unwrap_or(DVec3::ZERO);
    let portal_pos = bridger_pos + DVec3::new(100.0, 0.0, 0.0);
    let _ = sys
        .spawn_ball(Ball {
            id: portal_ball_id,
            type_id: 32731, // Default Physical Portal
            position: portal_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let expires_at_unix = ctx.now_unix_secs() + duration_s.round() as u64;
    cyno_state()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .bridges
        .insert(
            (ctx_key(ctx), ship_id),
            ActiveJumpBridge {
                bridger_char_id: char_id,
                bridger_ship_id: ship_id,
                portal_module_id: portal_id,
                portal_module_type_id: portal_tid,
                origin_system_id: sys_id,
                beacon: beacon.clone(),
                distance_ly: dist,
                expires_at_unix,
                portal_ball_id,
            },
        );
    if let Some(fleet_id) = ctx.db.with_snapshot(|s| fleet_of(s, char_id)) {
        // DERIVED argument order from the client `fleetSvc.OnBridgeModeChange` handler locals.
        ctx.emit_typed(
            crate::router::NotificationTarget::Fleet(fleet_id),
            OnBridgeModeChange {
                ship_id,
                solar_system_id: beacon.solar_system_id as i64,
                item_id: beacon.beacon_id,
                active: true,
            },
        );
    }
    let mut entries = vec![
        ("shipID", PyRep::Integer(ship_id)),
        ("moduleID", PyRep::Integer(portal_id)),
        ("beaconID", PyRep::Integer(beacon.beacon_id)),
        ("solarSystemID", PyRep::Integer(beacon.solar_system_id as i64)),
        ("portalBallID", PyRep::Integer(portal_ball_id)),
        ("bridgeBallID", PyRep::Integer(portal_ball_id)),
        ("distanceLY", PyRep::Real(dist)),
        ("activationConsumptionTypeID", PyRep::Integer(act_type as i64)),
        ("activationConsumptionQuantity", PyRep::Integer(if act_type != 0 { act_qty } else { 0 })),
        ("durationSeconds", PyRep::Real(duration_s)),
        ("bridgeActive", PyRep::Bool(true)),
    ];
    if let Some(sid) = structure_id_opt {
        entries.push(("structureID", PyRep::Integer(sid)));
    }
    Ok(PyRep::keyval(entries))
}

/// `beyonce.CmdGroupJumpToFleetModuleBeacon` / `beyonce.CmdGroupJumpToFleetDeployableBeacon`: conduit jump leader + nearby fleet members.
// parity: feat:navigation.conduit-jump
pub(super) async fn group_jump_to_fleet_beacon(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<JumpOutcome, BridgeRpcError> {
    let rpc = crate::router::RpcArgs::positional(args);
    let cyno_char = rpc.req_positive_i64(0, "cynoCharID")?;
    let target_sys = rpc.opt_u32(1, "targetSystemID")?.unwrap_or(0);
    let beacon_id = rpc.opt_i64(2, "beaconID")?.unwrap_or(0);
    if (31_000_000..32_000_000).contains(&target_sys) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {target_sys}"),
        ));
    }
    if (32_000_000..=32_999_999).contains(&target_sys) {
        return Err(err(
            "JumpDriveForbiddenInAbyssalSpace",
            format!("Jump drives cannot target Abyssal Deadspace system {target_sys}"),
        ));
    }
    if target_sys != 0
        && (!ctx.sde.solar_systems.contains_key(&target_sys)
            || Some(target_sys) == session.solar_system_id)
    {
        return Err(err(
            "InvalidJumpDestination",
            format!("Invalid jump destination solar system {target_sys}"),
        ));
    }
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session, false).await?;
    if sys_id == 30_100_000 || target_sys == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Conduit jumps cannot operate in or into Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&target_sys).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents conduit jumps into Pochven system {target_sys}"),
        ));
    }
    if target_sys != 0 {
        let (vals, _) = evaluated_attrs(ctx, char_id, ship_id, &[ATTR_JUMP_DRIVE_RANGE], None);
        let dist = distance_ly(ctx, sys_id, target_sys);
        if dist > vals[0] {
            return Err(err(
                "JumpDistanceOutOfRange",
                format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0]),
            ));
        }
    }
    let (beacon, landed_at_star) =
        resolve_fleet_beacon_or_destroyed(ctx, char_id, cyno_char, beacon_id, target_sys, is_deployable).await?;
    group_jump_to_resolved_beacon(ctx, session, beacon, landed_at_star)
        .await
        .map(|(outcome, _)| outcome)
}

/// Executes conduit jump for the caller + eligible nearby fleet members toward an already-resolved
/// fleet, deployable, or Pharolux structure beacon. Returns `(JumpOutcome, total_jumped_count)`.
pub(crate) async fn group_jump_to_resolved_beacon(
    ctx: &ServerContext,
    session: &mut SessionState,
    beacon: FleetModuleBeacon,
    landed_at_star: bool,
) -> Result<(JumpOutcome, usize), BridgeRpcError> {
    let (char_id, ship_id, sys_id, caller_ball) = caller_in_space(ctx, session, false).await?;
    if !ctx.sde.solar_systems.contains_key(&beacon.solar_system_id)
        || beacon.solar_system_id == sys_id
    {
        return Err(err(
            "InvalidJumpDestination",
            format!("Invalid jump destination solar system {}", beacon.solar_system_id),
        ));
    }
    if sys_id == 30_100_000 || beacon.solar_system_id == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Conduit jumps cannot operate in or into Zarzakh"));
    }
    let dest_sec = ctx
        .sde
        .solar_systems
        .get(&beacon.solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if dest_sec >= 0.45 {
        return Err(err(
            "CannotJumpToHighSec",
            format!("Conduit jumps cannot target high-security system {}", beacon.solar_system_id),
        ));
    }
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id),
        ));
    }
    if (32_000_000..=32_999_999).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInAbyssalSpace",
            format!("Conduit jumps cannot target Abyssal Deadspace system {}", beacon.solar_system_id),
        ));
    }
    if ctx.sde.solar_systems.get(&beacon.solar_system_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents conduit jumps into Pochven system {}", beacon.solar_system_id),
        ));
    }
    check_cyno_jammer_for_beacon(ctx, &beacon)?;
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    if ship_type.group_id == 30 {
        return Err(err("TitanCannotConduitJump", "Titans cannot perform conduit jumps"));
    }
    let flat_conduit_per_ly = type_attr(ctx, ship_type, ATTR_CONDUIT_CONSUMPTION);
    if flat_conduit_per_ly <= 0.0 {
        return Err(err(
            "ShipCannotConduitJump",
            format!("{} is not capable of conduit jumping", ship_type.name),
        ));
    }
    let accepts_beacon = ship_accepts_beacon(ctx, ship_type, effective_beacon_field_type(ctx, beacon.beacon_type_id));
    if type_attr(ctx, ship_type, ATTR_CAN_JUMP) <= 0.0 || !accepts_beacon {
        return Err(err(
            "ShipCannotJumpToBeacon",
            format!("{} cannot jump to beacon type {}", ship_type.name, beacon.beacon_type_id),
        ));
    }
    let is_rorqual = ship_type.group_id == 883;
    let is_blops = ship_type.group_id == 898;
    let (jdo_lvl, cjpg_lvl, indy_core_active) = ctx.db.with_snapshot(|s| {
        let sk = s.skills.get(&char_id);
        let jdo = sk.and_then(|m| m.get(&3456)).map(|r| r.level).unwrap_or(0);
        let cjpg = sk.and_then(|m| m.get(&83094)).map(|r| r.level).unwrap_or(0);
        let core = s.items.values().any(|i| {
            i.location_id == ship_id
                && s.module_active_states.contains(&i.item_id)
                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 515)
        });
        (jdo, cjpg, core)
    });
    if is_rorqual && indy_core_active {
        return Err(err("IndustrialCoreActive", "Rorqual cannot conduit jump while Industrial Core is active"));
    }
    let (vals, _) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[ATTR_JUMP_DRIVE_RANGE, ATTR_JUMP_FATIGUE_MULTIPLIER, 898],
        None,
    );
    let dist = distance_ly(ctx, sys_id, beacon.solar_system_id);
    if dist > vals[0] {
        return Err(err(
            "JumpDistanceOutOfRange",
            format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0]),
        ));
    }
    let fuel_type = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_CONSUMPTION_TYPE) as TypeId;
    let fuel_qty = (flat_conduit_per_ly * dist).ceil() as i64;
    let raw_cap_need = type_attr(ctx, ship_type, 898);
    let base_cap_need = if vals[2] > 0.0 { vals[2] } else { raw_cap_need };
    let cap_frac = if jdo_lvl > 0 && (vals[2] <= 0.0 || vals[2] >= raw_cap_need) {
        (base_cap_need * (1.0 - 0.05 * (jdo_lvl.min(5) as f64))).max(0.0)
    } else {
        base_cap_need.max(0.0)
    };
    let origin_handle = ctx.systems.get_or_spawn(sys_id);
    let mut post_jump_cap = None;
    if let Ok(Some(cs)) = origin_handle.get_combat_state(ship_id).await {
        let cap_needed = cs.max_capacitor * cap_frac;
        if cs.capacitor + 1e-4 < cap_needed {
            return Err(err(
                "NotEnoughCapacitor",
                format!("Conduit jump requires {cap_needed:.1} GJ capacitor, ship has {:.1} GJ", cs.capacitor),
            ));
        }
        post_jump_cap = Some((cs.capacitor - cap_needed).max(0.0));
    }
    if crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, ctx.now_unix(), None).is_some() {
        return Err(err("SessionChangeTimerActive", "Session change timer is active"));
    }
    consume_or_err(ctx, ship_id, fuel_type, fuel_qty, |have| {
        format!("Conduit jump requires {fuel_qty} x type {fuel_type}, ship holds {have}")
    })?;
    let max_passengers: usize = if is_blops || is_rorqual {
        30
    } else if matches!(ship_type.group_id, 547 | 659 | 5120) {
        let base = (5 + 5 * cjpg_lvl.min(5)) as usize;
        if ship_type.group_id == 5120 { base * 2 } else { base }
    } else {
        30
    };

    let members_to_jump = ctx.db.with_snapshot(|snap| {
        let Some(fid) = fleet_of(snap, char_id) else {
            return Vec::new();
        };
        let Some(fleet) = snap.fleets.get(&fid) else {
            return Vec::new();
        };
        fleet
            .members
            .iter()
            .filter_map(|&mid| {
                if mid == char_id {
                    return None;
                }
                let mem = fleet.member_details.get(&mid)?;
                if !mem.accepts_conduit_jumps {
                    return None;
                }
                let ch = snap.characters.get(&mid)?;
                if ch.solar_system_id != sys_id
                    || ch.active_ship_id == 0
                    || ch.station_id.is_some()
                    || ch.structure_id.is_some()
                {
                    return None;
                }
                let ship_item = snap.items.get(&ch.active_ship_id)?;
                let pt = ctx.sde.types.get(&ship_item.type_id)?;
                let eligible_hull = matches!(
                    pt.group_id,
                    29 | 31
                        | 237
                        | 25
                        | 324
                        | 830
                        | 831
                        | 834
                        | 893
                        | 1283
                        | 1527
                        | 420
                        | 541
                        | 1305
                        | 1534
                        | 26
                        | 358
                        | 832
                        | 833
                        | 894
                        | 906
                        | 963
                        | 1972
                        | 419
                        | 540
                        | 1201
                        | 27
                        | 898
                        | 900
                        | 381
                );
                let class_ok = if is_blops {
                    type_attr(ctx, pt, 3322) > 0.0
                        || is_covert_ops_cloak_capable(ctx, pt)
                        || ship_has_fitted_covert_cloak(snap, ctx, ch.active_ship_id)
                } else if is_rorqual {
                    type_attr(ctx, pt, ATTR_IS_INDY_CONDUIT_PASSENGER) > 0.0
                        || matches!(pt.group_id, 463 | 543 | 1283)
                        || matches!(pt.type_id, 32880 | 42244)
                } else {
                    eligible_hull
                };
                if !class_ok {
                    return None;
                }
                Some((mid, ch.active_ship_id))
            })
            .collect::<Vec<_>>()
    });

    let now = ctx.now_unix();
    let mut qualified_passengers = Vec::new();
    for (m_char, m_ship) in members_to_jump {
        if qualified_passengers.len() >= max_passengers {
            break;
        }
        let cd_until = ctx.db.with_snapshot(|s| s.characters.get(&m_char).map(|c| c.jump_cooldown_until_unix).unwrap_or(0));
        if cd_until > now
            || crate::services::space_destiny::check_session_change_timer_active(ctx, m_char, now, None).is_some()
            || crate::services::combat_crimewatch::check_crimewatch_action_restriction(ctx, m_char, m_ship, sys_id, "jump_drive", now).is_err()
        {
            continue;
        }
        let valid_move = ctx
            .validate_ship_movement(
                m_char,
                m_ship,
                crate::services::space_nav_movement_guard::MovementIntent::ConduitJump {
                    from_sys: sys_id,
                    to_sys: beacon.solar_system_id,
                    allow_bubbled: false,
                },
            )
            .await;
        if let Ok(vm) = valid_move {
            let is_tethered = vm.combat_state.as_ref().is_some_and(|cs| cs.is_tethered);
            if !is_tethered
                && vm
                    .ball
                    .as_ref()
                    .is_some_and(|b| b.position.distance(caller_ball.position) <= 10_000.0)
            {
                qualified_passengers.push((m_char, m_ship));
            }
        }
    }

    let mut jumped_passengers = Vec::new();
    for (m_char, m_ship) in &qualified_passengers {
        let p_dest = if landed_at_star {
            star_arrival_position(ctx, beacon.solar_system_id)
        } else {
            random_arrival_position(ctx, beacon.position)
        };
        let (pvals, _) = evaluated_attrs(
            ctx,
            *m_char,
            *m_ship,
            &[ATTR_JUMP_THROUGH_FATIGUE_MULTIPLIER, ATTR_JUMP_FATIGUE_MULTIPLIER],
            None,
        );
        let p_type_id = ctx.db.with_snapshot(|s| s.items.get(m_ship).map(|i| i.type_id)).unwrap_or(0);
        let p_hull_bonus = crate::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(ctx, p_type_id);
        let p_fatigue_mult = if p_hull_bonus >= 0.90 {
            0.10
        } else if pvals[0] > 0.0 && (pvals[0] - 1.0).abs() > 1e-6 {
            pvals[0]
        } else if p_hull_bonus > 0.0 {
            (1.0 - p_hull_bonus).clamp(0.0, 1.0)
        } else if pvals[1] > 0.0 {
            pvals[1]
        } else {
            1.0
        };
        if relocate_ship(ctx, *m_char, *m_ship, sys_id, beacon.solar_system_id, p_dest, dist, p_fatigue_mult)
            .await
            .is_ok()
        {
            jumped_passengers.push(*m_char);
            crate::services::map_service::record_solar_system_jump(ctx, *m_char, beacon.solar_system_id);
            ctx.emit_typed_with_id_type(
                crate::router::NotificationTarget::Character(*m_char),
                "clientID",
                OnSessionChanged {
                    is_remote: false,
                    session: PyRep::Integer(0),
                    change: PyRep::Dict(vec![
                        (
                            PyRep::String("solarsystemid".into()),
                            PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(beacon.solar_system_id as i64)]),
                        ),
                        (
                            PyRep::String("solarsystemid2".into()),
                            PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(beacon.solar_system_id as i64)]),
                        ),
                    ]),
                },
            );
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(*m_char),
                OnGroupJumpedPassenger {
                    anchor_char_id: char_id,
                    to_solar_system_id: beacon.solar_system_id as i64,
                },
            );
        }
    }
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        OnGroupJumpAnchorActivated {
            to_solar_system_id: beacon.solar_system_id as i64,
            num_passengers: jumped_passengers.len() as i64,
        },
    );

    let caller_dest = if landed_at_star {
        star_arrival_position(ctx, beacon.solar_system_id)
    } else {
        random_arrival_position(ctx, beacon.position)
    };
    let (cooldown_min, fatigue_min, bound) = relocate_caller(
        ctx,
        session,
        char_id,
        ship_id,
        sys_id,
        beacon.solar_system_id,
        caller_dest,
        dist,
        vals[1],
    )
    .await?;
    if let Some(rem_cap) = post_jump_cap {
        let dest_sys = ctx.systems.get_or_spawn(beacon.solar_system_id);
        if let Ok(Some(mut dcs)) = dest_sys.get_combat_state(ship_id).await {
            dcs.capacitor = rem_cap;
            let _ = dest_sys.set_combat_state(ship_id, dcs).await;
        }
    }
    let total_jumped = 1 + jumped_passengers.len();
    Ok((
        JumpOutcome {
            dist_ly: dist,
            fuel_type,
            fuel_qty,
            cooldown_min,
            fatigue_min,
            beyonce_bound_id: bound,
        },
        total_jumped,
    ))
}

/// `beyonce.CmdJumpThroughFleet(otherCharID, otherShipID, beaconID?, toSolarsystemID?)`: passenger jumps through an open portal.
// parity: feat:modules.covert-jump-portal-generator, feat:modules.jump-portal-bridge-rules, feat:navigation.jump-arrival-placement
pub(super) async fn jump_through_fleet(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<JumpOutcome, BridgeRpcError> {
    let rpc = crate::router::RpcArgs::positional(args);
    let other_char = rpc.req_positive_i64(0, "cynoCharID")?;
    let other_ship = rpc.req_positive_i64(1, "cynoShipID")?;
    let req_beacon = rpc.opt_i64(2, "beaconID")?.unwrap_or(0);
    let req_dest_sys = rpc.opt_i64(3, "destSystemID")?.unwrap_or(0);
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix_secs();
    if let (Some(ship_id), Some(sys_id)) = (session.ship_id, session.solar_system_id) {
        crate::services::combat_crimewatch::check_crimewatch_action_restriction(
            ctx,
            char_id,
            ship_id,
            sys_id,
            "jump_drive",
            now,
        )?;
    }
    require_same_fleet(ctx, char_id, other_char)?;
    let key = (ctx_key(ctx), other_ship);
    let bridge = cyno_state().read().unwrap_or_else(|e| e.into_inner()).bridges.get(&key).cloned();
    let bridge = bridge
        .filter(|b| {
            b.bridger_ship_id == other_ship
                && b.expires_at_unix > now
                && (req_beacon <= 0 || b.beacon.beacon_id == req_beacon)
                && (req_dest_sys <= 0 || b.beacon.solar_system_id as i64 == req_dest_sys)
        })
        .ok_or_else(|| err("NoActiveBridge", format!("Ship {other_ship} has no open jump portal")))?;
    let (char_id, ship_id, sys_id, ball) = caller_in_space(ctx, session, false).await?;
    if bridge.origin_system_id != sys_id {
        return Err(err("NoActiveBridge", "Jump portal is in a different solar system"));
    }
    let origin_handle = ctx.systems.get_or_spawn(sys_id);
    if origin_handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_tethered)
    {
        return Err(err(
            "CannotJumpWhileTethered",
            "Cannot jump through a portal bridge while tethered to a structure",
        ));
    }
    let Some(bridger_pos) = origin_handle.get_ball_position_sync(bridge.bridger_ship_id) else {
        cyno_state().write().unwrap_or_else(|e| e.into_inner()).bridges.remove(&key);
        return Err(err("NoActiveBridge", "Bridging ship is no longer in space"));
    };
    let dist_m = ball.position.distance(bridger_pos);
    if dist_m > 2_500.0 {
        return Err(err(
            "OutOfPortalRange",
            format!("Ship is {dist_m:.1} m from bridging ship (must be <= 2,500 m)"),
        ));
    }
    let mut landed_at_star = false;
    let beacon = if bridge.beacon.beacon_type_id == 35_840 {
        ctx.db.with_snapshot(|snap| {
            crate::services::space_destiny::space_cyno_filaments::validate_structure_cyno_beacon(
                snap,
                char_id,
                bridge.beacon.beacon_id,
                Some(bridge.beacon.solar_system_id),
            )
        })?;
        bridge.beacon.clone()
    } else {
        match live_beacon(
            ctx,
            bridge.beacon.char_id,
            bridge.beacon.beacon_id,
            bridge.beacon.solar_system_id,
        ) {
            Ok(b) => b,
            Err(_) if bridge.beacon.ship_id == 0 => bridge.beacon.clone(),
            Err(e) => {
                let destroyed_b = cyno_state()
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .destroyed_beacons
                    .get(&(ctx_key(ctx), bridge.beacon.ship_id))
                    .cloned();
                if let Some(db_b) = destroyed_b.filter(|b| {
                    b.beacon_id == bridge.beacon.beacon_id
                        && b.solar_system_id == bridge.beacon.solar_system_id
                        && b.expires_at_unix > now
                }) {
                    landed_at_star = true;
                    db_b
                } else {
                    return Err(e);
                }
            }
        }
    };
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id),
        ));
    }
    if (32_000_000..=32_999_999).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInAbyssalSpace",
            format!("Jump portals cannot target Abyssal Deadspace system {}", beacon.solar_system_id),
        ));
    }
    if beacon.solar_system_id == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Jump portals cannot target Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&beacon.solar_system_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents bridging into Pochven system {}", beacon.solar_system_id),
        ));
    }
    check_cyno_jammer_for_beacon(ctx, &beacon)?;
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    let portal_type = ctx
        .sde
        .types
        .get(&bridge.portal_module_type_id)
        .ok_or_else(|| err("ModuleNotFound", "Portal type unknown"))?;
    let required_attr = type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_PASSENGER_REQUIRED_ATTR) as AttributeId;
    let (pvals, _) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[required_attr, ATTR_JUMP_THROUGH_FATIGUE_MULTIPLIER],
        None,
    );
    let passenger_ok = required_attr != 0 && (pvals[0] > 0.0 || type_attr(ctx, ship_type, required_attr) > 0.0);
    if !passenger_ok {
        return Err(err(
            "JumpBridgeRestrictedShip",
            format!("{} may not use this jump portal (requires attribute {required_attr})", ship_type.name),
        ));
    }
    let bridger_type = ctx
        .db
        .with_snapshot(|s| s.items.get(&bridge.bridger_ship_id).map(|i| i.type_id))
        .and_then(|t| ctx.sde.types.get(&t))
        .ok_or_else(|| err("ShipNotFound", "Bridging ship unknown"))?;
    let bridger_per_ly = effective_jump_drive_consumption_per_ly(
        ctx,
        bridge.bridger_char_id,
        bridge.bridger_ship_id,
        bridger_type,
    );
    let mass = if ball.mass > 0.0 { ball.mass } else { ship_type.mass };
    let fuel_type = type_attr(ctx, bridger_type, ATTR_JUMP_DRIVE_CONSUMPTION_TYPE) as TypeId;
    // DERIVED: portal fuel = bridger effective jumpDriveConsumptionAmount * jumpPortalConsumptionMassFactor * passenger mass * LY.
    let factor = type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_MASS_FACTOR);
    let fuel_qty = (bridger_per_ly * factor * mass * bridge.distance_ly).ceil() as i64;
    if crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, ctx.now_unix(), None).is_some() {
        return Err(err("SessionChangeTimerActive", "Session change timer is active"));
    }
    consume_or_err(ctx, bridge.bridger_ship_id, fuel_type, fuel_qty, |have| {
        format!("Bridge requires {fuel_qty} x type {fuel_type}, bridging ship holds {have}")
    })?;
    let pos = if landed_at_star {
        star_arrival_position(ctx, beacon.solar_system_id)
    } else {
        random_arrival_position(ctx, beacon.position)
    };
    let hull_bonus = crate::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(ctx, ship_type_id);
    let is_covert_bridge = bridger_type.group_id == 898 || required_attr == ATTR_IS_COVERT;
    let mut bridge_fatigue_mult: f64 = if is_covert_bridge { 0.25 } else { 1.0 };
    if hull_bonus > 0.0 {
        bridge_fatigue_mult = bridge_fatigue_mult.min((1.0 - hull_bonus).clamp(0.0, 1.0));
    }
    if pvals[1] > 0.0 && pvals[1] <= 1.0 {
        bridge_fatigue_mult = bridge_fatigue_mult.min(pvals[1]);
    }
    let (cooldown_min, fatigue_min, bound) = relocate_caller(
        ctx,
        session,
        char_id,
        ship_id,
        sys_id,
        beacon.solar_system_id,
        pos,
        bridge.distance_ly,
        bridge_fatigue_mult,
    )
    .await?;
    Ok(JumpOutcome {
        dist_ly: bridge.distance_ly,
        fuel_type,
        fuel_qty,
        cooldown_min,
        fatigue_min,
        beyonce_bound_id: bound,
    })
}

fn jump_outcome_rep(o: &JumpOutcome, sys_id: u32) -> PyRep {
    PyRep::keyval([
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("distanceLY", PyRep::Real(o.dist_ly)),
        ("fuelTypeID", PyRep::Integer(o.fuel_type as i64)),
        ("fuelConsumed", PyRep::Integer(o.fuel_qty)),
        ("jumpCooldownMinutes", PyRep::Real(o.cooldown_min)),
        ("jumpFatigueMinutes", PyRep::Real(o.fatigue_min)),
        ("beyonceBoundID", PyRep::Integer(o.beyonce_bound_id as i64)),
    ])
}

async fn handle_jump_to_fleet_beacon_outcome(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<PyRep, BridgeRpcError> {
    jump_to_fleet_beacon(ctx, session, args, is_deployable)
        .await
        .map(|o| jump_outcome_rep(&o, session.solar_system_id.unwrap_or(0)))
}

async fn handle_bridge_to_fleet_beacon(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<PyRep, BridgeRpcError> {
    bridge_to_fleet_beacon(ctx, session, args, is_deployable).await
}

async fn handle_group_jump_to_fleet_beacon_outcome(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<PyRep, BridgeRpcError> {
    group_jump_to_fleet_beacon(ctx, session, args, is_deployable)
        .await
        .map(|o| jump_outcome_rep(&o, session.solar_system_id.unwrap_or(0)))
}

/// RPC entry point for the cyno / jump portal surface of `beyonce`.
pub(crate) async fn handle_cyno_portal(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("beyonce", "CmdJumpToFleetModuleBeacon") => {
            Some(handle_jump_to_fleet_beacon_outcome(ctx, session, args, false).await)
        }
        ("beyonce", "CmdJumpToFleetDeployableBeacon") => {
            Some(handle_jump_to_fleet_beacon_outcome(ctx, session, args, true).await)
        }
        // rpc-lint: allow legacy client alias CmdBridgeThroughStargate
        ("beyonce", "CmdBridgeToFleetModuleBeacon" | "CmdBridgeThroughStargate") => {
            Some(handle_bridge_to_fleet_beacon(ctx, session, args, false).await)
        }
        ("beyonce", "CmdBridgeToFleetDeployableBeacon") => {
            Some(handle_bridge_to_fleet_beacon(ctx, session, args, true).await)
        }
        ("beyonce", "CmdGroupJumpToFleetModuleBeacon") => {
            Some(handle_group_jump_to_fleet_beacon_outcome(ctx, session, args, false).await)
        }
        ("beyonce", "CmdGroupJumpToFleetDeployableBeacon") => {
            Some(handle_group_jump_to_fleet_beacon_outcome(ctx, session, args, true).await)
        }
        // rpc-lint: allow legacy client alias CmdJumpThroughPortal
        ("beyonce", "CmdJumpThroughFleet" | "CmdJumpThroughPortal") => Some(
            jump_through_fleet(ctx, session, args)
                .await
                .map(|o| jump_outcome_rep(&o, session.solar_system_id.unwrap_or(0))),
        ),
        ("beyonce", "GetCynoJammerState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(err("NoSolarSystem", "Session has no solarSystemID")));
            };
            // Client `cynoJammerIcon` treats the reply as the jammer's online sim time (`None` = not jammed).
            Some(Ok(cyno_jammer_state_ticks(ctx, sys_id).map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "fitting_conduit_jump_tests.rs"]
mod fitting_conduit_jump_tests;

#[cfg(test)]
#[path = "fitting_conduit_jump_extra_unit_tests.rs"]
mod fitting_conduit_jump_extra_unit_tests;
