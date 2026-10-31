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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// parity: feat:structures.mobile-cyno-beacon, feat:structures.mobile-micro-jump-unit, feat:structures.mobile-observatory-decloak-ping
// Well-known Mobile Deployable Groups
pub const GROUP_MOBILE_DEPOT: u32 = 1246;
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

pub const MOBILE_DEPLOYABLE_NOTE_ID: i64 = -8_650_000_000_000;
pub const MOBILE_DEPLOYABLE_NOTE_OWNER: i64 = 1;

/// Persisted runtime state for mobile deployable structures.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MobileDeployableRuntimeState {
    pub deployables: HashMap<i64, MobileDeployableRecord>,
    pub damaged_items: HashMap<i64, PersistedDamageRecord>,
    #[serde(default)]
    pub mmju_spools: Vec<MmjuSpoolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmjuSpoolEntry {
    pub ship_id: i64,
    pub mju_id: i64,
    pub solar_system_id: u32,
    pub spool_complete_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileDeployableRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub group_id: u32,
    pub owner_id: i64,
    pub solar_system_id: u32,
    pub anchor_start_unix: u64,
    pub anchor_duration_secs: u64,
    pub anchor_completed: bool,
    #[serde(default)]
    pub is_unanchored: bool,
    pub expires_at_unix: u64,
    pub last_interaction_unix: u64,
    pub reinforced_until_unix: Option<u64>,
    #[serde(default)]
    pub last_ping_unix: Option<u64>,
    pub position: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDamageRecord {
    pub shield_hp: f64,
    pub max_shield_hp: f64,
    pub armor_hp: f64,
    pub max_armor_hp: f64,
    pub structure_hp: f64,
    pub max_structure_hp: f64,
}

pub fn read_deployable_state_snap(snap: &eve_db::WorldStateSnapshot) -> MobileDeployableRuntimeState {
    snap.owner_notes
        .get(&MOBILE_DEPLOYABLE_NOTE_ID)
        .and_then(|n| serde_json::from_str::<MobileDeployableRuntimeState>(&n.note).ok())
        .unwrap_or_default()
}

pub fn write_deployable_state_snap<R>(
    snap: &mut eve_db::WorldStateSnapshot,
    f: impl FnOnce(&mut MobileDeployableRuntimeState) -> R,
) -> R {
    let mut state = read_deployable_state_snap(snap);
    let out = f(&mut state);
    if let Ok(json) = serde_json::to_string(&state) {
        let ts = snap
            .owner_notes
            .get(&MOBILE_DEPLOYABLE_NOTE_ID)
            .map(|n| n.timestamp_unix)
            .unwrap_or(0);
        snap.owner_notes.insert(
            MOBILE_DEPLOYABLE_NOTE_ID,
            eve_db::OwnerNoteRecord {
                note_id: MOBILE_DEPLOYABLE_NOTE_ID,
                character_id: MOBILE_DEPLOYABLE_NOTE_OWNER,
                item_id: 0,
                label: "S:MOBILE_DEPLOYABLE_RUNTIME_STATE".to_string(),
                note: json,
                timestamp_unix: ts,
            },
        );
    }
    out
}

pub fn read_deployable_state(ctx: &ServerContext) -> MobileDeployableRuntimeState {
    ctx.db.with_snapshot(read_deployable_state_snap)
}

pub fn write_deployable_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut MobileDeployableRuntimeState) -> R,
) -> R {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let out = write_deployable_state_snap(snap, f);
        if let Some(note) = snap.owner_notes.get_mut(&MOBILE_DEPLOYABLE_NOTE_ID) {
            note.timestamp_unix = now_unix;
        }
        out
    })
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
/// Mobile Micro Jump Units (group 1276), Cyno Beacons (group 4093), Mobile Observatories (group 4107), and CRAB Beacons (group 4137) cannot be scooped.
pub fn can_deployable_be_scooped(group_id: u32) -> bool {
    !matches!(
        group_id,
        GROUP_MOBILE_MICRO_JUMP_UNIT
            | GROUP_MOBILE_CYNO_BEACON
            | GROUP_MOBILE_OBSERVATORY
            | GROUP_CRAB_BEACON
    )
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
    _type_id: u32,
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

    // 2. Mobile Cyno Beacon & Mobile Observatory Anchoring III + space/fleet requirements
    if matches!(group_id, GROUP_MOBILE_CYNO_BEACON | GROUP_MOBILE_OBSERVATORY) {
        let char_id = session.character_id.unwrap_or(0);
        let anchoring_lvl = ctx.db.with_snapshot(|snap| {
            snap.skills
                .get(&char_id)
                .and_then(|sk| sk.get(&SKILL_ANCHORING))
                .map(|r| r.level)
                .unwrap_or(0)
        });
        if anchoring_lvl < 3 {
            return Err(BridgeRpcError {
                code: "SkillRequired".into(),
                message: "Deploying this structure requires Anchoring III".into(),
            });
        }
    }
    if group_id == GROUP_MOBILE_CYNO_BEACON && session.fleet_id.is_none() {
        return Err(BridgeRpcError {
            code: "MustBeInFleet".into(),
            message: "Deploying a Mobile Cynosural Beacon requires active fleet membership".into(),
        });
    }
    if group_id == GROUP_MOBILE_OBSERVATORY {
        if (31_000_000..32_000_000).contains(&sys_id) {
            return Err(BridgeRpcError {
                code: "CannotDeployInWormhole".into(),
                message: "Mobile Observatory cannot be deployed in wormhole space".into(),
            });
        }
        if (32_000_000..33_000_000).contains(&sys_id) {
            return Err(BridgeRpcError {
                code: "CannotDeployInAbyssalSpace".into(),
                message: "Mobile Observatory cannot be deployed in Abyssal space".into(),
            });
        }
        if ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070) {
            return Err(BridgeRpcError {
                code: "CannotDeployInPochven".into(),
                message: "Mobile Observatory cannot be deployed in Pochven".into(),
            });
        }
        let sys_sec = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(1.0);
        if sys_sec >= 0.45 {
            return Err(BridgeRpcError {
                code: "CannotDeployInHighSec".into(),
                message: "Mobile Observatory cannot be deployed in high-security space".into(),
            });
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
            return Err(BridgeRpcError {
                code: "CannotDeployInDeadspace".into(),
                message: "Mobile Observatory cannot be deployed in deadspace".into(),
            });
        }
    }
    if group_id == GROUP_CRAB_BEACON {
        crate::services::pve_crab_beacons::validate_crab_deployment_system(&ctx.sde, sys_id)?;
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
            if st.solar_system_id == sys_id {
                if let Some(pos_arr) = snap.item_positions.get(&st.structure_id) {
                    let spos = DVec3::from_array(*pos_arr);
                    if (anchor_pos - spos).length() < MIN_STRUCTURE_DISTANCE_M {
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

        // Distance from another Mobile Depot (>= 5 km)
        if group_id == GROUP_MOBILE_DEPOT {
            for it in snap.items.values() {
                if it.item_id != item_id && it.location_id == sys_id as i64 {
                    let it_grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
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
    let lifetime_secs = match group_id {
        GROUP_MOBILE_DEPOT => DEPOT_LIFETIME_SECS,
        GROUP_MOBILE_TRACTOR => MTU_LIFETIME_SECS,
        GROUP_MOBILE_MICRO_JUMP_UNIT => MMJU_ACTIVATION_DELAY_SECS.saturating_add(MMJU_LIFETIME_SECS),
        GROUP_MOBILE_CYNO_BEACON => {
            let active_dur = if type_id == TYPE_COVERT_MOBILE_CYNO_BEACON {
                COVERT_CYNO_BEACON_DURATION_SECS
            } else {
                CYNO_BEACON_DURATION_SECS
            };
            CYNO_BEACON_SPOOL_SECS.saturating_add(active_dur)
        }
        GROUP_MOBILE_OBSERVATORY => OBSERVATORY_LIFETIME_SECS,
        GROUP_CRAB_BEACON => crate::services::pve_crab_beacons::CRAB_BEACON_IDLE_LIFETIME_SECS,
        _ => 2 * 86_400,
    };

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
    solar_system_id: u32,
    dt: f64,
    now_unix: u64,
) {
    let (Some(db), Some(sde)) = (db_opt, sde_opt) else { return; };

    // 1. Expiry sweep: find all deployables in this solar system where now_unix >= expires_at_unix
    let expired_ids: Vec<i64> = db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        st.deployables
            .values()
            .filter(|r| r.solar_system_id == solar_system_id && now_unix >= r.expires_at_unix)
            .map(|r| r.item_id)
            .collect()
    });

    if !expired_ids.is_empty() {
        for &eid in &expired_ids {
            ballpark.balls.remove(&eid);
            ballpark.combat_states.remove(&eid);
        }
        db.with_mut_snapshot(|snap| {
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
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&eid);
                });
            }
        });
    }

    // 2. Clear expired reinforcement timers, process MMJU spools, and execute Mobile Observatory decloak pings
    let mut ready_mmju_jumps: Vec<(i64, [f64; 3])> = Vec::new();
    let mut seen_mmju_ships: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut observatory_pings: Vec<i64> = Vec::new();
    db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            for rec in st.deployables.values_mut() {
                if rec.solar_system_id == solar_system_id {
                    if let Some(reinf_until) = rec.reinforced_until_unix {
                        if now_unix >= reinf_until {
                            rec.reinforced_until_unix = None;
                        }
                    }
                    if rec.group_id == GROUP_MOBILE_OBSERVATORY
                        && !rec.is_unanchored
                        && now_unix >= rec.anchor_start_unix.saturating_add(OBSERVATORY_DEPLOY_SECS)
                        && now_unix < rec.expires_at_unix
                    {
                        let should_ping = match rec.last_ping_unix {
                            None => true,
                            Some(last) => now_unix >= last.saturating_add(OBSERVATORY_PING_INTERVAL_SECS),
                        };
                        if should_ping {
                            rec.last_ping_unix = Some(now_unix);
                            observatory_pings.push(rec.item_id);
                        }
                    }
                }
            }
            let mut remaining_spools = Vec::with_capacity(st.mmju_spools.len());
            for sp in st.mmju_spools.drain(..) {
                if sp.solar_system_id == solar_system_id && now_unix >= sp.spool_complete_unix {
                    if seen_mmju_ships.insert(sp.ship_id) {
                        if let Some(mju_rec) = st.deployables.get(&sp.mju_id) {
                            if !mju_rec.is_unanchored && now_unix < mju_rec.expires_at_unix {
                                ready_mmju_jumps.push((sp.ship_id, mju_rec.position));
                            }
                        }
                    }
                } else {
                    remaining_spools.push(sp);
                }
            }
            st.mmju_spools = remaining_spools;
        });
    });

    for (ship_id, mju_pos_arr) in ready_mmju_jumps {
        let (pilot_still_in_ship, dyn_mass_add) = db.with_snapshot(|snap| {
            let ok = snap
                .characters
                .values()
                .any(|c| c.active_ship_id == ship_id && c.station_id.is_none() && c.structure_id.is_none());
            let plate_add: f64 = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && matches!(i.flag as u16, 11..=34 | 92..=99 | 125..=132)
                        && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                })
                .filter_map(|i| {
                    let t = sde.types.get(&i.type_id)?;
                    let m_act = snap.module_active_states.contains(&i.item_id);
                    let base = if t.group_id == 46 && !m_act { 0.0 } else { t.default_attributes.get(&796).copied().unwrap_or(0.0) };
                    let active_add = if m_act { t.default_attributes.get(&67).copied().unwrap_or(0.0) } else { 0.0 };
                    Some(base + active_add)
                })
                .sum();
            (ok, plate_add)
        });
        if !pilot_still_in_ship {
            continue;
        }
        let mju_pos = DVec3::from_array(mju_pos_arr);
        let ship_valid = ballpark.balls.get(&ship_id).is_some_and(|b| {
            let base_hull_mass = sde.types.get(&b.type_id).map(|t| t.mass).unwrap_or(b.mass);
            let effective_mass = b.mass.max(base_hull_mass + dyn_mass_add);
            !matches!(b.mode, eve_destiny::MovementMode::Warp { .. })
                && effective_mass < MMJU_MAX_SHIP_MASS_KG
                && (b.position - mju_pos).length() <= 2_500.0
        }) && !ballpark
            .combat_states
            .get(&ship_id)
            .is_some_and(|cs| cs.is_cloaked || cs.is_mwd_mjd_disabled)
            && !ballpark.is_ball_scrambled(ship_id);
        if !ship_valid {
            continue;
        }
        if let Ok(new_pos) = ballpark.micro_jump(ship_id, MMJU_JUMP_DISTANCE_M) {
            db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(ship_id, [new_pos.x, new_pos.y, new_pos.z]);
            });
        }
    }

    for _obs_id in observatory_pings {
        let mut cloaked_ships: Vec<i64> = ballpark
            .combat_states
            .iter()
            .filter(|(_, cs)| {
                cs.is_cloaked
                    && cs.gate_cloak_remaining_ticks == 0
                    && cs.cloak_stabilization_remaining_s <= 0.0
            })
            .map(|(&sid, _)| sid)
            .collect();
        cloaked_ships.sort_unstable();
        for sid in cloaked_ships {
            if !mechanic_rng.roll(OBSERVATORY_DECLOAK_CHANCE) {
                continue;
            }
            let mut removed_cloaks = Vec::new();
            let mut kept_cycles = Vec::with_capacity(active_module_cycles.len());
            for cyc in active_module_cycles.drain(..) {
                let is_cloak = cyc.ship_id == sid
                    && (cyc.effect_name.to_ascii_lowercase().contains("cloak")
                        || sde.types.get(&cyc.module_type_id).is_some_and(|t| t.group_id == 330));
                if is_cloak {
                    removed_cloaks.push(cyc);
                } else {
                    kept_cycles.push(cyc);
                }
            }
            *active_module_cycles = kept_cycles;

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
                .unwrap_or(0.0);
            if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
                if cs.cloak_configured_targeting_delay_s <= 0.0 && sde_tgt_delay_s > 0.0 {
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
        }
    }

    // 3. MTU tractoring and looting
    let active_mtus: Vec<MobileDeployableRecord> = db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        st.deployables
            .values()
            .filter(|r| {
                r.solar_system_id == solar_system_id
                    && r.group_id == GROUP_MOBILE_TRACTOR
                    && !r.is_unanchored
                    && snap.items.contains_key(&r.item_id)
                    && (r.anchor_completed || (r.anchor_start_unix > 0 && now_unix >= r.anchor_start_unix.saturating_add(r.anchor_duration_secs)))
            })
            .cloned()
            .collect()
    });

    for mtu in active_mtus {
        // Skip and remove records whose item was deleted/destroyed (MD-P0-1)
        if !db.with_snapshot(|snap| snap.items.contains_key(&mtu.item_id)) {
            db.with_mut_snapshot(|snap| {
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&mtu.item_id);
                });
            });
            continue;
        }

        // Owner must be present in the solar system and active in space (not docked elsewhere)
        let is_docked_or_elsewhere = db.with_snapshot(|snap| {
            snap.characters.get(&mtu.owner_id).is_some_and(|c| {
                c.station_id.is_some() || c.structure_id.is_some() || c.solar_system_id != solar_system_id
            })
        });
        if is_docked_or_elsewhere {
            continue;
        }

        let owner_in_system = ballpark.ball_slim_meta.values().any(|m| m.char_id == Some(mtu.owner_id))
            || db.with_snapshot(|snap| {
                snap.characters.get(&mtu.owner_id).is_some_and(|c| {
                    c.solar_system_id == solar_system_id && c.station_id.is_none() && c.structure_id.is_none()
                })
            });
        if !owner_in_system {
            continue;
        }

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

        let current_mtu_vol: f64 = db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|c| c.location_id == mtu.item_id && c.quantity > 0)
                .map(|c| {
                    let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, c);
                    u_vol * (c.quantity.max(1) as f64)
                })
                .sum()
        });

        // Find candidate wrecks and cargo containers within range owned by MTU owner (MD-P1-1)
        let candidates = db.with_snapshot(|snap| {
            let mut list = Vec::new();
            for it in snap.items.values() {
                if it.location_id != solar_system_id as i64 || it.item_id == mtu.item_id {
                    continue;
                }
                let is_wreck = sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186);
                let is_container = it.type_id == 23 || sde.types.get(&it.type_id).is_some_and(|t| matches!(t.group_id, 12 | 340 | 448));
                if !is_wreck && !is_container {
                    continue;
                }
                if it.owner_id != mtu.owner_id {
                    continue;
                }

                // Exclude empty wrecks and cargo containers (MD-P1-1)
                let items_in_target: Vec<&eve_db::InventoryItemRecord> = snap.items
                    .values()
                    .filter(|c| c.location_id == it.item_id && c.quantity > 0)
                    .collect();
                if items_in_target.is_empty() {
                    continue;
                }

                if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                    let p = DVec3::from_array(*pos_arr);
                    let dist = (p - mtu_pos).length();
                    if dist <= max_tractor_range {
                        // Targets sitting in loot range that cannot fit in MTU capacity are processed (MD-P1-1)
                        if dist <= loot_radius {
                            let can_loot_any = items_in_target.iter().any(|c| {
                                let u_vol = crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(sde, c);
                                current_mtu_vol + u_vol * (c.quantity.max(1) as f64) <= mtu_capacity
                            });
                            if !can_loot_any {
                                continue;
                            }
                        }
                        list.push((it.item_id, dist, p));
                    }
                }
            }
            list
        });

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

            // Anchoring phase or unanchored: takes direct damage, never reinforces!
            let is_anchored = !rec.is_unanchored
                && (rec.anchor_completed
                    || (rec.anchor_start_unix > 0 && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)));
            if !is_anchored {
                let (applied, destroyed) = cs.apply_damage(damage);
                if destroyed {
                    ballpark.balls.remove(&ball_id);
                }
                return Some((applied, destroyed, ball_snap));
            }

            // Reinforcement threshold at <= 25% shield HP
            let shield_threshold = cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;
            if cs.shield_hp > shield_threshold {
                let damage_to_threshold = cs.shield_hp - shield_threshold;
                let incoming_total = damage.total();
                if incoming_total >= damage_to_threshold {
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
                    cs.shield_hp -= incoming_total;
                    return Some((incoming_total, false, ball_snap));
                }
            }
        }

        // Already reinforced or shields <= 25%: take damage directly without reinforcement
        let (applied, destroyed) = cs.apply_damage(damage);
        if destroyed {
            ballpark.balls.remove(&ball_id);
            let pos_arr = db.with_snapshot(|s| s.item_positions.get(&ball_id).copied()).unwrap_or([0.0; 3]);
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
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&ball_id);
                });
            });
        }
        return Some((applied, destroyed, ball_snap));
    }

    if it_grp == GROUP_MOBILE_TRACTOR {
        let (applied, destroyed, ball_opt) = ballpark.apply_damage_to_ball(ball_id, damage)?;
        if destroyed {
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
            db.with_mut_snapshot(|snap| {
                snap.items.remove(&ball_id);
                snap.item_positions.remove(&ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
                write_deployable_state_snap(snap, |st| {
                    st.deployables.remove(&ball_id);
                });
            });
        }
        return Some((applied, destroyed, ball_opt));
    }

    None
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
            message: format!(
                "Ship mass {:.0} kg exceeds Mobile Micro Jump Unit limit of {:.0} kg",
                eval_mass, MMJU_MAX_SHIP_MASS_KG
            ),
        });
    }
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot activate Mobile Micro Jump Unit while warping".into(),
        });
    }
    let ship_cs = sys_handle.get_combat_state(ship_id).await.ok().flatten();
    if ship_cs.as_ref().is_some_and(|cs| cs.is_cloaked) {
        return Err(BridgeRpcError {
            code: "ShipCloaked".into(),
            message: "Cannot activate Mobile Micro Jump Unit while cloaked".into(),
        });
    }
    if sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false)
        || ship_cs.as_ref().is_some_and(|cs| cs.is_mwd_mjd_disabled)
    {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot activate Mobile Micro Jump Unit while warp scrambled".into(),
        });
    }
    let already_spooling = read_deployable_state(ctx)
        .mmju_spools
        .iter()
        .any(|sp| sp.ship_id == ship_id && sp.spool_complete_unix > now_unix);
    if already_spooling {
        return Err(BridgeRpcError {
            code: "MicroJumpAlreadySpooling".into(),
            message: "Ship is already spooling a Mobile Micro Jump Unit".into(),
        });
    }

    let spool_complete_unix = now_unix.saturating_add(MMJU_SPOOL_TIME_SECS as u64);
    write_deployable_state(ctx, |st| {
        st.mmju_spools.push(MmjuSpoolEntry {
            ship_id,
            mju_id,
            solar_system_id: sys_id,
            spool_complete_unix,
        });
    });

    Ok(eve_proto::PyRep::keyval([
        ("mjuID", eve_proto::PyRep::Integer(mju_id)),
        ("shipID", eve_proto::PyRep::Integer(ship_id)),
        ("spoolSeconds", eve_proto::PyRep::Real(MMJU_SPOOL_TIME_SECS)),
        ("jumpDistanceMeters", eve_proto::PyRep::Real(MMJU_JUMP_DISTANCE_M)),
        ("spoolCompleteUnix", eve_proto::PyRep::Integer(spool_complete_unix as i64)),
    ]))
}
