//! `mobile_deployables_ext` — Extended V24.01 Mobile Deployable Mechanics (`feat:structures.mobile-cyno-beacon`,
//! `feat:structures.mobile-micro-jump-unit`, `feat:structures.mobile-observatory`,
//! `feat:structures.mobile-observatory-decloak-ping`, `feat:structures.mobile-scan-inhibitor`,
//! `feat:structures.mobile-siphon`).

#![forbid(unsafe_code)]

use crate::rng::MechanicRng;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::mobile_deployables::{
    read_deployable_state, read_deployable_state_snap, write_deployable_state,
    write_deployable_state_snap, MmjuSpoolEntry, MobileDeployableRecord, PendingDecloakNotice,
    COVERT_CYNO_BEACON_DURATION_SECS, CYNO_BEACON_DURATION_SECS, DEPOT_LIFETIME_SECS,
    GROUP_MOBILE_CYNO_BEACON, GROUP_MOBILE_DEPOT, GROUP_MOBILE_MICRO_JUMP_UNIT,
    GROUP_MOBILE_TRACTOR, MMJU_JUMP_DISTANCE_M, MMJU_LIFETIME_SECS, MMJU_MAX_SHIP_MASS_KG,
    MMJU_SPOOL_TIME_SECS, MTU_LIFETIME_SECS, TYPE_COVERT_MOBILE_CYNO_BEACON,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SdeRegistry};
use glam::DVec3;

pub const GROUP_MOBILE_SIPHON: u32 = 1247;
pub const GROUP_MOBILE_CYNO_INHIBITOR: u32 = 1249;
pub const GROUP_MOBILE_SCAN_INHIBITOR: u32 = 1275;
pub const GROUP_MOBILE_OBSERVATORY: u32 = 4107;

pub const TYPE_HIGHSEC_AUTH_CYNO_BEACON: u32 = 58906;
pub const TYPE_MOBILE_OBSERVATORY: u32 = 58904;
pub const TYPE_MOBILE_SCAN_INHIBITOR: u32 = 33589;

pub const OBSERVATORY_DEPLOY_DELAY_SECS: u64 = 600; // 10 minutes
pub const OBSERVATORY_ACTIVE_SECS: u64 = 7_200; // 2 hours
pub const OBSERVATORY_PING_INTERVAL_SECS: u64 = 600; // 10 minutes
pub const OBSERVATORY_TOTAL_PINGS: u32 = 12;
pub const OBSERVATORY_DECLOAK_PROB: f64 = 0.40; // 40% per ping after stabilization
pub const CLOAK_STABILIZATION_SECS: u64 = 900; // 15 minutes

pub const SCAN_INHIBITOR_DEPLOY_SECS: u64 = 60; // 1 minute
pub const SCAN_INHIBITOR_ACTIVE_SECS: u64 = 3_600; // 1 hour (SDE description)
pub const SCAN_INHIBITOR_RADIUS_M: f64 = 30_000.0; // 30 km
pub const SCAN_INHIBITOR_MIN_SAME_DIST_M: f64 = 100_000.0; // 100 km
pub const SCAN_INHIBITOR_MIN_INFRA_DIST_M: f64 = 75_000.0; // 75 km

pub const MMJU_MIN_SAME_DIST_M: f64 = 6_000.0; // 6 km
pub const MMJU_ACTIVATION_RANGE_M: f64 = 6_500.0; // 6,500 m surface distance
pub const MMJU_SPOOL_SECS_U64: u64 = 12;

pub const SIPHON_LIFETIME_SECS: u64 = 30 * 86_400; // persistent until unanchored/destroyed
pub const SIPHON_MAX_GRID_DIST_M: f64 = 150_000.0; // 150 km on-grid range
pub const SIPHON_CYCLE_SECS: u64 = 3_600; // 1 hour per siphon cycle

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Returns `true` if `group_id` is a single-use personal deployable that cannot be unanchored or scooped.
pub fn is_single_use_deployable(group_id: u32) -> bool {
    matches!(
        group_id,
        GROUP_MOBILE_MICRO_JUMP_UNIT
            | GROUP_MOBILE_CYNO_BEACON
            | GROUP_MOBILE_SCAN_INHIBITOR
            | GROUP_MOBILE_OBSERVATORY
            | GROUP_MOBILE_CYNO_INHIBITOR
    )
}

/// Resolves default anchoring/activation delay (seconds) when SDE dogma attribute `556` is absent.
pub fn default_deploy_delay_secs(sde: &SdeRegistry, type_id: u32, group_id: u32) -> f64 {
    if let Some(ms) = sde
        .types
        .get(&type_id)
        .and_then(|t| t.default_attributes.get(&556).copied())
    {
        return (ms / 1_000.0).max(1.0);
    }
    match group_id {
        GROUP_MOBILE_TRACTOR => 10.0,
        GROUP_MOBILE_OBSERVATORY => OBSERVATORY_DEPLOY_DELAY_SECS as f64,
        _ => 60.0,
    }
}

/// Resolves total lifetime (from `anchor_start_unix` until automatic decay) for a mobile deployable.
pub fn total_lifetime_secs(type_id: u32, group_id: u32, anchor_duration_secs: u64) -> u64 {
    match group_id {
        GROUP_MOBILE_DEPOT => DEPOT_LIFETIME_SECS,
        GROUP_MOBILE_TRACTOR => MTU_LIFETIME_SECS,
        GROUP_MOBILE_MICRO_JUMP_UNIT => anchor_duration_secs.saturating_add(MMJU_LIFETIME_SECS),
        GROUP_MOBILE_CYNO_BEACON => {
            let active = if type_id == TYPE_COVERT_MOBILE_CYNO_BEACON {
                COVERT_CYNO_BEACON_DURATION_SECS
            } else {
                CYNO_BEACON_DURATION_SECS
            };
            anchor_duration_secs.saturating_add(active)
        }
        GROUP_MOBILE_OBSERVATORY => anchor_duration_secs.saturating_add(OBSERVATORY_ACTIVE_SECS),
        GROUP_MOBILE_SCAN_INHIBITOR => anchor_duration_secs.saturating_add(SCAN_INHIBITOR_ACTIVE_SECS),
        GROUP_MOBILE_CYNO_INHIBITOR => 3_600,
        GROUP_MOBILE_SIPHON => SIPHON_LIFETIME_SECS,
        _ => 2 * 86_400,
    }
}

/// Resolves the pilot's active fleet ID from session or snapshot state.
pub fn caller_fleet_id(session: &SessionState, snap: &eve_db::WorldStateSnapshot, char_id: i64) -> Option<i64> {
    session.fleet_id.or_else(|| {
        snap.fleets
            .values()
            .find(|f| f.members.contains(&char_id))
            .map(|f| f.fleet_id)
    })
}

fn is_wormhole_system(sys_id: u32) -> bool {
    (31_000_000..32_000_000).contains(&sys_id)
}

fn is_pochven_system(sde: &SdeRegistry, sys_id: u32) -> bool {
    sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070)
}

fn is_abyssal_or_special_system(sys_id: u32) -> bool {
    sys_id >= 32_000_000
}

fn is_siphon_target_pos_group(group_id: u32) -> bool {
    matches!(group_id, 365 | 404 | 416 | 436 | 484 | 977)
}

/// Validates type-specific security, space-class, inhibitor/jammer, and proximity rules (`Step 4B`).
pub fn validate_ext_deployment_restrictions(
    ctx: &ServerContext,
    session: &SessionState,
    item_id: i64,
    type_id: u32,
    group_id: u32,
    sys_id: u32,
    anchor_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    let char_id = session.character_id.unwrap_or(0);
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);

    match group_id {
        GROUP_MOBILE_CYNO_BEACON => {
            let in_fleet = ctx
                .db
                .with_snapshot(|snap| caller_fleet_id(session, snap, char_id).is_some());
            if !in_fleet {
                return Err(err(
                    "MustBeInFleet",
                    "Deploying a Mobile Cynosural Beacon requires active fleet membership",
                ));
            }
            if sys_id == 30_100_000 {
                return Err(err(
                    "CynoDisallowedInZarzakh",
                    "Mobile Cynosural Beacons cannot be deployed in Zarzakh",
                ));
            }
            if is_wormhole_system(sys_id) {
                return Err(err(
                    "CantAnchorInWormhole",
                    "Mobile Cynosural Beacons cannot be deployed in wormhole space",
                ));
            }
            if is_pochven_system(&ctx.sde, sys_id) {
                return Err(err(
                    "CynoJammedInPochven",
                    "Region-wide Triglavian cyno jammer prevents deploying cynosural beacons in Pochven",
                ));
            }
            if is_abyssal_or_special_system(sys_id) {
                return Err(err(
                    "CantAnchorInAbyssal",
                    "Mobile Cynosural Beacons cannot be deployed in Abyssal space",
                ));
            }
            if sec >= 0.45 && type_id != TYPE_HIGHSEC_AUTH_CYNO_BEACON {
                return Err(err(
                    "CannotLightCynoInSystem",
                    format!("Mobile Cynosural Beacons cannot be deployed in high-security system {sys_id}"),
                ));
            }
            let is_covert = type_id == TYPE_COVERT_MOBILE_CYNO_BEACON
                || ctx
                    .sde
                    .types
                    .get(&type_id)
                    .and_then(|t| t.default_attributes.get(&1252).copied())
                    .unwrap_or(0.0)
                    > 0.0;
            if !is_covert {
                if crate::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(ctx, sys_id)
                    .is_some()
                {
                    return Err(err(
                        "CynoJammed",
                        format!("A system-wide cynosural jammer blocks Mobile Cynosural Beacon deployment in system {sys_id}"),
                    ));
                }
                let inhibited = ctx.db.with_snapshot(|snap| {
                    crate::services::structure_acl_defense_rules::is_cyno_inhibited(
                        ctx,
                        snap,
                        sys_id,
                        Some(anchor_pos),
                    )
                });
                if inhibited {
                    return Err(err(
                        "CynoInhibited",
                        "A Mobile Cynosural Inhibitor within 100 km blocks Mobile Cynosural Beacon deployment",
                    ));
                }
            }
        }
        GROUP_MOBILE_OBSERVATORY => {
            if sys_id == 30_100_000 {
                return Err(err(
                    "CantAnchorInZarzakh",
                    "Mobile Observatories cannot be deployed in Zarzakh",
                ));
            }
            if is_wormhole_system(sys_id) {
                return Err(err(
                    "CannotDeployInWormhole",
                    "Mobile Observatories cannot be deployed in wormhole space",
                ));
            }
            if is_pochven_system(&ctx.sde, sys_id) {
                return Err(err(
                    "CannotDeployInPochven",
                    "Mobile Observatories cannot be deployed in Pochven",
                ));
            }
            if is_abyssal_or_special_system(sys_id) {
                return Err(err(
                    "CannotDeployInAbyssalSpace",
                    "Mobile Observatories cannot be deployed in Abyssal space",
                ));
            }
            if sec >= 0.45 {
                return Err(err(
                    "CannotDeployInHighSec",
                    format!("Mobile Observatories cannot be deployed in high-security system {sys_id}"),
                ));
            }
            let in_deadspace = crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                reg.sites_by_system.get(&sys_id).is_some_and(|sites| {
                    sites.iter().any(|s| {
                        s.dungeon_id > 0
                            && (anchor_pos - DVec3::from_array(s.position)).length() <= 150_000.0
                    })
                })
            });
            if in_deadspace {
                return Err(err(
                    "CannotDeployInDeadspace",
                    "Mobile Observatory cannot be deployed in deadspace",
                ));
            }
        }
        GROUP_MOBILE_SCAN_INHIBITOR => {
            if sys_id == 30_100_000 || is_abyssal_or_special_system(sys_id) {
                return Err(err(
                    "RestrictionsInEffect",
                    "Mobile Scan Inhibitors cannot be deployed in Zarzakh or Abyssal space",
                ));
            }
            for gate in ctx.sde.stargates().values() {
                if gate.from_solar_system_id == sys_id
                    && (anchor_pos - gate.position).length() < SCAN_INHIBITOR_MIN_INFRA_DIST_M
                {
                    return Err(err(
                        "DeploymentTooCloseToGate",
                        "Mobile Scan Inhibitor cannot be deployed within 75 km of a stargate",
                    ));
                }
            }
            for station in ctx.sde.stations.values() {
                if station.solar_system_id == sys_id
                    && (anchor_pos - station.position).length() < SCAN_INHIBITOR_MIN_INFRA_DIST_M
                {
                    return Err(err(
                        "DeploymentTooCloseToStation",
                        "Mobile Scan Inhibitor cannot be deployed within 75 km of a station",
                    ));
                }
            }
            ctx.db.with_snapshot(|snap| {
                for st in snap.structures.values() {
                    if st.solar_system_id == sys_id && st.state != "Destroyed" {
                        if let Some(p) = snap.item_positions.get(&st.structure_id) {
                            if (anchor_pos - DVec3::from_array(*p)).length() < SCAN_INHIBITOR_MIN_INFRA_DIST_M {
                                return Err(err(
                                    "DeploymentTooCloseToStructure",
                                    "Mobile Scan Inhibitor cannot be deployed within 75 km of an Upwell structure",
                                ));
                            }
                        }
                    }
                }
                for it in snap.items.values() {
                    if it.item_id != item_id && it.location_id == sys_id as i64 {
                        let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                        if grp == GROUP_MOBILE_SCAN_INHIBITOR {
                            if let Some(p) = snap.item_positions.get(&it.item_id) {
                                if (anchor_pos - DVec3::from_array(*p)).length() < SCAN_INHIBITOR_MIN_SAME_DIST_M {
                                    return Err(err(
                                        "DeploymentTooCloseToSameType",
                                        "Mobile Scan Inhibitor cannot be deployed within 100 km of another Mobile Scan Inhibitor",
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok(())
            })?;
        }
        GROUP_MOBILE_MICRO_JUMP_UNIT => {
            if is_abyssal_or_special_system(sys_id) {
                return Err(err(
                    "RestrictionsInEffect",
                    "Mobile Micro Jump Units cannot be deployed in Abyssal space",
                ));
            }
            ctx.db.with_snapshot(|snap| {
                for it in snap.items.values() {
                    if it.item_id != item_id && it.location_id == sys_id as i64 {
                        let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                        if grp == GROUP_MOBILE_MICRO_JUMP_UNIT {
                            if let Some(p) = snap.item_positions.get(&it.item_id) {
                                if (anchor_pos - DVec3::from_array(*p)).length() < MMJU_MIN_SAME_DIST_M {
                                    return Err(err(
                                        "DeploymentTooCloseToSameType",
                                        "Mobile Micro Jump Unit cannot be deployed within 6 km of another Mobile Micro Jump Unit",
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok(())
            })?;
        }
        GROUP_MOBILE_SIPHON => {
            if sys_id == 30_100_000 || is_pochven_system(&ctx.sde, sys_id) || is_abyssal_or_special_system(sys_id) {
                return Err(err(
                    "RestrictionsInEffect",
                    "Mobile Siphon Units cannot be deployed in Pochven, Zarzakh, or Abyssal space",
                ));
            }
            let max_sec = ctx
                .sde
                .types
                .get(&type_id)
                .and_then(|t| t.default_attributes.get(&1032).copied())
                .unwrap_or(0.4);
            if sec >= max_sec + 0.05 {
                return Err(err(
                    "CantAnchorInHighSec",
                    format!("Mobile Siphon Unit cannot be deployed in high-security system {sys_id} (sec {sec:.2})"),
                ));
            }
            let has_target = ctx.db.with_snapshot(|snap| {
                snap.items.values().any(|it| {
                    it.location_id == sys_id as i64
                        && ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .is_some_and(|t| is_siphon_target_pos_group(t.group_id))
                        && snap
                            .item_positions
                            .get(&it.item_id)
                            .is_some_and(|p| (anchor_pos - DVec3::from_array(*p)).length() <= SIPHON_MAX_GRID_DIST_M)
                })
            });
            if !has_target {
                return Err(err(
                    "NoSiphonTargetInRange",
                    "Mobile Siphon Unit must be anchored within 150 km of an operational Starbase silo or reactor",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Records cloak activation timestamp (`now_unix`) on `ship_id` for the 15-minute Cloak Stabilization window.
pub fn record_ship_cloak_activated(ctx: &ServerContext, ship_id: i64) {
    let now_unix = ctx.now_unix();
    write_deployable_state(ctx, |st| {
        st.cloak_activated_unix.insert(ship_id, now_unix);
    });
}

/// Clears cloak activation timestamp when `ship_id` decloaks.
pub fn clear_ship_cloak_activated(ctx: &ServerContext, ship_id: i64) {
    write_deployable_state(ctx, |st| {
        st.cloak_activated_unix.remove(&ship_id);
    });
}

/// Returns `(item_id, position)` for all active (post-spool, pre-expiry) Mobile Scan Inhibitors (`33589`) in `sys_id`.
pub fn active_scan_inhibitor_positions(ctx: &ServerContext, sys_id: u32) -> Vec<(i64, DVec3)> {
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        st.deployables
            .values()
            .filter(|r| {
                r.solar_system_id == sys_id
                    && r.group_id == GROUP_MOBILE_SCAN_INHIBITOR
                    && !r.is_unanchored
                    && snap.items.contains_key(&r.item_id)
                    && now_unix < r.expires_at_unix
                    && (r.anchor_completed
                        || (r.anchor_start_unix > 0
                            && now_unix >= r.anchor_start_unix.saturating_add(r.anchor_duration_secs)))
            })
            .map(|r| {
                let pos = snap
                    .item_positions
                    .get(&r.item_id)
                    .copied()
                    .map(DVec3::from_array)
                    .unwrap_or_else(|| DVec3::from_array(r.position));
                (r.item_id, pos)
            })
            .collect()
    })
}

/// Returns `true` if `pos` lies within `30 km` of any active Mobile Scan Inhibitor in `inhibitors`.
pub fn is_pos_scan_inhibited(inhibitors: &[(i64, DVec3)], pos: DVec3) -> bool {
    inhibitors
        .iter()
        .any(|(_, ipos)| ipos.distance(pos) <= SCAN_INHIBITOR_RADIUS_M)
}

/// Handles `beyonce.CallComponentFromClient(unit_id, "microJumpDriver", "StartMicroJumpDriveForShip")`.
pub async fn handle_mmju_activate(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    unit_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    sync_system_deployables(ctx, sys_id).await;
    let now_unix = ctx.now_unix();

    let (rec, unit_pos, unit_radius) = ctx.db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        let rec = st.deployables.get(&unit_id).cloned()?;
        let it = snap.items.get(&unit_id)?;
        if it.location_id != sys_id as i64 || rec.group_id != GROUP_MOBILE_MICRO_JUMP_UNIT {
            return None;
        }
        let pos = snap
            .item_positions
            .get(&unit_id)
            .copied()
            .map(DVec3::from_array)
            .unwrap_or_else(|| DVec3::from_array(rec.position));
        let radius = ctx.sde.types.get(&it.type_id).map(|t| t.radius).unwrap_or(150.0);
        Some((rec, pos, radius))
    }).ok_or_else(|| err("ItemNotFound", format!("Mobile Micro Jump Unit {unit_id} is not present in system {sys_id}")))?;

    let is_active = !rec.is_unanchored
        && now_unix < rec.expires_at_unix
        && (rec.anchor_completed
            || (rec.anchor_start_unix > 0
                && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)));
    if !is_active {
        return Err(err(
            "DeployableAnchoring",
            "Mobile Micro Jump Unit has not yet completed its 60-second activation cycle",
        ));
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| err("DestinyCommandFailed", e))?
        .ok_or_else(|| err("ShipNotInSpace", "Active ship is not spawned in space"))?;

    if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
        return Err(err("ShipInWarp", "Cannot activate Mobile Micro Jump Unit while in warp"));
    }

    let surface_dist = ((ship_ball.position - unit_pos).length() - ship_ball.radius - unit_radius).max(0.0);
    if surface_dist > MMJU_ACTIVATION_RANGE_M {
        return Err(err(
            "OutOfRange",
            format!(
                "Ship is {surface_dist:.0} m from Mobile Micro Jump Unit (must be within {MMJU_ACTIVATION_RANGE_M:.0} m)"
            ),
        ));
    }

    let (ship_type_id, dyn_mass_add) = ctx.db.with_snapshot(|snap| {
        let tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(ship_ball.type_id);
        let plate_add: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(i.flag as u16, 11..=34 | 92..=99 | 125..=132)
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
            })
            .filter_map(|i| {
                let t = ctx.sde.types.get(&i.type_id)?;
                let m_act = snap.module_active_states.contains(&i.item_id);
                let base = if t.group_id == 46 && !m_act { 0.0 } else { t.default_attributes.get(&796).copied().unwrap_or(0.0) };
                let active_add = if m_act { t.default_attributes.get(&67).copied().unwrap_or(0.0) } else { 0.0 };
                Some(base + active_add)
            })
            .sum();
        (tid, plate_add)
    });
    let base_hull_mass = ctx.sde.types.get(&ship_type_id).map(|t| t.mass).unwrap_or(ship_ball.mass);
    let eval_mass = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|f| {
            f.evaluated_context(&ctx.sde)
                .ship
                .get_attribute(attr_ids::MASS, &ctx.sde)
                .max(ship_ball.mass)
                .max(base_hull_mass + dyn_mass_add)
        })
        .unwrap_or_else(|| ship_ball.mass.max(base_hull_mass + dyn_mass_add));
    if eval_mass >= MMJU_MAX_SHIP_MASS_KG {
        return Err(err(
            "ShipTooHeavy",
            format!(
                "Ship mass {eval_mass:.0} kg reaches or exceeds the 1,000,000,000 kg Mobile Micro Jump Unit limit"
            ),
        ));
    }

    let ship_cs = handle.get_combat_state(ship_id).await.ok().flatten();
    if ship_cs.as_ref().is_some_and(|cs| cs.is_cloaked) {
        return Err(err("ShipCloaked", "Cannot activate Mobile Micro Jump Unit while cloaked"));
    }
    let is_scrambled = handle.is_ball_scrambled(ship_id).await.unwrap_or(false)
        || ship_cs.is_some_and(|cs| cs.is_warp_scrambled() || cs.is_mwd_mjd_disabled);
    if is_scrambled {
        return Err(err(
            "WarpScrambled",
            "Cannot activate Mobile Micro Jump Unit while warp scrambled or propulsion-jammed",
        ));
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "jump")?;

    let dir = crate::services::mobile_deployables::mmju_jump_dir(&ship_ball);
    let dest_pos = ship_ball.position + dir * MMJU_JUMP_DISTANCE_M;
    crate::services::inventory_fitting::fitting_ship_forcefield::check_pos_forcefield_coverage(
        ctx,
        sys_id,
        char_id,
        ship_id,
        &[ship_ball.position, dest_pos],
    )
    .await?;
    let complete_at = now_unix.saturating_add(MMJU_SPOOL_SECS_U64);
    write_deployable_state(ctx, |st| {
        st.mmju_spools.insert(
            ship_id,
            MmjuSpoolEntry {
                ship_id,
                char_id,
                unit_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: complete_at,
                direction: dir.to_array(),
            },
        );
    });
    let _ = session;
    Ok(PyRep::keyval([
        ("unitID", PyRep::Integer(unit_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("spoolSeconds", PyRep::Real(MMJU_SPOOL_TIME_SECS)),
        ("completeAtUnix", PyRep::Integer(complete_at as i64)),
        ("jumpDistanceMeters", PyRep::Real(MMJU_JUMP_DISTANCE_M)),
    ]))
}

/// Processes pending Observatory pings, MMJU 12s spools, and Siphon cycles inside the solar system actor.
/// Runs BEFORE the expiry sweep in `step_mobile_deployables_in_actor` so final boundary pings/cycles execute cleanly.
pub fn step_ext_deployables_in_actor(
    ballpark: &mut eve_destiny::Ballpark,
    active_module_cycles: Option<&mut Vec<crate::system_actor::ActiveModuleCycle>>,
    deactivation_hook: Option<&crate::system_actor::ModuleDeactivationHook>,
    sde: &SdeRegistry,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    rng_opt: Option<&MechanicRng>,
    solar_system_id: u32,
    now_unix: u64,
) {
    step_mmju_spools_in_actor(
        ballpark,
        active_module_cycles.as_deref().map(Vec::as_slice),
        sde,
        db,
        solar_system_id,
        now_unix,
    );
    step_observatories_in_actor(
        ballpark,
        active_module_cycles,
        deactivation_hook,
        sde,
        db,
        rng_opt,
        solar_system_id,
        now_unix,
    );
    step_siphons_in_db(sde, db, solar_system_id, None, now_unix);
}

fn step_mmju_spools_in_actor(
    ballpark: &mut eve_destiny::Ballpark,
    active_module_cycles: Option<&[crate::system_actor::ActiveModuleCycle]>,
    sde: &SdeRegistry,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: u32,
    now_unix: u64,
) {
    let has_due = db.with_snapshot(|snap| {
        snap.owner_notes.values().any(|n| n.note.contains("\"mmju_spools\""))
            && read_deployable_state_snap(snap)
                .mmju_spools
                .values()
                .any(|e| e.solar_system_id == solar_system_id && now_unix >= e.complete_at_unix)
    });
    if !has_due {
        return;
    }
    let completed: Vec<MmjuSpoolEntry> = db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            let due_ships: Vec<i64> = st
                .mmju_spools
                .values()
                .filter(|e| e.solar_system_id == solar_system_id && now_unix >= e.complete_at_unix)
                .map(|e| e.ship_id)
                .collect();
            due_ships
                .into_iter()
                .filter_map(|sid| st.mmju_spools.remove(&sid))
                .collect()
        })
    });
    let siege_attr = crate::services::space_navigation_advanced::find_attr_id_by_name(sde, "siegeModeWarpStatus");
    let disallow_dock_attr = crate::services::space_navigation_advanced::find_attr_id_by_name(sde, "disallowDocking");
    let disallow_jump_attr = crate::services::space_navigation_advanced::find_attr_id_by_name(sde, "disallowDriveJumping");
    for entry in completed {
        let Some(ship_ball) = ballpark.balls.get(&entry.ship_id).cloned() else {
            continue;
        };
        let jump_dir = crate::services::mobile_deployables::mmju_jump_dir(&ship_ball);
        let dest_pos = ship_ball.position + jump_dir * MMJU_JUMP_DISTANCE_M;
        if active_module_cycles.is_some_and(|cycles| cycles.iter().any(|c| c.ship_id == entry.ship_id && c.immobilize_self)) {
            continue;
        }
        let valid_in_db = db.with_snapshot(|snap| {
            let st = read_deployable_state_snap(snap);
            let rec = st.deployables.get(&entry.unit_id)?;
            let is_active = !rec.is_unanchored
                && now_unix < rec.expires_at_unix
                && (rec.anchor_completed
                    || (rec.anchor_start_unix > 0
                        && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)));
            if rec.solar_system_id != solar_system_id || !is_active || !snap.items.contains_key(&entry.unit_id) {
                return None;
            }
            let pilot_aboard = snap.characters.get(&entry.char_id).is_some_and(|c| {
                c.active_ship_id == entry.ship_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.solar_system_id == solar_system_id
            });
            if !pilot_aboard {
                return None;
            }
            let skyhook_linked = snap
                .sov_runtime
                .skyhook_silo_links
                .values()
                .any(|&(_, sid, until, _)| sid == entry.ship_id && now_unix < until);
            let immobilizer_active = snap.items.values().any(|it| {
                it.location_id == entry.ship_id
                    && matches!(it.flag as u16, 11..=34 | 125..=132)
                    && snap.module_active_states.contains(&it.item_id)
                    && sde.types.get(&it.type_id).is_some_and(|t| {
                        let attr_nz = |aid: Option<u16>| {
                            aid.and_then(|a| t.default_attributes.get(&a).copied()).unwrap_or(0.0) != 0.0
                        };
                        attr_nz(siege_attr) || attr_nz(disallow_dock_attr) || attr_nz(disallow_jump_attr)
                    })
            });
            if skyhook_linked
                || immobilizer_active
                || crate::services::inventory_fitting::fitting_ship_forcefield::is_pos_forcefield_blocked_in_snap(
                    snap,
                    sde,
                    ballpark,
                    solar_system_id,
                    entry.char_id,
                    entry.ship_id,
                    &[ship_ball.position, dest_pos],
                )
            {
                return None;
            }
            let pos = snap
                .item_positions
                .get(&entry.unit_id)
                .copied()
                .map(DVec3::from_array)
                .unwrap_or_else(|| DVec3::from_array(rec.position));
            Some(pos)
        });
        let Some(unit_pos) = valid_in_db else {
            continue;
        };
        let is_blocked = ballpark.is_ball_scrambled(entry.ship_id)
            || ballpark.combat_states.get(&entry.ship_id).is_some_and(|cs| {
                cs.is_cloaked || cs.is_warp_scrambled() || cs.is_mwd_mjd_disabled
            });
        if is_blocked {
            continue;
        }
        let unit_radius = ballpark.balls.get(&entry.unit_id).map(|u| u.radius).unwrap_or(150.0);
        let surface_dist = ((ship_ball.position - unit_pos).length() - ship_ball.radius - unit_radius).max(0.0);
        let can_jump = !matches!(ship_ball.mode, MovementMode::Warp { .. })
            && surface_dist <= MMJU_ACTIVATION_RANGE_M
            && ship_ball.mass < MMJU_MAX_SHIP_MASS_KG;
        if !can_jump {
            continue;
        }
        if let Ok(new_pos) = ballpark.micro_jump(entry.ship_id, MMJU_JUMP_DISTANCE_M) {
            db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(entry.ship_id, new_pos.to_array());
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn step_observatories_in_actor(
    ballpark: &mut eve_destiny::Ballpark,
    mut active_module_cycles: Option<&mut Vec<crate::system_actor::ActiveModuleCycle>>,
    deactivation_hook: Option<&crate::system_actor::ModuleDeactivationHook>,
    sde: &SdeRegistry,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    rng_opt: Option<&MechanicRng>,
    solar_system_id: u32,
    now_unix: u64,
) {
    let (observatories, mut pings_done_map, cloak_starts): (
        Vec<MobileDeployableRecord>,
        std::collections::HashMap<i64, u32>,
        std::collections::HashMap<i64, u64>,
    ) = db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        let obs = st
            .deployables
            .values()
            .filter(|r| {
                r.solar_system_id == solar_system_id
                    && r.group_id == GROUP_MOBILE_OBSERVATORY
                    && !r.is_unanchored
                    && snap.items.contains_key(&r.item_id)
            })
            .cloned()
            .collect();
        (obs, st.observatory_pings_done, st.cloak_activated_unix)
    });
    if observatories.is_empty() {
        return;
    }

    let mut obs_sorted = observatories;
    obs_sorted.sort_by_key(|r| r.item_id);

    let mut cloaked_ships: Vec<(i64, f64)> = ballpark
        .combat_states
        .iter()
        .filter(|(_, cs)| cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
        .map(|(&sid, cs)| (sid, cs.cloak_stabilization_remaining_s))
        .collect();
    cloaked_ships.sort_by_key(|(sid, _)| *sid);

    let mut decloaked_ships: Vec<i64> = Vec::new();
    let mut updated_obs: Vec<(i64, u32)> = Vec::new();

    for obs in obs_sorted {
        let active_start = obs.anchor_start_unix.saturating_add(obs.anchor_duration_secs);
        if now_unix < active_start {
            continue;
        }
        let eff_now = now_unix.min(obs.expires_at_unix.saturating_sub(1));
        let due_pings = (((eff_now.saturating_sub(active_start)) / OBSERVATORY_PING_INTERVAL_SECS) as u32 + 1)
            .min(OBSERVATORY_TOTAL_PINGS);
        let done = pings_done_map.get(&obs.item_id).copied().unwrap_or(0);
        if due_pings <= done {
            continue;
        }
        pings_done_map.insert(obs.item_id, due_pings);
        updated_obs.push((obs.item_id, due_pings));

        for ping_idx in (done + 1)..=due_pings {
            let ping_ts = active_start.saturating_add(((ping_idx - 1) as u64) * OBSERVATORY_PING_INTERVAL_SECS);
            for &(ship_id, stab_rem_s) in &cloaked_ships {
                if decloaked_ships.contains(&ship_id) {
                    continue;
                }
                let cloak_start = cloak_starts.get(&ship_id).copied().unwrap_or(0);
                if stab_rem_s > 0.0 && ping_ts < cloak_start.saturating_add(CLOAK_STABILIZATION_SECS) {
                    continue;
                }
                if rng_opt.is_some_and(|r| r.roll(OBSERVATORY_DECLOAK_PROB)) {
                    decloaked_ships.push(ship_id);
                }
            }
        }
    }

    if updated_obs.is_empty() && decloaked_ships.is_empty() {
        return;
    }

    let mut notices = Vec::new();
    for &sid in &decloaked_ships {
        let mut removed_cloaks = Vec::new();
        if let Some(cycles) = active_module_cycles.as_deref_mut() {
            let mut kept = Vec::with_capacity(cycles.len());
            for cyc in cycles.drain(..) {
                let is_cloak = cyc.ship_id == sid
                    && (cyc.effect_name.to_ascii_lowercase().contains("cloak")
                        || sde.types.get(&cyc.module_type_id).is_some_and(|t| t.group_id == 330));
                if is_cloak {
                    removed_cloaks.push(cyc);
                } else {
                    kept.push(cyc);
                }
            }
            *cycles = kept;
        }

        let (pilot_opt, fallback_cloak_mod) = db.with_mut_snapshot(|snap| {
            for rc in &removed_cloaks {
                snap.module_active_states.remove(&rc.module_id);
                snap.module_active_charges.remove(&rc.module_id);
            }
            let pilot = snap
                .characters
                .values()
                .find(|c| c.active_ship_id == sid)
                .map(|c| c.character_id)
                .or_else(|| snap.items.get(&sid).map(|i| i.owner_id));
            let fb = if removed_cloaks.is_empty() {
                let found = snap
                    .items
                    .values()
                    .find(|i| {
                        i.location_id == sid
                            && sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 330)
                    })
                    .map(|i| (i.item_id, i.type_id));
                if let Some((mid, _)) = found {
                    snap.module_active_states.remove(&mid);
                }
                found
            } else {
                None
            };
            (pilot, fb)
        });

        if removed_cloaks.is_empty() {
            if let Some((mid, mtid)) = fallback_cloak_mod {
                removed_cloaks.push(crate::system_actor::ActiveModuleCycle {
                    ship_id: sid,
                    module_id: mid,
                    module_type_id: mtid,
                    owner_char_id: pilot_opt,
                    effect_name: "effects.Cloak".to_string(),
                    repeat: 1,
                    duration_s: 5.0,
                    ..Default::default()
                });
            }
        }

        let cloak_mod_id_opt = removed_cloaks.first().map(|c| c.module_id);
        let sde_tgt_delay_s = removed_cloaks
            .first()
            .and_then(|c| sde.types.get(&c.module_type_id))
            .and_then(|t| t.default_attributes.get(&560).copied())
            .map(|ms| if ms > 200.0 { ms / 1000.0 } else { ms })
            .unwrap_or(5.0)
            .max(5.0);
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            if cs.cloak_configured_targeting_delay_s <= 0.0 {
                cs.cloak_configured_targeting_delay_s = sde_tgt_delay_s;
            }
        }
        ballpark.deactivate_cloak(sid);
        if let Some(mut meta) = ballpark.ball_slim_meta.get(&sid).cloned() {
            meta.is_cloaked = false;
            ballpark.change_slim_item(sid, meta);
        }
        ballpark.emit_event(
            Some(sid),
            eve_destiny::DestinyEvent::OnSpecialFX {
                ball_id: sid,
                module_id: cloak_mod_id_opt,
                guid: "effects.Cloak".to_string(),
                is_active: false,
            },
        );
        if let (Some(cid), Some(hook)) = (pilot_opt, deactivation_hook) {
            for rc in &removed_cloaks {
                hook(cid, rc);
            }
        }
        if let Some(cid) = pilot_opt.filter(|&c| c > 0) {
            notices.push(PendingDecloakNotice {
                char_id: cid,
                ship_id: sid,
                module_id: cloak_mod_id_opt.unwrap_or(0),
            });
        }
    }

    db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            for (oid, cnt) in updated_obs {
                st.observatory_pings_done.insert(oid, cnt);
                if let Some(rec) = st.deployables.get_mut(&oid) {
                    rec.last_ping_unix = Some(now_unix);
                }
            }
            for &sid in &decloaked_ships {
                st.cloak_activated_unix.remove(&sid);
            }
            st.pending_decloak_notifications.extend(notices);
        });
    });
}

fn step_siphons_in_db(
    sde: &SdeRegistry,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    solar_system_id: u32,
    only_unit_id: Option<i64>,
    now_unix: u64,
) {
    let has_due_siphon = db.with_snapshot(|snap| {
        if !snap.owner_notes.values().any(|n| n.note.contains("1247")) {
            return false;
        }
        let st = read_deployable_state_snap(snap);
        st.deployables.values().any(|r| {
            if r.solar_system_id != solar_system_id
                || r.group_id != GROUP_MOBILE_SIPHON
                || !only_unit_id.is_none_or(|uid| r.item_id == uid)
                || r.is_unanchored
                || now_unix >= r.expires_at_unix
            {
                return false;
            }
            let active_t0 = r.anchor_start_unix.saturating_add(r.anchor_duration_secs);
            if now_unix < active_t0 {
                return false;
            }
            let last_t = st.siphon_last_cycle_unix.get(&r.item_id).copied().unwrap_or(active_t0).max(active_t0);
            now_unix.saturating_sub(last_t) >= SIPHON_CYCLE_SECS
        })
    });
    if !has_due_siphon {
        return;
    }
    db.with_mut_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        let mut units: Vec<(MobileDeployableRecord, u64)> = st
            .deployables
            .values()
            .filter(|r| {
                r.solar_system_id == solar_system_id
                    && r.group_id == GROUP_MOBILE_SIPHON
                    && only_unit_id.is_none_or(|uid| r.item_id == uid)
                    && !r.is_unanchored
                    && snap.items.contains_key(&r.item_id)
                    && now_unix < r.expires_at_unix
                    && (r.anchor_completed
                        || (r.anchor_start_unix > 0
                            && now_unix >= r.anchor_start_unix.saturating_add(r.anchor_duration_secs)))
            })
            .map(|r| {
                let active_t0 = r.anchor_start_unix.saturating_add(r.anchor_duration_secs);
                let last_t = st
                    .siphon_last_cycle_unix
                    .get(&r.item_id)
                    .copied()
                    .unwrap_or(active_t0)
                    .max(active_t0);
                (r.clone(), last_t)
            })
            .collect();
        units.sort_by_key(|(r, _)| r.item_id);

        let mut cycle_updates: Vec<(i64, u64)> = Vec::new();
        for (unit, last_t) in units {
            let cycles = now_unix.saturating_sub(last_t) / SIPHON_CYCLE_SECS;
            if cycles == 0 {
                continue;
            }
            let new_last = last_t.saturating_add(cycles * SIPHON_CYCLE_SECS);
            cycle_updates.push((unit.item_id, new_last));

            let Some(u_type) = sde.types.get(&unit.type_id) else {
                continue;
            };
            let cap_m3 = u_type.capacity.max(0.0);
            if cap_m3 <= 0.0 {
                continue;
            }
            let raw_rate = u_type.default_attributes.get(&1928).copied().unwrap_or(0.0) as i64;
            let pro_rate = u_type.default_attributes.get(&1929).copied().unwrap_or(0.0) as i64;
            let poly_rate = u_type.default_attributes.get(&1933).copied().unwrap_or(0.0) as i64;
            let waste_pct = u_type.default_attributes.get(&1930).copied().unwrap_or(10.0).max(0.0);

            let upos = snap
                .item_positions
                .get(&unit.item_id)
                .copied()
                .map(DVec3::from_array)
                .unwrap_or_else(|| DVec3::from_array(unit.position));

            let mut target_locs: Vec<i64> = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == solar_system_id as i64
                        && sde.types.get(&it.type_id).is_some_and(|t| is_siphon_target_pos_group(t.group_id))
                        && snap
                            .item_positions
                            .get(&it.item_id)
                            .is_some_and(|p| (upos - DVec3::from_array(*p)).length() <= SIPHON_MAX_GRID_DIST_M)
                })
                .map(|it| it.item_id)
                .collect();
            target_locs.sort_unstable();
            target_locs.dedup();
            if target_locs.is_empty() {
                continue;
            }

            let mut used_m3: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == unit.item_id && i.quantity > 0)
                .map(|i| sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.01) * (i.quantity as f64))
                .sum();

            for (rate, kind) in [(raw_rate, 0u8), (pro_rate, 1u8), (poly_rate, 2u8)] {
                if rate <= 0 {
                    continue;
                }
                let mut rem_steal = rate.saturating_mul(cycles as i64);
                let mut stack_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        target_locs.contains(&i.location_id)
                            && snap.items.get(&i.location_id).is_some_and(|pos| i.owner_id == pos.owner_id)
                            && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::None)
                            && i.quantity > 0
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                            && sde.types.get(&i.type_id).is_some_and(|t| match kind {
                                0 => matches!(t.group_id, 427 | 1884 | 1920..=1923),
                                1 => matches!(t.group_id, 428 | 429),
                                _ => t.group_id == 974,
                            })
                    })
                    .map(|i| i.item_id)
                    .collect();
                stack_ids.sort_unstable();

                for sid in stack_ids {
                    if rem_steal <= 0 {
                        break;
                    }
                    let Some((mat_tid, avail)) = snap.items.get(&sid).map(|i| (i.type_id, i.quantity as i64)) else {
                        continue;
                    };
                    let u_vol = sde.types.get(&mat_tid).map(|t| t.volume).unwrap_or(0.01).max(0.0001);
                    let max_fit = ((cap_m3 - used_m3).max(0.0) / u_vol).floor() as i64;
                    if max_fit <= 0 {
                        break;
                    }
                    // Total removed = stolen + ceil(stolen * waste%), capped by `avail`.
                    let max_from_avail = if waste_pct > 0.0 {
                        ((avail as f64) / (1.0 + waste_pct / 100.0)).floor().max(1.0) as i64
                    } else {
                        avail
                    };
                    let stolen = rem_steal.min(max_fit).min(max_from_avail).min(avail);
                    if stolen <= 0 {
                        continue;
                    }
                    let waste = (((stolen as f64) * (waste_pct / 100.0)).ceil() as i64).min(avail - stolen);
                    let total_drain = stolen + waste;

                    if let Some(src_it) = snap.items.get_mut(&sid) {
                        src_it.quantity = (src_it.quantity as i64 - total_drain).max(0) as i32;
                        src_it.version += 1;
                        if src_it.quantity <= 0 {
                            snap.items.remove(&sid);
                        }
                    }

                    let existing_dest = snap
                        .items
                        .values()
                        .find(|i| i.location_id == unit.item_id && i.type_id == mat_tid && !i.singleton)
                        .map(|i| i.item_id);
                    if let Some(did) = existing_dest {
                        if let Some(dst) = snap.items.get_mut(&did) {
                            dst.quantity = dst.quantity.saturating_add(stolen as i32);
                            dst.version += 1;
                        }
                    } else {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        snap.items.insert(
                            nid,
                            InventoryItemRecord {
                                item_id: nid,
                                type_id: mat_tid,
                                owner_id: unit.owner_id,
                                location_id: unit.item_id,
                                flag: InventoryFlag::Cargo,
                                quantity: stolen as i32,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }
                    used_m3 += u_vol * (stolen as f64);
                    rem_steal -= stolen;
                }
            }
        }

        if !cycle_updates.is_empty() {
            write_deployable_state_snap(snap, |st_mut| {
                for (uid, ts) in cycle_updates {
                    st_mut.siphon_last_cycle_unix.insert(uid, ts);
                }
            });
        }
    });
}

/// Flushes pending decloak notifications (`OnCloakModuleForceDeactivated`) and invalidates ship fittings.
pub fn flush_deployable_side_effects(ctx: &ServerContext) {
    let has_pending = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .values()
            .any(|n| n.note.contains("\"pending_decloak_notifications\":[{"))
    });
    if !has_pending {
        return;
    }
    let notices: Vec<PendingDecloakNotice> = write_deployable_state(ctx, |st| {
        std::mem::take(&mut st.pending_decloak_notifications)
    });
    for n in notices {
        ctx.invalidate_ship_fitting(n.ship_id);
        ctx.notify(
            NotificationTarget::Character(n.char_id),
            "OnCloakModuleForceDeactivated",
            "charid",
            vec![
                PyRep::String("CloakDeactivatedByObservatory".into()),
                PyRep::Dict(vec![
                    (PyRep::String("shipID".into()), PyRep::Integer(n.ship_id)),
                    (PyRep::String("moduleID".into()), PyRep::Integer(n.module_id)),
                ]),
            ],
        );
    }
}

/// Synchronous deployable step invoked during `ctx.advance_secs` before `cluster_tick` sweeps expired items.
pub fn step_system_deployables_sync(ctx: &ServerContext, solar_system_id: u32) {
    let now_unix = ctx.now_unix();
    let needs_work = ctx.db.with_snapshot(|snap| {
        if !snap.owner_notes.values().any(|n| n.note.contains("\"deployables\"")) {
            return false;
        }
        let st = read_deployable_state_snap(snap);
        let due_spool = st
            .mmju_spools
            .values()
            .any(|e| e.solar_system_id == solar_system_id && now_unix >= e.complete_at_unix);
        let due_obs_or_siphon = st.deployables.values().any(|r| {
            if r.solar_system_id != solar_system_id || r.is_unanchored {
                return false;
            }
            let t0 = r.anchor_start_unix.saturating_add(r.anchor_duration_secs);
            if now_unix < t0 {
                return false;
            }
            if r.group_id == GROUP_MOBILE_OBSERVATORY {
                let due = (((now_unix.min(r.expires_at_unix.saturating_sub(1)).saturating_sub(t0))
                    / OBSERVATORY_PING_INTERVAL_SECS) as u32
                    + 1)
                    .min(OBSERVATORY_TOTAL_PINGS);
                return due > st.observatory_pings_done.get(&r.item_id).copied().unwrap_or(0);
            }
            if r.group_id == GROUP_MOBILE_SIPHON && now_unix < r.expires_at_unix {
                let last_t = st.siphon_last_cycle_unix.get(&r.item_id).copied().unwrap_or(t0).max(t0);
                return now_unix.saturating_sub(last_t) >= SIPHON_CYCLE_SECS;
            }
            false
        });
        due_spool || due_obs_or_siphon
    });
    if !needs_work {
        return;
    }
    let blocked_due_ships: Vec<i64> = read_deployable_state(ctx)
        .mmju_spools
        .values()
        .filter(|e| {
            e.solar_system_id == solar_system_id
                && now_unix >= e.complete_at_unix
                && (crate::services::tactical_modules::check_ship_navigation_lock(ctx, e.ship_id, "jump").is_err()
                    || crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, e.ship_id, "jump").is_err())
        })
        .map(|e| e.ship_id)
        .collect();
    if !blocked_due_ships.is_empty() {
        write_deployable_state(ctx, |st| {
            for sid in blocked_due_ships {
                st.mmju_spools.remove(&sid);
            }
        });
    }
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let deact_guard = handle
        .module_deactivation_hook
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let (Ok(mut bp), Ok(mut actor_st)) = (handle.ballpark.write(), handle.actor_state.write()) {
        step_ext_deployables_in_actor(
            &mut bp,
            Some(&mut actor_st.active_module_cycles),
            deact_guard.as_ref(),
            &ctx.sde,
            &ctx.db,
            Some(&ctx.mechanic_rng),
            solar_system_id,
            now_unix,
        );
        crate::system_actor::sync_ball_positions(&bp, &handle.ball_positions, &handle.scrambled_balls);
        let mut mod_guard = handle.active_module_ships.write().unwrap_or_else(|e| e.into_inner());
        mod_guard.clear();
        for c in &actor_st.active_module_cycles {
            mod_guard.insert(c.ship_id);
        }
    }
    flush_deployable_side_effects(ctx);
}

/// Checks if `solar_system_id` has any due MMJU spools, Observatory pings, Siphon cycles, or expired deployables,
/// and if so steps the system deployable state and flushes notifications.
pub async fn sync_system_deployables(ctx: &ServerContext, solar_system_id: u32) {
    let now_unix = ctx.now_unix();
    let needs_actor_step = {
        let st = read_deployable_state(ctx);
        let due_spool = st
            .mmju_spools
            .values()
            .any(|e| e.solar_system_id == solar_system_id && now_unix >= e.complete_at_unix);
        let due_obs = st.deployables.values().any(|r| {
            if r.solar_system_id != solar_system_id || r.group_id != GROUP_MOBILE_OBSERVATORY || r.is_unanchored {
                return false;
            }
            let t0 = r.anchor_start_unix.saturating_add(r.anchor_duration_secs);
            if now_unix < t0 {
                return false;
            }
            let due = (((now_unix.min(r.expires_at_unix.saturating_sub(1)).saturating_sub(t0)) / OBSERVATORY_PING_INTERVAL_SECS)
                as u32
                + 1)
                .min(OBSERVATORY_TOTAL_PINGS);
            due > st.observatory_pings_done.get(&r.item_id).copied().unwrap_or(0)
        });
        let due_expiry = st
            .deployables
            .values()
            .any(|r| r.solar_system_id == solar_system_id && now_unix >= r.expires_at_unix);
        due_spool || due_obs || due_expiry
    };
    if needs_actor_step {
        let handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = handle.step_manual(0.0).await;
    }
    flush_deployable_side_effects(ctx);
}

#[cfg(test)] #[path = "mobile_deployables_ext_tests.rs"] mod mobile_deployables_ext_tests;

