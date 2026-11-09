//! Target-lock management, ECM jam/burst application, and grid bubble observer queries for [`Ballpark`].

use crate::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, TargetLock, WarpPhase};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

impl Ballpark {
    pub fn add_target_lock(&mut self, source_id: BallId, lock: TargetLock) -> bool {
        let source_blocked = self.combat_states.get(&source_id).is_some_and(|cs| {
            (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
                || cs.decloak_targeting_delay_remaining_s > 0.0
        });
        let target_untargetable = self.combat_states.get_mut(&lock.target_id).is_some_and(|cs| {
            cs.sync_action_breaks();
            cs.is_cloaked || cs.gate_cloak_remaining_ticks > 0 || cs.is_warp_exit_invulnerable()
        });
        if source_blocked || target_untargetable {
            self.pending_events.push(DestinyEvent::OnTarget {
                mode: "lost".to_string(),
                source_id,
                target_id: lock.target_id,
            });
            return false;
        }

        let mut uncloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&source_id) {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.warp_exit_invulnerable_remaining_ticks = 0;
            cs.sync_invuln_with_undock_timer = false;
            if cs.gate_cloak_remaining_ticks > 0 {
                cs.gate_cloak_remaining_ticks = 0;
                cs.is_cloaked = false;
                uncloaked = true;
            }
        }
        if uncloaked {
            self.finish_gate_decloak(source_id);
        }
        // Deduplicate by `target_id` so re-locking an in-progress or already-locked target never
        // allocates duplicate target slots on the server (`#749`).
        let locks = self.target_locks.entry(source_id).or_default();
        if let Some(existing) = locks.iter_mut().find(|l| l.target_id == lock.target_id) {
            if lock.locked || !existing.locked {
                *existing = lock;
            }
        } else {
            locks.push(lock);
        }
        true
    }

    pub fn remove_target_lock(&mut self, source_id: BallId, target_id: BallId) -> bool {
        if let Some(locks) = self.target_locks.get_mut(&source_id) {
            let before = locks.len();
            locks.retain(|l| l.target_id != target_id);
            if locks.len() < before {
                self.pending_events.push(DestinyEvent::OnTarget {
                    mode: "lost".to_string(),
                    source_id,
                    target_id,
                });
                return true;
            }
        }
        false
    }

    pub fn clear_target_locks(&mut self, source_id: BallId) -> usize {
        if let Some(locks) = self.target_locks.remove(&source_id) {
            let count = locks.len();
            for l in &locks {
                self.pending_events.push(DestinyEvent::OnTarget {
                    mode: "lost".to_string(),
                    source_id,
                    target_id: l.target_id,
                });
            }
            count
        } else {
            0
        }
    }

    /// Applies an ECM jam to `target_id`, clearing all existing locked targets and preventing
    /// target locking for `duration_s` seconds (`feat:modules.ecm`, `DG-5`).
    pub fn apply_ecm_jam(&mut self, target_id: BallId, duration_s: f64) {
        if let Some(cs) = self.combat_states.get_mut(&target_id) {
            cs.sync_action_breaks();
            if cs.is_cloaked || cs.is_invulnerable() {
                return;
            }
            cs.ecm_jammed_remaining_s = duration_s;
        }
        self.clear_target_locks(target_id);
    }

    /// Applies an ECM burst AoE around `source_id`, jamming all valid target balls within `burst_radius_m`
    /// for which `jam_chance_test(target_id)` succeeds (`feat:modules.ecm`, `DG-5`).
    pub fn apply_ecm_burst<F>(
        &mut self,
        source_id: BallId,
        burst_radius_m: f64,
        mut jam_test: F,
    ) -> Vec<BallId>
    where
        F: FnMut(BallId) -> bool,
    {
        let Some(source_pos) = self.balls.get(&source_id).map(|b| b.position) else {
            return Vec::new();
        };
        let candidates: Vec<BallId> = self
            .balls
            .iter()
            .filter(|(&id, b)| {
                id != source_id
                    && !self.is_static_ball(id)
                    && self.combat_states.get(&id).is_some_and(|cs| {
                        let mut c = cs.clone();
                        c.sync_action_breaks();
                        !c.is_cloaked && !c.is_invulnerable()
                    })
                    && (b.position - source_pos).length() <= burst_radius_m
            })
            .map(|(&id, _)| id)
            .collect();

        let mut jammed = Vec::new();
        for cid in candidates {
            if jam_test(cid) {
                self.apply_ecm_jam(cid, 20.0);
                jammed.push(cid);
            }
        }
        jammed
    }

    pub fn get_target_locks(&self, source_id: BallId) -> Vec<TargetLock> {
        self.target_locks
            .get(&source_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Drains and returns all bubble-scoped `DestinyEvent`s accumulated for `observer_ball_id`.
    pub fn drain_observer_events(&mut self, observer_ball_id: BallId) -> Vec<DestinyEvent> {
        self.observer_events
            .get_mut(&observer_ball_id)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Drains and returns the map of `observer_ball_id -> Vec<DestinyEvent>` for all dynamic observers.
    pub fn drain_all_observer_events(&mut self) -> BTreeMap<BallId, Vec<DestinyEvent>> {
        let mut out = BTreeMap::new();
        for (&id, events) in &mut self.observer_events {
            if !events.is_empty() {
                out.insert(id, std::mem::take(events));
            }
        }
        out
    }

    /// Returns all balls visible to `observer_ball_id` in its shared [`GridBubble`]:
    /// all system-wide static balls (`Rigid` / `Field`) plus uncloaked dynamic balls in the same [`GridBubble`].
    pub fn visible_balls_for_observer(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let Some(obs_gid) = self.ball_grid_ids.get(&observer_ball_id).copied() else {
            return self
                .balls
                .values()
                .filter(|b| {
                    self.is_static_ball(b.id)
                        || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                })
                .collect();
        };
        self.balls
            .values()
            .filter(|b| {
                if self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                {
                    return true;
                }
                if b.id == observer_ball_id {
                    return true;
                }
                let in_grid = self.ball_grid_ids.get(&b.id).copied() == Some(obs_gid);
                let is_cloaked = self.combat_states.get(&b.id).is_some_and(|c| c.is_cloaked);
                in_grid && !is_cloaked
            })
            .collect()
    }

    /// Returns only the dynamic (non-static) balls in the same shared [`GridBubble`] as `observer_ball_id`.
    pub fn dynamic_balls_in_bubble(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let Some(obs_gid) = self.ball_grid_ids.get(&observer_ball_id).copied() else {
            return Vec::new();
        };
        let Some(grid) = self.grids.get(&obs_gid) else {
            return Vec::new();
        };
        grid.members
            .iter()
            .filter_map(|&id| {
                let b = self.balls.get(&id)?;
                if id != observer_ball_id
                    && self.combat_states.get(&id).is_some_and(|c| c.is_cloaked)
                {
                    return None;
                }
                Some(b)
            })
            .collect()
    }

    /// Returns all balls visible to `observer_ball_id` (or within `radius_meters` if `observer_ball_id` is static).
    pub fn balls_in_bubble(&self, observer_ball_id: BallId, radius_meters: f64) -> Vec<&Ball> {
        if self.ball_grid_ids.contains_key(&observer_ball_id) {
            return self.visible_balls_for_observer(observer_ball_id);
        }
        let Some(observer) = self.balls.get(&observer_ball_id) else {
            return Vec::new();
        };
        let r2 = radius_meters * radius_meters;
        self.balls
            .values()
            .filter(|b| {
                self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                    || (b.position - observer.position).length_squared() <= r2
            })
            .collect()
    }
}

fn subwarp_step_state(
    pos: DVec3,
    vel: DVec3,
    goal_vel: DVec3,
    tau: f64,
    dt: f64,
) -> (DVec3, DVec3) {
    if dt <= 0.0 {
        return (pos, vel);
    }
    let tau = tau.max(1e-6);
    let ek = (-dt / tau).exp();
    let v_diff = vel - goal_vel;
    let next_pos = pos + goal_vel * dt + v_diff * (tau * (1.0 - ek));
    let next_vel = goal_vel + v_diff * ek;
    (next_pos, next_vel)
}

impl Ballpark {
    /// Resolves momentum-conserving collision deflection physics (`Destiny::Ball::DoCollision`, `SP-4`,
    /// `feat:navigation.collision-bumping`, restitution `0.85`) across dynamic ships and static hulls
    /// (stations, stargates, Upwell structures, celestials, asteroids), separates overlapping spheres
    /// with zero interpenetration, resets aligning ships to `WarpPhase::Aligning`, and broadcasts
    /// `SetBallPosition`/`SetBallVelocity`.
    pub(crate) fn step_collision_bumping(
        &mut self,
        pre_step_balls: &BTreeMap<BallId, crate::Ball>,
        pre_positions: &BTreeMap<BallId, (DVec3, DVec3, f64)>,
        dt: f64,
    ) {
        struct BumpEntry<'a> {
            id: BallId,
            uncol: &'a crate::Ball,
            cur_pos: DVec3,
            cur_vel: DVec3,
            radius: f64,
            mass: f64,
            is_static: bool,
            mb_parent: Option<BallId>,
            modified: bool,
        }

        let mut entries: Vec<BumpEntry<'_>> = Vec::with_capacity(self.balls.len());
        let mut dyn_indices: Vec<usize> = Vec::new();

        for (&id, ball) in &self.balls {
            let group_id = self.ball_group_ids.get(&id).copied().unwrap_or(0);
            // Asteroid Belt (`group 9`), Abyssal Spatial Hazard Clouds (`group 1971`), and
            // Abyssal Automated Pylons (`group 1973`/`1981`, whose `radius` encodes their
            // 15-40 km aura range) are volumetric/non-solid markers that must not eject ships;
            // solid celestial hulls (Star `group 6`, Planet `group 7`, Moon `group 8`),
            // stations, structures, stargates, and asteroids collide normally.
            if matches!(group_id, 9 | 1971 | 1973 | 1981)
                || matches!(
                    ball.type_id,
                    47_437
                        | 47_438
                        | 47_439
                        | 47_440
                        | 47_441
                        | 47_469
                        | 47_470
                        | 47_472
                        | 47_473
                        | 47_620
                        | 48_254
                        | 64_896
                        | 80_876
                )
                || matches!(ball.mode, MovementMode::Field | MovementMode::Mushroom)
                || matches!(ball.mode, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning)
                || matches!(
                    pre_step_balls.get(&id).map(|b| &b.mode),
                    Some(MovementMode::Warp { phase, .. }) if *phase != WarpPhase::Aligning
                )
                || self.combat_states.get(&id).is_some_and(|cs| {
                    let mut c = cs.clone();
                    c.sync_action_breaks();
                    c.gate_cloak_remaining_ticks > 0
                        || c.undock_invulnerable_remaining_ticks > 0
                        || c.is_cloaked
                })
            {
                continue;
            }
            let slim_cat = self.ball_slim_meta.get(&id).map(|m| m.category_id).unwrap_or(0);
            let is_station_or_struct =
                matches!(group_id, 6 | 7 | 8 | 15 | 1404..=1410 | 1657 | 2016 | 2017)
                    || slim_cat == 65;
            let is_static = self.is_static_ball(id)
                || matches!(ball.mode, MovementMode::Miniball)
                || is_station_or_struct
                || matches!(slim_cat, 2 | 3 | 22 | 23 | 40 | 46 | 65);
            let mb_parent = self.miniball_states.get(&id).map(|m| m.parent_id);
            let idx = entries.len();
            if !is_static {
                dyn_indices.push(idx);
            }
            entries.push(BumpEntry {
                id,
                uncol: ball,
                cur_pos: ball.position,
                cur_vel: ball.velocity,
                radius: ball.radius,
                mass: ball.mass.max(1.0),
                is_static,
                mb_parent,
                modified: false,
            });
        }

        if dyn_indices.is_empty() {
            return;
        }

        let mut best_swept: BTreeMap<BallId, (f64, DVec3, DVec3)> = BTreeMap::new();
        let mut collision_events = Vec::new();
        let mut bumped_dynamic_ids = BTreeSet::new();
        let restitution = 0.8;

        for i in 0..entries.len() {
            let is_static_a = entries[i].is_static;
            let start_dyn = if is_static_a {
                dyn_indices.partition_point(|&j| j <= i)
            } else {
                0
            };
            let j_count = if is_static_a {
                dyn_indices.len() - start_dyn
            } else {
                entries.len().saturating_sub(i + 1)
            };

            for step in 0..j_count {
                let j = if is_static_a {
                    dyn_indices[start_dyn + step]
                } else {
                    i + 1 + step
                };
                let (left, right) = entries.split_at_mut(j);
                let a = &mut left[i];
                let b = &mut right[0];
                let id_a = a.id;
                let id_b = b.id;
                let is_static_b = b.is_static;

                if a.mb_parent == Some(id_b)
                    || b.mb_parent == Some(id_a)
                    || (a.mb_parent.is_some() && a.mb_parent == b.mb_parent)
                {
                    continue;
                }

                let uncol_a = a.uncol;
                let uncol_b = b.uncol;
                let pre_a = pre_step_balls.get(&id_a).unwrap_or(uncol_a);
                let pre_b = pre_step_balls.get(&id_b).unwrap_or(uncol_b);
                let rad_a = a.radius;
                let rad_b = b.radius;
                let mass_a = a.mass;
                let mass_b = b.mass;
                let min_dist = rad_a + rad_b;

                // Exact `_destiny.so` `DoCollision` swept-sphere collision (`0x293d4`):
                // solves quadratic intersection between pre-tick and uncollided post-tick positions.
                let dp0 = pre_a.position - pre_b.position;
                let dv = (uncol_a.position - pre_a.position) - (uncol_b.position - pre_b.position);
                let a_coef = dv.length_squared();
                let b_coef = 2.0 * dp0.dot(dv);
                let c_coef = dp0.length_squared() - min_dist * min_dist;
                if c_coef > 0.0 && a_coef > 0.0 {
                    let disc = b_coef * b_coef - 4.0 * a_coef * c_coef;
                    if disc >= 0.0 {
                        let t_col = (-b_coef - disc.sqrt()) / (2.0 * a_coef);
                        if (0.0..=1.0).contains(&t_col) {
                            let dt1 = t_col * dt;
                            let dt2 = (1.0 - t_col) * dt;
                            let goal_a = crate::physics::compute_subwarp_goal_velocity(
                                pre_a,
                                pre_positions,
                                dt,
                            );
                            let goal_b = crate::physics::compute_subwarp_goal_velocity(
                                pre_b,
                                pre_positions,
                                dt,
                            );
                            let tau_a = pre_a.time_constant().max(1e-6);
                            let tau_b = pre_b.time_constant().max(1e-6);
                            let (p_ac, v_ac) = if is_static_a {
                                if (uncol_a.position - pre_a.position).length_squared() < 1e-12 {
                                    (pre_a.position, DVec3::ZERO)
                                } else {
                                    (
                                        pre_a.position.lerp(uncol_a.position, t_col),
                                        (uncol_a.position - pre_a.position) / dt.max(1e-6),
                                    )
                                }
                            } else {
                                subwarp_step_state(pre_a.position, pre_a.velocity, goal_a, tau_a, dt1)
                            };
                            let (p_bc, v_bc) = if is_static_b {
                                if (uncol_b.position - pre_b.position).length_squared() < 1e-12 {
                                    (pre_b.position, DVec3::ZERO)
                                } else {
                                    (
                                        pre_b.position.lerp(uncol_b.position, t_col),
                                        (uncol_b.position - pre_b.position) / dt.max(1e-6),
                                    )
                                }
                            } else {
                                subwarp_step_state(pre_b.position, pre_b.velocity, goal_b, tau_b, dt1)
                            };
                            let diff = p_bc - p_ac;
                            let dist_c = diff.length();
                            let nrm = if dist_c > 0.0 {
                                diff / dist_c
                            } else {
                                DVec3::X
                            };
                            let v_an = v_ac.dot(nrm);
                            let v_bn = v_bc.dot(nrm);

                            if !is_static_a {
                                let v_an_new = if !is_static_b {
                                    ((mass_a - mass_b) * v_an + 2.0 * mass_b * v_bn)
                                        / (mass_a + mass_b)
                                } else {
                                    -v_an
                                };
                                let v_ab = v_ac + nrm * (v_an_new - v_an);
                                let (_p_af, v_af) =
                                    subwarp_step_state(p_ac, v_ab, goal_a, tau_a, dt2);
                                let v_final = uncol_a.velocity + (v_af - uncol_a.velocity) * 0.85;
                                let ek = (-dt / tau_a).exp();
                                let pos_coeff = if (1.0 - ek).abs() > 1e-12 {
                                    dt / (1.0 - ek) - tau_a
                                } else {
                                    0.5 * dt
                                };
                                let mut p_final =
                                    uncol_a.position + (v_final - uncol_a.velocity) * pos_coeff;
                                if is_static_b {
                                    let d_static = p_final - uncol_b.position;
                                    if d_static.length() < min_dist {
                                        p_final = uncol_b.position
                                            + d_static.normalize_or_zero() * min_dist;
                                    }
                                }
                                if best_swept.get(&id_a).is_none_or(|&(bt, _, _)| t_col < bt) {
                                    best_swept.insert(id_a, (t_col, p_final, v_final));
                                }
                                bumped_dynamic_ids.insert(id_a);
                            }

                            if !is_static_b {
                                let v_bn_new = if !is_static_a {
                                    ((mass_b - mass_a) * v_bn + 2.0 * mass_a * v_an)
                                        / (mass_a + mass_b)
                                } else {
                                    -v_bn
                                };
                                let v_bb = v_bc + nrm * (v_bn_new - v_bn);
                                let (_p_bf, v_bf) =
                                    subwarp_step_state(p_bc, v_bb, goal_b, tau_b, dt2);
                                let v_final = uncol_b.velocity + (v_bf - uncol_b.velocity) * 0.85;
                                let ek = (-dt / tau_b).exp();
                                let pos_coeff = if (1.0 - ek).abs() > 1e-12 {
                                    dt / (1.0 - ek) - tau_b
                                } else {
                                    0.5 * dt
                                };
                                let mut p_final =
                                    uncol_b.position + (v_final - uncol_b.velocity) * pos_coeff;
                                if is_static_a {
                                    let d_static = p_final - uncol_a.position;
                                    if d_static.length() < min_dist {
                                        p_final = uncol_a.position
                                            + d_static.normalize_or_zero() * min_dist;
                                    }
                                }
                                if best_swept.get(&id_b).is_none_or(|&(bt, _, _)| t_col < bt) {
                                    best_swept.insert(id_b, (t_col, p_final, v_final));
                                }
                                bumped_dynamic_ids.insert(id_b);
                            }

                            collision_events.push((id_a, id_b));
                            continue;
                        }
                    }
                }

                let (pos_a, vel_a) = (a.cur_pos, a.cur_vel);
                let (pos_b, vel_b) = (b.cur_pos, b.cur_vel);
                let delta = pos_a - pos_b;
                let dist = delta.length();

                if dist < min_dist {
                    let normal = if dist > 1e-4 {
                        delta / dist
                    } else {
                        DVec3::X
                    };
                    let overlap = min_dist - dist;
                    let v_rel = (vel_a - vel_b).dot(normal);

                    if !is_static_a && !is_static_b {
                        if v_rel < 0.0 {
                            let total_inv_mass = (1.0 / mass_a) + (1.0 / mass_b);
                            let impulse = -(1.0 + restitution) * v_rel / total_inv_mass;
                            a.cur_vel += normal * (impulse / mass_a);
                            b.cur_vel -= normal * (impulse / mass_b);
                        }
                        let total_mass = mass_a + mass_b;
                        a.cur_pos += normal * (overlap * (mass_b / total_mass));
                        b.cur_pos -= normal * (overlap * (mass_a / total_mass));
                        a.modified = true;
                        b.modified = true;
                        bumped_dynamic_ids.insert(id_a);
                        bumped_dynamic_ids.insert(id_b);
                    } else if !is_static_a && is_static_b {
                        if overlap > 10_000.0 {
                            continue;
                        }
                        if v_rel < 0.0 {
                            a.cur_vel -= normal * ((1.0 + restitution) * v_rel);
                            collision_events.push((id_a, id_b));
                        }
                        a.cur_pos += normal * overlap;
                        a.modified = true;
                        bumped_dynamic_ids.insert(id_a);
                    } else if is_static_a && !is_static_b {
                        if overlap > 10_000.0 {
                            continue;
                        }
                        if v_rel < 0.0 {
                            b.cur_vel += normal * ((1.0 + restitution) * v_rel);
                            collision_events.push((id_a, id_b));
                        }
                        b.cur_pos -= normal * overlap;
                        b.modified = true;
                        bumped_dynamic_ids.insert(id_b);
                    }

                    if !is_static_a && !is_static_b {
                        collision_events.push((id_a, id_b));
                    }
                }
            }
        }

        let pending_ball_updates: Vec<(BallId, DVec3, DVec3)> = entries
            .into_iter()
            .filter(|e| e.modified)
            .map(|e| (e.id, e.cur_pos, e.cur_vel))
            .collect();
        for (id, pos, vel) in pending_ball_updates {
            if let Some(b) = self.balls.get_mut(&id) {
                b.position = pos;
                b.velocity = vel;
            }
        }

        for (id, (_t_col, pos_final, vel_final)) in best_swept {
            if let Some(b) = self.balls.get_mut(&id) {
                b.position = pos_final;
                b.velocity = vel_final;
            }
        }

        for (id_a, id_b) in collision_events {
            self.pending_events.push(DestinyEvent::Collision {
                ball_a: id_a,
                ball_b: id_b,
            });
        }

        for bumped_id in bumped_dynamic_ids {
            if let Some(b) = self.balls.get_mut(&bumped_id) {
                if let MovementMode::Warp {
                    origin,
                    destination,
                    warp_speed_au_s,
                    ..
                } = b.mode
                {
                    b.mode = MovementMode::Warp {
                        origin: b.position,
                        destination,
                        warp_speed_au_s,
                        total_distance: (destination - b.position)
                            .length()
                            .max((destination - origin).length()),
                        phase: WarpPhase::Aligning,
                        elapsed_warp_time: 0.0,
                    };
                }
                let pos_ev = DestinyEvent::SetBallPosition {
                    ball_id: bumped_id,
                    position: b.position.to_array(),
                };
                let vel_ev = DestinyEvent::SetBallVelocity {
                    ball_id: bumped_id,
                    velocity: b.velocity.to_array(),
                };
                self.pending_events.push(pos_ev.clone());
                self.pending_events.push(vel_ev.clone());
                if let Some(&gid) = self.ball_grid_ids.get(&bumped_id) {
                    self.queue_event_for_grid_observers(gid, None, &pos_ev);
                    self.queue_event_for_grid_observers(gid, None, &vel_ev);
                }
            }
        }
    }

    /// Returns `true` when `ball_id` is immune to warp disruption bubbles (Shuttles `group 31` or active Interdiction Nullifier).
    pub fn is_ball_bubble_immune(&self, ball_id: BallId) -> bool {
        self.ball_group_ids.get(&ball_id) == Some(&31)
            || self
                .combat_states
                .get(&ball_id)
                .map(|cs| cs.is_interdiction_nullified)
                .unwrap_or(false)
    }

    pub fn is_ball_scrambled(&self, ball_id: BallId) -> bool {
        if let Some(cs) = self.combat_states.get(&ball_id) {
            if cs.is_warp_scrambled() {
                return true;
            }
        }
        if !self.is_ball_bubble_immune(ball_id) {
            if let Some(ball) = self.balls.get(&ball_id) {
                if self.warp_bubble_mgr.is_scrambled_at(ball.position) {
                    return true;
                }
            }
        }
        false
    }

    pub fn set_interdiction_nullified(&mut self, ball_id: BallId, nullified: bool) -> bool {
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            cs.is_interdiction_nullified = nullified;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
#[path = "ballpark_targeting_unit_tests.rs"]
mod ballpark_targeting_unit_tests;
