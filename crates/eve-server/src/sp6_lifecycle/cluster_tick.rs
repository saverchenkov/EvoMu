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
                ballpark.add_static_ball_with_meta(
                    Ball {
                        id: st.structure_id,
                        type_id: st.type_id,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass,
                        radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    },
                    SlimItemMeta {
                        group_id,
                        category_id,
                        owner_id: st.owner_corp_id,
                        char_id: None,
                        corp_id: st.owner_corp_id,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                        name: (!st.name.is_empty()).then(|| st.name.clone()),
                        ..Default::default()
                    },
                );
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
        let deployable_state = crate::services::mobile_deployables::read_deployable_state_snap(snap);
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
            } else if let Some(rec) = deployable_state.deployables.get(&item.item_id) {
                if now_unix >= rec.expires_at_unix {
                    expired_item_ids.push(item.item_id);
                    continue;
                }
            } else if sde.types.get(&item.type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0)
                && !crate::services::mobile_deployables::is_mobile_structure(sde, item.type_id)
            {
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
                    is_cloaked: false, ..Default::default()
                },
            );
            if !is_drone {
                ballpark.mark_ball_non_free(ball_id, false);
            }
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
                    is_cloaked: false, ..Default::default()
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

        // 3b. Abandoned / unpiloted ships in space (`#918`, `#835`)
        for item in snap.items.values() {
            if item.location_id != solar_system_id as i64
                || !snap.characters.contains_key(&item.owner_id)
                || player_ship_ids.contains(&item.item_id)
                || ballpark.balls.contains_key(&item.item_id)
                || sp6
                    .disconnected_ships
                    .values()
                    .any(|d| d.ship_id == item.item_id)
            {
                continue;
            }
            let is_ship = sde
                .types
                .get(&item.type_id)
                .is_some_and(|t| t.category_id == 6 && t.type_id != 670);
            if !is_ship {
                continue;
            }
            let Some(&pos_arr) = snap.item_positions.get(&item.item_id) else {
                continue;
            };
            let pos = DVec3::from_array(pos_arr);
            let (group_id, category_id, mass, radius, max_vel, inertia, _warp_speed_au_s) =
                sde_ship_physics(sde, item.type_id);
            let (corp_id, alliance_id, sec_status) = snap
                .characters
                .get(&item.owner_id)
                .map(|c| {
                    (
                        c.corporation_id,
                        c.alliance_id.unwrap_or(-1),
                        c.security_status,
                    )
                })
                .unwrap_or((1_000_009, -1, 0.0));
            ballpark.set_ball_slim_meta(
                item.item_id,
                SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: item.owner_id,
                    char_id: None,
                    corp_id,
                    alliance_id,
                    security_status: sec_status,
                    is_cloaked: false,
                    ..Default::default()
                },
            );
            ballpark.add_ball(Ball {
                id: item.item_id,
                type_id: item.type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
            let mut cs = default_combat_state_for_type(sde, item.type_id);
            if let Some(hp) = sp6.ship_combat_hp.get(&item.item_id) {
                cs.shield_hp = hp.shield_hp.clamp(0.0, cs.max_shield_hp);
                cs.armor_hp = hp.armor_hp.clamp(0.0, cs.max_armor_hp);
                if hp.hull_hp > 0.0 {
                    cs.structure_hp = hp.hull_hp.clamp(0.0, cs.max_structure_hp);
                }
            }
            ballpark.combat_states.insert(item.item_id, cs);
        }

        // 4. Persisted scanner probes in space (`#543`)
        for p in sp6.active_scanner_probes.values() {
            if p.solar_system_id != solar_system_id || p.state < 0 {
                continue;
            }
            if ballpark.balls.contains_key(&p.probe_id) {
                continue;
            }
            let (group_id, category_id, mass, radius) = sde
                .types
                .get(&p.probe_type_id)
                .map(|t| (t.group_id, t.category_id, t.mass.max(1.0), t.radius.max(1.0)))
                .unwrap_or((479, 8, 50.0, 5.0));
            let (corp_id, alliance_id, sec_status) = snap
                .characters
                .get(&p.owner_char_id)
                .map(|c| {
                    (
                        c.corporation_id,
                        c.alliance_id.unwrap_or(-1),
                        c.security_status,
                    )
                })
                .unwrap_or((1_000_009, -1, 0.0));
            ballpark.set_ball_slim_meta(
                p.probe_id,
                SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: p.owner_char_id,
                    char_id: Some(p.owner_char_id),
                    corp_id,
                    alliance_id,
                    security_status: sec_status,
                    is_cloaked: false,
                    ..Default::default()
                },
            );
            ballpark.add_ball(Ball {
                id: p.probe_id,
                type_id: p.probe_type_id,
                position: DVec3::from_array(p.position),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
            ballpark
                .combat_states
                .insert(p.probe_id, default_combat_state_for_type(sde, p.probe_type_id));
        }
    });

    if !expired_item_ids.is_empty() || !expired_disconnected_chars.is_empty() {
        let expired_set: std::collections::HashSet<i64> =
            expired_item_ids.iter().copied().collect();
        with_mut_sp6_state(db, |st, snap| {
            for iid in &expired_item_ids {
                snap.items.remove(iid);
                snap.item_positions.remove(iid);
                st.space_object_meta.remove(iid);
                st.ship_combat_hp.remove(iid);
            }
            if !expired_set.is_empty() {
                snap.items
                    .retain(|_, child| !expired_set.contains(&child.location_id));
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

    let _ = crate::combat::death_pipeline::execute_live_self_destruct_sync(
        ctx,
        sys_id,
        char_id,
        ship_id,
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
            let origin_sys = crate::services::pve_special_sites_abyssal::resolve_character_home_station_and_system(
                snap,
                &ctx.sde,
                char_id,
            )
            .1;
            let default_lifetime_secs =
                if ch.solar_system_id == crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID {
                    crate::services::abyssal_void_mgr::VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS
                } else {
                    1_200
                };
            return Some(AbyssalPersistedSession {
                char_id,
                ship_id: ch.active_ship_id,
                pocket_system_id: ch.solar_system_id,
                origin_system_id: origin_sys,
                entered_unix: now_unix.saturating_sub(default_lifetime_secs),
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
    if sess.pocket_system_id == crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID {
        let vs = crate::services::pve_special_sites_abyssal::with_abyssal_state(ctx, |st| {
            st.void_space_sessions.remove(&sess.char_id)
        })
        .unwrap_or(crate::services::pve_special_sites::VoidSpaceSessionRecord {
            char_id: sess.char_id,
            ship_id: sess.ship_id,
            origin_solar_system_id: sess.origin_system_id,
            origin_position: [0.0, 0.0, 0.0],
            target_id: 0,
            entered_unix: sess.entered_unix,
            expires_at_sec: sess.collapse_at_unix,
            nebula_graphic_id: None,
        });
        crate::services::pve_abyssal_lifecycle::evacuate_void_space_session_sync(
            ctx,
            sess.char_id,
            &vs,
            now_unix,
        );
        return;
    }
    crate::services::pve_special_sites_abyssal::collapse_abyssal_pocket_and_pod_pilot_at(
        ctx,
        sess.char_id,
        sess.ship_id,
        sess.pocket_system_id,
        now_unix,
    );
}

/// Runs all time-driven SP-6 / SP-7 lifecycle progressions when server time advances from `prev_unix`
/// by `secs` seconds.
pub fn on_advance_secs(ctx: &ServerContext, prev_unix: u64, secs: u64) {
    on_advance_secs_with_sim_clock_rebase(ctx, prev_unix, secs, true);
}

/// Variant of [`on_advance_secs`] allowing callers like `timeControl.AdvanceClock` to advance
/// server time without pushing `DoSimClockRebase` onto live clients whose local `blue.os` clock
/// cannot be warped cleanly by `AdjustTimes`.
pub fn on_advance_secs_with_sim_clock_rebase(
    ctx: &ServerContext,
    prev_unix: u64,
    secs: u64,
    emit_sim_clock_rebase: bool,
) {
    on_advance_secs_internal(ctx, prev_unix, secs, emit_sim_clock_rebase, true);
}

pub fn on_advance_secs_internal(
    ctx: &ServerContext,
    prev_unix: u64,
    secs: u64,
    emit_sim_clock_rebase: bool,
    step_active_npc_ai_sync: bool,
) {
    if emit_sim_clock_rebase {
        crate::services::notify_emitters_tournaments_anomalies::emit_sim_clock_rebase_if_advanced(ctx, prev_unix, secs);
    }
    crate::services::space_destiny::space_probe_scanning::tick_probe_warp_end(ctx);
    let now_unix = ctx.now_unix();

    // 0. Sweep expired market orders and check season transitions across cluster ticks.
    crate::services::notify_emitters_seasons_tactical::check_and_emit_season_transitions(ctx);
    let swept = if ctx
        .db
        .with_snapshot(|s| crate::services::market_contracts::has_expired_orders(s, now_unix))
    {
        ctx.db.with_mut_snapshot(|s| {
            crate::services::market_contracts::sweep_expired_orders(s, now_unix)
        })
    } else {
        Vec::new()
    };
    if !swept.is_empty() {
        crate::services::market_contracts::emit_expired_order_notifications(ctx, &swept);
    }

    let mut cw_pilots: std::collections::BTreeSet<i64> = with_combat_runtime(ctx, |rt| {
        rt.crimewatch_timer_states
            .keys()
            .chain(rt.disapproval_flags.keys())
            .chain(rt.npc_timers.keys())
            .copied()
            .collect()
    });
    let pilot_states: Vec<(i64, crate::combat::CharacterCrimeWatchState)> =
        ctx.db.with_snapshot(|s| {
            for (&cid, st) in &s.crimewatch_states {
                if st.weapon_timer_until_unix > 0
                    || st.pvp_timer_until_unix > 0
                    || st.suspect_flag_until_unix > 0
                    || st.criminal_flag_until_unix > 0
                {
                    cw_pilots.insert(cid);
                }
            }
            cw_pilots
                .into_iter()
                .map(|pid| (pid, s.crimewatch_states.get(&pid).cloned().unwrap_or_default()))
                .collect()
        });
    for (pid, st) in pilot_states {
        crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications_with_state(ctx, pid, &st, now_unix);
    }

    // Sweep expired CrimeWatch timers and runtime maps (`#736`).
    let has_expired_cw = ctx.db.with_snapshot(|s| {
        s.crimewatch_states.values().any(|st| {
            (st.weapon_timer_until_unix > 0 && now_unix >= st.weapon_timer_until_unix)
                || (st.pvp_timer_until_unix > 0 && now_unix >= st.pvp_timer_until_unix)
                || (st.suspect_flag_until_unix > 0 && now_unix >= st.suspect_flag_until_unix)
                || (st.criminal_flag_until_unix > 0 && now_unix >= st.criminal_flag_until_unix)
        }) || s.crimewatch_aggressions.values().any(|r| r.expires_unix <= now_unix)
    });
    if has_expired_cw {
        ctx.crimewatch.with_mut_crimewatch(|cw| {
            cw.states.retain(|_, st| {
                if st.weapon_timer_until_unix > 0 && now_unix >= st.weapon_timer_until_unix {
                    st.weapon_timer_until_unix = 0;
                }
                if st.pvp_timer_until_unix > 0 && now_unix >= st.pvp_timer_until_unix {
                    st.pvp_timer_until_unix = 0;
                }
                if st.suspect_flag_until_unix > 0 && now_unix >= st.suspect_flag_until_unix {
                    st.suspect_flag_until_unix = 0;
                }
                if st.criminal_flag_until_unix > 0 && now_unix >= st.criminal_flag_until_unix {
                    st.criminal_flag_until_unix = 0;
                }
                st.weapon_timer_until_unix > 0
                    || st.pvp_timer_until_unix > 0
                    || st.suspect_flag_until_unix > 0
                    || st.criminal_flag_until_unix > 0
                    || st.pending_concord_arrival.is_some()
            });
            cw.aggression_mgr.records.retain(|_, r| r.expires_unix > now_unix);
        });
    }
    let has_expired_rt_cw = with_combat_runtime(ctx, |rt| {
        rt.disapproval_flags.values().any(|&exp| now_unix >= exp)
            || rt.npc_timers.values().any(|&exp| now_unix >= exp)
            || rt.duel_challenges.values().any(|&exp| now_unix >= exp)
            || rt.kill_right_expires_unix.values().any(|&exp| now_unix >= exp)
            || rt
                .limited_engagements
                .values()
                .any(|m| m.values().any(|&exp| now_unix >= exp))
    });
    if has_expired_rt_cw {
        let mut expired_kr_ids: Vec<i64> = Vec::new();
        with_mut_combat_runtime(ctx, |rt| {
            rt.disapproval_flags.retain(|_, &mut exp| now_unix < exp);
            rt.npc_timers.retain(|_, &mut exp| now_unix < exp);
            rt.duel_challenges.retain(|_, &mut exp| now_unix < exp);
            rt.kill_right_expires_unix.retain(|&kr_id, &mut exp| {
                if now_unix >= exp {
                    expired_kr_ids.push(kr_id);
                    false
                } else {
                    true
                }
            });
            for &kr_id in &expired_kr_ids {
                rt.kill_right_restricted_to.remove(&kr_id);
            }
            for m in rt.limited_engagements.values_mut() {
                m.retain(|_, &mut exp| now_unix < exp);
            }
            rt.limited_engagements.retain(|_, m| !m.is_empty());
        });
        if !expired_kr_ids.is_empty() {
            ctx.db.with_mut_snapshot(|snap| {
                for &kr_id in &expired_kr_ids {
                    snap.kill_rights.remove(&kr_id);
                }
            });
        }
    }

    // 1. Sync any in-memory self-destruct countdowns from `CombatRuntimeState` into `Sp6PersistedState`
    let rt_self_destructs: Vec<SelfDestructPersistedEntry> = with_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns.values().map(|cd| SelfDestructPersistedEntry {
            ship_id: cd.ship_id, char_id: cd.char_id, solar_system_id: cd.solar_system_id,
            started_unix: cd.started_unix, detonate_at_unix: cd.detonate_at_unix,
        }).collect()
    });

    let (mut completed_safe_logoffs, mut aborted_safe_logoffs) = (Vec::<SafeLogoffEntry>::new(), Vec::<(i64, &'static str)>::new());
    let mut due_self_destructs = Vec::<SelfDestructPersistedEntry>::new();
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

    let needs_sp6_lifecycle_sync = !rt_self_destructs.is_empty()
        || super::with_sp6_state(&ctx.db, |st| {
            !st.disconnected_ships.is_empty()
                || !st.safe_logoffs.is_empty()
                || !st.self_destructs.is_empty()
                || !st.abyssal_sessions.is_empty()
        });

    if needs_sp6_lifecycle_sync {
        // Pre-evaluate system-actor tackle & committed-warp predicates outside `with_mut_sp6_state`
        // so the world DB write lock is never held while reading ballpark/system-actor state (`#831`).
        let disc_candidates: Vec<(u32, i64)> = super::with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships
                .values()
                .filter(|d| !d.removed_from_space)
                .map(|d| (d.solar_system_id, d.ship_id))
                .collect()
        });
        let scrambled_uncommitted_disc_ships: std::collections::HashSet<i64> = disc_candidates
            .into_iter()
            .filter(|&(sys_id, ship_id)| {
                ctx.systems.is_ball_scrambled_sync(sys_id, ship_id)
                    && !ctx.systems.is_ball_in_committed_warp_sync(sys_id, ship_id)
            })
            .map(|(_, ship_id)| ship_id)
            .collect();

        with_mut_sp6_state(&ctx.db, |st, snap| {
            for cd in rt_self_destructs {
                let started = cd.started_unix;
                let detonate = cd.detonate_at_unix;
                st.self_destructs
                    .entry(cd.ship_id)
                    .and_modify(|e| {
                        e.started_unix = started;
                        e.detonate_at_unix = detonate;
                    })
                    .or_insert(cd);
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
                    if scrambled_uncommitted_disc_ships.contains(&disc.ship_id) {
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
    }

    for (cid, reason) in aborted_safe_logoffs {
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnSafeLogoffAborted {
                reason_code: reason.to_string(),
            },
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
        if !handle.is_ball_scrambled_sync(disc.ship_id) || handle.is_ball_warping_sync(disc.ship_id) {
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

    for disc in super::disconnect_hold::collect_expired_disconnected_ships_filtered(
        ctx, now_unix, true,
    ) {
        super::disconnect_hold::finalize_expired_disconnected_ship(ctx, disc);
    }

    let step_iters = if secs == 0 {
        0
    } else {
        secs.min(45)
    };
    let dedicated_physics_active = ctx
        .dedicated_physics_ticker_active
        .load(std::sync::atomic::Ordering::Acquire);
    let mut finished_warps: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for _ in 0..step_iters {
        if step_active_npc_ai_sync && !dedicated_physics_active {
            for &sys_id in active_systems.keys() {
                crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(ctx, sys_id);
                if let Some(mgr) = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner()).get_mut(&sys_id) {
                    if !mgr.entities.is_empty() {
                        let tick = ctx.systems.get_or_spawn(sys_id).ballpark.try_read().map(|b| b.current_tick).unwrap_or(0);
                        mgr.step_ai_sync(ctx, sys_id, tick, 1.0);
                    }
                }
            }
        }
        for (&sys_id, handle) in &disconnected_systems {
            if !dedicated_physics_active {
                let _tick = handle.try_step_full_tick(1.0);
            }
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
                let (_pos, arrived) =
                    handle.reconcile_disconnected_warp_sync(disc.ship_id, DVec3::from_array(dest_arr));
                if arrived {
                    finished_warps.insert(disc.char_id);
                }
            }
        }
    }
    if step_iters > 0 && !dedicated_physics_active {
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

    let expired_disconnected =
        super::disconnect_hold::collect_expired_disconnected_ships(ctx, now_unix);

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
        super::disconnect_hold::cleanup_ship_modules_and_cyno_on_despawn(
            ctx,
            entry.char_id,
            entry.ship_id,
        );
        handle.try_remove_ball(entry.ship_id);
        ctx.emit_typed(
            NotificationTarget::Character(entry.char_id),
            crate::router::notify_registry::OnSafeLogoffActivated,
        );
        super::disconnect_safe_logoff_sessions(ctx, entry.char_id);
    }

    // 4. Finalize expired ungraceful logoff / disconnect ship removal timers (`plan:SP-6`)
    for disc in expired_disconnected {
        super::disconnect_hold::finalize_expired_disconnected_ship(ctx, disc);
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

    // 6b. Sweep expired Void Space sessions, despawn expired un-entered Abyssal trace balls, and clear expired Zarzakh gate locks.
    crate::services::pve_abyssal_lifecycle::sweep_expired_void_space_sessions_sync(ctx, now_unix);
    let expired_traces: Vec<(u32, i64)> = if crate::services::pve_special_sites_abyssal::read_abyssal_state(
        ctx,
        |st| st.traces.values().any(|t| t.expires_at_sec > 0 && now_unix >= t.expires_at_sec),
    ) {
        crate::services::pve_special_sites_abyssal::with_abyssal_state(ctx, |st| {
            let mut out = Vec::new();
            st.traces.retain(|&tid, t| {
                if t.expires_at_sec > 0 && now_unix >= t.expires_at_sec {
                    out.push((t.origin_solar_system_id, tid));
                    false
                } else {
                    true
                }
            });
            out
        })
    } else {
        Vec::new()
    };
    for (origin_sys, tid) in expired_traces {
        if let Some(h) = ctx.systems.get_handle(origin_sys) {
            h.try_remove_ball(tid);
        }
    }
    let expired_zarzakh_locks: Vec<(i64, i64)> = if ctx
        .db
        .with_snapshot(|snap| snap.zarzakh_gate_locks.values().any(|&(_, exp)| now_unix >= exp))
    {
        ctx.db.with_mut_snapshot(|snap| {
            let mut out = Vec::new();
            snap.zarzakh_gate_locks.retain(|&cid, &mut (gate_id, exp)| {
                if now_unix >= exp {
                    out.push((cid, gate_id));
                    false
                } else {
                    true
                }
            });
            out
        })
    } else {
        Vec::new()
    };
    for (cid, gate_id) in expired_zarzakh_locks {
        crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
            &ctx.quasar_gateway,
            cid,
            30_100_000,
            gate_id,
            now_unix,
        );
    }

    crate::services::notify_emitters_skills_trade_godma::sweep_due_login_milestones_and_alpha_injectors(
        ctx,
        now_unix,
    );

    // 7. Decay expired wrecks (2h = 7200s), corpses (2h = 7200s), jetcans (1h = 3600s),
    //    and Mobile Warp Disruptors (SDE decay = 48h), and complete anchoring Mobile Warp Disruptors
    //    (`feat:lifecycle.wreck-restart`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`)
    let mut balls_to_remove: Vec<(u32, i64, bool)> = Vec::new();
    let (expired_ids, newly_anchored_ids, bubbles_to_activate): (
        Vec<(i64, u32, bool)>,
        Vec<i64>,
        Vec<(u32, i64, DVec3, f64)>,
    ) = ctx.db.with_snapshot(|snap| {
        let st = &snap.sp6_lifecycle;
        let mut expired_ids: Vec<(i64, u32, bool)> = Vec::new();
        let mut newly_anchored_ids: Vec<i64> = Vec::new();
        let mut bubbles_to_activate: Vec<(u32, i64, DVec3, f64)> = Vec::new();
        let deployable_state = crate::services::mobile_deployables::read_deployable_state_snap(snap);

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
            } else if let Some(rec) = deployable_state.deployables.get(&item.item_id) {
                if now_unix >= rec.expires_at_unix {
                    expired_ids.push((item.item_id, sys_id, false));
                } else {
                    let delay_s = obj_meta.map(|m| m.anchoring_delay_secs).unwrap_or(0.0);
                    let already_done = obj_meta.is_some_and(|m| m.anchor_completed);
                    if !already_done && !rec.is_unanchored && now_unix >= rec.anchor_start_unix.saturating_add(delay_s.ceil() as u64) {
                        newly_anchored_ids.push(item.item_id);
                    }
                }
            } else if (matches!(group_id, 12 | 340 | 448)
                || ctx.sde.types.get(&item.type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0))
                && !crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, item.type_id)
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
        (expired_ids, newly_anchored_ids, bubbles_to_activate)
    });

    if !expired_ids.is_empty() || !newly_anchored_ids.is_empty() {
        with_mut_sp6_state(&ctx.db, |st, snap| {
            for iid in newly_anchored_ids {
                st.space_object_meta
                    .entry(iid)
                    .or_default()
                    .anchor_completed = true;
            }

            if !expired_ids.is_empty() {
                let mut expired_set = std::collections::HashSet::with_capacity(expired_ids.len());
                for (iid, sys_id, is_bubble) in &expired_ids {
                    snap.items.remove(iid);
                    snap.item_positions.remove(iid);
                    st.space_object_meta.remove(iid);
                    st.ship_combat_hp.remove(iid);
                    expired_set.insert(*iid);
                    balls_to_remove.push((*sys_id, *iid, *is_bubble));
                }
                snap.items
                    .retain(|_, child| !expired_set.contains(&child.location_id));
                crate::services::mobile_deployables::write_deployable_state_snap(snap, |dst| {
                    for (iid, _, _) in &expired_ids {
                        dst.deployables.remove(iid);
                    }
                });
            }
        });
    }

    if !balls_to_remove.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for &(_, iid, _) in &balls_to_remove {
                rt.container_lifetimes.remove(&iid);
            }
        });
    }
    for (sys_id, iid, is_bubble) in balls_to_remove {
        if let Some(handle) = ctx.systems.get_handle(sys_id) {
            if is_bubble {
                handle.try_remove_warp_bubble(iid);
            }
            handle.try_remove_ball(iid);
        }
    }
    for (sys_id, iid, pos, radius) in bubbles_to_activate {
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle.try_add_mobile_warp_disruptor(iid, pos, radius);
    }
}

/// Production 1 Hz background cluster tick (`#735`).
/// Honors `ctx.is_clock_paused()` and `ctx.time_acceleration_multiplier()`, running the unified
/// synchronous subsystem pipeline (`run_clock_advanced_sync_pipeline`) alongside async cluster tasks.
pub async fn tick_background_cluster(ctx: &ServerContext) {
    tick_background_cluster_with_step(ctx, ctx.time_acceleration_multiplier()).await;
}

/// Runs one background cluster tick where the server clock advanced by `explicit_step_secs` (`>= 1`).
/// IceBelt anomalies are permanent system anomalies respawned by `tick_all_ice_belt_respawns_sync`
/// 4 hours after depletion, so they must not be deleted by generic combat-anomaly lifetime pruning.
pub(crate) fn sweep_expired_cosmic_anomalies_and_signatures(ctx: &ServerContext, now_unix: u64) {
    let has_expired_cosmic = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.values().any(|anoms| {
            anoms.iter().any(|a| {
                !matches!(a.anomaly_type.as_str(), "IceBelt" | "OreAnomaly")
                    && a.lifetime_seconds > 0
                    && now_unix >= a.created_unix.saturating_add(a.lifetime_seconds)
            })
        }) || snap.cosmic_signatures.values().any(|sigs| {
            sigs.iter().any(|s| {
                !s.sig_group.eq_ignore_ascii_case("wormhole")
                    && !matches!(s.sig_group.as_str(), "IceBelt" | "OreAnomaly" | "Ore" | "Gas")
                    && s.lifetime_seconds > 0
                    && now_unix >= s.created_unix.saturating_add(s.lifetime_seconds)
            })
        })
    });
    if has_expired_cosmic {
        let mut expired_anoms_by_sys: Vec<(u32, Vec<String>)> = Vec::new();
        let mut expired_sigs_by_sys: Vec<(u32, Vec<String>)> = Vec::new();
        ctx.db.with_mut_snapshot(|snap| {
            for (&sys_id, anoms) in snap.cosmic_anomalies.iter_mut() {
                let mut removed = Vec::new();
                anoms.retain(|a| {
                    let keep = matches!(a.anomaly_type.as_str(), "IceBelt" | "OreAnomaly")
                        || a.lifetime_seconds == 0
                        || now_unix < a.created_unix.saturating_add(a.lifetime_seconds);
                    if !keep && !a.cleared {
                        removed.push(a.anomaly_id.clone());
                    }
                    keep
                });
                if !removed.is_empty() {
                    expired_anoms_by_sys.push((sys_id, removed));
                }
            }
            for (&sys_id, sigs) in snap.cosmic_signatures.iter_mut() {
                let mut removed = Vec::new();
                sigs.retain(|s| {
                    let keep = s.sig_group.eq_ignore_ascii_case("wormhole")
                        || matches!(s.sig_group.as_str(), "IceBelt" | "OreAnomaly" | "Ore" | "Gas")
                        || s.lifetime_seconds == 0
                        || now_unix < s.created_unix.saturating_add(s.lifetime_seconds);
                    if !keep && !s.collapsed {
                        removed.push(s.signature_id.clone());
                    }
                    keep
                });
                if !removed.is_empty() {
                    expired_sigs_by_sys.push((sys_id, removed));
                }
            }
        });
        for (sys_id, removed_ids) in expired_anoms_by_sys {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
                ctx,
                sys_id,
                &[],
                &removed_ids,
            );
        }
        for (sys_id, removed_ids) in expired_sigs_by_sys {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
                ctx,
                sys_id,
                &[],
                &removed_ids,
            );
        }
    }
}

pub async fn tick_background_cluster_with_step(ctx: &ServerContext, explicit_step_secs: u64) {
    if explicit_step_secs == 0 {
        return;
    }
    let now_unix = ctx.now_unix();
    let _ = crate::services::serverops_cluster::step_downtime_countdown_warnings(ctx, now_unix);
    if crate::services::serverops_cluster::is_auto_cluster_tidi_enabled(ctx) {
        let _ = crate::services::serverops_cluster::rebalance_overloaded_worker_nodes(ctx);
        let _ = crate::services::serverops_cluster::recompute_cluster_node_tidi(ctx);
    }
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let sec_of_day = now_unix % 86_400;
    let in_downtime_window = sec_of_day >= dt_sec && sec_of_day < dt_sec.saturating_add(900);
    let now_day_idx = now_unix.saturating_sub(dt_sec) / 86_400;
    let last_dt_day_idx = super::with_sp6_state(&ctx.db, |st| st.last_downtime_day_idx);
    let action = match last_dt_day_idx {
        Some(last_idx) if now_day_idx > last_idx => with_mut_sp6_state(&ctx.db, |st, _snap| {
            st.last_downtime_day_idx = Some(now_day_idx);
            if in_downtime_window { 2 } else { 1 }
        }),
        None => with_mut_sp6_state(&ctx.db, |st, _snap| {
            st.last_downtime_day_idx = Some(now_day_idx);
            0
        }),
        _ => 0,
    };
    if action == 2 && crate::services::serverops_cluster::is_scheduled_downtime_enabled(ctx) {
        let _ = crate::services::serverops_cluster::start_daily_downtime_window(ctx);
    } else if action > 0 {
        let _ = crate::services::serverops_cluster::execute_daily_maintenance_jobs(ctx);
    } else {
        let _ = crate::services::serverops_cluster::maybe_complete_scheduled_downtime_restart(ctx, now_unix);
    }

    let cached_prev = ctx
        .cluster_tick_last_unix
        .load(std::sync::atomic::Ordering::Acquire);
    let db_prev = if cached_prev == 0 {
        ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("cluster_tick_last_unix")
                .and_then(|s| s.parse::<u64>().ok())
        })
    } else {
        Some(cached_prev)
    };
    let (prev, tick_secs) = match db_prev {
        Some(prior) if now_unix <= prior => {
            // Tick overrun (`now_unix == prior`) or backward wall-clock step (`now_unix < prior`):
            // never move `cluster_tick_last_unix` backward and return a 0-second delta (`#863`).
            ctx.cluster_tick_last_unix
                .fetch_max(prior, std::sync::atomic::Ordering::AcqRel);
            (prior, 0)
        }
        Some(prior) => {
            let mut cur = cached_prev;
            let claimed_prev = loop {
                if cur >= now_unix {
                    break None;
                }
                match ctx.cluster_tick_last_unix.compare_exchange_weak(
                    cur,
                    now_unix,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                ) {
                    Ok(old) => break Some(if old == 0 { prior } else { old }),
                    Err(actual) => cur = actual,
                }
            };
            match claimed_prev {
                Some(p) if now_unix > p => (p, now_unix.saturating_sub(p)),
                Some(p) => (p, 0),
                None => (now_unix, 0),
            }
        }
        None => {
            let initial_prev = now_unix.saturating_sub(explicit_step_secs.max(1));
            let claimed_prev = match ctx.cluster_tick_last_unix.compare_exchange(
                0,
                now_unix,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => Some(initial_prev),
                Err(actual) if actual < now_unix => {
                    let mut cur = actual;
                    loop {
                        if cur >= now_unix {
                            break None;
                        }
                        match ctx.cluster_tick_last_unix.compare_exchange_weak(
                            cur,
                            now_unix,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        ) {
                            Ok(old) => break Some(old),
                            Err(next) => cur = next,
                        }
                    }
                }
                Err(_) => None,
            };
            match claimed_prev {
                Some(p) => (p, now_unix.saturating_sub(p)),
                None => (now_unix, 0),
            }
        }
    };
    let persisted = {
        let cur = ctx
            .cluster_tick_persisted_unix
            .load(std::sync::atomic::Ordering::Relaxed);
        if cur == 0 {
            if let Some(ts) = db_prev {
                ctx.cluster_tick_persisted_unix
                    .store(ts, std::sync::atomic::Ordering::Relaxed);
                ts
            } else {
                0
            }
        } else {
            cur
        }
    };
    let should_persist_tick = db_prev.is_none()
        || (now_unix >= persisted.saturating_add(60)
            && ctx.db.with_snapshot(|s| s.pve_special_sites.initialized));
    if should_persist_tick {
        let offset_secs = ctx.clock().offset_secs();
        let _ = ctx.db.tx("cluster_tick_persist", |wtx| {
            wtx.snap
                .service_state_blobs
                .insert("cluster_tick_last_unix".into(), now_unix.to_string());
            wtx.snap
                .service_state_blobs
                .insert("__server_now_unix".into(), now_unix.to_string());
            wtx.set_clock_offset_secs(offset_secs);
            Ok::<_, eve_db::TxError>(())
        });
        ctx.cluster_tick_persisted_unix
            .store(now_unix, std::sync::atomic::Ordering::Relaxed);
    }

    // Execute the unified synchronous per-tick pipeline (`#735`):
    // - `on_advance_secs_internal(ctx, prev, tick_secs, false, false)` (including disconnected warp reconciliation)
    // - `pve_special_sites::on_server_clock_advanced`
    // - `tick_all_metenox_moon_drills`
    // - `purge_expired_boosters_all`
    // - `advance_skill_queues`
    // - `tick_fleet_lifecycle`
    // - `tick_all_ice_belt_respawns_sync`
    // - `sweep_expired_bookmarks_and_notify`
    // - jetcan & moon extraction natural decay
    // - `active_moon_probes` survey arrival notifications
    // - `sync_all_alliance_timers`
    // - `sync_all_structures`
    if tick_secs > 0 {
        ctx.run_clock_advanced_sync_pipeline(prev, tick_secs, false, false);
    }
    crate::services::industry_pi::pi_helpers::tick_all_colonies_background(ctx, now_unix);

    crate::services::mining_advanced::tick_all_moon_extractions(ctx).await;
    crate::services::mining_phased_autotarget::tick_phased_asteroid_fields(ctx, now_unix).await;
    crate::services::quasar_payment_store_types::process_due_payment_schedules(&ctx.db, &ctx.quasar_gateway.pending_notices, now_unix);
    let pending_gw_notices: Vec<_> = ctx
        .quasar_gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    for (scope, payload) in pending_gw_notices {
        let _ = ctx.quasar_gateway.publish_notice(scope, payload, now_unix);
    }
    crate::services::quasar_cosmetics_skinr::settle_completed_skinr_jobs_for_db(&ctx.db, now_unix);
    for (scope, payload, ts) in crate::services::quasar_social_chat::drain_local_notices() {
        let _ = ctx.quasar_gateway.publish_notice(scope, payload, ts);
    }

    sweep_expired_cosmic_anomalies_and_signatures(ctx, now_unix);
    crate::services::space_destiny::expire_and_respawn_wormholes_in_cluster(ctx, now_unix).await;
    crate::services::space_destiny::expire_all_overdue_probes(ctx, now_unix).await;
    crate::services::sov_gameplay::tick_sov_gameplay(ctx, now_unix).await;
    crate::services::fw_gameplay::tick_fw_gameplay(ctx, now_unix).await;
    crate::services::combat_npc_encounters::tick_overdue_npc_bounty_payouts(ctx, now_unix);
    let _ = crate::services::pve_ess_reserve_banks::tick_ess_main_bank_autopayout(ctx, now_unix);
    let _ = crate::services::pve_ess_reserve_banks::tick_ess_reserve_bank_links(ctx, now_unix).await;

    let has_pending_bounty_ticks = with_combat_runtime(ctx, |rt| {
        rt.pending_bounty_ticks.values().any(|&pending| pending > 0.0)
    });
    let due_bounty_ticks: Vec<(u32, i64)> = if has_pending_bounty_ticks {
        with_mut_combat_runtime(ctx, |rt| {
            let mut due = Vec::new();
            for (&(sys_id, char_id), &pending) in &rt.pending_bounty_ticks {
                if pending <= 0.0 {
                    continue;
                }
                let next_flush = *rt
                    .bounty_tick_next_flush_unix
                    .entry((sys_id, char_id))
                    .or_insert_with(|| now_unix.saturating_add(1_200));
                if now_unix >= next_flush {
                    due.push((sys_id, char_id));
                }
            }
            for key in &due {
                rt.bounty_tick_next_flush_unix.remove(key);
            }
            due
        })
    } else {
        Vec::new()
    };
    for (sys_id, char_id) in due_bounty_ticks {
        let _ = crate::services::combat_npc_encounters::flush_npc_bounty_tick(
            ctx,
            sys_id,
            char_id,
            0.0,
        );
    }

    crate::combat::crimewatch::tick_concord_responses(ctx, now_unix).await;

    let _ = crate::services::mining_cycle::process_mining_cycles(ctx).await;
    let _ = crate::sp6_lifecycle::step_repeating_drone_mining(ctx, now_unix).await;
    let _ = crate::sp6_lifecycle::step_returning_drones(ctx).await;
    crate::services::combat_crimewatch::combat_npcs_drones::step_engaged_drones(ctx, now_unix).await;
    let active_handles = ctx.systems.active_system_handles();
    let active_sys_set: std::collections::HashSet<u32> =
        active_handles.iter().map(|&(sys_id, _)| sys_id).collect();
    let (chars_in_system, piloted_ships_by_sys): (
        std::collections::HashSet<u32>,
        std::collections::HashMap<u32, i64>,
    ) = if active_handles.is_empty() {
        (
            std::collections::HashSet::new(),
            std::collections::HashMap::new(),
        )
    } else {
        ctx.db.with_snapshot(|snap| {
            let mut chars_sys = std::collections::HashSet::new();
            let mut piloted_map = std::collections::HashMap::new();
            for c in snap.characters.values() {
                if active_sys_set.contains(&c.solar_system_id) {
                    chars_sys.insert(c.solar_system_id);
                    if c.station_id.is_none()
                        && c.structure_id.is_none()
                        && c.active_ship_id > 0
                    {
                        piloted_map
                            .entry(c.solar_system_id)
                            .or_insert(c.active_ship_id);
                    }
                }
            }
            (chars_sys, piloted_map)
        })
    };
    for &(sys_id, _) in &active_handles {
        if (30_000_000..31_000_000).contains(&sys_id) {
            let has_piloted_ship_in_space = piloted_ships_by_sys.contains_key(&sys_id);
            if has_piloted_ship_in_space {
                let has_belt = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .iter()
                    .any(|c| c.group_id == 9);
                let has_station = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .is_some_and(|s| !s.stations.is_empty());
                if has_belt && has_station {
                    let _ = ctx.living_world.ensure_system_mining_op(sys_id, &ctx.sde);
                }
                if ctx
                    .sde
                    .stargates()
                    .values()
                    .any(|g| g.from_solar_system_id == sys_id)
                {
                    let _ = ctx.living_world.ensure_system_hauler_convoy(sys_id, &ctx.sde);
                    let _ = ctx
                        .living_world
                        .materialize_system_lod2(sys_id, &ctx.systems, &ctx.sde)
                        .await;
                }
            }
        }
    }
    let _ = ctx
        .living_world
        .step_all_hauler_convoys(&ctx.systems, &ctx.db, &ctx.sde)
        .await;
    let _ = ctx
        .living_world
        .step_all_mining_ops(&ctx.systems, &ctx.db, &ctx.sde)
        .await;
    crate::services::pve_fob_observatory::step_living_world_fob_and_observatory_spawns(ctx).await;

    for &(sys_id, ref handle) in &active_handles {
        if handle.is_hibernated() && !chars_in_system.contains(&sys_id) && handle.idle_ticks() > 0 {
            continue;
        }
        let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, sys_id).await;
    }
    if !active_handles.is_empty() {
        let (
            navy_present,
            drifter_present,
            cs_alive_set,
            mordu_alive_set,
            officer_alive_set,
            cs_cooldowns,
        ) = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            let mut navy_present = std::collections::HashSet::new();
            let mut drifter_present = std::collections::HashSet::new();
            let mut cs_alive = std::collections::HashSet::new();
            let mut mordu_alive = std::collections::HashSet::new();
            let mut officer_alive = std::collections::HashSet::new();
            for n in rt.npc_entities.values() {
                if !active_sys_set.contains(&n.solar_system_id) {
                    continue;
                }
                if n.archetype == "Navy" || n.archetype.ends_with("Navy") {
                    navy_present.insert(n.solar_system_id);
                } else if n.archetype == "Drifter" {
                    drifter_present.insert(n.solar_system_id);
                } else if n.archetype == "CloneSoldier" {
                    cs_alive.insert(n.solar_system_id);
                } else if n.archetype == "MorduCommander" {
                    mordu_alive.insert(n.solar_system_id);
                } else if ctx
                    .sde
                    .types
                    .get(&n.type_id)
                    .is_some_and(|t| matches!(t.group_id, 553 | 559 | 564 | 569 | 574))
                {
                    officer_alive.insert(n.solar_system_id);
                }
            }
            (
                navy_present,
                drifter_present,
                cs_alive,
                mordu_alive,
                officer_alive,
                rt.clone_soldier_cooldown_until.clone(),
            )
        });
        let mut navy_plans = Vec::new();
        for &(sys_id, ref handle) in &active_handles {
            if handle.is_hibernated()
                && !chars_in_system.contains(&sys_id)
                && handle.idle_ticks() > 0
            {
                continue;
            }
            if let Some(plan) =
                crate::services::combat_npc_police::plan_empire_navy_patrol_with_preloaded(
                    ctx,
                    sys_id,
                    navy_present.contains(&sys_id),
                    piloted_ships_by_sys.get(&sys_id).copied(),
                )
                .await
            {
                navy_plans.push(plan);
            }
        }
        // Plan belt rats in-place under `ctx.npc_spawns` without removing `NpcSpawnManager`
        // across `.await` points (`#869`).
        let mut belt_plans = Vec::new();
        let mut belt_id_total: i64 = 0;
        {
            let mut guard = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
            for &(sys_id, ref handle) in &active_handles {
                let mgr = guard.entry(sys_id).or_default();
                if handle.is_hibernated()
                    && !chars_in_system.contains(&sys_id)
                    && handle.idle_ticks() > 0
                {
                    let has_due_timer = mgr
                        .clone_soldier_respawn_timers
                        .get(&sys_id)
                        .is_some_and(|&until| now_unix >= until)
                        || mgr
                            .respawn_timers
                            .iter()
                            .any(|(&(s, _), &until)| s == sys_id && now_unix >= until);
                    if !has_due_timer {
                        continue;
                    }
                }
                let (planned_groups, special_specs) = mgr.plan_belt_rats_for_system(
                    ctx,
                    sys_id,
                    drifter_present.contains(&sys_id),
                    cs_cooldowns.get(&sys_id).copied(),
                    cs_alive_set.contains(&sys_id),
                    mordu_alive_set.contains(&sys_id),
                    officer_alive_set.contains(&sys_id),
                );
                let group_npcs: i64 = planned_groups
                    .iter()
                    .map(|(_, sg, _)| sg.npc_type_ids.len() as i64)
                    .sum();
                let count = group_npcs + special_specs.len() as i64;
                if count > 0 {
                    belt_id_total += count;
                    belt_plans.push((sys_id, planned_groups, special_specs, count));
                }
            }
        }
        let navy_id_total: i64 = navy_plans.iter().map(|p| p.specs.len() as i64).sum();
        let total_new_ids = navy_id_total + belt_id_total;
        if total_new_ids > 0 {
            let mut next_start_id = ctx.db.with_mut_snapshot(|snap| {
                let start = snap.next_entity_id + 1;
                snap.next_entity_id += total_new_ids;
                start
            });
            let mut built_specialized = Vec::new();
            let mut spawned_belt_entities_by_sys = Vec::new();
            for plan in navy_plans {
                let count = plan.specs.len() as i64;
                let _ = crate::services::combat_npc_police::execute_navy_patrol_plan_with_start_id(
                    ctx,
                    plan,
                    next_start_id,
                    &mut built_specialized,
                )
                .await;
                next_start_id += count;
            }
            for (sys_id, planned_groups, special_specs, count) in belt_plans {
                let (_spawned_ids, entities) =
                    crate::combat::npc_ai::NpcSpawnManager::execute_belt_rat_plan_detached_with_start_id(
                        ctx,
                        sys_id,
                        planned_groups,
                        special_specs,
                        next_start_id,
                        &mut built_specialized,
                    )
                    .await;
                if !entities.is_empty() {
                    spawned_belt_entities_by_sys.push((sys_id, entities));
                }
                next_start_id += count;
            }
            if !spawned_belt_entities_by_sys.is_empty() {
                let mut guard = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
                for (sys_id, entities) in spawned_belt_entities_by_sys {
                    let mgr = guard.entry(sys_id).or_default();
                    for (ball_id, entity) in entities {
                        mgr.entities.insert(ball_id, entity);
                    }
                }
            }
            if !built_specialized.is_empty() {
                crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                    for (ball_id, entity) in built_specialized {
                        rt.npc_entities.insert(ball_id, entity);
                    }
                });
            }
        }
    }

    let mut has_long_idle_candidate = false;
    for (sys_id, handle) in active_handles {
        let is_hibernated_empty = handle.is_hibernated() && !chars_in_system.contains(&sys_id);
        if is_hibernated_empty {
            let idle_count = handle.record_idle_tick();
            // Refcounts: SystemManager registry (1) + actor task (1) + this loop's `handle` (1) = 3.
            if idle_count >= 60 && std::sync::Arc::strong_count(&handle.ballpark) <= 3 {
                has_long_idle_candidate = true;
            }
        } else if !ctx
            .dedicated_physics_ticker_active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let phys_steps = explicit_step_secs.clamp(1, 60);
            for step_idx in 0..phys_steps {
                if step_idx == 0 && handle.take_auto_stepped_at(now_unix) {
                    continue;
                }
                step_single_system_physics_step(ctx, sys_id, &handle).await;
            }
        }
        if (32_000_000..=32_999_999).contains(&sys_id) {
            crate::services::pve_special_sites_abyssal::prune_out_of_bounds_abyssal_probes(ctx, sys_id);
            crate::services::notify_emitters_tournaments_anomalies::sync_deathzone_state_notifications(ctx, sys_id);
        }
        if !is_hibernated_empty {
            let _ = crate::services::insurgency_services::step_system_insurgency_tick(ctx, sys_id).await;
            let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::step_wreck_and_jetcan_decay(
                ctx, sys_id, now_unix,
            )
            .await;
        }
    }
    if has_long_idle_candidate {
        let _ = ctx.systems.evict_idle_systems_with_min_ticks(300, 60).await;
    }
}

/// Maximum physics steps replayed per ticker wake when the runtime falls behind (`#870`).
pub const MAX_PHYSICS_BACKLOG_STEPS: u64 = 3;
/// Per-system physics step ask timeout so a wedged actor never hangs the ticker (`#841`).
pub const SYSTEM_STEP_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Clamps wall-clock physics ticker elapsed seconds to `1..=MAX_PHYSICS_BACKLOG_STEPS` (`#870`).
pub fn clamp_physics_ticker_backlog(raw_elapsed_secs: u64) -> u64 {
    raw_elapsed_secs.clamp(1, MAX_PHYSICS_BACKLOG_STEPS)
}

struct CatchUnwindBoxFuture<F>(std::pin::Pin<Box<F>>);

impl<F: std::future::Future> std::future::Future for CatchUnwindBoxFuture<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner = &mut self.0;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.as_mut().poll(cx)))?
            .map(Ok)
    }
}

/// Runs `fut` inside `catch_unwind` across every poll so panics inside ticker futures are logged
/// and counted rather than killing the background ticker task (`#841`).
pub async fn catch_unwind_future<F: std::future::Future>(fut: F) -> Result<F::Output, String> {
    CatchUnwindBoxFuture(Box::pin(fut)).await.map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string())
    })
}

async fn step_single_system_physics_step(
    ctx: &ServerContext,
    sys_id: u32,
    handle: &crate::system_actor::SystemHandle,
) {
    let step_res = catch_unwind_future(async {
        match tokio::time::timeout(SYSTEM_STEP_ASK_TIMEOUT, handle.step_manual_tick_only(1.0)).await
        {
            Ok(Ok(_tick)) => {}
            Ok(Err(err)) => {
                tracing::warn!(
                    solar_system_id = sys_id,
                    error = %err,
                    "SystemActor step_manual_tick_only returned error"
                );
                return;
            }
            Err(_) => {
                tracing::warn!(
                    solar_system_id = sys_id,
                    timeout_ms = SYSTEM_STEP_ASK_TIMEOUT.as_millis() as u64,
                    "SystemActor step_manual_tick_only timed out; skipping stuck system step"
                );
                return;
            }
        }
        let _ = tokio::time::timeout(
            SYSTEM_STEP_ASK_TIMEOUT,
            crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai(ctx, sys_id),
        )
        .await;
        if let Some(mgr) = ctx
            .npc_spawns
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&sys_id)
        {
            let tick = handle.ballpark.try_read().map(|b| b.current_tick).unwrap_or(0);
            let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, sys_id);
            mgr.step_ai_sync(ctx, sys_id, tick, 1.0 * tidi);
        }
    })
    .await;
    if let Err(panic_msg) = step_res {
        tracing::error!(
            solar_system_id = sys_id,
            panic = %panic_msg,
            "Per-system driver caught panic during physics step (#919)"
        );
    }
}

/// Steps 1 Hz Destiny physics and NPC AI across all active solar systems.
/// Runs on a dedicated wall-clock ticker in production so slow mining or
/// SQLite persistence in `tick_background_cluster` cannot delay warp completion.
/// Capped at [`MAX_PHYSICS_BACKLOG_STEPS`] (`3`) steps per wake (`#870`).
/// Hibernated empty systems perform zero per-tick work, and active systems are driven
/// concurrently under per-system supervision (`#919`, `#866`).
pub async fn step_active_systems_physics_tick(ctx: &ServerContext, elapsed_secs: u64) {
    let now_unix = ctx.now_unix();
    let mult = ctx.time_acceleration_multiplier();
    let steps = elapsed_secs
        .saturating_mul(mult)
        .clamp(1, MAX_PHYSICS_BACKLOG_STEPS);
    let active_handles = ctx.systems.active_system_handles();
    if active_handles.is_empty() {
        return;
    }
    let chars_in_system: std::collections::HashSet<u32> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .map(|c| c.solar_system_id)
            .collect()
    });
    let mut active_to_step = Vec::with_capacity(active_handles.len());
    for (sys_id, handle) in active_handles {
        if handle.is_hibernated() && !chars_in_system.contains(&sys_id) {
            handle.record_idle_tick();
            continue;
        }
        active_to_step.push((sys_id, handle));
    }
    if active_to_step.len() <= 1 {
        for (sys_id, handle) in active_to_step {
            for step_idx in 0..steps {
                if step_idx == 0 && handle.take_auto_stepped_at(now_unix) {
                    continue;
                }
                step_single_system_physics_step(ctx, sys_id, &handle).await;
            }
        }
    } else {
        let mut join_set = tokio::task::JoinSet::new();
        for (sys_id, handle) in active_to_step {
            let ctx_cloned = ctx.clone();
            join_set.spawn(async move {
                for step_idx in 0..steps {
                    if step_idx == 0 && handle.take_auto_stepped_at(now_unix) {
                        continue;
                    }
                    step_single_system_physics_step(&ctx_cloned, sys_id, &handle).await;
                }
            });
        }
        while join_set.join_next().await.is_some() {}
    }
}

#[cfg(test)] #[path = "cluster_lifecycle_tests.rs"] mod cluster_lifecycle_tests;
#[cfg(test)] #[path = "cluster_tick_unit_tests.rs"] mod cluster_tick_unit_tests;
#[cfg(test)] #[path = "cluster_tick_deep_extra_unit_tests.rs"] mod cluster_tick_deep_extra_unit_tests;

