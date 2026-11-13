//! 1 Hz `Ballpark::step` physics loop, sub-warp & warp kinematics, collision bounce, and `combat` equations.

use crate::{
    Ball, BallId, Ballpark, DestinyEvent, MovementMode, WarpPhase, AU_METERS,
    BUBBLE_TRANSITION_THRESHOLD_METERS, DECLOAK_DISTANCE_METERS,
};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap, HashSet};

const PROXIMITY_CELL_SIZE_METERS: f64 = 4_000.0;
const MAX_PROXIMITY_CELLS_PER_BALL: i128 = 64;

#[inline]
pub(crate) fn aabb_overlaps(min_a: DVec3, max_a: DVec3, min_b: DVec3, max_b: DVec3) -> bool {
    !(max_a.x < min_b.x
        || max_b.x < min_a.x
        || max_a.y < min_b.y
        || max_b.y < min_a.y
        || max_a.z < min_b.z
        || max_b.z < min_a.z)
}

#[inline]
pub(crate) fn spatial_cell_range(
    min_b: DVec3,
    max_b: DVec3,
    cell_size: f64,
    max_cells: i128,
) -> Option<((i64, i64, i64), (i64, i64, i64))> {
    if !min_b.is_finite() || !max_b.is_finite() {
        return None;
    }
    let inv = 1.0 / cell_size;
    let c0 = (
        (min_b.x * inv).floor() as i64,
        (min_b.y * inv).floor() as i64,
        (min_b.z * inv).floor() as i64,
    );
    let c1 = (
        (max_b.x * inv).floor() as i64,
        (max_b.y * inv).floor() as i64,
        (max_b.z * inv).floor() as i64,
    );
    let sx = (c1.0 as i128 - c0.0 as i128) + 1;
    let sy = (c1.1 as i128 - c0.1 as i128) + 1;
    let sz = (c1.2 as i128 - c0.2 as i128) + 1;
    let total = sx.checked_mul(sy).and_then(|v| v.checked_mul(sz))?;
    if total > max_cells {
        None
    } else {
        Some((c0, c1))
    }
}

impl Ballpark {
    /// Advances the entire solar system simulation by `dt` seconds (standard EVE server tick `dt = 1.0`).
    /// Rejects non-positive or non-finite `dt`.
    pub fn step(&mut self, dt: f64) -> Vec<DestinyEvent> {
        if dt <= 0.0 || !dt.is_finite() {
            return Vec::new();
        }
        let dt = dt.min(60.0);
        self.current_tick += 1;
        if self.sim_unix_secs > 0 {
            self.sim_unix_secs = self.sim_unix_secs.saturating_add(dt.round().max(0.0) as u64);
        }
        let current_tick = self.current_tick;

        let dynamic_ids: Vec<BallId> = self
            .balls
            .keys()
            .copied()
            .filter(|&id| !self.is_static_ball(id))
            .collect();

        if dynamic_ids.is_empty()
            && self.combat_states.is_empty()
            && self.target_locks.is_empty()
            && self.mushroom_states.is_empty()
            && self.troll_states.is_empty()
            && self.miniball_states.is_empty()
            && self.formation_states.is_empty()
            && self.grids.is_empty()
        {
            return std::mem::take(&mut self.pending_events);
        }

        // Snapshot grid membership before stepping
        let grids_before: BTreeMap<BallId, u64> = dynamic_ids
            .iter()
            .filter_map(|&id| self.ball_grid_ids.get(&id).copied().map(|g| (id, g)))
            .collect();

        let mut positions: BTreeMap<BallId, (DVec3, DVec3, f64)> = BTreeMap::new();
        let mut pre_step_dynamic_balls: BTreeMap<BallId, Ball> = BTreeMap::new();
        for &id in &dynamic_ids {
            if let Some(b) = self.balls.get(&id) {
                positions.insert(id, (b.position, b.velocity, b.radius));
                pre_step_dynamic_balls.insert(id, b.clone());
                let target_id = match b.mode {
                    MovementMode::Follow { target_id, .. }
                    | MovementMode::Orbit { target_id, .. }
                    | MovementMode::Missile { target_id } => Some(target_id),
                    _ => None,
                };
                if let Some(tid) = target_id {
                    if !positions.contains_key(&tid) {
                        if let Some(tb) = self.balls.get(&tid) {
                            positions.insert(tid, (tb.position, tb.velocity, tb.radius));
                        }
                    }
                }
            }
        }

        let leader_frames: BTreeMap<BallId, (DVec3, DVec3, DVec3, DVec3)> = self
            .formation_states
            .values()
            .filter_map(|fs| {
                self.balls
                    .get(&fs.leader_id)
                    .map(|lb| (fs.leader_id, formation_leader_frame(lb)))
            })
            .collect();
        let mushroom_states = self.mushroom_states.clone();
        let troll_states = self.troll_states.clone();
        let formation_states = self.formation_states.clone();

        let mut warp_exit_stops = Vec::new();
        let mut expired_mushrooms = Vec::new();
        let mut frozen_trolls = Vec::new();
        let non_free_ball_ids = self.non_free_ball_ids.clone();

        for &ball_id in &dynamic_ids {
            let Some(ball) = self.balls.get_mut(&ball_id) else {
                continue;
            };
            if matches!(
                ball.mode,
                MovementMode::Rigid | MovementMode::Field | MovementMode::Miniball
            ) || non_free_ball_ids.contains(&ball.id)
            {
                if non_free_ball_ids.contains(&ball.id) {
                    ball.velocity = DVec3::ZERO;
                }
                continue;
            }

            if matches!(ball.mode, MovementMode::Mushroom) {
                // `_destiny.so` `Ballpark::Evolve` (`0x2d170..0x2d1eb`): stationary shockwave whose
                // radius expands by R(t) = R_max * (t / duration)^{0.25} and expires at t >= duration.
                ball.velocity = DVec3::ZERO;
                if let Some(ms) = mushroom_states.get(&ball.id) {
                    let elapsed_s = (current_tick.saturating_sub(ms.effect_stamp) as f64) * dt;
                    let t_norm = if ms.duration_s > 0.0 {
                        elapsed_s / ms.duration_s
                    } else {
                        1.0
                    };
                    if t_norm >= 1.0 - 1e-12 {
                        expired_mushrooms.push(ball.id);
                    } else if t_norm > 0.0 {
                        ball.radius = ms.max_radius * t_norm.sqrt().sqrt();
                    }
                }
                continue;
            }

            if matches!(ball.mode, MovementMode::Troll) {
                // `_destiny.so` `Ballpark::Evolve` (`0x2eff0`, `0x2d103..0x2d139`, `0x2da54..0x2dbb8`):
                // free ball coasting with zero goal velocity until `current_tick >= effect_stamp`,
                // then converting into `MovementMode::Rigid` (`DSTBALL_RIGID`).
                integrate_subwarp(ball, DVec3::ZERO, dt);
                if let Some(ts) = troll_states.get(&ball.id) {
                    if current_tick >= ts.effect_stamp {
                        ball.mode = MovementMode::Rigid;
                        ball.velocity = DVec3::ZERO;
                        ball.speed_fraction = 0.0;
                        frozen_trolls.push(ball.id);
                    }
                }
                continue;
            }

            if matches!(ball.mode, MovementMode::Boid) {
                // `_destiny.so` `Ballpark::Evolve` (`0x2eff0`): free sub-warp ball integrating velocity/position
                // under exponential drag without expiring into Rigid.
                integrate_subwarp(ball, DVec3::ZERO, dt);
                continue;
            }

            if matches!(ball.mode, MovementMode::Formation) {
                // `_destiny.so` `Ballpark::Evolve` (`0x275e5..0x2782e`): steers toward the leader's local
                // formation slot with quadratic damping inside one tick's travel distance.
                let goal_vel = formation_states
                    .get(&ball.id)
                    .and_then(|fs| {
                        leader_frames.get(&fs.leader_id).map(|&(lpos, right, up, fwd)| {
                            let slot_pos = lpos
                                + right * fs.slot_offset.x
                                + up * fs.slot_offset.y
                                + fwd * fs.slot_offset.z;
                            compute_formation_goal_velocity(ball, slot_pos, dt)
                        })
                    })
                    .unwrap_or(DVec3::ZERO);
                integrate_subwarp(ball, goal_vel, dt);
                continue;
            }

            if let MovementMode::Missile { target_id } = ball.mode {
                step_missile_ball(ball, positions.get(&target_id).map(|&(p, _, _)| p), dt);
                continue;
            }

            if let MovementMode::Warp {
                mut destination,
                warp_speed_au_s,
                mut phase,
                mut elapsed_warp_time,
                origin,
                mut total_distance,
            } = ball.mode.clone()
            {
                let is_nullified_or_shuttle = self.ball_group_ids.get(&ball.id) == Some(&31)
                    || self
                        .combat_states
                        .get(&ball.id)
                        .is_some_and(|cs| cs.is_interdiction_nullified);
                // While still sub-warp aligning, entering or sitting in a bubble aborts alignment immediately,
                // except for shuttles and active Interdiction Nullifiers.
                // Once in the warp tunnel (Accelerating/Cruising/Decelerating), newly anchored bubbles do not yank ships mid-flight.
                if phase == WarpPhase::Aligning
                    && !is_nullified_or_shuttle
                    && self.warp_bubble_mgr.is_scrambled_at(ball.position)
                {
                    ball.velocity = DVec3::ZERO;
                    ball.speed_fraction = 0.0;
                    ball.mode = MovementMode::Stop;
                    warp_exit_stops.push(ball.id);
                    continue;
                }

                let was_aligning = phase == WarpPhase::Aligning;
                let mut pre_intercepted = false;
                if !is_nullified_or_shuttle && phase == WarpPhase::Aligning {
                    let to_dest = destination - ball.position;
                    let warp_dir = to_dest.normalize_or_zero();
                    let align_threshold = ball.max_velocity * (0.75 - 1e-12);
                    if warp_dir != DVec3::ZERO && ball.velocity.dot(warp_dir) >= align_threshold {
                        if let Some((_bid, intercept_pos)) =
                            self.warp_bubble_mgr.check_interception(origin, destination)
                        {
                            destination = intercept_pos;
                            total_distance = (intercept_pos - origin).length();
                            pre_intercepted = true;
                        }
                    }
                }
                let exited = step_warp_ball(
                    ball,
                    dt,
                    origin,
                    destination,
                    warp_speed_au_s,
                    total_distance,
                    &mut phase,
                    &mut elapsed_warp_time,
                );
                if !is_nullified_or_shuttle && !pre_intercepted && was_aligning && phase != WarpPhase::Aligning {
                    if let Some((_bid, intercept_pos)) =
                        self.warp_bubble_mgr.check_interception(origin, destination)
                    {
                        if let MovementMode::Warp {
                            destination: ref mut dest_mut,
                            total_distance: ref mut total_dist_mut,
                            ..
                        } = ball.mode
                        {
                            *dest_mut = intercept_pos;
                            *total_dist_mut = (intercept_pos - origin).length();
                        }
                    }
                }
                if exited {
                    warp_exit_stops.push(ball.id);
                }
                continue;
            }

            let goal_velocity = compute_subwarp_goal_velocity(ball, &positions, dt);
            integrate_subwarp(ball, goal_velocity, dt);
        }

        for id in expired_mushrooms {
            let _ = self.remove_ball(id);
        }
        for id in frozen_trolls {
            self.troll_states.remove(&id);
            self.static_ball_ids.insert(id);
            self.static_epoch = self.static_epoch.wrapping_add(1);
        }
        for (mb_id, ms) in self.miniball_states.clone() {
            if let Some((ppos, pvel)) = self
                .balls
                .get(&ms.parent_id)
                .map(|p| (p.position, p.velocity))
            {
                if let Some(mb) = self.balls.get_mut(&mb_id) {
                    mb.position = ppos + ms.relative_offset;
                    mb.velocity = pvel;
                }
            } else {
                let _ = self.remove_ball(mb_id);
            }
        }

        // Collision bounce physics (`Destiny::Ball::DoCollision`, `SP-4`, `feat:navigation.collision-bumping`)
        self.step_collision_bumping_with_dynamics(&dynamic_ids, &pre_step_dynamic_balls, &positions, dt);

        // Gate cloak, post-warp invulnerability, and undock invulnerability decay and break on active movement (`SP-3`, `SP-4`, `SP-5`)
        let elapsed_ticks = (dt.floor() as u32).max(1);
        let mut gate_decloaked = Vec::new();
        for (&id, cs) in &mut self.combat_states {
            cs.sync_action_breaks();
            if cs.gate_cloak_remaining_ticks > 0 {
                let has_moved = self.balls.get(&id).is_some_and(|b| {
                    !matches!(b.mode, MovementMode::Stop)
                        || b.speed_fraction > 0.0
                        || b.velocity.length_squared() > 1.0
                });
                if has_moved {
                    cs.gate_cloak_remaining_ticks = 0;
                    cs.is_cloaked = false;
                    cs.undock_invulnerable_remaining_ticks = 0;
                    cs.sync_invuln_with_undock_timer = false;
                    gate_decloaked.push(id);
                } else {
                    cs.gate_cloak_remaining_ticks =
                        cs.gate_cloak_remaining_ticks.saturating_sub(elapsed_ticks);
                    if cs.gate_cloak_remaining_ticks == 0 {
                        cs.is_cloaked = false;
                        cs.sync_invuln_with_undock_timer = false;
                        gate_decloaked.push(id);
                    } else {
                        cs.is_cloaked = true;
                    }
                }
            }
            if cs.warp_exit_invulnerable_remaining_ticks > 0 {
                let has_active_move = self.balls.get(&id).is_some_and(|b| {
                    !matches!(b.mode, MovementMode::Stop) || b.speed_fraction > 0.0
                });
                if has_active_move {
                    cs.warp_exit_invulnerable_remaining_ticks = 0;
                    cs.undock_invulnerable_remaining_ticks = 0;
                    cs.sync_invuln_with_undock_timer = false;
                } else {
                    cs.warp_exit_invulnerable_remaining_ticks =
                        cs.warp_exit_invulnerable_remaining_ticks.saturating_sub(elapsed_ticks);
                    if cs.warp_exit_invulnerable_remaining_ticks == 0 {
                        cs.sync_invuln_with_undock_timer = false;
                    }
                }
            }
            if cs.undock_invulnerable_remaining_ticks > 0 {
                cs.undock_invulnerable_remaining_ticks =
                    cs.undock_invulnerable_remaining_ticks.saturating_sub(elapsed_ticks);
                if cs.undock_invulnerable_remaining_ticks == 0 {
                    cs.sync_invuln_with_undock_timer = false;
                }
            }
            if cs.is_cloaked && cs.cloak_stabilization_remaining_s > 0.0 {
                cs.cloak_stabilization_remaining_s =
                    (cs.cloak_stabilization_remaining_s - dt).max(0.0);
            }
            if !cs.is_cloaked && cs.decloak_targeting_delay_remaining_s > 0.0 {
                cs.decloak_targeting_delay_remaining_s =
                    (cs.decloak_targeting_delay_remaining_s - dt).max(0.0);
            }
        }

        // Check 2,000m surface-to-surface proximity decloak and active Breacher Pod DoT
        let mut cloaked_aabb_min = DVec3::splat(f64::INFINITY);
        let mut cloaked_aabb_max = DVec3::splat(f64::NEG_INFINITY);
        let mut any_cloaked = false;
        for (&cid, cs) in &self.combat_states {
            if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
                if let Some(cb) = self.balls.get(&cid) {
                    if !matches!(cb.mode, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning)
                    {
                        any_cloaked = true;
                        let start_c = positions.get(&cid).map(|&(p, _, _)| p).unwrap_or(cb.position);
                        let pad = DVec3::splat(cb.radius.max(0.0) + DECLOAK_DISTANCE_METERS);
                        cloaked_aabb_min = cloaked_aabb_min.min(start_c.min(cb.position) - pad);
                        cloaked_aabb_max = cloaked_aabb_max.max(start_c.max(cb.position) + pad);
                    }
                }
            }
        }
        let mut dyn_decloak_cells: HashMap<(i64, i64, i64), Vec<(BallId, DVec3, f64)>> =
            HashMap::new();
        let mut large_decloak_entries: Vec<(BallId, DVec3, f64)> = Vec::new();
        let mut updated_dyn_positions: Vec<(BallId, DVec3, f64)> = Vec::new();
        if any_cloaked {
            updated_dyn_positions.reserve(dynamic_ids.len());
            for &id in &dynamic_ids {
                if let Some(b) = self.balls.get(&id) {
                    let entry = (b.id, b.position, b.radius);
                    updated_dyn_positions.push(entry);
                    let start_b = positions.get(&id).map(|&(p, _, _)| p).unwrap_or(b.position);
                    let rad_b = DVec3::splat(b.radius.max(0.0));
                    let b_min = start_b.min(b.position) - rad_b;
                    let b_max = start_b.max(b.position) + rad_b;
                    if let Some((c0, c1)) = spatial_cell_range(
                        b_min,
                        b_max,
                        PROXIMITY_CELL_SIZE_METERS,
                        MAX_PROXIMITY_CELLS_PER_BALL,
                    ) {
                        for cx in c0.0..=c1.0 {
                            for cy in c0.1..=c1.1 {
                                for cz in c0.2..=c1.2 {
                                    dyn_decloak_cells.entry((cx, cy, cz)).or_default().push(entry);
                                }
                            }
                        }
                    } else {
                        large_decloak_entries.push(entry);
                    }
                }
            }
            let static_grid_decloak_ids = self
                .grids
                .values()
                .flat_map(|g| g.members.iter().copied())
                .filter(|id| !self.static_ball_ids.contains(id) && self.is_static_ball(*id));
            for id in self
                .static_ball_ids
                .iter()
                .copied()
                .chain(static_grid_decloak_ids)
            {
                if let Some(b) = self.balls.get(&id) {
                    let rad_b = DVec3::splat(b.radius.max(0.0));
                    if aabb_overlaps(
                        b.position - rad_b,
                        b.position + rad_b,
                        cloaked_aabb_min,
                        cloaked_aabb_max,
                    ) {
                        large_decloak_entries.push((b.id, b.position, b.radius));
                    }
                }
            }
        }
        let cloaked_snapshot: HashSet<BallId> = if any_cloaked {
            self.combat_states
                .iter()
                .filter_map(|(&id, c)| if c.is_cloaked { Some(id) } else { None })
                .collect()
        } else {
            HashSet::new()
        };
        let combat_ball_ids: Vec<BallId> = self.combat_states.keys().copied().collect();
        let mut proximity_decloaked = Vec::new();
        let mut mjfg_recompute_ids = Vec::new();
        let mut mjfg_completed_ids = Vec::new();
        let mut dot_updates = Vec::new();
        let mut seen_decloak_peers: HashSet<BallId> = HashSet::new();
        for id_a in combat_ball_ids {
            let Some((pos_a, rad_a, type_a, mode_a)) = self
                .balls
                .get(&id_a)
                .map(|b| (b.position, b.radius, b.type_id, b.mode.clone()))
            else {
                continue;
            };
            let is_cmd_destroyer = (37480..=37483).contains(&type_a);
            let is_scrambled = self.is_ball_scrambled(id_a);
            let has_target_locks = self
                .target_locks
                .get(&id_a)
                .is_some_and(|locks| !locks.is_empty());
            let is_in_warp_tunnel_a =
                matches!(mode_a, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning);
            let is_field_or_bubble_b = |id_b: BallId| {
                self.warp_bubble_mgr.bubbles.contains_key(&id_b)
                    || self.balls.get(&id_b).is_some_and(|b| {
                        matches!(b.mode, MovementMode::Field | MovementMode::Mushroom)
                            || matches!(b.mode, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning)
                    })
                    || cloaked_snapshot.contains(&id_b)
            };
            if let Some(cs) = self.combat_states.get_mut(&id_a) {
                if cs.is_tethered {
                    if has_target_locks || is_scrambled || cs.is_mwd_mjd_disabled || cs.disallow_assistance {
                        cs.is_tethered = false;
                    } else {
                        cs.shield_hp = (cs.shield_hp + cs.max_shield_hp * 0.10 * dt).min(cs.max_shield_hp);
                        cs.armor_hp = (cs.armor_hp + cs.max_armor_hp * 0.10 * dt).min(cs.max_armor_hp);
                        cs.structure_hp = (cs.structure_hp + cs.max_structure_hp * 0.10 * dt).min(cs.max_structure_hp);
                    }
                }
                if is_cmd_destroyer {
                    let prev_cap = cs.last_step_capacitor.unwrap_or(cs.max_capacitor);
                    if cs.mjfg_spool_remaining_s <= 0.0
                        && prev_cap - cs.capacitor >= 70.0
                        && !is_scrambled
                        && !cs.is_mwd_mjd_disabled
                    {
                        cs.mjfg_spool_remaining_s = 9.0;
                        if cs.base_signature_radius.is_none() {
                            cs.base_signature_radius = Some(cs.signature_radius);
                        }
                        if cs.mjfg_sig_multiplier.is_none() {
                            cs.mjfg_sig_multiplier = Some(2.5);
                            mjfg_recompute_ids.push(id_a);
                        }
                    }
                    if cs.mjfg_spool_remaining_s > 0.0 {
                        if is_scrambled || cs.is_mwd_mjd_disabled {
                            cs.mjfg_spool_remaining_s = 0.0;
                            if cs.mjfg_sig_multiplier.take().is_some() {
                                mjfg_recompute_ids.push(id_a);
                            }
                        } else {
                            cs.mjfg_spool_remaining_s =
                                (cs.mjfg_spool_remaining_s - dt).max(0.0);
                            if cs.mjfg_spool_remaining_s <= 1e-6 {
                                cs.mjfg_spool_remaining_s = 0.0;
                                if cs.mjfg_sig_multiplier.take().is_some() {
                                    mjfg_recompute_ids.push(id_a);
                                }
                                mjfg_completed_ids.push(id_a);
                            }
                        }
                    }
                }
                if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 && !is_in_warp_tunnel_a {
                    let start_a = positions.get(&id_a).map(|&(p, _, _)| p).unwrap_or(pos_a);
                    let pad = DVec3::splat(rad_a.max(0.0) + DECLOAK_DISTANCE_METERS);
                    let q_min = start_a.min(pos_a) - pad;
                    let q_max = start_a.max(pos_a) + pad;
                    let mut triggered_decloak = false;
                    let check_peer = |id_b: BallId, pos_b: DVec3, rad_b: f64| -> bool {
                        if id_a == id_b || is_field_or_bubble_b(id_b) {
                            return false;
                        }
                        let start_b = positions.get(&id_b).map(|&(p, _, _)| p).unwrap_or(pos_b);
                        let p0 = start_a - start_b;
                        let dp = (pos_a - start_a) - (pos_b - start_b);
                        let dp_len2 = dp.length_squared();
                        let t_min = if dp_len2 > 1e-9 {
                            (-p0.dot(dp) / dp_len2).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        let min_center_dist =
                            (p0 + dp * t_min).length().min((pos_a - pos_b).length());
                        let surface_dist = min_center_dist - rad_a - rad_b;
                        surface_dist <= DECLOAK_DISTANCE_METERS
                    };

                    if let Some((c0, c1)) = spatial_cell_range(
                        q_min,
                        q_max,
                        PROXIMITY_CELL_SIZE_METERS,
                        MAX_PROXIMITY_CELLS_PER_BALL,
                    ) {
                        seen_decloak_peers.clear();
                        'cell_decloak: for cx in c0.0..=c1.0 {
                            for cy in c0.1..=c1.1 {
                                for cz in c0.2..=c1.2 {
                                    if let Some(bucket) = dyn_decloak_cells.get(&(cx, cy, cz)) {
                                        for &(id_b, pos_b, rad_b) in bucket {
                                            if seen_decloak_peers.insert(id_b)
                                                && check_peer(id_b, pos_b, rad_b)
                                            {
                                                triggered_decloak = true;
                                                break 'cell_decloak;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !triggered_decloak {
                            for &(id_b, pos_b, rad_b) in &large_decloak_entries {
                                if seen_decloak_peers.insert(id_b)
                                    && check_peer(id_b, pos_b, rad_b)
                                {
                                    triggered_decloak = true;
                                    break;
                                }
                            }
                        }
                    } else {
                        for &(id_b, pos_b, rad_b) in &updated_dyn_positions {
                            if check_peer(id_b, pos_b, rad_b) {
                                triggered_decloak = true;
                                break;
                            }
                        }
                        if !triggered_decloak {
                            for &(id_b, pos_b, rad_b) in &large_decloak_entries {
                                if check_peer(id_b, pos_b, rad_b) {
                                    triggered_decloak = true;
                                    break;
                                }
                            }
                        }
                    }

                    if triggered_decloak {
                        cs.is_cloaked = false;
                        cs.is_covert_cloak = false;
                        cs.cloak_stabilization_remaining_s = 0.0;
                        if cs.cloak_configured_targeting_delay_s > 0.0 {
                            cs.decloak_targeting_delay_remaining_s =
                                cs.cloak_configured_targeting_delay_s;
                        }
                        proximity_decloaked.push(id_a);
                    }
                }
                if let Some((flat_dps, pct_dps, rem_ticks)) = cs.active_breacher_dot {
                    let max_total = cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp;
                    let tick_dmg = flat_dps.min(pct_dps * max_total) * dt;
                    let (applied, destroyed) = cs.apply_true_damage(tick_dmg);
                    let shield_ratio = if cs.max_shield_hp > 0.0 {
                        (cs.shield_hp / cs.max_shield_hp).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let armor_ratio = if cs.max_armor_hp > 0.0 {
                        (cs.armor_hp / cs.max_armor_hp).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let structure_ratio = if cs.max_structure_hp > 0.0 {
                        (cs.structure_hp / cs.max_structure_hp).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let next_rem = rem_ticks.saturating_sub(1);
                    dot_updates.push((
                        id_a,
                        flat_dps,
                        pct_dps,
                        next_rem,
                        applied > 0.0,
                        shield_ratio,
                        armor_ratio,
                        structure_ratio,
                        destroyed,
                    ));
                    if rem_ticks <= 1 || destroyed {
                        cs.active_breacher_dot = None;
                    } else {
                        cs.active_breacher_dot = Some((flat_dps, pct_dps, next_rem));
                    }
                }

                // Passive Shield and Capacitor recharge simulation (C-2, feat:combat.passive-shield-recharge-peak)
                if cs.shield_hp < cs.max_shield_hp && cs.shield_recharge_time_ms > 0.0 {
                    cs.shield_hp = combat::compute_passive_recharge(
                        cs.shield_hp,
                        cs.max_shield_hp,
                        cs.shield_recharge_time_ms,
                        dt * 1000.0,
                    );
                }
                if cs.capacitor < cs.max_capacitor && cs.capacitor_recharge_time_ms > 0.0 {
                    cs.capacitor = combat::compute_passive_recharge(
                        cs.capacitor,
                        cs.max_capacitor,
                        cs.capacitor_recharge_time_ms,
                        dt * 1000.0,
                    );
                }
                cs.last_step_capacitor = Some(cs.capacitor);
            }
        }
        let now_unix_secs = self.effective_unix_secs();
        for (
            dot_id,
            flat_dps,
            pct_dps,
            remaining_ticks,
            did_apply,
            shield_ratio,
            armor_ratio,
            structure_ratio,
            destroyed,
        ) in dot_updates
        {
            let expiry_filetime = if remaining_ticks > 0 {
                crate::unix_to_filetime(now_unix_secs.saturating_add(remaining_ticks as u64))
            } else {
                0
            };
            self.emit_event(
                Some(dot_id),
                DestinyEvent::OnDotVictimUpdated {
                    ball_id: dot_id,
                    flat_dps,
                    pct_dps,
                    remaining_ticks,
                    expiry_filetime,
                },
            );
            if did_apply {
                self.emit_event(
                    Some(dot_id),
                    DestinyEvent::OnDamageStateChange {
                        ball_id: dot_id,
                        shield_ratio,
                        armor_ratio,
                        structure_ratio,
                    },
                );
            }
            if destroyed {
                self.emit_event(
                    Some(dot_id),
                    DestinyEvent::TerminalExplosion {
                        ball_id: dot_id,
                        destruction_effect_id: None,
                    },
                );
                let _ = self.remove_ball_preserving_observer_events(dot_id);
            }
        }
        for rid in mjfg_recompute_ids {
            self.recompute_ball_attributes(rid);
        }
        for mjfg_id in mjfg_completed_ids {
            let _ = self.micro_jump_field(mjfg_id, 100_000.0, 6_000.0, 25);
        }
        for decloaked_id in gate_decloaked {
            self.finish_gate_decloak(decloaked_id);
        }
        for decloaked_id in proximity_decloaked {
            self.emit_event(
                Some(decloaked_id),
                DestinyEvent::CloakBall {
                    ball_id: decloaked_id,
                    cloaked: 0,
                },
            );
            self.emit_event(
                Some(decloaked_id),
                DestinyEvent::OnSpecialFX {
                    ball_id: decloaked_id,
                    module_id: None,
                    guid: "effects.Cloak".to_string(),
                    is_active: false,
                },
            );
            if let Some(c) = self.combat_states.get_mut(&decloaked_id) {
                c.cloak_velocity_multiplier = None;
                c.was_cloaked_last_tick = false;
            }
            let speed_change = self.recompute_ball_attributes_quiet(decloaked_id);
            if let Some(meta) = self.ball_slim_meta.get_mut(&decloaked_id) {
                meta.is_cloaked = false;
            }
            self.reveal_ball_to_grid_observers(decloaked_id);
            if let Some(next_max_v) = speed_change {
                self.set_ball_max_velocity(decloaked_id, next_max_v);
            }
        }

        // Reconcile shared dynamic GridBubble membership after movement (`SP-2`, `SP-3`, `Grid.md:13-15`).
        // Warping balls stay in their departure bubble throughout Aligning, Accelerating, and Cruising
        // and only transition once Decelerating (or landing) inside their destination grid, avoiding
        // orphan intermediate GridBubbles and spurious mid-warp AddBalls/RemoveBalls (`DestinyManager.cpp:365-380`).
        let mut grids_to_contract: Vec<u64> = Vec::new();
        for &ball_id in &dynamic_ids {
            let Some((new_pos, mode)) = self
                .balls
                .get(&ball_id)
                .map(|b| (b.position, b.mode.clone()))
            else {
                continue;
            };
            let is_warping = matches!(mode, MovementMode::Warp { .. });
            let exited_warp = warp_exit_stops.contains(&ball_id);
            let old_gid = self.ball_grid_ids.get(&ball_id).copied();
            let still_in_old = old_gid
                .and_then(|gid| self.grids.get(&gid))
                .is_some_and(|g| {
                    let d = (new_pos - g.center).abs();
                    d.x <= BUBBLE_TRANSITION_THRESHOLD_METERS
                        && d.y <= BUBBLE_TRANSITION_THRESHOLD_METERS
                        && d.z <= BUBBLE_TRANSITION_THRESHOLD_METERS
                        && g.contains(new_pos)
                });
            if still_in_old {
                if !is_warping && !exited_warp {
                    if let Some(gid) = old_gid {
                        let _ = self.try_expand_grid(gid, new_pos);
                        if !grids_to_contract.contains(&gid) {
                            grids_to_contract.push(gid);
                        }
                    }
                }
                continue;
            }
            if let MovementMode::Warp {
                destination,
                phase,
                ..
            } = mode
            {
                match phase {
                    WarpPhase::Aligning | WarpPhase::Accelerating | WarpPhase::Cruising => {
                        if old_gid.is_some() {
                            continue;
                        }
                        let new_gid = self.find_or_create_grid_for_position(new_pos);
                        if let Some(g) = self.grids.get_mut(&new_gid) {
                            g.members.insert(ball_id);
                        }
                        self.ball_grid_ids.insert(ball_id, new_gid);
                        continue;
                    }
                    WarpPhase::Decelerating => {
                        let existing_dest_gid = self
                            .grids
                            .iter()
                            .find_map(|(&gid, g)| g.contains(destination).then_some(gid));
                        let target_gid = if let Some(dgid) = existing_dest_gid {
                            if !self.grids[&dgid].contains(new_pos) {
                                continue;
                            }
                            if let Some(gid) = old_gid {
                                if gid != dgid {
                                    if let Some(g) = self.grids.get_mut(&gid) {
                                        g.members.remove(&ball_id);
                                    }
                                    self.contract_or_delete_grid(gid);
                                }
                            }
                            dgid
                        } else {
                            let (min_b, max_b) = self.prospective_grid_bounds(destination);
                            let in_dest_bounds = new_pos.x >= min_b.x
                                && new_pos.x <= max_b.x
                                && new_pos.y >= min_b.y
                                && new_pos.y <= max_b.y
                                && new_pos.z >= min_b.z
                                && new_pos.z <= max_b.z;
                            if !in_dest_bounds {
                                continue;
                            }
                            if let Some(gid) = old_gid {
                                if let Some(g) = self.grids.get_mut(&gid) {
                                    g.members.remove(&ball_id);
                                }
                                self.contract_or_delete_grid(gid);
                            }
                            self.find_or_create_grid_for_position(destination)
                        };
                        if let Some(g) = self.grids.get_mut(&target_gid) {
                            g.members.insert(ball_id);
                        }
                        self.ball_grid_ids.insert(ball_id, target_gid);
                        continue;
                    }
                }
            }
            // Sub-warp ships pushing a grid boundary expand the grid (up to 10,000 km) if no adjacent grid wall blocks it.
            // Warping ships that dropped out of warp on this tick (`exited_warp`) never stretch grid boundaries (`Grid.md:13`).
            if !exited_warp {
                if let Some(gid) = old_gid {
                    if self.try_expand_grid(gid, new_pos) {
                        continue;
                    }
                }
            }
            if let Some(gid) = old_gid {
                if let Some(g) = self.grids.get_mut(&gid) {
                    g.members.remove(&ball_id);
                }
                self.contract_or_delete_grid(gid);
            }
            let new_gid = self.find_or_create_grid_for_position(new_pos);
            if let Some(g) = self.grids.get_mut(&new_gid) {
                g.members.insert(ball_id);
            }
            self.ball_grid_ids.insert(ball_id, new_gid);
        }
        for gid in grids_to_contract {
            self.contract_or_delete_grid(gid);
        }

        // Emit AddBalls2 and RemoveBalls for grid wall crossings and cloak transitions (`SP-2`, `#871`).
        let dyn_grid_info: Vec<(BallId, u64, u64, bool, bool)> = dynamic_ids
            .iter()
            .filter_map(|&id| {
                let &before = grids_before.get(&id)?;
                let &after = self.ball_grid_ids.get(&id)?;
                let (cloaked, was_cloaked) = self
                    .combat_states
                    .get(&id)
                    .map(|c| (c.is_cloaked, c.was_cloaked_last_tick))
                    .unwrap_or((false, false));
                Some((id, before, after, cloaked, was_cloaked))
            })
            .collect();
        let mut before_by_grid: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut after_by_grid: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut changed_in_grid: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut any_changed_or_cloaked = false;
        for (idx, &(_id, before, after, cloaked, was_cloaked)) in dyn_grid_info.iter().enumerate() {
            before_by_grid.entry(before).or_default().push(idx);
            after_by_grid.entry(after).or_default().push(idx);
            if before != after || cloaked != was_cloaked {
                any_changed_or_cloaked = true;
                changed_in_grid.entry(before).or_default().push(idx);
                if after != before {
                    changed_in_grid.entry(after).or_default().push(idx);
                }
            }
        }
        if any_changed_or_cloaked {
            for &(obs_id, obs_before, obs_after, _, _) in &dyn_grid_info {
                if !self.is_registered_observer(obs_id) {
                    continue;
                }
                let mut entered = Vec::new();
                let mut left = Vec::new();
                if obs_before != obs_after {
                    if let Some(after_list) = after_by_grid.get(&obs_after) {
                        for &other_idx in after_list {
                            let (other_id, other_before, other_after, other_cloaked, other_was_cloaked) =
                                dyn_grid_info[other_idx];
                            if obs_id == other_id {
                                continue;
                            }
                            let was_same = obs_before == other_before && !other_was_cloaked;
                            let now_same = obs_after == other_after && !other_cloaked;
                            if !was_same && now_same {
                                if let Some(other_ball) = self.balls.get(&other_id) {
                                    entered.push(other_ball.clone());
                                }
                            }
                        }
                    }
                    if let Some(before_list) = before_by_grid.get(&obs_before) {
                        for &other_idx in before_list {
                            let (other_id, other_before, other_after, other_cloaked, other_was_cloaked) =
                                dyn_grid_info[other_idx];
                            if obs_id == other_id {
                                continue;
                            }
                            let was_same = obs_before == other_before && !other_was_cloaked;
                            let now_same = obs_after == other_after && !other_cloaked;
                            if was_same && !now_same {
                                left.push(other_id);
                            }
                        }
                    }
                } else if let Some(changed_list) = changed_in_grid.get(&obs_before) {
                    for &other_idx in changed_list {
                        let (other_id, other_before, other_after, other_cloaked, other_was_cloaked) =
                            dyn_grid_info[other_idx];
                        if obs_id == other_id {
                            continue;
                        }
                        let was_same = obs_before == other_before && !other_was_cloaked;
                        let now_same = obs_after == other_after && !other_cloaked;
                        if !was_same && now_same {
                            if let Some(other_ball) = self.balls.get(&other_id) {
                                entered.push(other_ball.clone());
                            }
                        } else if was_same && !now_same {
                            left.push(other_id);
                        }
                    }
                }
                if !entered.is_empty() {
                    // A ball that itself crossed into another grid is sent that grid's balls with
                    // `AddBalls`, as evemu_Crucible `SystemBubble::Add` -> `SendAddBalls(pSE)` does for an
                    // entity entering a bubble (`SystemBubble.cpp:330-332`, `:697-745`). Balls that stay
                    // put get the newcomer via `AddBalls2`.
                    let ev = if obs_before != obs_after {
                        DestinyEvent::AddBalls { balls: entered }
                    } else {
                        DestinyEvent::AddBalls2 { balls: entered }
                    };
                    self.observer_events.entry(obs_id).or_default().push(ev);
                }
                if !left.is_empty() {
                    self.observer_events
                        .entry(obs_id)
                        .or_default()
                        .push(DestinyEvent::RemoveBalls { ball_ids: left });
                }
            }
        }
        for &id in &dynamic_ids {
            if let Some(cs) = self.combat_states.get_mut(&id) {
                cs.was_cloaked_last_tick = cs.is_cloaked;
            }
        }

        for ball_id in warp_exit_stops {
            let mut landing_pos = None;
            if let Some(b) = self.balls.get_mut(&ball_id) {
                let max_v = b.max_velocity.max(0.0);
                if !b.velocity.is_finite() {
                    b.velocity = DVec3::ZERO;
                } else {
                    let speed = b.velocity.length();
                    if speed > max_v {
                        b.velocity = if max_v > 0.0 {
                            (b.velocity / speed) * max_v
                        } else {
                            DVec3::ZERO
                        };
                    }
                }
                if b.position.is_finite() {
                    landing_pos = Some(b.position.to_array());
                }
            }
            if let Some(cs) = self.combat_states.get_mut(&ball_id) {
                cs.is_emergency_warping = false;
                cs.warp_exit_invulnerable_remaining_ticks = 10;
                cs.undock_invulnerable_remaining_ticks =
                    cs.undock_invulnerable_remaining_ticks.max(10);
                cs.sync_invuln_with_undock_timer = true;
            }
            let ev_pos = landing_pos.map(|position| DestinyEvent::SetBallPosition { ball_id, position });
            let ev = DestinyEvent::Stop { ball_id };
            let ev_massive = DestinyEvent::SetBallMassive {
                ball_id,
                is_massive: true,
            };
            let ev_deact = DestinyEvent::OnDeactivatingWarp { ball_id };
            let ev_sfx = DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: None,
                guid: "effects.Warping".to_string(),
                is_active: false,
            };
            if let Some(ref pos_ev) = ev_pos {
                self.pending_events.push(pos_ev.clone());
            }
            self.pending_events.push(ev.clone());
            self.pending_events.push(ev_massive.clone());
            self.pending_events.push(ev_deact.clone());
            self.pending_events.push(ev_sfx.clone());
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                // `OnDeactivatingWarp` is raised by the client's own destiny tick; it is kept in
                // `pending_events` for the simulation but never queued onto the wire.
                if let Some(ref pos_ev) = ev_pos {
                    self.queue_event_for_grid_observers(gid, None, pos_ev);
                }
                self.queue_event_for_grid_observers(gid, None, &ev);
                self.queue_event_for_grid_observers(gid, None, &ev_massive);
                self.queue_event_for_grid_observers(gid, None, &ev_sfx);
            }
        }

        for cs in self.combat_states.values_mut() {
            cs.sync_action_breaks();
            if cs.ecm_jammed_remaining_s > 0.0 {
                cs.ecm_jammed_remaining_s = (cs.ecm_jammed_remaining_s - dt).max(0.0);
            }
            if cs.lance_disrupted_remaining_s > 0.0 {
                cs.lance_disrupted_remaining_s = (cs.lance_disrupted_remaining_s - dt).max(0.0);
            }
        }

        // Advance target locks and validate lock breaks (`DG-4`, `DG-5`).
        let mut lost_events = Vec::new();
        let mut completed_events = Vec::new();
        let mut locks_to_retain = BTreeMap::new();

        for (&source_id, locks) in &self.target_locks {
            let source_ball = self.balls.get(&source_id);
            let source_cs = self.combat_states.get(&source_id);
            let source_cloaked = source_cs
                .is_some_and(|c| c.is_cloaked || c.decloak_targeting_delay_remaining_s > 0.0);
            let source_jammed = source_cs.is_some_and(|c| c.ecm_jammed_remaining_s > 0.0);
            let source_warping = source_ball
                .map(|b| matches!(b.mode, MovementMode::Warp { .. }))
                .unwrap_or(true);

            if source_ball.is_none() || source_cloaked || source_warping || source_jammed {
                for lock in locks {
                    lost_events.push((source_id, lock.target_id));
                }
                continue;
            }

            let mut retained = Vec::new();
            for mut lock in locks.clone() {
                let target_ball = self.balls.get(&lock.target_id);
                let target_untarg = self
                    .combat_states
                    .get(&lock.target_id)
                    .is_some_and(|c| c.is_cloaked || c.is_warp_exit_invulnerable());
                let target_warping = target_ball
                    .map(|b| matches!(b.mode, MovementMode::Warp { .. }))
                    .unwrap_or(true);
                let out_of_range = if let (Some(sb), Some(tb), Some(max_r)) =
                    (source_ball, target_ball, lock.max_range_m)
                {
                    (sb.position - tb.position).length() > max_r
                } else {
                    false
                };

                if target_ball.is_none() || target_untarg || target_warping || out_of_range {
                    lost_events.push((source_id, lock.target_id));
                } else {
                    if !lock.locked {
                        lock.remaining_lock_ms -= dt * 1000.0;
                        if lock.remaining_lock_ms <= 0.0 {
                            lock.locked = true;
                            lock.remaining_lock_ms = 0.0;
                            completed_events.push((source_id, lock.target_id));
                        }
                    }
                    retained.push(lock);
                }
            }
            if !retained.is_empty() {
                locks_to_retain.insert(source_id, retained);
            }
        }
        self.target_locks = locks_to_retain;

        for (source_id, target_id) in lost_events {
            self.pending_events.push(DestinyEvent::OnTarget {
                mode: "lost".to_string(),
                source_id,
                target_id,
            });
        }
        for (source_id, target_id) in completed_events {
            self.pending_events.push(DestinyEvent::OnTarget {
                mode: "locked".to_string(),
                source_id,
                target_id,
            });
        }

        std::mem::take(&mut self.pending_events)
    }
}

/// Combat, Weapons, EWAR, Target Lock, and CrimeWatch 2.0 Equations (`#43`–`#65`, `#132`–`#138`).
pub mod combat {
    use crate::DamageVector;
    use glam::DVec3;

    /// Computes target lock time in seconds (`#43`):
    /// $$t_{\text{lock}} = \frac{40000}{R_{\text{scan}} \cdot \text{arsinh}^2(\sigma_{\text{target}})}$$
    pub fn compute_lock_time_seconds(scan_resolution: f64, target_sig_radius: f64) -> f64 {
        let denom = scan_resolution.max(1.0) * target_sig_radius.max(1.0).asinh().powi(2);
        (40_000.0 / denom).max(0.25)
    }

    /// Computes angular velocity $\omega = \frac{\|\vec{d} \times \Delta \vec{v}\|}{\|\vec{d}\|^2}$ in rad/s (`#46`).
    pub fn compute_angular_velocity(
        attacker_pos: DVec3,
        attacker_vel: DVec3,
        target_pos: DVec3,
        target_vel: DVec3,
    ) -> f64 {
        let d = target_pos - attacker_pos;
        let dist_sq = d.length_squared();
        if dist_sq < 1.0 {
            return 0.0;
        }
        let dv = target_vel - attacker_vel;
        d.cross(dv).length() / dist_sq
    }

    /// Computes the analytical EVE passive recharge curve (used by Capacitor and Shield regeneration):
    ///
    /// $$C(t) = C_{\max} \left(1 + \left(\sqrt{\frac{C_0}{C_{\max}}} - 1\right) e^{-\frac{5 \Delta t}{\tau_{\text{recharge}}}}\right)^2$$
    pub fn compute_passive_recharge(
        current: f64,
        max_capacity: f64,
        recharge_time_ms: f64,
        dt_ms: f64,
    ) -> f64 {
        if max_capacity <= 0.0 || recharge_time_ms <= 0.0 || current >= max_capacity {
            return current.min(max_capacity);
        }
        let c0_ratio = (current.max(0.0) / max_capacity).sqrt();
        let decay = (-5.0 * dt_ms / recharge_time_ms).exp();
        let term = 1.0 + (c0_ratio - 1.0) * decay;
        (max_capacity * term * term).clamp(0.0, max_capacity)
    }

    /// Computes EVE Turret ChanceToHit with explicit signature resolution (`C-1`):
    /// $$P_{\text{hit}} = 0.5^{\left(\frac{\omega \cdot S_{\text{res}}}{\omega_{\text{track}} \cdot \sigma_{\text{target}}}\right)^2 + \left(\frac{\max(0, d - R_{\text{opt}})}{R_{\text{falloff}}}\right)^2}$$
    pub fn compute_turret_chance_to_hit_with_res(
        angular_velocity: f64,
        turret_tracking: f64,
        signature_resolution: f64,
        target_sig_radius: f64,
        distance: f64,
        optimal_range: f64,
        falloff_range: f64,
    ) -> f64 {
        let track_denom = (turret_tracking * target_sig_radius).max(1e-6);
        let track_term =
            ((angular_velocity * signature_resolution.max(1.0)) / track_denom).powi(2);
        let range_excess = (distance - optimal_range).max(0.0);
        let range_term = if falloff_range > 0.0 {
            (range_excess / falloff_range).powi(2)
        } else if range_excess > 0.0 {
            100.0
        } else {
            0.0
        };
        0.5_f64.powf(track_term + range_term)
    }

    /// Computes EVE Turret ChanceToHit (`#47`):
    /// $$P_{\text{hit}} = 0.5^{\left(\frac{\omega \cdot 40000}{\omega_{\text{track}} \cdot \sigma_{\text{target}}}\right)^2 + \left(\frac{\max(0, d - R_{\text{opt}})}{R_{\text{falloff}}}\right)^2}$$
    pub fn compute_turret_chance_to_hit(
        angular_velocity: f64,
        turret_tracking: f64,
        target_sig_radius: f64,
        distance: f64,
        optimal_range: f64,
        falloff_range: f64,
    ) -> f64 {
        compute_turret_chance_to_hit_with_res(
            angular_velocity,
            turret_tracking,
            40_000.0,
            target_sig_radius,
            distance,
            optimal_range,
            falloff_range,
        )
    }

    /// Resolves a turret shot given `chance_to_hit` and deterministic uniform roll `u in [0.0, 1.0)` (`#48`–`#49`).
    /// - If `u >= chance_to_hit`: Miss (`0.0` multiplier).
    /// - If `u < 0.01`: Wrecking Shot (`3.0` multiplier).
    /// - Otherwise: Normal hit with quality $Q = 0.49 + u \in [0.50, 1.49)$.
    pub fn resolve_turret_shot(
        base_damage: DamageVector,
        chance_to_hit: f64,
        roll_u: f64,
    ) -> (DamageVector, bool) {
        if roll_u >= chance_to_hit {
            return (DamageVector::default(), false);
        }
        if roll_u < 0.01 {
            return (base_damage.scaled(3.0), true);
        }
        let q = (0.49 + roll_u).clamp(0.49, 1.49);
        (base_damage.scaled(q), false)
    }

    /// Computes EVE Missile Damage Application factor (`#50`–`#51`):
    /// $$D = D_{\text{base}} \cdot \min\left(1.0, \frac{\sigma_t}{E_r}, \left(\frac{\sigma_t}{E_r} \cdot \frac{E_v}{v_t}\right)^{\text{drf}}\right)$$
    /// Accepts either the SDE pre-computed `aoeDamageReductionFactor` (`<= 1.0`) or raw DRF (`> 1.0`).
    pub fn compute_missile_damage_factor(
        target_sig_radius: f64,
        target_velocity: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        damage_reduction_factor: f64,
    ) -> f64 {
        let sig_ratio = target_sig_radius / explosion_radius.max(1.0);
        let vel_ratio = explosion_velocity / target_velocity.max(1.0);
        let exponent = if damage_reduction_factor <= 1.0 {
            damage_reduction_factor.max(1e-3)
        } else {
            (damage_reduction_factor.ln() / 5.5_f64.ln()).abs()
        };
        let combined = (sig_ratio * vel_ratio).powf(exponent);
        1.0_f64.min(sig_ratio).min(combined).clamp(0.0, 1.0)
    }

    /// Computes Entropic Disintegrator spool multiplier after `spool_cycles` (`#54`).
    pub fn compute_entropic_spool_multiplier(
        spool_cycles: u32,
        max_spool_cycles: u32,
        step_bonus: f64,
    ) -> f64 {
        1.0 + (spool_cycles.min(max_spool_cycles) as f64) * step_bonus
    }

    /// Computes CONCORD response delay in seconds for a high-sec solar system (`0.5 <= sec <= 1.0`, `#135`).
    pub fn concord_response_seconds(security_status: f64) -> f64 {
        let s = (security_status * 10.0).round() / 10.0;
        if s >= 0.9 {
            6.0
        } else if s >= 0.8 {
            7.0
        } else if s >= 0.7 {
            10.0
        } else if s >= 0.6 {
            14.0
        } else if s >= 0.5 {
            19.0
        } else {
            f64::INFINITY
        }
    }
}

pub(crate) fn compute_subwarp_goal_velocity(
    ball: &Ball,
    positions: &BTreeMap<BallId, (DVec3, DVec3, f64)>,
    dt: f64,
) -> DVec3 {
    match &ball.mode {
        MovementMode::GotoDirection { direction } => {
            let dir = direction.normalize_or_zero();
            dir * (ball.max_velocity * ball.speed_fraction)
        }
        MovementMode::Follow { target_id, range } => {
            if let Some(&(target_pos, _target_vel, target_radius)) = positions.get(target_id) {
                let r_total = *range + ball.radius + target_radius;
                let diff = ball.position - target_pos;
                let dist = diff.length();
                let stop_point = if dist > 0.0 {
                    target_pos + (diff / dist) * r_total
                } else {
                    target_pos + DVec3::new(r_total, 0.0, 0.0)
                };
                let err = stop_point - ball.position;
                let err_sq = err.length_squared();
                let max_spd = ball.max_velocity * ball.speed_fraction;
                let step_dist = max_spd * dt;
                let step_dist_sq = step_dist * step_dist;
                if err_sq == 0.0 || max_spd <= 0.0 {
                    DVec3::ZERO
                } else if err_sq < step_dist_sq && step_dist_sq > 0.0 {
                    let ratio = err_sq / step_dist_sq;
                    err.normalize() * (max_spd * ratio * ratio)
                } else {
                    err.normalize() * max_spd
                }
            } else {
                DVec3::ZERO
            }
        }
        MovementMode::Orbit { target_id, range } => {
            if let Some(&(target_pos, target_vel, target_radius)) = positions.get(target_id) {
                let v_max = ball.max_velocity * ball.speed_fraction;
                let to_t = target_pos - ball.position;
                let dist = to_t.length();
                if dist <= 1e-9 || v_max <= 0.0 || dt <= 0.0 {
                    return DVec3::ZERO;
                }
                let u = to_t / dist;
                let cross_vel = u.cross(ball.velocity);
                let nrm = if cross_vel.length_squared() > 1e-12 {
                    cross_vel.normalize()
                } else {
                    let z_cross = u.cross(DVec3::NEG_Z);
                    if z_cross.length_squared() > 1e-12 {
                        z_cross.normalize()
                    } else {
                        let angle = (ball.id as f32) as f64;
                        let (sin_a, cos_a) = (angle.sin(), angle.cos());
                        let pole_ref = if u.z < 0.0 {
                            DVec3::new(-cos_a, -sin_a, 0.0)
                        } else {
                            DVec3::new(cos_a, sin_a, 0.0)
                        };
                        u.cross(pole_ref).normalize_or_zero()
                    }
                };
                let tang = nrm.cross(u).normalize_or_zero();
                let r_des = *range + ball.radius + target_radius;
                let tau = ball.time_constant().max(1e-6);
                let a_max = v_max / tau;
                let d12 = ((r_des - dist) * 0.1 / v_max).clamp(-1.0, 1.0);
                let d15 = dist.max(0.1);
                let d11 = (a_max * d15).sqrt().min(v_max);
                let arc = dt * d11;
                let x = (arc * d12 / d15) / (d12 * d12 + 1.0).sqrt();
                let lterm = if x < -0.994_014_667_510_986_3 {
                    -6.885_381_841_077_475
                } else if x.abs() > 1e-12 {
                    (1.0 + x).ln()
                } else {
                    x
                };
                let phi = if d12.abs() >= 1e-12 {
                    lterm / d12
                } else {
                    arc / d15
                };
                let r_new = dist * (d12 * phi).exp();
                let (sp, cp) = phi.sin_cos();
                let p_orbit = ball.position + u * (dist - r_new * cp) + tang * (r_new * sp);
                let v_dir = (u * (sp - d12 * cp) + tang * (cp + d12 * sp)).normalize_or_zero();
                let v_spiral = v_dir * d11 + target_vel;
                let ek = (-dt / tau).exp();
                let omk = 1.0 - ek;
                let pos_denom = dt - tau * omk;
                let raw_goal = if v_spiral.length() >= 0.05 * v_max
                    && pos_denom.abs() > 1e-12
                    && omk.abs() > 1e-12
                {
                    let v_goal_pos =
                        (p_orbit - ball.position - ball.velocity * (tau * omk)) / pos_denom;
                    let v_goal_vel = (v_spiral - ball.velocity * ek) / omk;
                    (v_goal_pos + v_goal_vel) * 0.5
                } else {
                    (p_orbit - ball.position) / dt - ball.velocity
                };
                let raw_len = raw_goal.length();
                if raw_len > v_max {
                    raw_goal * (v_max / raw_len)
                } else {
                    raw_goal
                }
            } else {
                DVec3::ZERO
            }
        }
        MovementMode::Missile { .. }
        | MovementMode::Mushroom
        | MovementMode::Boid
        | MovementMode::Troll
        | MovementMode::Miniball
        | MovementMode::Formation
        | MovementMode::Stop
        | MovementMode::Warp { .. }
        | MovementMode::Field
        | MovementMode::Rigid => DVec3::ZERO,
    }
}

/// Computes the leader's `(position, right, up, forward)` local orthonormal basis (`_destiny.so` `0x27620..0x27739`).
pub(crate) fn formation_leader_frame(leader: &Ball) -> (DVec3, DVec3, DVec3, DVec3) {
    let fwd = if leader.velocity.length_squared() > 1e-4 {
        leader.velocity.normalize()
    } else if let MovementMode::GotoDirection { direction } = leader.mode {
        if direction.length_squared() > 1e-4 {
            direction.normalize()
        } else {
            DVec3::X
        }
    } else {
        DVec3::X
    };
    let ref_up = if fwd.y.abs() < 0.99 { DVec3::Y } else { DVec3::Z };
    let right = ref_up.cross(fwd).normalize_or_zero();
    let up = fwd.cross(right).normalize_or_zero();
    (leader.position, right, up, fwd)
}

/// Computes `DSTBALL_FORMATION` goal velocity toward `slot_pos` with quadratic damping inside one tick (`_destiny.so` `0x2773e..0x277ed`).
pub(crate) fn compute_formation_goal_velocity(ball: &Ball, slot_pos: DVec3, dt: f64) -> DVec3 {
    let delta = slot_pos - ball.position;
    let dist = delta.length();
    let v_max = ball.max_velocity * ball.speed_fraction;
    let step_dist = v_max * dt;
    if dist <= 1e-9 || v_max <= 0.0 || step_dist <= 1e-9 {
        return DVec3::ZERO;
    }
    let dir = delta / dist;
    if dist >= step_dist {
        dir * v_max
    } else {
        let ratio = dist / step_dist;
        dir * (v_max * ratio * ratio)
    }
}

/// `DSTBALL_MISSILE` movement, following evemu_Crucible `DestinyManager::ProcessState`
/// (`DestinyManager.cpp:165-174`): no inertial ramp; each tick the heading is re-aimed at the
/// target point and the missile moves at constant `maxVelocity` along it
/// (`velocity = heading * maxSpeed`, `position += velocity * dt`). The target point is the target
/// ball's current position (`MISSILE_Struct.targetID`, `DestinyStructs.h:170-179`); once the target
/// is gone the missile keeps flying along its last velocity (`DestinyManager.cpp:166`).
pub(crate) fn step_missile_ball(ball: &mut Ball, target_pos: Option<DVec3>, dt: f64) {
    if dt <= 0.0 {
        return;
    }
    let heading = match target_pos {
        Some(p) => (p - ball.position).normalize_or_zero(),
        None => ball.velocity.normalize_or_zero(),
    };
    ball.velocity = heading * ball.max_velocity;
    ball.position += ball.velocity * dt;
}

/// Integrates exponential sub-warp movement over `dt` seconds:
/// $$\vec{v}(t + \Delta t) = \vec{v}_{\text{goal}} + (\vec{v}(t) - \vec{v}_{\text{goal}}) e^{-\Delta t / \tau}$$
pub(crate) fn integrate_subwarp(ball: &mut Ball, goal_velocity: DVec3, dt: f64) {
    if dt <= 0.0 {
        return;
    }
    let tau = ball.time_constant().max(1e-3);
    let decay = (-dt / tau).exp();
    let v_diff = ball.velocity - goal_velocity;
    let displacement = goal_velocity * dt + v_diff * tau * (1.0 - decay);
    ball.position += displacement;
    ball.velocity = goal_velocity + v_diff * decay;
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_warp_ball(
    ball: &mut Ball,
    remaining_dt: f64,
    mut origin: DVec3,
    destination: DVec3,
    warp_speed_au_s: f64,
    mut total_distance: f64,
    phase: &mut WarpPhase,
    elapsed_warp_time: &mut f64,
) -> bool {
    let to_dest = destination - ball.position;
    let warp_dir = to_dest.normalize_or_zero();
    if warp_dir == DVec3::ZERO {
        ball.velocity = DVec3::ZERO;
        ball.speed_fraction = 0.0;
        ball.mode = MovementMode::Stop;
        return true;
    }

    if *phase == WarpPhase::Aligning {
        let speed = ball.velocity.length();
        let vel_dir = if speed > 1e-12 {
            ball.velocity / speed
        } else {
            DVec3::ZERO
        };
        let already_aligned = speed > ball.max_velocity * 0.75 && vel_dir.dot(warp_dir) > 0.99;
        if already_aligned {
            *phase = WarpPhase::Accelerating;
            origin = ball.position;
            total_distance = (destination - origin).length();
            *elapsed_warp_time = 0.0;
        } else {
            let goal_vel = warp_dir * ball.max_velocity;
            integrate_subwarp(ball, goal_vel, remaining_dt);
            ball.mode = MovementMode::Warp {
                origin,
                destination,
                warp_speed_au_s,
                total_distance,
                phase: *phase,
                elapsed_warp_time: *elapsed_warp_time,
            };
            return false;
        }
    }

    let k_a = warp_speed_au_s.max(1e-4);
    let k_d = (k_a / 3.0).min(2.0);
    let v_cruise = warp_speed_au_s * AU_METERS;
    let raw_dist = total_distance.max((destination - origin).length());
    let eff_total = raw_dist + 1.0;
    let active_dir = (destination - origin).normalize_or_zero();
    let dir = if active_dir != DVec3::ZERO {
        active_dir
    } else {
        warp_dir
    };

    let v_peak = v_cruise.min(eff_total * k_a * k_d / (k_a + k_d));
    let t_accel_end = if v_peak > k_a {
        (v_peak / k_a).ln() / k_a
    } else {
        0.0
    };
    let t_cruise_dur = (eff_total / v_peak - 1.0 / k_a - 1.0 / k_d).max(0.0);
    let s_decel_start = v_peak / k_d;

    let t = *elapsed_warp_time;
    if t <= t_accel_end {
        *phase = WarpPhase::Accelerating;
        let traveled = (k_a * t).exp().min(eff_total);
        let speed = ball.velocity.length().max(k_a * traveled);
        ball.position = destination - dir * (raw_dist - traveled);
        ball.velocity = dir * speed;
        *elapsed_warp_time += remaining_dt;
    } else if t - t_accel_end < t_cruise_dur {
        *phase = WarpPhase::Cruising;
        let t_cruise = t - t_accel_end;
        let traveled = (v_peak / k_a + v_peak * t_cruise).min(eff_total);
        ball.position = destination - dir * (raw_dist - traveled);
        ball.velocity = dir * v_peak;
        *elapsed_warp_time += remaining_dt;
    } else {
        *phase = WarpPhase::Decelerating;
        let t_decel = t - (t_accel_end + t_cruise_dur);
        let rem_plus_1 = s_decel_start * (-k_d * t_decel).exp();
        let stop_thresh = (0.5 * ball.max_velocity).min(100.0).max(1.0);
        if rem_plus_1 * k_d < stop_thresh {
            let rem_dist = (rem_plus_1 - 1.0).max(0.0);
            let speed = (rem_plus_1 * k_d).min(ball.max_velocity.max(0.0));
            ball.position = destination - dir * rem_dist;
            ball.velocity = dir * speed;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
            integrate_subwarp(ball, DVec3::ZERO, remaining_dt);
            return true;
        }
        let rem_dist = (rem_plus_1 - 1.0).max(0.0);
        let speed = rem_plus_1 * k_d;
        ball.position = destination - dir * rem_dist;
        ball.velocity = dir * speed;
        *elapsed_warp_time += remaining_dt;
    }

    ball.mode = MovementMode::Warp {
        origin,
        destination,
        warp_speed_au_s,
        total_distance: raw_dist,
        phase: if *elapsed_warp_time <= t_accel_end {
            WarpPhase::Accelerating
        } else if *elapsed_warp_time - t_accel_end < t_cruise_dur {
            WarpPhase::Cruising
        } else {
            WarpPhase::Decelerating
        },
        elapsed_warp_time: *elapsed_warp_time,
    };
    false
}

/// Returns `true` iff `ball` is in `WarpPhase::Decelerating` and will drop below the sub-warp
/// dropout threshold within the next `1.0 s` simulation tick (`rem_plus_1 * e^{-k_d} * k_d < stop_thresh`).
///
/// Mirrors the exact `WarpPhase::Decelerating` dropout condition in `integrate_warp` so docking
/// RPCs can bound 1 Hz client/server warp-arrival tolerance strictly to `<= 1.0 s`.
pub fn is_warp_finishing_within_1s(ball: &Ball) -> bool {
    let MovementMode::Warp {
        destination,
        warp_speed_au_s,
        phase,
        ..
    } = ball.mode
    else {
        return false;
    };
    if phase != WarpPhase::Decelerating || !warp_speed_au_s.is_finite() {
        return false;
    }
    let k_d = (warp_speed_au_s.max(1e-4) / 3.0).min(2.0);
    let rem_plus_1 = ball.position.distance(destination) + 1.0;
    let stop_thresh = (0.5 * ball.max_velocity).min(100.0).max(1.0);
    rem_plus_1 * (-k_d).exp() * k_d < stop_thresh
}

impl Ballpark {
    pub(crate) fn reveal_ball_to_grid_observers(&mut self, ball_id: BallId) {
        if let Some(c) = self.combat_states.get_mut(&ball_id) {
            c.was_cloaked_last_tick = false;
        }
        if let (Some(&gid), Some(b)) = (
            self.ball_grid_ids.get(&ball_id),
            self.balls.get(&ball_id).cloned(),
        ) {
            self.queue_event_for_grid_observers(
                gid,
                Some(ball_id),
                &DestinyEvent::AddBalls2 { balls: vec![b] },
            );
        }
    }

    pub(crate) fn finish_gate_decloak(&mut self, ball_id: BallId) {
        self.emit_event(
            Some(ball_id),
            DestinyEvent::CloakBall {
                ball_id,
                cloaked: 0,
            },
        );
        self.emit_event(
            Some(ball_id),
            DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: None,
                guid: "effects.Cloak".to_string(),
                is_active: false,
            },
        );
        if let Some(c) = self.combat_states.get_mut(&ball_id) {
            c.cloak_velocity_multiplier = None;
            c.was_cloaked_last_tick = false;
        }
        let speed_change = self.recompute_ball_attributes_quiet(ball_id);
        if let Some(meta) = self.ball_slim_meta.get_mut(&ball_id) {
            meta.is_cloaked = false;
        }
        self.reveal_ball_to_grid_observers(ball_id);
        if let Some(next_max_v) = speed_change {
            self.set_ball_max_velocity(ball_id, next_max_v);
        }
    }

    pub(crate) fn remove_ball_preserving_observer_events(
        &mut self,
        ball_id: BallId,
    ) -> Option<Ball> {
        let was_registered = self.is_registered_observer(ball_id);
        let saved_obs = self.observer_events.remove(&ball_id).unwrap_or_default();
        let removed = self.remove_ball(ball_id);
        if was_registered && !saved_obs.is_empty() {
            self.observer_events
                .entry(ball_id)
                .or_default()
                .extend(saved_obs);
        }
        removed
    }
}

#[cfg(test)] #[path = "physics_unit_tests.rs"] mod physics_unit_tests;

