//! Background cluster scheduler (`plan:SP-7`), ballpark object reload (`feat:lifecycle.world-restart-space-objects`,
//! `feat:lifecycle.wreck-restart`, `feat:lifecycle.structure-restart`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`),
//! server-driven self-destruct (`feat:lifecycle.self-destruct-server-timer`), Abyssal disconnect collapse
//! (`feat:lifecycle.abyssal-disconnect`), and daily downtime wreck/mission maintenance
//! (`feat:lifecycle.wreck-downtime`, `feat:lifecycle.mission-downtime-respawn`).

#![forbid(unsafe_code)]

use super::{
    load_sp6_state_from_snap, safe_logoff_blocking_space_conditions,
    sde_mobile_warp_disruptor_decay_secs, sde_ship_physics, with_mut_sp6_state,
    AbyssalPersistedSession, DisconnectedShipEntry, SafeLogoffEntry, SelfDestructPersistedEntry,
    JETCAN_DECAY_LIFETIME_SECS, WRECK_DECAY_LIFETIME_SECS,
};

pub const PLANETARY_LAUNCH_DECAY_SECS: u64 = 5 * 86_400; // 5d (432,000s)
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use crate::system_actor::PendingAnchoringDisruptor;
use eve_db::WorldRepository;
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::Arc;

fn default_combat_state_for_type(sde: &SdeRegistry, type_id: u32) -> CombatState {
    let a = sde.types.get(&type_id).map(|t| &t.default_attributes);
    let shield = a.and_then(|m| m.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied()).unwrap_or(100.0);
    let armor = a.and_then(|m| m.get(&eve_sde::attr_ids::ARMOR_HP).copied()).unwrap_or(100.0);
    let hull = a.and_then(|m| m.get(&eve_sde::attr_ids::HP_STRUCT).copied()).unwrap_or(500.0);
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: hull,
        max_structure_hp: hull,
        ..CombatState::default()
    }
}

/// Re-materializes persisted in-space objects into `ballpark` when `SystemRegistry::get_or_spawn(solar_system_id)`
/// initializes a solar system actor (`feat:lifecycle.world-restart-space-objects`, `feat:lifecycle.wreck-restart`,
/// `feat:lifecycle.structure-restart`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`,
/// `feat:lifecycle.drones-in-space-persist-restart`, `plan:SP-6`, `plan:SP-7`).
// parity: feat:lifecycle.world-restart-space-objects, feat:lifecycle.wreck-restart, feat:lifecycle.structure-restart, feat:lifecycle.mobile_warp_disruptor-lifecycle, feat:lifecycle.drones-in-space-persist-restart
pub fn populate_persisted_space_objects(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
    db: &Arc<WorldRepository>,
) -> Vec<PendingAnchoringDisruptor> {
    let now_unix = db.clock().now_unix_secs();
    let mut pending_disruptors = Vec::new();
    let mut expired_item_ids: Vec<i64> = Vec::new();
    let mut expired_disconnected_chars: Vec<(i64, i64, [f64; 3])> = Vec::new();

    db.with_snapshot(|snap| {
        let sp6 = load_sp6_state_from_snap(snap);

        // 1. Upwell Structures (`feat:lifecycle.structure-restart`)
        for st in snap.structures.values() {
            if st.solar_system_id == solar_system_id && st.state != "Destroyed" {
                if ballpark.balls.contains_key(&st.structure_id) {
                    continue;
                }
                let st_type = sde.types.get(&st.type_id);
                let (group_id, category_id, radius, mass) = st_type
                    .map(|t| {
                        (
                            t.group_id,
                            t.category_id,
                            t.radius.max(1_000.0),
                            t.mass.max(1_000_000_000.0),
                        )
                    })
                    .unwrap_or((1657, 65, 10_000.0, 100_000_000_000.0));
                let pos = DVec3::from_array(st.position);
                ballpark.set_ball_slim_meta(
                    st.structure_id,
                    SlimItemMeta {
                        group_id,
                        category_id,
                        owner_id: st.owner_corp_id,
                        char_id: None,
                        corp_id: st.owner_corp_id,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                );
                ballpark.add_ball(Ball {
                    id: st.structure_id,
                    type_id: st.type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                });
                let sde_cs = default_combat_state_for_type(sde, st.type_id);
                let max_sh = sde_cs.max_shield_hp.max(st.shield_hp).max(1.0);
                let max_ar = sde_cs.max_armor_hp.max(st.armor_hp).max(1.0);
                let max_hu = sde_cs.max_structure_hp.max(st.hull_hp).max(1.0);
                ballpark.combat_states.insert(
                    st.structure_id,
                    CombatState {
                        shield_hp: st.shield_hp.clamp(0.0, max_sh),
                        max_shield_hp: max_sh,
                        armor_hp: st.armor_hp.clamp(0.0, max_ar),
                        max_armor_hp: max_ar,
                        structure_hp: st.hull_hp.clamp(1.0, max_hu),
                        max_structure_hp: max_hu,
                        ..CombatState::default()
                    },
                );
            }
        }

        // Collect active player ship IDs so we don't spawn docked/offline ships as generic items
        let player_ship_ids: std::collections::HashSet<i64> = snap
            .characters
            .values()
            .map(|c| c.active_ship_id)
            .collect();

        // 2. Persisted space items: wrecks, jetcans, corpses, anchored deployables, Mobile Warp Disruptors, abandoned drones
        for item in snap.items.values() {
            if item.location_id != solar_system_id as i64 {
                continue;
            }
            if player_ship_ids.contains(&item.item_id) {
                continue;
            }
            let (group_id, category_id, mass, radius, max_vel) = sde
                .types
                .get(&item.type_id)
                .map(|t| {
                    let mv = t.default_attributes.get(&37).copied().unwrap_or(0.0);
                    (
                        t.group_id,
                        t.category_id,
                        t.mass.max(100.0),
                        t.radius.max(10.0),
                        mv,
                    )
                })
                .unwrap_or_else(|| {
                    if item.type_id == 26557 {
                        (186, 2, 10_000.0, 30.0, 0.0)
                    } else if item.type_id == 23 {
                        (12, 2, 1_000.0, 20.0, 0.0)
                    } else if item.type_id == 25 {
                        (14, 2, 80.0, 5.0, 0.0)
                    } else {
                        (0, 0, 1_000.0, 25.0, 0.0)
                    }
                });

            // Ignore non-space ships (handled in step 3) and asteroids (handled by SpawnAsteroid / PopulateAsteroids)
            if category_id == 6 || category_id == 25 || snap.asteroids.contains_key(&item.item_id) {
                continue;
            }

            let obj_meta = sp6.space_object_meta.get(&item.item_id);
            let is_wreck = item.type_id == 26557 || group_id == 186;
            let is_jetcan = item.type_id == 23;
            let is_corpse = item.type_id == 25;
            let is_mwd_bubble = group_id == 361;
            let is_drone = category_id == 18 || category_id == 87;
            let is_deployable_or_container = category_id == 22
                || sde.types.get(&item.type_id).is_some_and(|t| t.effect_ids.contains(&649))
                || obj_meta.and_then(|m| m.anchor_start_unix).is_some()
                || snap.item_positions.contains_key(&item.item_id);

            if !is_wreck
                && !is_jetcan
                && !is_corpse
                && !is_mwd_bubble
                && !is_drone
                && !is_deployable_or_container
            {
                continue;
            }

            // Check decay lifetimes before spawning (`feat:lifecycle.wreck-restart`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`, `feat:lifecycle.pi-launch-canister`)
            if obj_meta.and_then(|m| m.planet_id).is_some() {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(PLANETARY_LAUNCH_DECAY_SECS) {
                        expired_item_ids.push(item.item_id);
                        continue;
                    }
                }
            } else if is_wreck || is_corpse {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(WRECK_DECAY_LIFETIME_SECS) {
                        expired_item_ids.push(item.item_id);
                        continue;
                    }
                }
            } else if is_jetcan {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(JETCAN_DECAY_LIFETIME_SECS) {
                        expired_item_ids.push(item.item_id);
                        continue;
                    }
                }
            } else if is_mwd_bubble {
                if let Some(anchor_start) =
                    obj_meta.and_then(|m| m.anchor_start_unix.or(m.created_unix))
                {
                    let decay_secs = sde_mobile_warp_disruptor_decay_secs(sde, item.type_id);
                    if now_unix >= anchor_start.saturating_add(decay_secs) {
                        expired_item_ids.push(item.item_id);
                        continue;
                    }
                }
            } else if sde.types.get(&item.type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0) {
                if let Some(anchor_start) =
                    obj_meta.and_then(|m| m.anchor_start_unix.or(m.created_unix))
                {
                    if now_unix >= anchor_start.saturating_add(super::SECURE_CONTAINER_DECAY_SECS) {
                        expired_item_ids.push(item.item_id);
                        continue;
                    }
                }
            }

            let ball_id = if is_drone {
                obj_meta
                    .and_then(|m| m.drone_ball_id)
                    .unwrap_or(item.item_id)
            } else {
                item.item_id
            };
            if ballpark.balls.contains_key(&ball_id) {
                continue;
            }

            let pos_arr = snap
                .item_positions
                .get(&item.item_id)
                .or_else(|| snap.item_positions.get(&ball_id))
                .copied()
                .unwrap_or([0.0, 0.0, 0.0]);
            let pos = DVec3::from_array(pos_arr);

            ballpark.set_ball_slim_meta(
                ball_id,
                SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: item.owner_id,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                },
            );
            ballpark.add_ball(Ball {
                id: ball_id,
                type_id: item.type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: if is_drone { max_vel } else { 0.0 },
                inertia: 1.0,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
            let mut cs = default_combat_state_for_type(sde, item.type_id);
            if let Some(hp) = sp6
                .ship_combat_hp
                .get(&item.item_id)
                .or_else(|| sp6.ship_combat_hp.get(&ball_id))
            {
                cs.shield_hp = hp.shield_hp.clamp(0.0, cs.max_shield_hp);
                cs.armor_hp = hp.armor_hp.clamp(0.0, cs.max_armor_hp);
                if hp.hull_hp > 0.0 {
                    cs.structure_hp = hp.hull_hp.clamp(0.0, cs.max_structure_hp);
                }
            }
            ballpark.combat_states.insert(ball_id, cs);

            if is_mwd_bubble {
                let bubble_radius = obj_meta
                    .and_then(|m| m.bubble_radius_m)
                    .or_else(|| {
                        sde.types
                            .get(&item.type_id)
                            .and_then(|t| t.default_attributes.get(&104).copied())
                    })
                    .unwrap_or(20_000.0);
                let anchor_start = obj_meta.and_then(|m| m.anchor_start_unix).unwrap_or(0);
                let anchor_delay = obj_meta.map(|m| m.anchoring_delay_secs).unwrap_or(0.0);
                let completed_at = anchor_start.saturating_add(anchor_delay.ceil() as u64);
                ballpark.set_ball_free(ball_id, false);
                ballpark.set_ball_radius(ball_id, bubble_radius);
                ballpark.set_ball_harmonic(ball_id, 1);
                if obj_meta.is_some_and(|m| m.anchor_completed) || now_unix >= completed_at {
                    ballpark.add_mobile_warp_disruptor(ball_id, pos, bubble_radius);
                } else {
                    let rem_s = (completed_at.saturating_sub(now_unix) as f64).max(1.0);
                    pending_disruptors.push(PendingAnchoringDisruptor {
                        id: ball_id,
                        type_id: item.type_id,
                        position: pos,
                        radius: bubble_radius,
                        remaining_s: rem_s,
                    });
                }
            }
        }

        // 3. Disconnected player ships still within their log-off window (`plan:SP-6`)
        for disc in sp6.disconnected_ships.values() {
            if disc.solar_system_id != solar_system_id || disc.removed_from_space {
                continue;
            }
            if now_unix >= disc.remove_at_unix {
                let final_pos = disc.emergency_warp_dest.unwrap_or(disc.last_position);
                expired_disconnected_chars.push((disc.char_id, disc.ship_id, final_pos));
                continue;
            }
            if ballpark.balls.contains_key(&disc.ship_id) {
                continue;
            }
            let pos = DVec3::from_array(disc.last_position);
            let (group_id, category_id, mass, radius, max_vel, inertia, warp_speed_au_s) =
                sde_ship_physics(sde, disc.ship_type_id);
            let (corp_id, alliance_id, sec_status) = snap
                .characters
                .get(&disc.char_id)
                .map(|c| {
                    (
                        c.corporation_id,
                        c.alliance_id.unwrap_or(-1),
                        c.security_status,
                    )
                })
                .unwrap_or((1_000_009, -1, 0.0));
            let mode = if let Some(dest_arr) = disc.emergency_warp_dest {
                MovementMode::new_warp(pos, DVec3::from_array(dest_arr), warp_speed_au_s)
            } else {
                MovementMode::Stop
            };
            ballpark.set_ball_slim_meta(
                disc.ship_id,
                SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: disc.char_id,
                    char_id: Some(disc.char_id),
                    corp_id,
                    alliance_id,
                    security_status: sec_status,
                    is_cloaked: false,
                },
            );
            ballpark.add_ball(Ball {
                id: disc.ship_id,
                type_id: disc.ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode,
            });
            let mut cs = default_combat_state_for_type(sde, disc.ship_type_id);
            if let Some(hp) = sp6.ship_combat_hp.get(&disc.ship_id) {
                cs.shield_hp = hp.shield_hp.clamp(0.0, cs.max_shield_hp);
                cs.armor_hp = hp.armor_hp.clamp(0.0, cs.max_armor_hp);
                if hp.hull_hp > 0.0 {
                    cs.structure_hp = hp.hull_hp.clamp(0.0, cs.max_structure_hp);
                }
            }
            ballpark.combat_states.insert(disc.ship_id, cs);
        }
    });

    if !expired_item_ids.is_empty() || !expired_disconnected_chars.is_empty() {
        with_mut_sp6_state(db, |st, snap| {
            for iid in &expired_item_ids {
                snap.items.remove(iid);
                snap.item_positions.remove(iid);
                st.space_object_meta.remove(iid);
                st.ship_combat_hp.remove(iid);
                snap.items.retain(|_, child| child.location_id != *iid);
            }
            for (cid, sid, final_pos) in expired_disconnected_chars {
                snap.item_positions.insert(sid, final_pos);
                if let Some(d) = st.disconnected_ships.get_mut(&cid) {
                    d.last_position = final_pos;
                    d.removed_from_space = true;
                }
            }
        });
    }

    pending_disruptors
}

/// Seeds abandoned [`crate::services::combat_crimewatch::DroneSpaceState`] entries for any persisted
/// in-space drones (`category_id == 18`, `location_id` in `30_000_000..=34_000_000`) when a
/// [`ServerContext`] initializes from SQLite (`feat:lifecycle.drones-in-space-persist-restart`).
// parity: feat:lifecycle.drones-in-space-persist-restart
pub fn seed_persisted_abandoned_drones(ctx: &ServerContext) {
    let mut drones = Vec::new();
    let mut fighters = Vec::new();
    ctx.db.with_snapshot(|snap| {
        for item in snap.items.values() {
            if !(30_000_000..=34_000_000).contains(&item.location_id) {
                continue;
            }
            let Some(t) = ctx.sde.types.get(&item.type_id) else {
                continue;
            };
            let ball_id = snap
                .sp6_lifecycle
                .space_object_meta
                .get(&item.item_id)
                .and_then(|m| m.drone_ball_id)
                .unwrap_or(item.item_id);
            if t.category_id == 87 {
                let max_squadron_size = t
                    .default_attributes
                    .get(&2215)
                    .copied()
                    .map(|v| v as u32)
                    .unwrap_or(9);
                fighters.push(crate::router::context::ActiveFighterSquadron {
                    ball_id,
                    item_id: item.item_id,
                    type_id: item.type_id,
                    owner_char_id: item.owner_id,
                    parent_ship_id: 0,
                    tube_index: 0,
                    squadron_size: (item.quantity.max(1) as u32).min(max_squadron_size),
                    target_id: None,
                    is_abandoned: true,
                    mwd_active: false,
                });
                continue;
            }
            if t.category_id != 18 {
                continue;
            }
            let attr = |aid: u16| -> Option<f64> {
                item.dynamic_attributes
                    .get(&aid)
                    .or_else(|| t.default_attributes.get(&aid))
                    .copied()
            };
            let bw_used = attr(1272).unwrap_or(5.0);
            let max_vel = attr(37).unwrap_or(1_500.0);
            let opt_range = attr(54).unwrap_or(5_000.0);
            let dmg_mult = attr(64).unwrap_or(1.0);
            let dmg_vec = eve_destiny::DamageVector {
                em: attr(114).unwrap_or(0.0) * dmg_mult,
                explosive: attr(116).unwrap_or(0.0) * dmg_mult,
                kinetic: attr(117).unwrap_or(0.0) * dmg_mult,
                thermal: attr(118).unwrap_or(0.0) * dmg_mult,
            };
            drones.push(crate::services::combat_crimewatch::DroneSpaceState {
                ball_id,
                drone_item_id: item.item_id,
                type_id: item.type_id,
                group_id: t.group_id,
                owner_char_id: 0,
                original_owner_char_id: item.owner_id,
                parent_ship_id: 0,
                solar_system_id: item.location_id as u32,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: true,
                is_sentry: max_vel < 1.0,
                bandwidth_used: bw_used,
                max_velocity: max_vel,
                optimal_range: opt_range,
                damage_vector: dmg_vec,
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            });
        }
    });
    if !drones.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for d in drones {
                rt.active_drones_in_space.insert(d.ball_id, d);
            }
        });
    }
    if !fighters.is_empty() {
        if let Ok(mut guard) = ctx.active_fighters.write() {
            for f in fighters {
                guard.insert(f.ball_id, f);
            }
        }
    }
}


/// Synchronously executes server-driven self-destruct when `now_unix >= detonate_at_unix`
/// (`feat:lifecycle.self-destruct-server-timer`).
fn execute_due_self_destruct_sync(
    ctx: &ServerContext,
    entry: &SelfDestructPersistedEntry,
    now_unix: u64,
) {
    let sys_id = entry.solar_system_id;
    let char_id = entry.char_id;
    let ship_id = entry.ship_id;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let pos = sys_handle
        .get_ball_position_sync(ship_id)
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.item_positions
                    .get(&ship_id)
                    .copied()
                    .map(DVec3::from_array)
            })
        })
        .unwrap_or(DVec3::ZERO);

    let _ = crate::combat::death_pipeline::execute_death_pipeline_sync(
        ctx,
        sys_id,
        char_id,
        ship_id,
        char_id,
        pos,
        now_unix,
    );
}

/// Collapses a character's Abyssal session if they are stranded in an Abyssal system (`32_000_000..=32_999_999`)
/// with an expired or missing `abyssal_sessions` entry (`feat:lifecycle.abyssal-disconnect`, `plan:SP-7`).
pub(crate) fn collapse_stranded_abyssal_if_needed_sync(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) {
    let due_opt = with_mut_sp6_state(&ctx.db, |st, snap| {
        if let Some(ab) = st.abyssal_sessions.get(&char_id) {
            if now_unix >= ab.collapse_at_unix {
                st.disconnected_ships.remove(&char_id);
                st.safe_logoffs.remove(&char_id);
                return st.abyssal_sessions.remove(&char_id);
            }
            return None;
        }
        let ch = snap.characters.get(&char_id)?;
        if (32_000_000..=32_999_999).contains(&ch.solar_system_id) {
            st.disconnected_ships.remove(&char_id);
            st.safe_logoffs.remove(&char_id);
            return Some(AbyssalPersistedSession {
                char_id,
                ship_id: ch.active_ship_id,
                pocket_system_id: ch.solar_system_id,
                origin_system_id: 30_000_142,
                entered_unix: now_unix.saturating_sub(1_200),
                collapse_at_unix: now_unix,
            });
        }
        None
    });
    if let Some(ab) = due_opt {
        execute_due_abyssal_collapse_sync(ctx, &ab, now_unix);
    }
}

/// Synchronously executes Abyssal pocket collapse at 20m (`1200s`), destroying both the active ship
/// and the capsule, writing both ship and capsule killmails, clearing active implants, and respawning
/// the pilot at `home_station_id` in a rookie corvette (`606`) (`feat:lifecycle.abyssal-disconnect`).
fn execute_due_abyssal_collapse_sync(
    ctx: &ServerContext,
    sess: &AbyssalPersistedSession,
    now_unix: u64,
) {
    crate::services::pve_special_sites_abyssal::collapse_abyssal_pocket_and_pod_pilot_at(
        ctx,
        sess.char_id,
        sess.ship_id,
        sess.pocket_system_id,
        now_unix,
    );
}

/// Runs all time-driven SP-6 / SP-7 lifecycle progressions when server time advances from `prev_unix`
/// by `secs` seconds (or when `tick_background_cluster` runs with `secs == 0`).
pub fn on_advance_secs(ctx: &ServerContext, _prev_unix: u64, secs: u64) {
    let now_unix = ctx.now_unix();

    // 0. Sweep expired market orders across cluster ticks (`feat:market.order-expiry`).
    let swept = ctx.db.with_mut_snapshot(|s| crate::services::market_contracts::sweep_expired_orders(s, now_unix));
    if !swept.is_empty() {
        crate::services::market_contracts::emit_expired_order_notifications(ctx, &swept);
    }

    let cw_pilots: Vec<i64> = with_combat_runtime(ctx, |rt| rt.crimewatch_timer_states.keys().copied().collect());
    for pid in cw_pilots {
        crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(ctx, pid, now_unix);
    }

    // 1. Sync any in-memory self-destruct countdowns from `CombatRuntimeState` into `Sp6PersistedState`
    let rt_self_destructs: Vec<SelfDestructPersistedEntry> = with_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns.values().map(|cd| SelfDestructPersistedEntry {
            ship_id: cd.ship_id, char_id: cd.char_id, solar_system_id: cd.solar_system_id,
            started_unix: cd.started_unix, detonate_at_unix: cd.detonate_at_unix,
        }).collect()
    });

    let (mut completed_safe_logoffs, mut aborted_safe_logoffs) = (Vec::<SafeLogoffEntry>::new(), Vec::<(i64, &'static str)>::new());
    let (mut expired_disconnected, mut due_self_destructs) = (Vec::<DisconnectedShipEntry>::new(), Vec::<SelfDestructPersistedEntry>::new());
    let (mut due_abyssal, mut active_warping_disconnected) = (Vec::<AbyssalPersistedSession>::new(), Vec::<DisconnectedShipEntry>::new());

    // Evaluate blocking space conditions outside `with_mut_sp6_state` so we never re-enter `ctx.db`
    let pending_safe_entries: Vec<SafeLogoffEntry> = super::with_sp6_state(&ctx.db, |st| {
        st.safe_logoffs
            .values()
            .filter(|e| !e.completed)
            .cloned()
            .collect()
    });
    let blocked_safe_char_ids: std::collections::HashSet<i64> = pending_safe_entries
        .into_iter()
        .filter(|entry| {
            let (weap_until, npc_until, pvp_until, crim_until) =
                super::active_timers_for_char(ctx, entry.char_id, now_unix);
            let has_timer = weap_until > now_unix
                || npc_until > now_unix
                || pvp_until > now_unix
                || crim_until > now_unix;
            let sd_active = with_combat_runtime(ctx, |rt| {
                rt.self_destruct_countdowns
                    .get(&entry.ship_id)
                    .is_some_and(|cd| now_unix < cd.detonate_at_unix)
            });
            has_timer
                || sd_active
                || !safe_logoff_blocking_space_conditions(
                    ctx,
                    entry.char_id,
                    entry.ship_id,
                    entry.solar_system_id,
                )
                .is_empty()
                || ctx
                    .systems
                    .is_ball_scrambled_sync(entry.solar_system_id, entry.ship_id)
        })
        .map(|entry| entry.char_id)
        .collect();

    with_mut_sp6_state(&ctx.db, |st, snap| {
        for cd in rt_self_destructs {
            st.self_destructs.entry(cd.ship_id).or_insert(cd);
        }

        // Prune disconnected_ships entries whose ship was destroyed or pilot docked/changed ship
        st.disconnected_ships.retain(|&cid, disc| {
            snap.items.contains_key(&disc.ship_id)
                && snap.characters.get(&cid).is_some_and(|c| {
                    c.active_ship_id == disc.ship_id
                        && c.station_id.is_none()
                        && c.structure_id.is_none()
                })
        });

        let safe_keys: Vec<i64> = st.safe_logoffs.keys().copied().collect();
        for cid in safe_keys {
            let Some(entry) = st.safe_logoffs.get(&cid).cloned() else {
                continue;
            };
            if entry.completed {
                continue;
            }
            if blocked_safe_char_ids.contains(&cid) {
                st.safe_logoffs.remove(&cid);
                aborted_safe_logoffs.push((cid, "SAFE_LOGOFF_ABORT_MODULE"));
                continue;
            }
            if now_unix >= entry.complete_at_unix {
                if let Some(e) = st.safe_logoffs.get_mut(&cid) {
                    e.completed = true;
                }
                completed_safe_logoffs.push(entry);
            }
        }

        for disc in st.disconnected_ships.values_mut() {
            if !disc.removed_from_space {
                if ctx.systems.is_ball_scrambled_sync(disc.solar_system_id, disc.ship_id) {
                    disc.emergency_warp_dest = None;
                } else if disc.emergency_warp_dest.is_some() {
                    active_warping_disconnected.push(disc.clone());
                }
            }
        }

        let sd_keys: Vec<i64> = st
            .self_destructs
            .iter()
            .filter(|(_, cd)| now_unix >= cd.detonate_at_unix)
            .map(|(&sid, _)| sid)
            .collect();
        for sid in sd_keys {
            if let Some(cd) = st.self_destructs.remove(&sid) {
                due_self_destructs.push(cd);
            }
        }

        let ab_keys: Vec<i64> = st
            .abyssal_sessions
            .iter()
            .filter(|(_, ab)| now_unix >= ab.collapse_at_unix)
            .map(|(&cid, _)| cid)
            .collect();
        for cid in ab_keys {
            if let Some(ab) = st.abyssal_sessions.remove(&cid) {
                due_abyssal.push(ab);
            }
        }
    });

    for (cid, reason) in aborted_safe_logoffs {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSafeLogoffAborted",
            "charid",
            vec![eve_proto::PyRep::String(reason.to_string())],
        );
    }

    // 2. Step disconnected emergency warps through the shared `Ballpark` (`R50-P1-1`, `BF-SP6-SYNTH-WARP-BALLPARK`)
    //    and advance NPC AI on active solar systems (`R50-P1-4`)
    let active_systems: std::collections::BTreeMap<u32, crate::system_actor::SystemHandle> =
        ctx.systems.active_system_handles().into_iter().collect();
    let mut disconnected_systems: std::collections::BTreeMap<u32, crate::system_actor::SystemHandle> =
        std::collections::BTreeMap::new();
    for disc in &active_warping_disconnected {
        let handle = disconnected_systems
            .entry(disc.solar_system_id)
            .or_insert_with(|| ctx.systems.get_or_spawn(disc.solar_system_id))
            .clone();
        let Some(dest_arr) = disc.emergency_warp_dest else {
            continue;
        };
        if !handle.is_ball_scrambled_sync(disc.ship_id) {
            let (_gid, _cid, mass, radius, max_vel, inertia, warp_speed_au_s) =
                sde_ship_physics(&ctx.sde, disc.ship_type_id);
            let cur_pos = handle
                .get_ball_position_sync(disc.ship_id)
                .unwrap_or_else(|| DVec3::from_array(disc.last_position));
            handle.prepare_disconnected_warp_sync(
                disc.ship_id,
                disc.ship_type_id,
                radius,
                mass,
                inertia,
                max_vel,
                warp_speed_au_s,
                cur_pos,
                DVec3::from_array(dest_arr),
            );
        }
    }

    let step_iters = if secs > 0 { secs.min(45) } else { 0 };
    let mut finished_warps: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for _ in 0..step_iters {
        for &sys_id in active_systems.keys() {
            crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(ctx, sys_id);
            if let Some(mgr) = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner()).get_mut(&sys_id) {
                let tick = ctx.systems.get_or_spawn(sys_id).ballpark.read().map(|b| b.current_tick).unwrap_or(0);
                mgr.step_ai_sync(ctx, sys_id, tick, 1.0);
            }
        }
        for (&sys_id, handle) in &disconnected_systems {
            let _tick = handle.try_step_full_tick(1.0);
            for disc in active_warping_disconnected
                .iter()
                .filter(|d| d.solar_system_id == sys_id)
            {
                if finished_warps.contains(&disc.char_id) {
                    continue;
                }
                let Some(dest_arr) = disc.emergency_warp_dest else {
                    continue;
                };
                if handle.is_ball_scrambled_sync(disc.ship_id) {
                    finished_warps.insert(disc.char_id);
                    continue;
                }
                let (_pos, arrived) =
                    handle.reconcile_disconnected_warp_sync(disc.ship_id, DVec3::from_array(dest_arr));
                if arrived {
                    finished_warps.insert(disc.char_id);
                }
            }
        }
    }
    if step_iters > 0 {
        for handle in disconnected_systems.values() {
            handle.set_last_auto_stepped_unix(now_unix);
        }
    }

    if !active_warping_disconnected.is_empty() {
        let mut warp_updates: Vec<(i64, i64, [f64; 3], bool)> = Vec::new();
        for disc in active_warping_disconnected {
            let handle = ctx.systems.get_or_spawn(disc.solar_system_id);
            let cur_pos = handle
                .get_ball_position_sync(disc.ship_id)
                .unwrap_or_else(|| DVec3::from_array(disc.last_position));
            let finished = finished_warps.contains(&disc.char_id)
                || disc.emergency_warp_dest.is_some_and(|dest_arr| {
                    handle.is_ball_scrambled_sync(disc.ship_id)
                        || cur_pos.distance(DVec3::from_array(dest_arr)) <= 2_500.0
                });
            warp_updates.push((
                disc.char_id,
                disc.ship_id,
                [cur_pos.x, cur_pos.y, cur_pos.z],
                finished,
            ));
        }
        with_mut_sp6_state(&ctx.db, |st, snap| {
            for (cid, sid, pos_arr, finished) in warp_updates {
                snap.item_positions.insert(sid, pos_arr);
                if let Some(d) = st.disconnected_ships.get_mut(&cid) {
                    d.last_position = pos_arr;
                    if finished {
                        d.emergency_warp_dest = None;
                    }
                }
            }
        });
    }

    with_mut_sp6_state(&ctx.db, |st, _snap| {
        for disc in st.disconnected_ships.values_mut() {
            if !disc.removed_from_space && now_unix >= disc.remove_at_unix {
                disc.removed_from_space = true;
                expired_disconnected.push(disc.clone());
            }
        }
    });

    // 3. Finalize completed 30s Safe Logoffs (`plan:SP-6`, `feat:character.safe-logoff`)
    // parity: feat:character.safe-logoff
    for entry in completed_safe_logoffs {
        super::abandon_drones_and_fighters_for_ship_sync(
            ctx,
            entry.char_id,
            entry.ship_id,
            entry.solar_system_id,
        );
        let handle = ctx.systems.get_or_spawn(entry.solar_system_id);
        if let Some(pos) = handle.get_ball_position_sync(entry.ship_id) {
            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
                snap.item_positions
                    .insert(entry.ship_id, [pos.x, pos.y, pos.z]);
                Ok(())
            });
        }
        handle.try_remove_ball(entry.ship_id);
        ctx.notify(
            NotificationTarget::Character(entry.char_id),
            "OnSafeLogoffActivated",
            "charid",
            vec![],
        );
    }

    // 4. Finalize expired ungraceful logoff / disconnect ship removal timers (`plan:SP-6`)
    for disc in expired_disconnected {
        let handle = ctx.systems.get_or_spawn(disc.solar_system_id);
        let final_pos = handle
            .get_ball_position_sync(disc.ship_id)
            .map(|v| [v.x, v.y, v.z])
            .unwrap_or(disc.last_position);
        handle.try_remove_ball(disc.ship_id);
        with_mut_sp6_state(&ctx.db, |st, snap| {
            if snap.items.contains_key(&disc.ship_id) {
                snap.item_positions.insert(disc.ship_id, final_pos);
            }
            if let Some(d) = st.disconnected_ships.get_mut(&disc.char_id) {
                d.last_position = final_pos;
                d.removed_from_space = true;
            }
        });
    }

    // 5. Detonate due server-driven self-destructs (`feat:lifecycle.self-destruct-server-timer`)
    for cd in due_self_destructs {
        with_mut_combat_runtime(ctx, |rt| {
            rt.self_destruct_countdowns.remove(&cd.ship_id);
        });
        execute_due_self_destruct_sync(ctx, &cd, now_unix);
    }

    // 6. Collapse expired Abyssal pockets (`feat:lifecycle.abyssal-disconnect`)
    for ab in due_abyssal {
        with_mut_sp6_state(&ctx.db, |st, _snap| {
            st.disconnected_ships.remove(&ab.char_id);
            st.safe_logoffs.remove(&ab.char_id);
        });
        execute_due_abyssal_collapse_sync(ctx, &ab, now_unix);
    }

    // 7. Decay expired wrecks (2h = 7200s), corpses (2h = 7200s), jetcans (1h = 3600s),
    //    and Mobile Warp Disruptors (SDE decay = 48h), and complete anchoring Mobile Warp Disruptors
    //    (`feat:lifecycle.wreck-restart`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`)
    let mut balls_to_remove: Vec<(u32, i64, bool)> = Vec::new();
    let mut bubbles_to_activate: Vec<(u32, i64, DVec3, f64)> = Vec::new();

    with_mut_sp6_state(&ctx.db, |st, snap| {
        let mut expired_ids: Vec<(i64, u32, bool)> = Vec::new();
        let mut newly_anchored_ids: Vec<i64> = Vec::new();

        for item in snap.items.values() {
            if !(30_000_000..=34_000_000).contains(&item.location_id) {
                continue;
            }
            let sys_id = item.location_id as u32;
            let group_id = ctx
                .sde
                .types
                .get(&item.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            let is_wreck = item.type_id == 26557 || group_id == 186;
            let is_corpse = item.type_id == 25;
            let is_jetcan = item.type_id == 23;
            let is_mwd_bubble = group_id == 361;
            let obj_meta = st.space_object_meta.get(&item.item_id);

            if obj_meta.and_then(|m| m.planet_id).is_some() {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(PLANETARY_LAUNCH_DECAY_SECS) {
                        expired_ids.push((item.item_id, sys_id, false));
                    }
                }
            } else if is_wreck || is_corpse {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(WRECK_DECAY_LIFETIME_SECS) {
                        expired_ids.push((item.item_id, sys_id, false));
                    }
                }
            } else if is_jetcan {
                if let Some(created) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created.saturating_add(JETCAN_DECAY_LIFETIME_SECS) {
                        expired_ids.push((item.item_id, sys_id, false));
                    }
                }
            } else if is_mwd_bubble {
                if let Some(start_u) =
                    obj_meta.and_then(|m| m.anchor_start_unix.or(m.created_unix))
                {
                    let decay_secs =
                        sde_mobile_warp_disruptor_decay_secs(&ctx.sde, item.type_id);
                    if now_unix >= start_u.saturating_add(decay_secs) {
                        expired_ids.push((item.item_id, sys_id, true));
                    } else {
                        let delay_s = obj_meta.map(|m| m.anchoring_delay_secs).unwrap_or(0.0);
                        let already_done = obj_meta.is_some_and(|m| m.anchor_completed);
                        if !already_done && now_unix >= start_u.saturating_add(delay_s.ceil() as u64)
                        {
                            let pos_arr = snap.item_positions.get(&item.item_id).copied().unwrap_or([0.0, 0.0, 0.0]);
                            let radius = obj_meta.and_then(|m| m.bubble_radius_m).unwrap_or(20_000.0);
                            newly_anchored_ids.push(item.item_id);
                            bubbles_to_activate.push((sys_id, item.item_id, DVec3::from_array(pos_arr), radius));
                        }
                    }
                }
            } else if matches!(group_id, 12 | 340 | 448)
                || ctx.sde.types.get(&item.type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0)
            {
                if let Some(start_u) = obj_meta.and_then(|m| m.anchor_start_unix) {
                    if now_unix >= start_u.saturating_add(super::SECURE_CONTAINER_DECAY_SECS) {
                        expired_ids.push((item.item_id, sys_id, false));
                    } else {
                        let delay_s = obj_meta.map(|m| m.anchoring_delay_secs).unwrap_or(0.0);
                        let already_done = obj_meta.is_some_and(|m| m.anchor_completed);
                        if !already_done && now_unix >= start_u.saturating_add(delay_s.ceil() as u64) {
                            newly_anchored_ids.push(item.item_id);
                        }
                    }
                } else if let Some(created_u) = obj_meta.and_then(|m| m.created_unix) {
                    if now_unix >= created_u.saturating_add(WRECK_DECAY_LIFETIME_SECS) {
                        expired_ids.push((item.item_id, sys_id, false));
                    }
                }
            }
        }

        for iid in newly_anchored_ids {
            st.space_object_meta
                .entry(iid)
                .or_default()
                .anchor_completed = true;
        }

        for (iid, sys_id, is_bubble) in expired_ids {
            snap.items.remove(&iid);
            snap.item_positions.remove(&iid);
            st.space_object_meta.remove(&iid);
            st.ship_combat_hp.remove(&iid);
            snap.items.retain(|_, child| child.location_id != iid);
            balls_to_remove.push((sys_id, iid, is_bubble));
        }
    });

    if !balls_to_remove.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for &(_, iid, _) in &balls_to_remove {
                rt.container_lifetimes.remove(&iid);
            }
        });
    }
    for (sys_id, iid, is_bubble) in balls_to_remove {
        let handle = ctx.systems.get_or_spawn(sys_id);
        if is_bubble {
            handle.try_remove_warp_bubble(iid);
        }
        handle.try_remove_ball(iid);
    }
    for (sys_id, iid, pos, radius) in bubbles_to_activate {
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle.try_add_mobile_warp_disruptor(iid, pos, radius);
    }
}

/// Production 1Hz background cluster tick (`plan:SP-7`).
/// Advances all server-authoritative timers (`on_advance_secs`), checks 11:00 UTC downtime crossing
/// (`run_daily_downtime_and_jobs`), completes due skill-queue items (`OnSkillTrained`), executes due
/// CONCORD responses (`step_concord`), despawns expired anomalies/signatures, steps extended NPC AI
/// across active systems even when unobserved (`feat:lifecycle.world-unobserved-system-sim`), and
/// processes container decay.
pub async fn tick_background_cluster(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let now_day_idx = now_unix.saturating_sub(dt_sec) / 86_400;
    let crossed_downtime = with_mut_sp6_state(&ctx.db, |st, _snap| match st.last_downtime_day_idx {
        Some(last_idx) if now_day_idx > last_idx => {
            st.last_downtime_day_idx = Some(now_day_idx);
            true
        }
        None => {
            st.last_downtime_day_idx = Some(now_day_idx);
            false
        }
        _ => false,
    });
    if crossed_downtime {
        let _ = crate::services::wallet_serverops::run_daily_downtime_and_jobs(ctx);
    }

    on_advance_secs(ctx, now_unix, 0);
    ctx.advance_skill_queues(1.0);

    ctx.db.with_mut_snapshot(|snap| {
        for anoms in snap.cosmic_anomalies.values_mut() {
            anoms.retain(|a| {
                a.lifetime_seconds == 0
                    || now_unix < a.created_unix.saturating_add(a.lifetime_seconds)
            });
        }
        for sigs in snap.cosmic_signatures.values_mut() {
            sigs.retain(|s| {
                s.sig_group.eq_ignore_ascii_case("wormhole")
                    || s.lifetime_seconds == 0
                    || now_unix < s.created_unix.saturating_add(s.lifetime_seconds)
            });
        }
    });
    crate::services::space_destiny::expire_and_respawn_wormholes_in_cluster(ctx, now_unix).await;
    crate::services::space_destiny::expire_all_overdue_probes(ctx, now_unix).await;

    let mut temp_cw = crate::combat::crimewatch::CrimeWatchDirector::new();
    if let Ok(mut cw) = ctx.crimewatch.write() {
        for (&cid, st) in cw.states.iter_mut() {
            if let Some((_, arrival_unix, _, _)) = st.pending_concord_arrival {
                if now_unix >= arrival_unix {
                    let cloned = st.clone();
                    st.pending_concord_arrival = None;
                    temp_cw.states.insert(cid, cloned);
                }
            }
        }
    }
    if !temp_cw.states.is_empty() {
        let _ = temp_cw.step_concord(ctx, now_unix, now_unix).await;
    }

    let _ = crate::services::mining_cycle::process_mining_cycles(ctx).await;
    let _ = crate::sp6_lifecycle::step_repeating_drone_mining(ctx, now_unix).await;
    let _ = crate::sp6_lifecycle::step_returning_drones(ctx).await;
    crate::services::combat_crimewatch::combat_npcs_drones::step_engaged_drones(ctx, now_unix).await;
    let _ = ctx
        .living_world
        .step_all_hauler_convoys(&ctx.systems, &ctx.db, &ctx.sde)
        .await;

    let active_handles = ctx.systems.active_system_handles();
    for (sys_id, handle) in active_handles {
        let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, sys_id).await;
        {
            let mut mgr = ctx
                .npc_spawns
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&sys_id)
                .unwrap_or_default();
            let _ = mgr.spawn_belt_rats_for_system(ctx, sys_id).await;
            ctx.npc_spawns
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sys_id, mgr);
        }
        if !handle.take_auto_stepped_at(now_unix) {
            let _tick = handle
                .step_manual(1.0)
                .await
                .map(|b| b.tick)
                .unwrap_or(0);
            crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai(ctx, sys_id).await;
            if let Some(mgr) = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner()).get_mut(&sys_id) {
                let tick = handle.ballpark.read().map(|b| b.current_tick).unwrap_or(0);
                mgr.step_ai_sync(ctx, sys_id, tick, 1.0);
            }
        }
        let _ = crate::services::insurgency_services::step_system_insurgency_tick(ctx, sys_id).await;
        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::step_wreck_and_jetcan_decay(
            ctx, sys_id, now_unix,
        )
        .await;
    }
}
