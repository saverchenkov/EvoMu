//! `mobile_deployables` — Mobile Deployable Structures Service and Mechanics (`feat:structures.mobile-depot`,
//! `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-tractor`,
//! `feat:structures.mobile-tractor-rules`, `feat:structures.mobile-cyno-beacon`,
//! `feat:structures.mobile-micro-jump-unit`, `feat:structures.mobile-deploy-restrictions`).
//!
//! Handles deployment restrictions (starter systems, career-agent systems, proximity to gates/stations/structures/MTUs),
//! anchoring lifecycles, Mobile Depot reinforcement & refitting, MTU automated tractoring & looting,
//! Mobile Cynosural Beacon spool & fleet jump visibility, and Mobile Micro Jump Unit 12s warm-up & 100km jumps.

use crate::router::ServerContext;
use eve_destiny::{BallId, DamageVector};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Well-known Mobile Deployable Groups
pub const GROUP_MOBILE_DEPOT: u32 = 1246;
pub const GROUP_MOBILE_TRACTOR: u32 = 1250;
pub const GROUP_MOBILE_MICRO_JUMP_UNIT: u32 = 1276;
pub const GROUP_MOBILE_CYNO_BEACON: u32 = 4093;
pub const GROUP_MOBILE_WARP_DISRUPTOR: u32 = 361;
pub const CATEGORY_DEPLOYABLE: u32 = 22;

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

pub const MOBILE_DEPLOYABLE_NOTE_ID: i64 = -8_650_000_000_000;
pub const MOBILE_DEPLOYABLE_NOTE_OWNER: i64 = 1;

/// Persisted runtime state for mobile deployable structures.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MobileDeployableRuntimeState {
    pub deployables: HashMap<i64, MobileDeployableRecord>,
    pub damaged_items: HashMap<i64, PersistedDamageRecord>,
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
    pub expires_at_unix: u64,
    pub last_interaction_unix: u64,
    pub reinforced_until_unix: Option<u64>,
    pub reinforcement_used: bool,
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
    pub reinforcement_used: bool,
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
        matches!(
            t.group_id,
            GROUP_MOBILE_DEPOT
                | GROUP_MOBILE_TRACTOR
                | GROUP_MOBILE_MICRO_JUMP_UNIT
                | GROUP_MOBILE_CYNO_BEACON
        )
    })
}

/// Checks if a mobile structure can be scooped back into cargo.
/// Mobile Micro Jump Units (group 1276) and Cyno Beacons (group 4093) cannot be scooped.
pub fn can_deployable_be_scooped(group_id: u32) -> bool {
    !matches!(
        group_id,
        GROUP_MOBILE_MICRO_JUMP_UNIT | GROUP_MOBILE_CYNO_BEACON
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

    // 2. Mobile Cyno Beacon fleet requirement
    if group_id == GROUP_MOBILE_CYNO_BEACON && session.fleet_id.is_none() {
        return Err(BridgeRpcError {
            code: "MustBeInFleet".into(),
            message: "Deploying a Mobile Cynosural Beacon requires active fleet membership".into(),
        });
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
        GROUP_MOBILE_MICRO_JUMP_UNIT => MMJU_LIFETIME_SECS,
        GROUP_MOBILE_CYNO_BEACON => {
            if type_id == TYPE_COVERT_MOBILE_CYNO_BEACON {
                COVERT_CYNO_BEACON_DURATION_SECS
            } else {
                CYNO_BEACON_DURATION_SECS
            }
        }
        _ => 2 * 86_400,
    };

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
            expires_at_unix: now_unix.saturating_add(lifetime_secs),
            last_interaction_unix: now_unix,
            reinforced_until_unix: None,
            reinforcement_used: damaged.as_ref().map(|d| d.reinforcement_used).unwrap_or(false),
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
        let reinf_used = st.deployables.get(&item_id).map(|r| r.reinforcement_used).unwrap_or(false);
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
                reinforcement_used: reinf_used,
            },
        );
    });
}

/// Advances mobile deployable simulation in the solar system actor on each server tick (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`, `feat:structures.mobile-depot-lifetime-reinforce`).
/// Executes automated expiry sweeps and MTU automated tractoring & hold-capacity looting.
pub fn step_mobile_deployables_in_actor(
    ballpark: &mut eve_destiny::Ballpark,
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

    // 2. MTU tractoring and looting
    let active_mtus: Vec<MobileDeployableRecord> = db.with_snapshot(|snap| {
        let st = read_deployable_state_snap(snap);
        st.deployables
            .values()
            .filter(|r| {
                r.solar_system_id == solar_system_id
                    && r.group_id == GROUP_MOBILE_TRACTOR
                    && (r.anchor_completed || now_unix >= r.anchor_start_unix.saturating_add(r.anchor_duration_secs))
            })
            .cloned()
            .collect()
    });

    for mtu in active_mtus {
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
        let mtu_capacity = sde
            .types
            .get(&mtu.type_id)
            .map(|t| t.capacity)
            .filter(|&c| c > 0.0)
            .unwrap_or(27_000.0);

        // Find candidate wrecks and cargo containers within 125km owned by the MTU owner
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
                // Only wrecks/containers MTU owner may loot (wiki: "MTUs do not loot wrecks that are not yours")
                if it.owner_id != mtu.owner_id {
                    continue;
                }
                if let Some(pos_arr) = snap.item_positions.get(&it.item_id) {
                    let p = DVec3::from_array(*pos_arr);
                    let dist = (p - mtu_pos).length();
                    if dist <= 125_000.0 {
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

        if target_dist <= 2_500.0 {
            // Loot cargo into MTU hold, enforcing capacity
            db.with_mut_snapshot(|snap| {
                let mut current_vol: f64 = snap
                    .items
                    .values()
                    .filter(|c| c.location_id == mtu.item_id && c.quantity > 0)
                    .map(|c| {
                        let u_vol = sde.types.get(&c.type_id).map(|t| t.volume).unwrap_or(0.1);
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
                    let u_vol = sde.types.get(&child.type_id).map(|t| t.volume).unwrap_or(0.1);
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
            // Tractor target towards MTU at 1,000 m/s
            let dir = (mtu_pos - target_pos).normalize_or_zero();
            let tractor_speed = 1_000.0;
            let step = tractor_speed * dt;
            let new_pos = if step >= target_dist - 2_500.0 {
                mtu_pos - dir * 2_500.0
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

            // Anchoring phase: never reinforces!
            let is_anchoring = !rec.anchor_completed && now_unix < rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs);
            if is_anchoring {
                let (applied, destroyed) = cs.apply_damage(damage);
                if destroyed {
                    ballpark.balls.remove(&ball_id);
                }
                return Some((applied, destroyed, ball_snap));
            }

            // Reinforcement threshold at <= 25% shield HP
            let shield_threshold = cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;
            if !rec.reinforcement_used && cs.shield_hp > shield_threshold {
                let damage_to_threshold = cs.shield_hp - shield_threshold;
                let incoming_total = damage.total();
                if incoming_total >= damage_to_threshold {
                    cs.shield_hp = shield_threshold;
                    let reinf_until = now_unix.saturating_add(DEPOT_REINFORCE_DURATION_SECS);
                    db.with_mut_snapshot(|snap| {
                        write_deployable_state_snap(snap, |st| {
                            if let Some(r) = st.deployables.get_mut(&ball_id) {
                                r.reinforced_until_unix = Some(reinf_until);
                                r.reinforcement_used = true;
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
            });
        }
        return Some((applied, destroyed, ball_opt));
    }

    None
}
