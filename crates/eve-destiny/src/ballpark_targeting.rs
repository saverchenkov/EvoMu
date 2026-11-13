//! Target-lock management, ECM jam/burst application, and grid bubble observer queries for [`Ballpark`].

use crate::physics::{aabb_overlaps, spatial_cell_range};
use crate::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, TargetLock, WarpPhase};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const COLLISION_CELL_SIZE_METERS: f64 = 4_000.0;
const MAX_CELLS_PER_BALL: i128 = 27;

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
    /// Removes empty queue entries for balls no longer present in `self.balls` (`#868`).
    pub fn drain_observer_events(&mut self, observer_ball_id: BallId) -> Vec<DestinyEvent> {
        if self.balls.contains_key(&observer_ball_id) {
            self.observer_events
                .get_mut(&observer_ball_id)
                .map(std::mem::take)
                .unwrap_or_default()
        } else {
            self.observer_events
                .remove(&observer_ball_id)
                .unwrap_or_default()
        }
    }

    /// Drains and returns the map of `observer_ball_id -> Vec<DestinyEvent>` for all dynamic observers.
    /// Drops empty queue entries for removed balls after draining (`#868`).
    pub fn drain_all_observer_events(&mut self) -> BTreeMap<BallId, Vec<DestinyEvent>> {
        let mut out = BTreeMap::new();
        for (&id, events) in &mut self.observer_events {
            if !events.is_empty() {
                out.insert(id, std::mem::take(events));
            }
        }
        let balls = &self.balls;
        self.observer_events
            .retain(|id, events| !events.is_empty() || balls.contains_key(id));
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
    fn is_collision_exempt(
        &self,
        id: BallId,
        ball: &Ball,
        pre_ball_opt: Option<&Ball>,
    ) -> bool {
        let group_id = self.ball_group_ids.get(&id).copied().unwrap_or(0);
        // Asteroid Belt (`group 9`), Mobile Warp Disruptor (`group 361`),
        // Abyssal Spatial Hazard Clouds (`group 1971`), and
        // Abyssal Automated Pylons (`group 1973`/`1981`, whose `radius` encodes their
        // 5-40 km bubble/aura range) are volumetric/non-solid markers that must not eject ships;
        // solid celestial hulls (Star `group 6`, Planet `group 7`, Moon `group 8`),
        // stations, structures, stargates, and asteroids collide normally.
        if matches!(group_id, 9 | 361 | 1971 | 1973 | 1981)
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
                pre_ball_opt.map(|b| &b.mode),
                Some(MovementMode::Warp { phase, .. }) if *phase != WarpPhase::Aligning
            )
        {
            return true;
        }
        self.combat_states.get(&id).is_some_and(|cs| {
            let action_broken =
                cs.sync_invuln_with_undock_timer && cs.undock_invulnerable_remaining_ticks == 0;
            let gate_cloak = if action_broken {
                0
            } else {
                cs.gate_cloak_remaining_ticks
            };
            let is_cloaked = if action_broken && cs.gate_cloak_remaining_ticks > 0 {
                false
            } else {
                cs.is_cloaked
            };
            gate_cloak > 0 || cs.undock_invulnerable_remaining_ticks > 0 || is_cloaked
        })
    }

    /// Resolves momentum-conserving collision deflection physics (`Destiny::Ball::DoCollision`, `SP-4`,
    /// `feat:navigation.collision-bumping`, restitution `0.85`) across dynamic ships and static hulls
    /// (stations, stargates, Upwell structures, celestials, asteroids), separates overlapping spheres
    /// with zero interpenetration, resets aligning ships to `WarpPhase::Aligning`, and broadcasts
    /// `SetBallPosition`/`SetBallVelocity`.
    /// Uses spatial-cell bucketing for dynamic balls and filters static balls by active dynamic
    /// bounding volumes so idle system statics cost O(1) collision pair checks per tick (`#867`).
    #[cfg(test)]
    pub(crate) fn step_collision_bumping(
        &mut self,
        pre_step_balls: &BTreeMap<BallId, crate::Ball>,
        pre_positions: &BTreeMap<BallId, (DVec3, DVec3, f64)>,
        dt: f64,
    ) {
        let dynamic_ids: Vec<BallId> = self
            .balls
            .keys()
            .copied()
            .filter(|&id| !self.is_static_ball(id))
            .collect();
        self.step_collision_bumping_with_dynamics(&dynamic_ids, pre_step_balls, pre_positions, dt);
    }

    pub(crate) fn step_collision_bumping_with_dynamics(
        &mut self,
        dynamic_ids: &[BallId],
        pre_step_dynamic_balls: &BTreeMap<BallId, crate::Ball>,
        pre_positions: &BTreeMap<BallId, (DVec3, DVec3, f64)>,
        dt: f64,
    ) {
        if dynamic_ids.is_empty() {
            return;
        }

        struct BumpEntry<'a> {
            id: BallId,
            uncol: &'a crate::Ball,
            pre_pos: DVec3,
            cur_pos: DVec3,
            cur_vel: DVec3,
            aabb_min: DVec3,
            aabb_max: DVec3,
            radius: f64,
            mass: f64,
            is_static: bool,
            is_aligning: bool,
            mb_parent: Option<BallId>,
            modified: bool,
        }

        fn build_entry<'a>(
            id: BallId,
            ball: &'a crate::Ball,
            pre_ball_opt: Option<&crate::Ball>,
            mb_parent: Option<BallId>,
            is_static: bool,
        ) -> BumpEntry<'a> {
            let pre_pos = pre_ball_opt.map_or(ball.position, |b| b.position);
            let is_aligning = matches!(
                ball.mode,
                MovementMode::Warp {
                    phase: WarpPhase::Aligning,
                    ..
                }
            ) || matches!(
                pre_ball_opt.map(|b| &b.mode),
                Some(MovementMode::Warp {
                    phase: WarpPhase::Aligning,
                    ..
                })
            );
            let rad_vec = DVec3::splat(ball.radius.max(0.0));
            let aabb_min = pre_pos.min(ball.position) - rad_vec;
            let aabb_max = pre_pos.max(ball.position) + rad_vec;
            BumpEntry {
                id,
                uncol: ball,
                pre_pos,
                cur_pos: ball.position,
                cur_vel: ball.velocity,
                aabb_min,
                aabb_max,
                radius: ball.radius,
                mass: ball.mass.max(1.0),
                is_static,
                is_aligning,
                mb_parent,
                modified: false,
            }
        }

        let mut entries: Vec<BumpEntry<'_>> = Vec::with_capacity(dynamic_ids.len());
        let mut seen_entry_ids: HashSet<BallId> = HashSet::with_capacity(dynamic_ids.len());
        let mut dyn_aabb_min = DVec3::splat(f64::INFINITY);
        let mut dyn_aabb_max = DVec3::splat(f64::NEG_INFINITY);
        let mut dyn_cells: HashSet<(i64, i64, i64)> = HashSet::new();
        let mut has_large_dyn = false;
        let mut has_non_static = false;

        for &id in dynamic_ids {
            if !seen_entry_ids.insert(id) {
                continue;
            }
            let Some(ball) = self.balls.get(&id) else {
                continue;
            };
            let pre_ball_opt = pre_step_dynamic_balls.get(&id);
            if self.is_collision_exempt(id, ball, pre_ball_opt) {
                continue;
            }
            let group_id = self.ball_group_ids.get(&id).copied().unwrap_or(0);
            let slim_cat = self.ball_slim_meta.get(&id).map(|m| m.category_id).unwrap_or(0);
            let is_station_or_struct =
                matches!(group_id, 6 | 7 | 8 | 15 | 1404..=1410 | 1657 | 2016 | 2017)
                    || slim_cat == 65;
            let is_static = self.is_static_ball(id)
                || self.non_free_ball_ids.contains(&id)
                || matches!(ball.mode, MovementMode::Miniball)
                || is_station_or_struct
                || matches!(slim_cat, 2 | 3 | 22 | 23 | 25 | 40 | 46 | 65);
            let mb_parent = self.miniball_states.get(&id).map(|m| m.parent_id);
            let entry = build_entry(id, ball, pre_ball_opt, mb_parent, is_static);
            if !is_static {
                has_non_static = true;
                dyn_aabb_min = dyn_aabb_min.min(entry.aabb_min);
                dyn_aabb_max = dyn_aabb_max.max(entry.aabb_max);
                if let Some((c0, c1)) = spatial_cell_range(
                    entry.aabb_min,
                    entry.aabb_max,
                    COLLISION_CELL_SIZE_METERS,
                    MAX_CELLS_PER_BALL,
                ) {
                    for cx in c0.0..=c1.0 {
                        for cy in c0.1..=c1.1 {
                            for cz in c0.2..=c1.2 {
                                dyn_cells.insert((cx, cy, cz));
                            }
                        }
                    }
                } else {
                    has_large_dyn = true;
                }
            }
            entries.push(entry);
        }

        if !has_non_static {
            return;
        }

        let static_grid_ids = self.grids.values().flat_map(|g| g.members.iter().copied()).filter(|id| {
            !self.static_ball_ids.contains(id) && self.is_static_ball(*id)
        });
        for id in self.static_ball_ids.iter().copied().chain(static_grid_ids) {
            if !seen_entry_ids.insert(id) {
                continue;
            }
            let Some(ball) = self.balls.get(&id) else {
                continue;
            };
            let pre_ball_opt = pre_step_dynamic_balls.get(&id);
            let pre_pos = pre_ball_opt.map_or(ball.position, |b| b.position);
            let rad_vec = DVec3::splat(ball.radius.max(0.0));
            let s_min = pre_pos.min(ball.position) - rad_vec;
            let s_max = pre_pos.max(ball.position) + rad_vec;
            if !aabb_overlaps(s_min, s_max, dyn_aabb_min, dyn_aabb_max) {
                continue;
            }
            if !has_large_dyn {
                if let Some((c0, c1)) = spatial_cell_range(
                    s_min,
                    s_max,
                    COLLISION_CELL_SIZE_METERS,
                    MAX_CELLS_PER_BALL,
                ) {
                    let mut hits_cell = false;
                    'cell_probe: for cx in c0.0..=c1.0 {
                        for cy in c0.1..=c1.1 {
                            for cz in c0.2..=c1.2 {
                                if dyn_cells.contains(&(cx, cy, cz)) {
                                    hits_cell = true;
                                    break 'cell_probe;
                                }
                            }
                        }
                    }
                    if !hits_cell {
                        continue;
                    }
                }
            }
            if self.is_collision_exempt(id, ball, pre_ball_opt) {
                continue;
            }
            let mb_parent = self.miniball_states.get(&id).map(|m| m.parent_id);
            entries.push(build_entry(id, ball, pre_ball_opt, mb_parent, true));
        }

        entries.sort_unstable_by_key(|e| e.id);

        let mut dyn_indices: Vec<usize> = Vec::with_capacity(dynamic_ids.len());
        let mut cell_map: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
        let mut large_indices: Vec<usize> = Vec::new();
        let mut entry_cells: Vec<Option<((i64, i64, i64), (i64, i64, i64))>> =
            Vec::with_capacity(entries.len());

        for (idx, e) in entries.iter().enumerate() {
            if !e.is_static {
                dyn_indices.push(idx);
            }
            let range = spatial_cell_range(
                e.aabb_min,
                e.aabb_max,
                COLLISION_CELL_SIZE_METERS,
                MAX_CELLS_PER_BALL,
            );
            if let Some((c0, c1)) = range {
                for cx in c0.0..=c1.0 {
                    for cy in c0.1..=c1.1 {
                        for cz in c0.2..=c1.2 {
                            cell_map.entry((cx, cy, cz)).or_default().push(idx);
                        }
                    }
                }
            } else {
                large_indices.push(idx);
            }
            entry_cells.push(range);
        }

        let mut best_swept: BTreeMap<BallId, (f64, DVec3, DVec3)> = BTreeMap::new();
        let mut collision_events = Vec::new();
        let mut bumped_dynamic_ids = BTreeSet::new();
        let restitution = 0.8;
        let mut seen_stamp: Vec<usize> = vec![0; entries.len()];
        let mut candidates: Vec<usize> = Vec::new();

        for i in 0..entries.len() {
            let is_static_a = entries[i].is_static;
            let token = i + 1;
            candidates.clear();
            match entry_cells[i] {
                Some((c0, c1)) => {
                    for cx in c0.0..=c1.0 {
                        for cy in c0.1..=c1.1 {
                            for cz in c0.2..=c1.2 {
                                if let Some(bucket) = cell_map.get(&(cx, cy, cz)) {
                                    for &j in bucket {
                                        if j > i
                                            && (!is_static_a || !entries[j].is_static)
                                            && seen_stamp[j] != token
                                        {
                                            seen_stamp[j] = token;
                                            candidates.push(j);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for &j in &large_indices {
                        if j > i
                            && (!is_static_a || !entries[j].is_static)
                            && seen_stamp[j] != token
                        {
                            seen_stamp[j] = token;
                            candidates.push(j);
                        }
                    }
                    candidates.sort_unstable();
                }
                None => {
                    if is_static_a {
                        let start = dyn_indices.partition_point(|&j| j <= i);
                        candidates.extend_from_slice(&dyn_indices[start..]);
                    } else {
                        candidates.extend((i + 1)..entries.len());
                    }
                }
            }

            for &j in &candidates {
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

                if !aabb_overlaps(a.aabb_min, a.aabb_max, b.aabb_min, b.aabb_max) {
                    continue;
                }

                // Ships aligning for warp must clear static station/gate/celestial bounding spheres
                // (e.g. undocking 500m off a 100 km station sphere and warping to a celestial behind
                // it) without bouncing indefinitely, while remaining bumpable by other dynamic ships.
                if (a.is_aligning && is_static_b) || (b.is_aligning && is_static_a) {
                    continue;
                }

                let uncol_a = a.uncol;
                let uncol_b = b.uncol;
                let pre_pos_a = a.pre_pos;
                let pre_pos_b = b.pre_pos;
                let rad_a = a.radius;
                let rad_b = b.radius;
                let mass_a = a.mass;
                let mass_b = b.mass;
                let min_dist = rad_a + rad_b;

                // Exact `_destiny.so` `DoCollision` swept-sphere collision (`0x293d4`):
                // solves quadratic intersection between pre-tick and uncollided post-tick positions.
                let dp0 = pre_pos_a - pre_pos_b;
                let dv = (uncol_a.position - pre_pos_a) - (uncol_b.position - pre_pos_b);
                let a_coef = dv.length_squared();
                let b_coef = 2.0 * dp0.dot(dv);
                let c_coef = dp0.length_squared() - min_dist * min_dist;
                if c_coef > 0.0 && a_coef > 0.0 {
                    let disc = b_coef * b_coef - 4.0 * a_coef * c_coef;
                    if disc >= 0.0 {
                        let t_col = (-b_coef - disc.sqrt()) / (2.0 * a_coef);
                        if (0.0..=1.0).contains(&t_col) {
                            let pre_a = pre_step_dynamic_balls.get(&id_a).unwrap_or(uncol_a);
                            let pre_b = pre_step_dynamic_balls.get(&id_b).unwrap_or(uncol_b);
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
