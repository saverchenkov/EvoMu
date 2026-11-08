//! `mobile_deployables` — Mobile Deployable Structures Service and Mechanics (`feat:structures.mobile-depot`,
//! `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-tractor`,
//! `feat:structures.mobile-tractor-rules`, `feat:structures.mobile-deploy-restrictions`).
//!
//! Handles deployment restrictions (starter systems, career-agent systems, proximity to gates/stations/structures/MTUs),
//! anchoring lifecycles, Mobile Depot reinforcement & refitting, and MTU automated tractoring & looting.

use crate::router::ServerContext;
use eve_destiny::{BallId, DamageVector};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use glam::DVec3;

// parity: feat:structures.mobile-cyno-beacon, feat:structures.mobile-micro-jump-unit, feat:structures.mobile-observatory-decloak-ping
// Well-known Mobile Deployable Groups
pub const GROUP_MOBILE_DEPOT: u32 = 1246;
pub const GROUP_MOBILE_CYNO_INHIBITOR: u32 = 1249;
pub const GROUP_MOBILE_TRACTOR: u32 = 1250;
pub const GROUP_MOBILE_MICRO_JUMP_UNIT: u32 = 1276;
pub const GROUP_MOBILE_CYNO_BEACON: u32 = 4093;
pub const GROUP_MOBILE_OBSERVATORY: u32 = 4107;
pub const GROUP_CRAB_BEACON: u32 = 4137;
pub const GROUP_MOBILE_WARP_DISRUPTOR: u32 = 361;
pub const CATEGORY_DEPLOYABLE: u32 = 22;
pub const SKILL_ANCHORING: u32 = 11584;

// Well-known Type IDs
pub const TYPE_MOBILE_DEPOT: u32 = 33474;
pub const TYPE_MOBILE_CYNO_INHIBITOR: u32 = 33476;
pub const TYPE_WETU_DEPOT: u32 = 33520;
pub const TYPE_YURT_DEPOT: u32 = 33522;
pub const TYPE_MTU_STANDARD: u32 = 33475;
pub const TYPE_MTU_PACKRAT: u32 = 33700;
pub const TYPE_MTU_MAGPIE: u32 = 33702;
pub const TYPE_MTU_CONSORTIUM: u32 = 81951;
pub const TYPE_MOBILE_CYNO_BEACON: u32 = 57319;
pub const TYPE_COVERT_MOBILE_CYNO_BEACON: u32 = 59630;
pub const TYPE_MOBILE_MJU: u32 = 33591;
pub const TYPE_MOBILE_OBSERVATORY: u32 = 58904;

// Deployment Distance Rules (meters)
pub const MIN_GATE_DISTANCE_M: f64 = 50_000.0;
pub const MIN_STATION_DISTANCE_M: f64 = 50_000.0;
pub const MIN_STRUCTURE_DISTANCE_M: f64 = 50_000.0;
pub const MIN_MTU_TO_MTU_DISTANCE_M: f64 = 5_000.0;
pub const MIN_DEPOT_TO_DEPOT_DISTANCE_M: f64 = 5_000.0;
pub const CYNO_INHIBITOR_RANGE_M: f64 = 100_000.0;
pub const MIN_CYNO_INHIBITOR_TO_INHIBITOR_DISTANCE_M: f64 = 200_000.0;

// Lifetimes & Timers (seconds)
pub const DEPOT_ANCHOR_TIME_SECS: u64 = 60;
pub const DEPOT_REINFORCE_DURATION_SECS: u64 = 2 * 86_400; // 48 hours (2 days)
pub const DEPOT_LIFETIME_SECS: u64 = 30 * 86_400; // 30 days
pub const DEPOT_REINFORCE_SHIELD_THRESHOLD: f64 = 0.25; // 25% shield HP
pub const MTU_LIFETIME_SECS: u64 = 2 * 86_400; // 2 days
pub const MMJU_ACTIVATION_DELAY_SECS: u64 = 60; // 1 minute
pub const MMJU_LIFETIME_SECS: u64 = 2 * 86_400; // 2 days
pub const MMJU_SPOOL_TIME_SECS: f64 = 12.0;
pub const MMJU_JUMP_DISTANCE_M: f64 = 100_000.0; // 100 km
pub const MMJU_MAX_SHIP_MASS_KG: f64 = 1_000_000_000.0; // 1 billion kg
pub const CYNO_BEACON_SPOOL_SECS: u64 = 60; // 1 minute
pub const CYNO_BEACON_DURATION_SECS: u64 = 3_600; // 60 minutes
pub const COVERT_CYNO_BEACON_DURATION_SECS: u64 = 1_800; // 30 minutes
pub const OBSERVATORY_DEPLOY_SECS: u64 = 600; // 10 minutes
pub const OBSERVATORY_PING_INTERVAL_SECS: u64 = 600; // 10 minutes
pub const OBSERVATORY_LIFETIME_SECS: u64 = 600 + 2 * 3_600; // 10m deploy + 2h active
pub const OBSERVATORY_DECLOAK_CHANCE: f64 = 0.40; // 40% per ping
pub const CYNO_INHIBITOR_ACTIVATION_DELAY_SECS: u64 = 120; // 2 minutes (120 s)
pub const CYNO_INHIBITOR_LIFETIME_SECS: u64 = 3_600; // 1 hour (3,600 s)

pub use eve_db::{
    MmjuSpoolEntry, MobileDeployableRecord, MobileDeployableRuntimeState, PendingDecloakNotice,
    PersistedDamageRecord,
};

pub fn read_deployable_state_snap(snap: &eve_db::WorldStateSnapshot) -> MobileDeployableRuntimeState {
    snap.deployable_state.clone()
}

pub fn write_deployable_state_snap<R>(
    snap: &mut eve_db::WorldStateSnapshot,
    f: impl FnOnce(&mut MobileDeployableRuntimeState) -> R,
) -> R {
    f(&mut snap.deployable_state)
}

pub fn read_deployable_state(ctx: &ServerContext) -> MobileDeployableRuntimeState {
    ctx.db.with_snapshot(read_deployable_state_snap)
}

pub fn write_deployable_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut MobileDeployableRuntimeState) -> R,
) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.deployable_state))
}

/// Checks whether a given type ID represents a mobile structure (`feat:structures.mobile-deploy-restrictions`).
/// Only returns true for personal mobile deployables (Mobile Depot, MTU, MMJU, Cyno Beacon).
/// Mobile Warp Disruptors (group 361) and anchorable containers are NOT mobile structures.
pub fn is_mobile_structure(sde: &SdeRegistry, type_id: u32) -> bool {
    sde.types.get(&type_id).is_some_and(|t| {
        let cat = t.category_id;
        let grp = t.group_id;
        matches!(
            grp,
            GROUP_MOBILE_DEPOT
                | 1247
                | 1249
                | GROUP_MOBILE_TRACTOR
                | 1274
                | 1275
                | GROUP_MOBILE_MICRO_JUMP_UNIT
                | 1297
                | 1312
                | GROUP_MOBILE_CYNO_BEACON
                | 4107
                | 4137
                | 4913
        ) || (cat == CATEGORY_DEPLOYABLE && !matches!(grp, 361 | 1149 | 4477 | 4499 | 4810))
    })
}

/// Checks if a mobile structure can be scooped back into cargo.
pub fn can_deployable_be_scooped(group_id: u32) -> bool {
    group_id != GROUP_CRAB_BEACON
        && !crate::services::mobile_deployables_ext::is_single_use_deployable(group_id)
}

/// Determines if `sys_id` is a new-player starter system or career-agent system (`feat:structures.mobile-deploy-restrictions`).
pub fn is_starter_or_career_system(sde: &SdeRegistry, sys_id: u32) -> bool {
    let is_starter = sde
        .schools
        .values()
        .any(|s| (11..=22).contains(&s.school_id) && s.solar_system_id == sys_id);
    if is_starter {
        return true;
    }
    sde.agents
        .values()
        .any(|a| a.agent_type_id == 12 && a.solar_system_id == Some(sys_id))
}

/// Validates proximity and system restrictions before anchoring a mobile structure (`feat:structures.mobile-deploy-restrictions`).
pub fn check_mobile_deployment_restrictions(
    ctx: &ServerContext,
    session: &SessionState,
    item_id: i64,
    type_id: u32,
    group_id: u32,
    sys_id: u32,
    anchor_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    // 1. Starter / Career agent system restriction
    if is_starter_or_career_system(&ctx.sde, sys_id) {
        return Err(BridgeRpcError {
            code: "RestrictionsInEffect".into(),
            message: "Deployment restrictions in effect: mobile structures cannot be anchored in starter or career agent systems".into(),
        });
    }

    // 2. Type-specific security, space-class, fleet, inhibitor, and special proximity rules (Step 4B)
    crate::services::mobile_deployables_ext::validate_ext_deployment_restrictions(
        ctx, session, item_id, type_id, group_id, sys_id, anchor_pos,
    )?;
    if group_id == GROUP_CRAB_BEACON {
        if crate::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(ctx, sys_id).is_some() {
            return Err(BridgeRpcError::user_error("CynoJammed"));
        }
        crate::services::pve_crab_beacons::validate_crab_deployment_system(&ctx.sde, sys_id)?;
    }
    if group_id == GROUP_MOBILE_CYNO_INHIBITOR {
        let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
        if sys_sec >= 0.45 || sys_id >= 31_000_000 || sys_id == 30_100_000 {
            return Err(BridgeRpcError {
                code: "CantAnchorInHighSecOrWormhole".into(),
                message: format!(
                    "Mobile Cynosural Inhibitor cannot be anchored in high-sec, wormhole, Abyssal, or Zarzakh space (system {sys_id})"
                ),
            });
        }
    }

    // 3. Stargate distance check (>= 50 km)
    for gate in ctx.sde.stargates().values() {
        if gate.from_solar_system_id == sys_id && (anchor_pos - gate.position).length() < MIN_GATE_DISTANCE_M {
            return Err(BridgeRpcError {
                code: "DeploymentTooCloseToGate".into(),
                message: format!(
                    "Cannot anchor within 50km of a stargate (distance: {:.1} km)",
                    (anchor_pos - gate.position).length() / 1_000.0
                ),
            });
        }
    }

    // 4. Station distance check (>= 50 km)
    for station in ctx.sde.stations.values() {
        if station.solar_system_id == sys_id && (anchor_pos - station.position).length() < MIN_STATION_DISTANCE_M {
            return Err(BridgeRpcError {
                code: "DeploymentTooCloseToStation".into(),
                message: format!(
                    "Cannot anchor within 50km of a station (distance: {:.1} km)",
                    (anchor_pos - station.position).length() / 1_000.0
                ),
            });
        }
    }

    // 5. Structure & same-type distance checks
    ctx.db.with_snapshot(|snap| {
        // Distance from Upwell structures (>= 50 km)
        for st in snap.structures.values() {
            if st.solar_system_id == sys_id && st.state != "Destroyed" {
                let spos = snap
                    .item_positions
                    .get(&st.structure_id)
                    .copied()
                    .map(DVec3::from_array)
                    .unwrap_or_else(|| DVec3::from_array(st.position));
                let min_st_dist = if group_id == 1247 && matches!(st.type_id, 35835 | 35836 | 81826) {
                    5_000.0
                } else {
                    MIN_STRUCTURE_DISTANCE_M
                };
                if (anchor_pos - spos).length() < min_st_dist {
                    return Err(BridgeRpcError {
                        code: "DeploymentTooCloseToStructure".into(),
                        message: format!(
                            "Cannot anchor within 50km of an Upwell structure (distance: {:.1} km)",
                            (anchor_pos - spos).length() / 1_000.0
                        ),
                    });
                }
            }
        }

        // Distance from another MTU (>= 5 km)
        if group_id == GROUP_MOBILE_TRACTOR {
            for it in snap.items.values() {
                if it.item_id != item_id && it.location_id == sys_id as i64 {
                    let it_grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                    if it_grp == GROUP_MOBILE_TRACTOR {
                        if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                            let opos = DVec3::from_array(*pos_arr);
                            if (anchor_pos - opos).length() < MIN_MTU_TO_MTU_DISTANCE_M {
                                return Err(BridgeRpcError {
                                    code: "DeploymentTooCloseToSameType".into(),
                                    message: format!(
                                        "Cannot anchor within 5km of another Mobile Tractor Unit (distance: {:.1} km)",
                                        (anchor_pos - opos).length() / 1_000.0
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Distance from another Mobile Depot, other anchored deployables, or SDE celestials (>= 5 km)
        if group_id == GROUP_MOBILE_DEPOT {
            for cel in ctx.sde.celestials_in_system(sys_id) {
                if (anchor_pos != DVec3::ZERO || cel.position != DVec3::ZERO)
                    && matches!(cel.group_id, 6..=9)
                    && (anchor_pos - cel.position).length() < MIN_DEPOT_TO_DEPOT_DISTANCE_M
                {
                    return Err(BridgeRpcError {
                        code: "DeploymentTooCloseToCelestial".into(),
                        message: format!(
                            "Cannot anchor within 5km of a celestial body (distance: {:.1} km)",
                            (anchor_pos - cel.position).length() / 1_000.0
                        ),
                    });
                }
            }
            let dep_state = read_deployable_state_snap(snap);
            for it in snap.items.values() {
                if it.item_id != item_id && it.location_id == sys_id as i64 {
                    let (it_grp, it_cat) = ctx.sde.types.get(&it.type_id).map(|t| (t.group_id, t.category_id)).unwrap_or((0, 0));
                    if it_grp == GROUP_MOBILE_DEPOT {
                        if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                            let opos = DVec3::from_array(*pos_arr);
                            if (anchor_pos - opos).length() < MIN_DEPOT_TO_DEPOT_DISTANCE_M {
                                return Err(BridgeRpcError {
                                    code: "DeploymentTooCloseToSameType".into(),
                                    message: format!(
                                        "Cannot anchor within 5km of another Mobile Depot (distance: {:.1} km)",
                                        (anchor_pos - opos).length() / 1_000.0
                                    ),
                                });
                            }
                        }
                    } else if it_cat == CATEGORY_DEPLOYABLE || dep_state.deployables.contains_key(&it.item_id) {
                        if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                            let opos = DVec3::from_array(*pos_arr);
                            if (anchor_pos - opos).length() < MIN_DEPOT_TO_DEPOT_DISTANCE_M {
                                return Err(BridgeRpcError {
                                    code: "DeploymentTooCloseToStructure".into(),
                                    message: format!(
                                        "Cannot anchor within 5km of another deployable structure (distance: {:.1} km)",
                                        (anchor_pos - opos).length() / 1_000.0
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Distance from another Mobile Cynosural Inhibitor (>= 200 km so 100 km inhibition spheres do not overlap)
        if group_id == GROUP_MOBILE_CYNO_INHIBITOR {
            for it in snap.items.values() {
                if it.item_id != item_id && it.location_id == sys_id as i64 {
                    let it_grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                    if it_grp == GROUP_MOBILE_CYNO_INHIBITOR {
                        if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                            let opos = DVec3::from_array(*pos_arr);
                            if (anchor_pos - opos).length() < MIN_CYNO_INHIBITOR_TO_INHIBITOR_DISTANCE_M {
                                return Err(BridgeRpcError {
                                    code: "MobileCynoInhibitorOverlap".into(),
                                    message: format!(
                                        "Cannot anchor within 200km of another Mobile Cynosural Inhibitor (distance: {:.1} km)",
                                        (anchor_pos - opos).length() / 1_000.0
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    })
}

/// Registers a newly deployed mobile structure record and restores any preserved damage.
pub fn register_mobile_deployable(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    group_id: u32,
    owner_id: i64,
    solar_system_id: u32,
    anchor_duration_secs: u64,
    position: DVec3,
) -> Option<PersistedDamageRecord> {
    let now_unix = ctx.now_unix();
    let lifetime_secs = if group_id == GROUP_CRAB_BEACON {
        crate::services::pve_crab_beacons::CRAB_BEACON_IDLE_LIFETIME_SECS
    } else {
        crate::services::mobile_deployables_ext::total_lifetime_secs(
            type_id,
            group_id,
            anchor_duration_secs,
        )
    };
    let deployed_fleet_id = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .find(|f| f.members.contains(&owner_id))
            .map(|f| f.fleet_id)
    });

    if group_id == GROUP_CRAB_BEACON {
        crate::services::pve_crab_beacons::register_crab_beacon(
            ctx,
            item_id,
            type_id,
            owner_id,
            solar_system_id,
            position,
        );
    }

    write_deployable_state(ctx, |st| {
        let damaged = st.damaged_items.remove(&item_id);
        st.observatory_pings_done.remove(&item_id);
        st.siphon_last_cycle_unix.remove(&item_id);
        let record = MobileDeployableRecord {
            item_id,
            type_id,
            group_id,
            owner_id,
            solar_system_id,
            anchor_start_unix: now_unix,
            anchor_duration_secs,
            anchor_completed: false,
            is_unanchored: false,
            expires_at_unix: now_unix.saturating_add(lifetime_secs),
            last_interaction_unix: now_unix,
            reinforced_until_unix: None,
            last_ping_unix: None,
            position: [position.x, position.y, position.z],
            deployed_fleet_id,
        };
        st.deployables.insert(item_id, record);
        damaged
    })
}

/// Refreshes the 30-day lifetime of a Mobile Depot upon pilot interaction (`feat:structures.mobile-depot-lifetime-reinforce`).
pub fn touch_mobile_depot_interaction(ctx: &ServerContext, item_id: i64) {
    let now_unix = ctx.now_unix();
    write_deployable_state(ctx, |st| {
        if let Some(rec) = st.deployables.get_mut(&item_id) {
            if rec.group_id == GROUP_MOBILE_DEPOT {
                rec.last_interaction_unix = now_unix;
                rec.expires_at_unix = now_unix.saturating_add(DEPOT_LIFETIME_SECS);
            }
        }
    });
}

/// Preserves damage state when a mobile structure is scooped into cargo (`feat:structures.mobile-depot-lifetime-reinforce`).
pub fn record_scooped_deployable_damage(
    ctx: &ServerContext,
    item_id: i64,
    combat: &eve_destiny::CombatState,
) {
    write_deployable_state(ctx, |st| {
        st.deployables.remove(&item_id);
        st.damaged_items.insert(
            item_id,
            PersistedDamageRecord {
                shield_hp: combat.shield_hp,
                max_shield_hp: combat.max_shield_hp,
                armor_hp: combat.armor_hp,
                max_armor_hp: combat.max_armor_hp,
                structure_hp: combat.structure_hp,
                max_structure_hp: combat.max_structure_hp,
            },
        );
    });
}

/// Advances mobile deployable simulation in the solar system actor on each server tick (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`, `feat:structures.mobile-depot-lifetime-reinforce`).
/// Executes automated expiry sweeps and MTU automated tractoring & hold-capacity looting.
pub fn step_mobile_deployables_in_actor(
    ballpark: &mut eve_destiny::Ballpark,
    active_module_cycles: &mut Vec<crate::system_actor::ActiveModuleCycle>,
    mechanic_rng: &crate::rng::MechanicRng,
    deactivation_hook: Option<&crate::system_actor::ModuleDeactivationHook>,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    rng_opt: Option<&crate::rng::MechanicRng>,
    solar_system_id: u32,
    dt: f64,
    now_unix: u64,
) {
    let (Some(db), Some(sde)) = (db_opt, sde_opt) else { return; };

    use crate::services::mobile_deployables_ext::{
        GROUP_MOBILE_SIPHON, OBSERVATORY_PING_INTERVAL_SECS, OBSERVATORY_TOTAL_PINGS, SIPHON_CYCLE_SECS,
    };

    // Single lightweight snapshot check to return early on idle ticks without repeated locks/JSON parses.
    let (needs_ext, expired_ids, has_timer_transition, active_mtus) = db.with_snapshot(|snap| {
        let meta_due = snap.sp6_lifecycle.space_object_meta.values().any(|meta| {
            !meta.anchor_completed
                && meta
                    .anchor_start_unix
                    .is_some_and(|start| now_unix >= start.saturating_add(meta.anchoring_delay_secs as u64))
        });
        let st = &snap.deployable_state;
        if st.deployables.is_empty() && st.mmju_spools.is_empty() {
            return (false, Vec::new(), meta_due, Vec::new());
        }

        let mmju_due = st
            .mmju_spools
            .values()
            .any(|e| e.solar_system_id == solar_system_id && now_unix >= e.complete_at_unix);
        let mut ext_due = mmju_due;
        let mut expired = Vec::new();
        let mut dep_due = false;
        let mut mtus = Vec::new();

        for r in st.deployables.values() {
            if r.solar_system_id != solar_system_id {
                continue;
            }
            let active_t0 = r.anchor_start_unix.saturating_add(r.anchor_duration_secs);
            if now_unix >= r.expires_at_unix {
                expired.push(r.item_id);
            }
            if r.reinforced_until_unix.is_some_and(|u| now_unix >= u)
                || (!r.is_unanchored && !r.anchor_completed && r.anchor_start_unix > 0 && now_unix >= active_t0)
            {
                dep_due = true;
            }
            if !ext_due && !r.is_unanchored && now_unix >= active_t0 {
                if r.group_id == GROUP_MOBILE_OBSERVATORY && snap.items.contains_key(&r.item_id) {
                    let eff_now = now_unix.min(r.expires_at_unix.saturating_sub(1));
                    let due_pings = (((eff_now.saturating_sub(active_t0)) / OBSERVATORY_PING_INTERVAL_SECS) as u32 + 1)
                        .min(OBSERVATORY_TOTAL_PINGS);
                    if due_pings > st.observatory_pings_done.get(&r.item_id).copied().unwrap_or(0) {
                        ext_due = true;
                    }
                } else if r.group_id == GROUP_MOBILE_SIPHON && now_unix < r.expires_at_unix {
                    let last_t = st.siphon_last_cycle_unix.get(&r.item_id).copied().unwrap_or(active_t0).max(active_t0);
                    if now_unix.saturating_sub(last_t) >= SIPHON_CYCLE_SECS {
                        ext_due = true;
                    }
                }
            }
            if r.group_id == GROUP_MOBILE_TRACTOR
                && !r.is_unanchored
                && snap.items.contains_key(&r.item_id)
                && (r.anchor_completed || (r.anchor_start_unix > 0 && now_unix >= active_t0))
            {
                mtus.push(r.clone());
            }
        }

        (ext_due, expired, dep_due || meta_due, mtus)
    });

    if !needs_ext && expired_ids.is_empty() && !has_timer_transition && active_mtus.is_empty() {
        return;
    }

    // 0. Process pending MMJU 12s spools, Mobile Observatory 10m pings, and Mobile Siphon cycles before expiry removal
    if needs_ext {
        crate::services::mobile_deployables_ext::step_ext_deployables_in_actor(
            ballpark,
            Some(active_module_cycles),
            deactivation_hook,
            sde,
            db,
            rng_opt.or(Some(mechanic_rng)),
            solar_system_id,
            now_unix,
        );
    }

    // 1. Expiry sweep: find all deployables in this solar system where now_unix >= expires_at_unix
    if !expired_ids.is_empty() {
        for &eid in &expired_ids {
            ballpark.balls.remove(&eid);
            ballpark.combat_states.remove(&eid);
        }
        db.with_mut_snapshot(|snap| {
            write_deployable_state_snap(snap, |st| {
                for &eid in &expired_ids {
                    st.deployables.remove(&eid);
                    st.observatory_pings_done.remove(&eid);
                    st.siphon_last_cycle_unix.remove(&eid);
                }
            });
            for eid in expired_ids {
                let child_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == eid)
                    .map(|i| i.item_id)
                    .collect();
                for cid in child_ids {
                    snap.items.remove(&cid);
                }
                snap.items.remove(&eid);
                snap.item_positions.remove(&eid);
                snap.sp6_lifecycle.space_object_meta.remove(&eid);
            }
        });
    }

    // 2. Clear expired reinforcement timers and mark completed anchoring transitions
    if has_timer_transition {
        db.with_mut_snapshot(|snap| {
            if !snap.deployable_state.deployables.is_empty() {
                write_deployable_state_snap(snap, |st| {
                    for rec in st.deployables.values_mut() {
                        if rec.solar_system_id == solar_system_id {
                            if rec.reinforced_until_unix.is_some_and(|u| now_unix >= u) {
                                rec.reinforced_until_unix = None;
                            }
                            if !rec.is_unanchored
                                && !rec.anchor_completed
                                && rec.anchor_start_unix > 0
                                && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)
                            {
                                rec.anchor_completed = true;
                            }
                        }
                    }
                });
            }
            for meta in snap.sp6_lifecycle.space_object_meta.values_mut() {
                if !meta.anchor_completed {
                    if let Some(start) = meta.anchor_start_unix {
                        let delay = meta.anchoring_delay_secs as u64;
                        if now_unix >= start.saturating_add(delay) {
                            meta.anchor_completed = true;
                        }
                    }
                }
            }
        });
    }

    // 3. MTU tractoring and looting
    enum MtuScan {
        MissingItem,
        Skip,
        Candidates(Vec<(i64, f64, DVec3)>),
    }

    for mtu in active_mtus {
        let mtu_pos = DVec3::from_array(mtu.position);
        let mtu_type = sde.types.get(&mtu.type_id);
        let mtu_capacity = mtu_type
            .map(|t| t.capacity)
            .filter(|&c| c > 0.0)
            .unwrap_or(27_000.0); // Derived fallback: 27,000 m³
        // SDE dogma attribute 54 (maxRange) or 76 (maxTargetRange) for MTU tractor beam range (MD-P1-4)
        let max_tractor_range = mtu_type
            .and_then(|t| t.default_attributes.get(&54).or_else(|| t.default_attributes.get(&76)).copied())
            .unwrap_or(125_000.0); // Derived fallback: 125km
        // SDE dogma attribute 1045 (maxTractorVelocity) for MTU tractor beam pulling speed (MD-P1-4)
        let tractor_speed = mtu_type
            .and_then(|t| t.default_attributes.get(&1045).copied())
            .unwrap_or(1_000.0); // Derived fallback: 1,000 m/s
        let loot_radius = 2_500.0; // Derived: standard EVE looting radius

        let owner_in_ballpark = ballpark.ball_slim_meta.values().any(|m| m.char_id == Some(mtu.owner_id));
        let scan = db.with_snapshot(|snap| {
            if !snap.items.contains_key(&mtu.item_id) {
                return MtuScan::MissingItem;
            }
            let owner_char = snap.characters.get(&mtu.owner_id);
            if owner_char.is_some_and(|c| {
                c.station_id.is_some() || c.structure_id.is_some() || c.solar_system_id != solar_system_id
            }) {
                return MtuScan::Skip;
            }
            let owner_in_system = owner_in_ballpark
                || owner_char.is_some_and(|c| {
                    c.solar_system_id == solar_system_id && c.station_id.is_none() && c.structure_id.is_none()
                });
            if !owner_in_system {
                return MtuScan::Skip;
            }

            let mut current_mtu_vol = 0.0;
            let mut raw_targets: Vec<(i64, DVec3)> = Vec::new();
            for it in snap.items.values() {
                if it.location_id == mtu.item_id && it.quantity > 0 {
                    let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, it);
                    current_mtu_vol += u_vol * (it.quantity.max(1) as f64);
                    continue;
                }
                if it.location_id != solar_system_id as i64 || it.item_id == mtu.item_id || it.owner_id != mtu.owner_id {
                    continue;
                }
                let is_wreck = sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186);
                let is_container = it.type_id == 23
                    || sde.types.get(&it.type_id).is_some_and(|t| matches!(t.group_id, 12 | 340 | 448));
                if !is_wreck && !is_container {
                    continue;
                }
                if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                    raw_targets.push((it.item_id, DVec3::from_array(*pos_arr)));
                }
            }

            let mut list = Vec::new();
            for (tid, p) in raw_targets {
                let dist = (p - mtu_pos).length();
                if dist > max_tractor_range {
                    continue;
                }
                let mut has_any = false;
                let mut can_loot_any = false;
                for c in snap.items.values() {
                    if c.location_id == tid && c.quantity > 0 {
                        has_any = true;
                        if dist <= loot_radius {
                            let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, c);
                            if current_mtu_vol + u_vol * (c.quantity.max(1) as f64) <= mtu_capacity {
                                can_loot_any = true;
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                if !has_any || (dist <= loot_radius && !can_loot_any) {
                    continue;
                }
                list.push((tid, dist, p));
            }
            MtuScan::Candidates(list)
        });

        let candidates = match scan {
            MtuScan::MissingItem => {
                db.with_mut_snapshot(|snap| {
                    write_deployable_state_snap(snap, |st| {
                        st.deployables.remove(&mtu.item_id);
                    });
                });
                continue;
            }
            MtuScan::Skip => continue,
            MtuScan::Candidates(list) => list,
        };

        if candidates.is_empty() {
            continue;
        }

        // Closest first
        let Some((target_id, target_dist, target_pos)) = candidates
            .into_iter()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        else {
            continue;
        };

        if target_dist <= loot_radius {
            // Loot cargo into MTU hold, enforcing capacity
            db.with_mut_snapshot(|snap| {
                let mut current_vol: f64 = snap
                    .items
                    .values()
                    .filter(|c| c.location_id == mtu.item_id && c.quantity > 0)
                    .map(|c| {
                        let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, c);
                        u_vol * (c.quantity.max(1) as f64)
                    })
                    .sum();

                let child_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|c| c.location_id == target_id && c.quantity > 0)
                    .map(|c| c.item_id)
                    .collect();

                for cid in child_ids {
                    let Some(child) = snap.items.get(&cid).cloned() else { continue; };
                    let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, &child);
                    let total_vol = u_vol * (child.quantity.max(1) as f64);
                    if current_vol + total_vol <= mtu_capacity {
                        if let Some(child_mut) = snap.items.get_mut(&cid) {
                            child_mut.location_id = mtu.item_id;
                            child_mut.flag = eve_db::InventoryFlag::Cargo;
                            child_mut.version += 1;
                        }
                        current_vol += total_vol;
                    }
                }
            });
        } else {
            // Tractor target towards MTU at tractor_speed
            let dir = (mtu_pos - target_pos).normalize_or_zero();
            let step = tractor_speed * dt;
            let new_pos = if step >= target_dist - loot_radius {
                mtu_pos - dir * loot_radius
            } else {
                target_pos + dir * step
            };
            db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(target_id, [new_pos.x, new_pos.y, new_pos.z]);
            });
            if let Some(b) = ballpark.balls.get_mut(&target_id) {
                b.position = new_pos;
            }
        }
    }
}

/// Evaluates ball damage in solar system actor for Mobile Depots and MTUs (`feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-tractor-rules`).
pub fn handle_ball_damage_in_actor(
    db_opt: &Option<std::sync::Arc<eve_db::WorldRepository>>,
    sde_opt: &Option<std::sync::Arc<eve_sde::SdeRegistry>>,
    solar_system_id: u32,
    ball_id: BallId,
    damage: DamageVector,
    ballpark: &mut eve_destiny::Ballpark,
    now_unix: u64,
) -> Option<(f64, bool, Option<eve_destiny::Ball>)> {
    let db = db_opt.as_ref()?;
    let sde = sde_opt.as_ref()?;

    let (item_type_id, owner_id) = db.with_snapshot(|snap| {
        snap.items.get(&ball_id).map(|it| (it.type_id, it.owner_id))
    })?;

    let it_grp = sde.types.get(&item_type_id).map(|t| t.group_id).unwrap_or(0);

    if it_grp == GROUP_MOBILE_DEPOT {
        let state = db.with_snapshot(read_deployable_state_snap);
        let rec_opt = state.deployables.get(&ball_id).cloned();

        let ball_snap = ballpark.balls.get(&ball_id).cloned();
        let cs = ballpark.combat_states.entry(ball_id).or_default();

        if let Some(rec) = rec_opt {
            // Invulnerable during active reinforcement
            if let Some(reinf_until) = rec.reinforced_until_unix {
                if now_unix < reinf_until {
                    return Some((0.0, false, ball_snap));
                }
            }

            // During 60-second anchoring countdown or when unanchored: takes direct damage and can be destroyed without reinforcement
            let anchoring_until_unix = rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs);
            let is_anchoring = !rec.anchor_completed && rec.anchor_start_unix > 0 && anchoring_until_unix > now_unix;
            if !(rec.is_unanchored || is_anchoring) {
                // Reinforcement threshold at 25% shield HP once anchored (applying shield resistances)
                let shield_threshold = (cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD).max(0.0);
                if cs.shield_hp > shield_threshold + 1e-6 {
                    let damage_to_threshold = (cs.shield_hp - shield_threshold).max(0.0);
                    let resisted_shield_dmg = damage.em.max(0.0) * cs.shield_resonance[0]
                        + damage.thermal.max(0.0) * cs.shield_resonance[1]
                        + damage.kinetic.max(0.0) * cs.shield_resonance[2]
                        + damage.explosive.max(0.0) * cs.shield_resonance[3];
                    if resisted_shield_dmg <= 0.0 {
                        return Some((0.0, false, ball_snap));
                    }
                    if resisted_shield_dmg + 1e-6 >= damage_to_threshold {
                        cs.shield_hp = shield_threshold;
                        let reinf_until = now_unix.saturating_add(DEPOT_REINFORCE_DURATION_SECS);
                        db.with_mut_snapshot(|snap| {
                            write_deployable_state_snap(snap, |st| {
                                if let Some(r) = st.deployables.get_mut(&ball_id) {
                                    r.reinforced_until_unix = Some(reinf_until);
                                }
                            });
                        });
                        return Some((damage_to_threshold, false, ball_snap));
                    } else {
                        cs.shield_hp -= resisted_shield_dmg;
                        return Some((resisted_shield_dmg, false, ball_snap));
                    }
                }
            }
        }

        // Unanchored, anchoring, post-reinforcement, or shields <= 25%: take damage directly and run full cleanup on destruction
        let (applied, destroyed) = cs.apply_damage(damage);
        if destroyed {
            let _ = ballpark.remove_ball(ball_id);
            let pos_arr = db
                .with_snapshot(|s| s.item_positions.get(&ball_id).copied())
                .or_else(|| ball_snap.as_ref().map(|b| b.position.to_array()))
                .unwrap_or([0.0; 3]);
            let mut spawned_can_id = None;
            db.with_mut_snapshot(|snap| {
                let hold_items: Vec<(i64, i32)> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == ball_id && i.quantity > 0)
                    .map(|i| (i.item_id, i.quantity))
                    .collect();
                if !hold_items.is_empty() {
                    snap.next_entity_id += 1;
                    let can_id = snap.next_entity_id;
                    snap.items.insert(
                        can_id,
                        eve_db::InventoryItemRecord {
                            item_id: can_id,
                            type_id: 23,
                            owner_id,
                            location_id: solar_system_id as i64,
                            flag: eve_db::InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    snap.item_positions.insert(can_id, pos_arr);
                    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, can_id, now_unix, false);
                    spawned_can_id = Some(can_id);

                    for (iid, _) in hold_items {
                        let roll = ((now_unix ^ (iid as u64)).wrapping_mul(6364136223846793005) >> 32) as f64 / 4294967296.0;
                        if roll < 0.50 {
                            if let Some(it) = snap.items.get_mut(&iid) {
                                it.location_id = can_id;
                                it.flag = eve_db::InventoryFlag::Cargo;
                                it.version += 1;
                            }
                        } else {
                            snap.items.remove(&iid);
                        }
                    }
                }
                snap.items.remove(&ball_id);
                snap.item_positions.remove(&ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
                snap.sp6_lifecycle.ship_combat_hp.remove(&ball_id);
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&ball_id);
                });
            });
            if let Some(can_id) = spawned_can_id {
                let _ = ballpark.add_static_ball_with_meta(
                    eve_destiny::Ball {
                        id: can_id,
                        type_id: 23,
                        position: DVec3::from_array(pos_arr),
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 1_000.0,
                        radius: 20.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    eve_destiny::SlimItemMeta {
                        group_id: 12,
                        category_id: 2,
                        owner_id,
                        ..Default::default()
                    },
                );
            }
        }
        return Some((applied, destroyed, ball_snap));
    }

    if it_grp == GROUP_MOBILE_TRACTOR {
        let (applied, destroyed, ball_opt) = ballpark.apply_damage_to_ball(ball_id, damage)?;
        if destroyed {
            let _ = ballpark.remove_ball(ball_id);
            // Drop 50% loot into cargo container
            let pos_arr = db.with_snapshot(|s| s.item_positions.get(&ball_id).copied()).unwrap_or([0.0; 3]);
            db.with_mut_snapshot(|snap| {
                let hold_items: Vec<(i64, i32)> = snap.items.values()
                    .filter(|i| i.location_id == ball_id && i.quantity > 0)
                    .map(|i| (i.item_id, i.quantity))
                    .collect();
                if !hold_items.is_empty() {
                    snap.next_entity_id += 1;
                    let can_id = snap.next_entity_id;
                    snap.items.insert(
                        can_id,
                        eve_db::InventoryItemRecord {
                            item_id: can_id,
                            type_id: 23,
                            owner_id,
                            location_id: solar_system_id as i64,
                            flag: eve_db::InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    snap.item_positions.insert(can_id, pos_arr);
                    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, can_id, now_unix, false);

                    for (iid, _) in hold_items {
                        let roll = ((now_unix ^ (iid as u64)).wrapping_mul(6364136223846793005) >> 32) as f64 / 4294967296.0;
                        if roll < 0.50 {
                            if let Some(it) = snap.items.get_mut(&iid) {
                                it.location_id = can_id;
                                it.flag = eve_db::InventoryFlag::Cargo;
                                it.version += 1;
                            }
                        } else {
                            snap.items.remove(&iid);
                        }
                    }
                }
                snap.items.remove(&ball_id);
                snap.item_positions.remove(&ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
                snap.sp6_lifecycle.ship_combat_hp.remove(&ball_id);
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&ball_id);
                });
            });
        }
        return Some((applied, destroyed, ball_opt));
    }

    if matches!(
        it_grp,
        GROUP_MOBILE_CYNO_BEACON | GROUP_MOBILE_MICRO_JUMP_UNIT | GROUP_MOBILE_OBSERVATORY
    ) {
        let (applied, destroyed, ball_opt) = ballpark.apply_damage_to_ball(ball_id, damage)?;
        if destroyed {
            let _ = ballpark.remove_ball(ball_id);
            db.with_mut_snapshot(|snap| {
                snap.items.remove(&ball_id);
                snap.item_positions.remove(&ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
                snap.sp6_lifecycle.ship_combat_hp.remove(&ball_id);
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&ball_id);
                });
            });
        }
        return Some((applied, destroyed, ball_opt));
    }

    None
}

pub(crate) fn mmju_jump_dir(b: &eve_destiny::Ball) -> DVec3 {
    if b.velocity.length_squared() > 1e-4 {
        b.velocity.normalize()
    } else if let eve_destiny::MovementMode::GotoDirection { direction } = b.mode {
        if direction.length_squared() > 1e-4 {
            direction.normalize()
        } else {
            DVec3::X
        }
    } else {
        DVec3::X
    }
}

/// Activates a Mobile Micro Jump Unit (`feat:structures.mobile-micro-jump-unit`) for the caller's ship.
/// Any ship under 1,000,000,000 kg (friendly or hostile) within 2,500m of an active MMJU
/// begins a 12-second warm-up and jumps 100 km in its facing direction with no cooldown or concurrent user limit.
pub async fn handle_activate_micro_jump_unit(
    ctx: &ServerContext,
    session: &SessionState,
    mju_id: i64,
) -> Result<eve_proto::PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship".into(),
    })?;
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solar system".into(),
    })?;
    if mju_id <= 0 {
        return Err(BridgeRpcError {
            code: "MissingItemID".into(),
            message: "microJumpDriver.ActivateMicroJumpDrive requires target mjuID".into(),
        });
    }

    let now_unix = ctx.now_unix();
    let rec = read_deployable_state(ctx)
        .deployables
        .get(&mju_id)
        .cloned()
        .ok_or_else(|| BridgeRpcError {
            code: "DeployableNotFound".into(),
            message: format!("Mobile Micro Jump Unit {mju_id} not found"),
        })?;

    if rec.group_id != GROUP_MOBILE_MICRO_JUMP_UNIT || rec.solar_system_id != sys_id || rec.is_unanchored {
        return Err(BridgeRpcError {
            code: "InvalidDeployable".into(),
            message: format!("Item {mju_id} is not an active Mobile Micro Jump Unit in this system"),
        });
    }
    if now_unix < rec.anchor_start_unix.saturating_add(MMJU_ACTIVATION_DELAY_SECS) {
        return Err(BridgeRpcError {
            code: "DeployableAnchoring".into(),
            message: "Mobile Micro Jump Unit is still anchoring (1 minute activation delay)".into(),
        });
    }
    if now_unix >= rec.expires_at_unix {
        return Err(BridgeRpcError {
            code: "DeployableExpired".into(),
            message: "Mobile Micro Jump Unit has expired".into(),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Ship is not in space".into(),
        })?;
    let mju_pos = DVec3::from_array(rec.position);
    if (ship_ball.position - mju_pos).length() > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: "Ship must be within 2,500m of the Mobile Micro Jump Unit".into(),
        });
    }
    let dyn_mass_add: f64 = ctx.db.with_snapshot(|snap| {
        snap.items
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
            .sum()
    });
    let base_hull_mass = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id))
        .and_then(|tid| ctx.sde.types.get(&tid))
        .map(|t| t.mass)
        .unwrap_or(ship_ball.mass);
    let eval_mass = if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let ev = fit.evaluated_context(&ctx.sde);
        ev.ship
            .get_attribute(eve_dogma::attr_ids::MASS, &ctx.sde)
            .max(ship_ball.mass)
            .max(base_hull_mass + dyn_mass_add)
    } else {
        ship_ball.mass.max(base_hull_mass + dyn_mass_add)
    };
    if eval_mass >= MMJU_MAX_SHIP_MASS_KG {
        return Err(BridgeRpcError {
            code: "ShipTooHeavy".into(),
            message: format!("Ship mass {:.0} kg exceeds Mobile Micro Jump Unit limit of {:.0} kg", eval_mass, MMJU_MAX_SHIP_MASS_KG),
        });
    }
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(BridgeRpcError { code: "ShipInWarp".into(), message: "Cannot activate Mobile Micro Jump Unit while warping".into() });
    }
    let ship_cs = sys_handle.get_combat_state(ship_id).await.ok().flatten();
    if ship_cs.as_ref().is_some_and(|cs| cs.is_cloaked) {
        return Err(BridgeRpcError { code: "ShipCloaked".into(), message: "Cannot activate Mobile Micro Jump Unit while cloaked".into() });
    }
    if sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) || ship_cs.as_ref().is_some_and(|cs| cs.is_mwd_mjd_disabled) {
        return Err(BridgeRpcError { code: "WarpScrambled".into(), message: "Cannot activate Mobile Micro Jump Unit while warp scrambled".into() });
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "jump")?;
    let jump_dir = mmju_jump_dir(&ship_ball);
    let dest_pos = ship_ball.position + jump_dir * MMJU_JUMP_DISTANCE_M;
    crate::services::inventory_fitting::fitting_ship_forcefield::check_pos_forcefield_coverage(
        ctx,
        sys_id,
        char_id,
        ship_id,
        &[ship_ball.position, dest_pos],
    )
    .await?;
    let already_spooling = read_deployable_state(ctx)
        .mmju_spools
        .values()
        .any(|sp| sp.ship_id == ship_id && sp.complete_at_unix > now_unix);
    if already_spooling {
        return Err(BridgeRpcError {
            code: "MicroJumpAlreadySpooling".into(),
            message: "Ship is already spooling a Mobile Micro Jump Unit".into(),
        });
    }

    let spool_complete_unix = now_unix.saturating_add(MMJU_SPOOL_TIME_SECS as u64);
    write_deployable_state(ctx, |st| {
        st.mmju_spools.insert(
            ship_id,
            MmjuSpoolEntry {
                ship_id,
                char_id,
                unit_id: mju_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: spool_complete_unix,
                direction: jump_dir.to_array(),
            },
        );
    });

    Ok(eve_proto::PyRep::keyval([
        ("mjuID", eve_proto::PyRep::Integer(mju_id)),
        ("shipID", eve_proto::PyRep::Integer(ship_id)),
        ("spoolSeconds", eve_proto::PyRep::Real(MMJU_SPOOL_TIME_SECS)),
        ("jumpDistanceMeters", eve_proto::PyRep::Real(MMJU_JUMP_DISTANCE_M)),
        ("spoolCompleteUnix", eve_proto::PyRep::Integer(spool_complete_unix as i64)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deployable_runtime_state_backward_compat_mmju_spools() {
        let legacy_json = r#"{
            "deployables": {
                "7001": {
                    "item_id": 7001,
                    "type_id": 33475,
                    "group_id": 1246,
                    "owner_id": 9001,
                    "solar_system_id": 30002537,
                    "anchor_start_unix": 1000,
                    "anchor_duration_secs": 60,
                    "anchor_completed": true,
                    "expires_at_unix": 50000,
                    "last_interaction_unix": 1000,
                    "reinforced_until_unix": null,
                    "position": [10.0, 20.0, 30.0]
                }
            },
            "damaged_items": {},
            "mmju_spools": [
                {
                    "ship_id": 8001,
                    "char_id": 9001,
                    "unit_id": 7001,
                    "solar_system_id": 30002537,
                    "started_unix": 1200,
                    "complete_at_unix": 1212,
                    "direction": [1.0, 0.0, 0.0]
                }
            ]
        }"#;
        let parsed: MobileDeployableRuntimeState =
            serde_json::from_str(legacy_json).expect("legacy Vec mmju_spools must deserialize");
        assert_eq!(parsed.deployables.len(), 1);
        assert!(parsed.deployables.contains_key(&7001));
        assert_eq!(parsed.mmju_spools.len(), 1);
        assert_eq!(parsed.mmju_spools[&8001].unit_id, 7001);
    }
}

#[cfg(test)]
#[path = "mobile_deployables_unit_tests.rs"]
mod mobile_deployables_unit_tests;

