//! Guided missile ball lifecycle inside `system_actor` (`#1079`, `#1077` E7-E9):
//! next-tick `AddBalls2` acquisition batching, post-spawn `OnSpecialFX` deployment FX,
//! moving-target surface-distance ETA, target-loss cleanup, and delayed impact damage.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DVec3, DamageVector, DestinyEvent, MovementMode,
    SlimItemMeta, SpecialFxWireMeta,
};
use eve_sde::SdeRegistry;

use super::{
    ActiveGuidedMissile, ActiveModuleCycle, CharacterId, PendingCycleAction,
    PendingDestroyedOutbox, PendingMissileDeployFx, PendingMissileLaunch,
};

const MIN_MISSILE_BALL_ID: i64 = 9_950_000_000;
const MISSILE_DEPLOY_FX_GUID: &str = "effects.MissileDeployment";
const MIN_CLIENT_VISUAL_SIM_S: f64 = 1.0;

/// Guided launcher groups whose modules spawn `MovementMode::Missile` follow balls
/// (excludes Defender `512`, Bomb `862`, and Vorton `4060`).
#[inline]
pub(crate) fn is_guided_missile_launcher_group(group_id: i64) -> bool {
    matches!(group_id, 506..=511 | 524 | 771 | 1245)
}

/// Queues 1 missile launch per launcher in `cycle` (`linked_turrets` or single `module_id`)
/// for next-tick `AddBalls2` batching.
pub(crate) fn enqueue_missile_launches_for_cycle(
    cycle: &ActiveModuleCycle,
    ballpark: &Ballpark,
    db_opt: &Option<Arc<WorldRepository>>,
    pending_missile_launches: &mut Vec<PendingMissileLaunch>,
) {
    if !cycle.weapon_is_guided_missile {
        return;
    }
    let Some(target_ball_id) = cycle.target_id else {
        return;
    };
    let Some(ammo_type_id) = cycle.missile_charge_type_id else {
        return;
    };
    let slim_meta = ballpark.ball_slim_meta.get(&cycle.ship_id);
    let mut owner_id = slim_meta.map(|m| m.owner_id).unwrap_or(0);
    let corp_id = slim_meta.map(|m| m.corp_id).unwrap_or(0);
    let alliance_id = slim_meta.map(|m| m.alliance_id).unwrap_or(0);
    if owner_id == 0 {
        if let Some(cid) = cycle.owner_char_id {
            owner_id = cid;
        } else if let Some(db) = db_opt.as_ref() {
            owner_id = db.with_snapshot(|s| s.items.get(&cycle.ship_id).map(|i| i.owner_id).unwrap_or(0));
        }
    }
    let char_id = cycle.owner_char_id.unwrap_or(owner_id);
    let launchers: Vec<i64> = if cycle.linked_turrets.is_empty() {
        vec![cycle.module_id]
    } else {
        cycle.linked_turrets.clone()
    };
    let n = launchers.len().max(1) as f64;
    let per_missile_dmg = DamageVector::new(
        cycle.damage_vector.em / n,
        cycle.damage_vector.thermal / n,
        cycle.damage_vector.kinetic / n,
        cycle.damage_vector.explosive / n,
    );
    let duration_ms = (cycle.duration_s.max(0.1)) * 1000.0;
    for launcher_module_id in launchers {
        pending_missile_launches.push(PendingMissileLaunch {
            queued_tick: ballpark.current_tick,
            char_id,
            owner_id,
            corp_id,
            alliance_id,
            attacker_ship_id: cycle.ship_id,
            target_ball_id,
            master_module_id: cycle.module_id,
            launcher_module_id,
            module_type_id: cycle.module_type_id,
            ammo_type_id,
            max_velocity_ms: cycle.missile_max_velocity_ms.max(1.0),
            flight_time_s: cycle.missile_flight_time_s.max(0.1),
            ball_radius: cycle.missile_ball_radius.max(1.0),
            explosion_radius_m: cycle.missile_explosion_radius_m,
            explosion_velocity_ms: cycle.missile_explosion_velocity_ms,
            drf: cycle.missile_drf,
            damage_vector: per_missile_dmg,
            duration_ms,
        });
    }
}

/// Emits queued deployment `OnSpecialFX` (`effects.MissileDeployment`) to `ballpark` owner and
/// grid observers on the tick following missile `AddBalls2` acquisition.
pub(crate) fn flush_pending_missile_deploy_fx(
    ballpark: &mut Ballpark,
    pending_deploy_fx: &mut Vec<PendingMissileDeployFx>,
) {
    if pending_deploy_fx.is_empty() {
        return;
    }
    let now_tick = ballpark.current_tick;
    let mut ready = Vec::new();
    let mut deferred = Vec::new();
    for fx in pending_deploy_fx.drain(..) {
        if fx.emit_at_tick <= now_tick {
            ready.push(fx);
        } else {
            deferred.push(fx);
        }
    }
    *pending_deploy_fx = deferred;
    for fx in ready {
        if !ballpark.balls.contains_key(&fx.attacker_ship_id) {
            continue;
        }
        ballpark.special_fx_meta.insert(
            (
                fx.attacker_ship_id,
                Some(fx.master_module_id),
                MISSILE_DEPLOY_FX_GUID.to_string(),
            ),
            SpecialFxWireMeta {
                module_type_id: Some(fx.module_type_id),
                target_id: Some(fx.target_ball_id),
                other_type_id: Some(fx.ammo_type_id),
                is_offensive: true,
                duration_ms: Some(fx.duration_ms),
                repeat: Some(1),
            },
        );
        ballpark.emit_event(
            Some(fx.attacker_ship_id),
            DestinyEvent::OnSpecialFX {
                ball_id: fx.attacker_ship_id,
                module_id: Some(fx.master_module_id),
                guid: MISSILE_DEPLOY_FX_GUID.to_string(),
                is_active: true,
            },
        );
    }
}

/// Snapshots `(missile_pos, target_pos)` before `ballpark.step(dt_sim)` so moving-target
/// segment checks cannot tunnel past a target inside a tick.
pub(crate) fn snapshot_missile_positions(
    ballpark: &Ballpark,
    active_missiles: &[ActiveGuidedMissile],
) -> HashMap<BallId, (DVec3, DVec3)> {
    let mut snap = HashMap::with_capacity(active_missiles.len());
    for m in active_missiles {
        if let (Some(mb), Some(tb)) = (
            ballpark.balls.get(&m.ball_id),
            ballpark.balls.get(&m.target_ball_id),
        ) {
            snap.insert(m.ball_id, (mb.position, tb.position));
        }
    }
    snap
}

/// Minimum distance from origin to segment `p0 -> p1` and clamped parameter `t in [0, 1]`.
#[inline]
fn segment_origin_min_dist(p0: DVec3, p1: DVec3) -> (f64, f64) {
    let seg = p1 - p0;
    let len2 = seg.length_squared();
    if len2 <= 1e-12 || !len2.is_finite() {
        return (p0.length(), 0.0);
    }
    let t = (-p0.dot(seg) / len2).clamp(0.0, 1.0);
    ((p0 + seg * t).length(), t)
}

/// Advances all live guided missiles by `dt_sim`, cleaning up lost/expired missiles via
/// `RemoveBalls` and applying delayed impact damage once the minimum client visual floor is met.
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_guided_missiles_in_actor(
    ballpark: &mut Ballpark,
    active_missiles: &mut Vec<ActiveGuidedMissile>,
    pre_positions: &HashMap<BallId, (DVec3, DVec3)>,
    rah_incoming_damage: &mut HashMap<BallId, [f64; 4]>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<WorldRepository>>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    destroyed_balls: &mut Vec<(BallId, Option<CharacterId>)>,
    pending_destroyed_outbox: Option<&PendingDestroyedOutbox>,
    dt_sim: f64,
    mut depot_reinf_shadow: Option<(&mut HashMap<BallId, (u64, u64)>, u64)>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    if active_missiles.is_empty() {
        return;
    }
    let dt_pos = if dt_sim > 0.0 && dt_sim.is_finite() {
        dt_sim
    } else {
        0.0
    };
    let current_tick = ballpark.current_tick;
    let mut to_remove_ids: Vec<BallId> = Vec::new();
    let mut pending_impacts: Vec<(ActiveGuidedMissile, DVec3)> = Vec::new();

    active_missiles.retain_mut(|m| {
        let Some(mb_pos) = ballpark.balls.get(&m.ball_id).map(|b| b.position) else {
            return false;
        };
        let target_ball = ballpark.balls.get(&m.target_ball_id);
        let target_cs = ballpark.combat_states.get(&m.target_ball_id);
        let target_lost = target_ball.is_none()
            || target_ball.is_some_and(|tb| matches!(tb.mode, MovementMode::Warp { .. }))
            || target_cs.is_some_and(|cs| cs.blocks_targeted_effect() || cs.structure_hp <= 0.0);
        if target_lost {
            to_remove_ids.push(m.ball_id);
            return false;
        }
        let tb = target_ball.expect("checked Some above");
        let prev_elapsed = m.elapsed_sim_s;
        m.elapsed_sim_s += dt_pos;

        if !m.impacted {
            let surface_r = tb.radius.max(1.0);
            let p1 = mb_pos - tb.position;
            let (min_dist, frac) = if let Some(&(pre_m, pre_t)) = pre_positions.get(&m.ball_id) {
                segment_origin_min_dist(pre_m - pre_t, p1)
            } else {
                (p1.length(), 1.0)
            };
            if min_dist <= surface_r {
                let impact_elapsed = prev_elapsed + frac * dt_pos;
                if impact_elapsed <= m.flight_time_s + 1e-6 {
                    m.impacted = true;
                }
            }
        }

        if !m.impacted && m.elapsed_sim_s >= m.flight_time_s - 1e-9 {
            to_remove_ids.push(m.ball_id);
            return false;
        }
        if m.impacted && current_tick > m.spawn_tick && m.elapsed_sim_s + 1e-9 >= m.min_visual_sim_s
        {
            pending_impacts.push((m.clone(), tb.position));
            to_remove_ids.push(m.ball_id);
            return false;
        }
        true
    });

    for (m, _) in pending_impacts {
        let effective_dmg = super::cycles::compute_missile_effective_damage(
            m.damage_vector,
            m.explosion_radius_m,
            m.explosion_velocity_ms,
            m.drf,
            m.target_ball_id,
            ballpark,
        );
        if effective_dmg.total() > 0.0 {
            let reinf_sh = depot_reinf_shadow.as_deref_mut().map(|(m, seq)| (&mut **m, *seq));
            super::cycles::apply_resolved_weapon_damage(
                m.attacker_ship_id,
                Some(m.char_id).filter(|&c| c > 0),
                m.module_type_id,
                m.target_ball_id,
                effective_dmg,
                ballpark,
                rah_incoming_damage,
                sde_opt,
                db_opt,
                pending_cycle_actions,
                destroyed_balls,
                pending_destroyed_outbox,
                reinf_sh,
                post_unlock,
            );
        }
    }

    if !to_remove_ids.is_empty() {
        ballpark.remove_balls_batch(&to_remove_ids);
    }
}

/// Flushes launches queued on an earlier tick into one batched `AddBalls2` per grid and schedules
/// deployment `OnSpecialFX` for the next tick.
pub(crate) fn flush_pending_missile_launches(
    ballpark: &mut Ballpark,
    pending_launches: &mut Vec<PendingMissileLaunch>,
    active_missiles: &mut Vec<ActiveGuidedMissile>,
    pending_deploy_fx: &mut Vec<PendingMissileDeployFx>,
    next_missile_ball_id: &mut i64,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    if pending_launches.is_empty() {
        return;
    }
    let now_tick = ballpark.current_tick;
    let mut ready = Vec::new();
    let mut deferred = Vec::new();
    for launch in pending_launches.drain(..) {
        if launch.queued_tick < now_tick {
            ready.push(launch);
        } else {
            deferred.push(launch);
        }
    }
    *pending_launches = deferred;
    if ready.is_empty() {
        return;
    }
    if *next_missile_ball_id < MIN_MISSILE_BALL_ID {
        *next_missile_ball_id = MIN_MISSILE_BALL_ID;
    }

    let mut grouped: BTreeMap<(BallId, BallId), Vec<PendingMissileLaunch>> = BTreeMap::new();
    for launch in ready {
        grouped
            .entry((launch.attacker_ship_id, launch.target_ball_id))
            .or_default()
            .push(launch);
    }

    let mut spawn_items: Vec<(Ball, CombatState, Option<SlimItemMeta>)> = Vec::new();
    let mut newly_active: Vec<ActiveGuidedMissile> = Vec::new();
    let mut fx_batches: BTreeMap<(BallId, i64), PendingMissileDeployFx> = BTreeMap::new();

    for ((attacker_id, target_id), launches) in grouped {
        let Some(attacker_ball) = ballpark.balls.get(&attacker_id).cloned() else {
            continue;
        };
        let Some(target_ball) = ballpark.balls.get(&target_id).cloned() else {
            continue;
        };
        let target_lost = matches!(target_ball.mode, MovementMode::Warp { .. })
            || ballpark
                .combat_states
                .get(&target_id)
                .is_some_and(|cs| cs.blocks_targeted_effect() || cs.structure_hp <= 0.0);
        if target_lost {
            continue;
        }
        let to_target = target_ball.position - attacker_ball.position;
        let center_dist = to_target.length();
        let aim_dir = if center_dist > 1e-6 && center_dist.is_finite() {
            to_target / center_dist
        } else {
            DVec3::X
        };
        let ref_axis = if aim_dir.y.abs() < 0.9 {
            DVec3::Y
        } else {
            DVec3::X
        };
        let right = aim_dir.cross(ref_axis).normalize_or_zero();
        let up = right.cross(aim_dir).normalize_or_zero();

        let forward_offset = attacker_ball
            .radius
            .max(0.0)
            .min((center_dist - target_ball.radius.max(0.0)).max(0.0));
        let count = launches.len();
        let lateral_r = if count > 1 {
            (attacker_ball.radius * 0.25).clamp(2.0, 25.0)
        } else {
            0.0
        };

        for (idx, launch) in launches.into_iter().enumerate() {
            while ballpark.balls.contains_key(next_missile_ball_id) {
                *next_missile_ball_id += 1;
            }
            let ball_id = *next_missile_ball_id;
            *next_missile_ball_id += 1;

            let lateral = if count > 1 {
                let theta = std::f64::consts::TAU * (idx as f64) / (count as f64);
                (right * theta.cos() + up * theta.sin()) * lateral_r
            } else {
                DVec3::ZERO
            };
            let spawn_pos = attacker_ball.position + aim_dir * forward_offset + lateral;
            let vel_dir = (target_ball.position - spawn_pos)
                .normalize_or_zero();
            let vel_dir = if vel_dir.length_squared() > 0.5 {
                vel_dir
            } else {
                aim_dir
            };
            let initial_vel = vel_dir * launch.max_velocity_ms;

            let (ammo_group_id, ammo_mass) = sde_opt
                .as_ref()
                .and_then(|sde| sde.types.get(&launch.ammo_type_id))
                .map(|t| (t.group_id as u32, t.mass.max(1.0)))
                .unwrap_or((384, 100.0));

            let ball = Ball {
                id: ball_id,
                type_id: launch.ammo_type_id,
                position: spawn_pos,
                velocity: initial_vel,
                max_velocity: launch.max_velocity_ms,
                inertia: 0.1,
                mass: ammo_mass,
                radius: launch.ball_radius,
                speed_fraction: 1.0,
                mode: MovementMode::Missile {
                    target_id: launch.target_ball_id,
                },
            };
            let cs = CombatState {
                shield_hp: 0.0,
                max_shield_hp: 0.0,
                armor_hp: 0.0,
                max_armor_hp: 0.0,
                structure_hp: 10.0,
                max_structure_hp: 10.0,
                signature_radius: launch.ball_radius,
                ..CombatState::default()
            };
            let slim = SlimItemMeta {
                group_id: ammo_group_id,
                category_id: 8,
                owner_id: launch.owner_id,
                char_id: None,
                corp_id: launch.corp_id,
                alliance_id: launch.alliance_id,
                source_ship_id: Some(launch.attacker_ship_id),
                launch_modules: vec![launch.launcher_module_id],
                ..SlimItemMeta::default()
            };
            spawn_items.push((ball, cs, Some(slim)));

            newly_active.push(ActiveGuidedMissile {
                ball_id,
                spawn_tick: now_tick,
                char_id: launch.char_id,
                attacker_ship_id: launch.attacker_ship_id,
                target_ball_id: launch.target_ball_id,
                module_id: launch.launcher_module_id,
                module_type_id: launch.module_type_id,
                ammo_type_id: launch.ammo_type_id,
                max_velocity_ms: launch.max_velocity_ms,
                flight_time_s: launch.flight_time_s,
                elapsed_sim_s: 0.0,
                min_visual_sim_s: MIN_CLIENT_VISUAL_SIM_S,
                impacted: false,
                explosion_radius_m: launch.explosion_radius_m,
                explosion_velocity_ms: launch.explosion_velocity_ms,
                drf: launch.drf,
                damage_vector: launch.damage_vector,
            });

            fx_batches
                .entry((launch.attacker_ship_id, launch.master_module_id))
                .or_insert(PendingMissileDeployFx {
                    emit_at_tick: now_tick.saturating_add(1),
                    attacker_ship_id: launch.attacker_ship_id,
                    target_ball_id: launch.target_ball_id,
                    master_module_id: launch.master_module_id,
                    module_type_id: launch.module_type_id,
                    ammo_type_id: launch.ammo_type_id,
                    duration_ms: launch.duration_ms,
                });
        }
    }

    if !spawn_items.is_empty() {
        ballpark.spawn_balls_batched(spawn_items);
        active_missiles.extend(newly_active);
        pending_deploy_fx.extend(fx_batches.into_values());
    }
}
