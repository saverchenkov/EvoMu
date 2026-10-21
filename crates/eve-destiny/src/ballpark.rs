//! `Ballpark` spatial grid management, ball lifecycle, warp bubbles, cloaking, MJD/MJFG, and target locking.

use crate::{
    Ball, BallId, Ballpark, DamageVector, DestinyEvent, GridBubble, MovementMode, SlimItemMeta,
    TargetLock, WarpBubble, WarpBubbleMgr, BUBBLE_RADIUS_METERS,
    BUBBLE_TRANSITION_THRESHOLD_METERS, DECLOAK_DISTANCE_METERS,
    GRID_WALL_EXPANSION_THRESHOLD_METERS, MAX_GRID_EXTENT_METERS,
};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

fn default_constellation_id() -> u32 {
    20_000_020
}

impl Ballpark {
    pub fn new(solar_system_id: u32) -> Self {
        Self::with_bubble_radius(solar_system_id, BUBBLE_RADIUS_METERS)
    }

    pub fn with_bubble_radius(solar_system_id: u32, bubble_radius_meters: f64) -> Self {
        Self {
            solar_system_id,
            constellation_id: default_constellation_id(),
            current_tick: 1,
            bubble_radius_meters: bubble_radius_meters.max(1.0),
            balls: BTreeMap::new(),
            combat_states: BTreeMap::new(),
            pending_events: Vec::new(),
            static_ball_ids: BTreeSet::new(),
            ball_group_ids: BTreeMap::new(),
            ball_slim_meta: BTreeMap::new(),
            grids: BTreeMap::new(),
            ball_grid_ids: BTreeMap::new(),
            next_grid_id: 1,
            observer_events: BTreeMap::new(),
            target_locks: BTreeMap::new(),
            warp_bubble_mgr: WarpBubbleMgr::default(),
        }
    }

    pub fn set_bubble_radius(&mut self, radius_meters: f64) {
        self.bubble_radius_meters = radius_meters.max(1.0);
    }

    /// Inserts a system-wide static ball (star, planet, moon, asteroid belt, stargate, station)
    /// without queuing dynamic `AddBall` notifications or waking `LOD 0` hibernation (`SP-1`).
    pub fn add_static_ball(&mut self, ball: Ball, group_id: u32) -> bool {
        let category_id = if group_id == 15 { 3 } else { 2 };
        let meta = SlimItemMeta {
            group_id,
            category_id,
            owner_id: 1,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
        };
        self.add_static_ball_with_meta(ball, meta)
    }

    /// Inserts a system-wide static ball with explicit [`SlimItemMeta`] (`SP-1`, `P-7`).
    pub fn add_static_ball_with_meta(&mut self, ball: Ball, meta: SlimItemMeta) -> bool {
        if self.balls.contains_key(&ball.id) {
            return false;
        }
        let ball_id = ball.id;
        if matches!(ball.mode, MovementMode::Field) {
            self.warp_bubble_mgr
                .add_bubble(WarpBubble::new(ball_id, ball.position, ball.radius));
        }
        self.static_ball_ids.insert(ball_id);
        self.ball_group_ids.insert(ball_id, meta.group_id);
        self.ball_slim_meta.insert(ball_id, meta);
        self.balls.insert(ball_id, ball);
        true
    }

    pub fn set_ball_group_id(&mut self, ball_id: BallId, group_id: u32) {
        self.ball_group_ids.insert(ball_id, group_id);
        self.ball_slim_meta
            .entry(ball_id)
            .and_modify(|m| m.group_id = group_id)
            .or_insert_with(|| SlimItemMeta {
                group_id,
                ..SlimItemMeta::default()
            });
    }

    pub fn ball_group_id(&self, ball_id: BallId) -> Option<u32> {
        self.ball_group_ids.get(&ball_id).copied()
    }

    pub fn set_ball_slim_meta(&mut self, ball_id: BallId, meta: SlimItemMeta) {
        self.ball_group_ids.insert(ball_id, meta.group_id);
        self.ball_slim_meta.insert(ball_id, meta);
    }

    pub fn ball_slim_meta(&self, ball_id: BallId) -> Option<&SlimItemMeta> {
        self.ball_slim_meta.get(&ball_id)
    }

    /// Returns `true` if `ball_id` is a system-wide static celestial/gate/station (`MovementMode::Rigid` / `static_ball_ids`).
    pub fn is_static_ball(&self, ball_id: BallId) -> bool {
        self.static_ball_ids.contains(&ball_id)
            || self
                .balls
                .get(&ball_id)
                .is_some_and(|b| matches!(b.mode, MovementMode::Rigid))
    }

    /// Returns `true` if any dynamic (non-static) ball is currently in the `Ballpark`.
    pub fn has_dynamic_balls(&self) -> bool {
        self.balls.keys().any(|&id| !self.is_static_ball(id))
    }

    /// Finds an existing [`GridBubble`] containing `pos`, or creates a new non-overlapping
    /// [`GridBubble`] whose boundaries are clamped against any adjacent pre-existing grid walls (`Grid.md:13-15`).
    pub fn find_or_create_grid_for_position(&mut self, pos: DVec3) -> u64 {
        for (&gid, grid) in &self.grids {
            if grid.contains(pos) {
                return gid;
            }
        }

        let r = self.bubble_radius_meters;
        let mut min_b = pos - DVec3::splat(r);
        let mut max_b = pos + DVec3::splat(r);

        // Clamp against any pre-existing grids so adjacent grids never overlap or merge (`Grid.md:15`).
        for existing in self.grids.values() {
            if existing.overlaps_bounds(min_b, max_b) {
                if pos.x > existing.max_bounds.x {
                    min_b.x = min_b.x.max(existing.max_bounds.x + 1.0);
                    max_b.x = max_b.x.max(min_b.x + 2.0 * r);
                } else if pos.x < existing.min_bounds.x {
                    max_b.x = max_b.x.min(existing.min_bounds.x - 1.0);
                    min_b.x = min_b.x.min(max_b.x - 2.0 * r);
                } else if pos.y > existing.max_bounds.y {
                    min_b.y = min_b.y.max(existing.max_bounds.y + 1.0);
                    max_b.y = max_b.y.max(min_b.y + 2.0 * r);
                } else if pos.y < existing.min_bounds.y {
                    max_b.y = max_b.y.min(existing.min_bounds.y - 1.0);
                    min_b.y = min_b.y.min(max_b.y - 2.0 * r);
                } else if pos.z > existing.max_bounds.z {
                    min_b.z = min_b.z.max(existing.max_bounds.z + 1.0);
                    max_b.z = max_b.z.max(min_b.z + 2.0 * r);
                } else if pos.z < existing.min_bounds.z {
                    max_b.z = max_b.z.min(existing.min_bounds.z - 1.0);
                    min_b.z = min_b.z.min(max_b.z - 2.0 * r);
                }
            }
        }

        let id = self.next_grid_id.max(1);
        self.next_grid_id = id + 1;
        let center = (min_b + max_b) * 0.5;
        let grid = GridBubble {
            id,
            center,
            min_bounds: min_b,
            max_bounds: max_b,
            base_radius: r,
            members: BTreeSet::new(),
        };
        self.grids.insert(id, grid);
        id
    }

    /// Returns the shared [`GridBubble`] ID containing `ball_id`, if any (`SP-2`).
    pub fn bubble_id_for_ball(&self, ball_id: BallId) -> Option<u64> {
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            return Some(gid);
        }
        let ball = self.balls.get(&ball_id)?;
        self.grids
            .values()
            .find(|g| g.contains(ball.position))
            .map(|g| g.id)
    }

    /// Expands `grid_id`'s boundaries when a non-warping ball at `target_pos` comes within
    /// [`GRID_WALL_EXPANSION_THRESHOLD_METERS`] (`100 km`) of a grid wall or crosses it
    /// (up to [`MAX_GRID_EXTENT_METERS`] from `center`), provided doing so does not overlap
    /// or merge with any other pre-existing grid (`Grid.md:13-15`).
    pub fn try_expand_grid(&mut self, grid_id: u64, target_pos: DVec3) -> bool {
        let Some(grid) = self.grids.get(&grid_id) else {
            return false;
        };
        let max_ext = BUBBLE_TRANSITION_THRESHOLD_METERS.max(grid.base_radius);
        let delta = (target_pos - grid.center).abs();
        if delta.x > max_ext || delta.y > max_ext || delta.z > max_ext {
            return false;
        }
        let thresh = DVec3::splat(GRID_WALL_EXPANSION_THRESHOLD_METERS);
        let max_min = grid.center - DVec3::splat(max_ext);
        let max_max = grid.center + DVec3::splat(max_ext);
        let new_min = grid.min_bounds.min((target_pos - thresh).max(max_min));
        let new_max = grid.max_bounds.max((target_pos + thresh).min(max_max));
        for (&other_id, other) in &self.grids {
            if other_id != grid_id && other.overlaps_bounds(new_min, new_max) {
                return false;
            }
        }
        if let Some(g) = self.grids.get_mut(&grid_id) {
            g.min_bounds = new_min;
            g.max_bounds = new_max;
        }
        true
    }

    /// Contracts an expanded grid back toward its `base_radius` around `center` or deletes the grid if empty (`Grid.md:13`).
    pub fn contract_or_delete_grid(&mut self, grid_id: u64) {
        let Some(is_empty) = self.grids.get(&grid_id).map(|g| g.members.is_empty()) else {
            return;
        };
        if is_empty {
            self.grids.remove(&grid_id);
            return;
        }
        let member_positions: Vec<DVec3> = self
            .grids
            .get(&grid_id)
            .map(|g| {
                g.members
                    .iter()
                    .filter_map(|id| self.balls.get(id).map(|b| b.position))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(g) = self.grids.get_mut(&grid_id) {
            let thresh = DVec3::splat(GRID_WALL_EXPANSION_THRESHOLD_METERS);
            let max_ext = MAX_GRID_EXTENT_METERS.max(g.base_radius);
            let max_min = g.center - DVec3::splat(max_ext);
            let max_max = g.center + DVec3::splat(max_ext);
            let base_min = g.center - DVec3::splat(g.base_radius);
            let base_max = g.center + DVec3::splat(g.base_radius);
            let mut req_min = base_min;
            let mut req_max = base_max;
            for p in member_positions {
                req_min = req_min.min((p - thresh).max(max_min));
                req_max = req_max.max((p + thresh).min(max_max));
            }
            g.min_bounds = g.min_bounds.max(req_min);
            g.max_bounds = g.max_bounds.min(req_max);
        }
    }

    /// Returns `true` if `ball_a` and `ball_b` share the same spatial grid bubble (`SP-2`).
    /// Visibility is a transitive equivalence relation over dynamic balls in the same [`GridBubble`];
    /// system-wide static balls (`Rigid`) are visible from every grid in the solar system.
    pub fn in_same_bubble(&self, ball_a: BallId, ball_b: BallId) -> bool {
        if !self.balls.contains_key(&ball_a) || !self.balls.contains_key(&ball_b) {
            return false;
        }
        if self.is_static_ball(ball_a) || self.is_static_ball(ball_b) {
            return true;
        }
        match (self.ball_grid_ids.get(&ball_a), self.ball_grid_ids.get(&ball_b)) {
            (Some(ga), Some(gb)) => ga == gb,
            _ => false,
        }
    }

    pub(crate) fn queue_event_for_grid_observers(
        &mut self,
        grid_id: u64,
        exclude_ball_id: Option<BallId>,
        event: &DestinyEvent,
    ) {
        let Some(members) = self.grids.get(&grid_id).map(|g| g.members.clone()) else {
            return;
        };
        for obs_id in members {
            if Some(obs_id) == exclude_ball_id {
                continue;
            }
            self.observer_events
                .entry(obs_id)
                .or_default()
                .push(event.clone());
        }
    }

    /// Inserts a new dynamic `Ball` into the solar system, assigning it to a shared [`GridBubble`]
    /// and notifying observers in that grid via `AddBalls2`.
    pub fn add_ball(&mut self, ball: Ball) -> bool {
        if self.balls.contains_key(&ball.id) {
            return false;
        }
        let ball_id = ball.id;
        let ball_pos = ball.position;
        if matches!(ball.mode, MovementMode::Field) {
            self.warp_bubble_mgr
                .add_bubble(WarpBubble::new(ball_id, ball_pos, ball.radius));
        }
        self.pending_events
            .push(DestinyEvent::AddBall { ball: ball.clone() });
        self.balls.insert(ball_id, ball.clone());
        self.combat_states.entry(ball_id).or_default();
        self.observer_events.entry(ball_id).or_default();
        let grid_id = self.find_or_create_grid_for_position(ball_pos);
        if let Some(g) = self.grids.get_mut(&grid_id) {
            g.members.insert(ball_id);
        }
        self.ball_grid_ids.insert(ball_id, grid_id);
        let is_cloaked = self
            .combat_states
            .get(&ball_id)
            .is_some_and(|c| c.is_cloaked);
        if !is_cloaked {
            self.queue_event_for_grid_observers(
                grid_id,
                Some(ball_id),
                &DestinyEvent::AddBalls2 { balls: vec![ball] },
            );
        }
        true
    }

    /// Queues a `SetState` destiny update for `observer_ball_id` (`P-7`).
    pub fn queue_set_state(&mut self, observer_ball_id: BallId) -> bool {
        if !self.balls.contains_key(&observer_ball_id) {
            return false;
        }
        self.observer_events
            .entry(observer_ball_id)
            .or_default()
            .push(DestinyEvent::SetState { observer_ball_id });
        true
    }

    pub fn remove_ball(&mut self, ball_id: BallId) -> Option<Ball> {
        let removed = self.balls.remove(&ball_id)?;
        self.warp_bubble_mgr.remove_bubble(ball_id);
        let old_grid_id = self.ball_grid_ids.remove(&ball_id);
        self.static_ball_ids.remove(&ball_id);
        self.ball_group_ids.remove(&ball_id);
        self.ball_slim_meta.remove(&ball_id);
        self.combat_states.remove(&ball_id);
        self.target_locks.remove(&ball_id);
        for locks in self.target_locks.values_mut() {
            locks.retain(|l| l.target_id != ball_id);
        }
        self.observer_events.remove(&ball_id);
        self.pending_events
            .push(DestinyEvent::RemoveBall { ball_id });
        if let Some(gid) = old_grid_id {
            if let Some(g) = self.grids.get_mut(&gid) {
                g.members.remove(&ball_id);
            }
            self.queue_event_for_grid_observers(
                gid,
                Some(ball_id),
                &DestinyEvent::RemoveBall { ball_id },
            );
            self.contract_or_delete_grid(gid);
        }
        Some(removed)
    }

    /// Applies incoming damage to `ball_id`, updates its [`CombatState`], emits
    /// `DestinyEvent::OnDamageStateChange` to all observers in the entity's grid,
    /// and if destroyed, queues `DestinyEvent::TerminalExplosion` and removes the ball.
    pub fn apply_damage_to_ball(
        &mut self,
        ball_id: BallId,
        dmg: DamageVector,
    ) -> Option<(f64, bool, Option<Ball>)> {
        let ball_snap = self.balls.get(&ball_id).cloned();
        let grid_id = self.ball_grid_ids.get(&ball_id).copied();
        let cs = self.combat_states.entry(ball_id).or_default();
        let (applied, destroyed) = cs.apply_damage(dmg);
        let s_ratio = if cs.max_shield_hp > 0.0 {
            (cs.shield_hp / cs.max_shield_hp).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let a_ratio = if cs.max_armor_hp > 0.0 {
            (cs.armor_hp / cs.max_armor_hp).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let h_ratio = if cs.max_structure_hp > 0.0 {
            (cs.structure_hp / cs.max_structure_hp).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let ev = DestinyEvent::OnDamageStateChange {
            ball_id,
            shield_ratio: s_ratio,
            armor_ratio: a_ratio,
            structure_ratio: h_ratio,
        };
        let fleet_ev = DestinyEvent::OnFleetDamageStateChange {
            ball_id,
            shield_ratio: s_ratio,
            armor_ratio: a_ratio,
            structure_ratio: h_ratio,
        };
        self.pending_events.push(ev.clone());
        self.pending_events.push(fleet_ev.clone());
        if let Some(gid) = grid_id {
            self.queue_event_for_grid_observers(gid, None, &ev);
            self.queue_event_for_grid_observers(gid, None, &fleet_ev);
        } else {
            let q = self.observer_events.entry(ball_id).or_default();
            q.push(ev);
            q.push(fleet_ev);
        }
        if destroyed {
            let term_ev = DestinyEvent::TerminalExplosion {
                ball_id,
                wreck_ball_id: 0,
            };
            if let Some(gid) = grid_id {
                self.queue_event_for_grid_observers(gid, None, &term_ev);
            } else {
                self.observer_events.entry(ball_id).or_default().push(term_ev);
            }
            let _ = self.remove_ball(ball_id);
        }
        Some((applied, destroyed, ball_snap))
    }

    pub fn add_warp_bubble(&mut self, id: BallId, position: DVec3, radius: f64) {
        self.warp_bubble_mgr
            .add_bubble(WarpBubble::new(id, position, radius));
    }

    pub fn remove_warp_bubble(&mut self, id: BallId) -> Option<WarpBubble> {
        self.warp_bubble_mgr.remove_bubble(id)
    }

    pub fn set_warp_scramble_strength(&mut self, ball_id: BallId, strength: i32) -> bool {
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            cs.warp_scramble_strength = strength;
            true
        } else {
            false
        }
    }

    pub fn set_warp_core_strength(&mut self, ball_id: BallId, strength: i32) -> bool {
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            cs.warp_core_strength = strength;
            true
        } else {
            false
        }
    }

    pub fn is_ball_scrambled(&self, ball_id: BallId) -> bool {
        let is_nullified = self
            .combat_states
            .get(&ball_id)
            .map(|cs| cs.is_interdiction_nullified)
            .unwrap_or(false);
        if let Some(cs) = self.combat_states.get(&ball_id) {
            if cs.is_warp_scrambled() {
                return true;
            }
        }
        if !is_nullified {
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

    /// Deploys an interdiction sphere (e.g. from an interdictor probe or HIC bubble) (`feat:modules.interdiction-sphere`).
    pub fn add_interdiction_sphere(&mut self, id: BallId, position: DVec3, radius: f64) -> bool {
        let ball = Ball {
            id,
            type_id: 22634, // Warp Disruption Probe / Interdiction Sphere
            position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: radius.max(1.0),
            speed_fraction: 0.0,
            mode: MovementMode::Field,
        };
        self.add_ball(ball)
    }

    /// Deploys an anchorable mobile warp disruptor (`feat:modules.mobile-warp-disruptor`).
    pub fn add_mobile_warp_disruptor(&mut self, id: BallId, position: DVec3, radius: f64) -> bool {
        let ball = Ball {
            id,
            type_id: 26888, // Mobile Warp Disruptor
            position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: radius.max(1.0),
            speed_fraction: 0.0,
            mode: MovementMode::Field,
        };
        self.add_ball(ball)
    }

    fn heading_for_ball(&self, ball_id: BallId) -> (DVec3, DVec3) {
        let b = &self.balls[&ball_id];
        let dir = if b.velocity.length_squared() > 1e-4 {
            b.velocity.normalize()
        } else if let MovementMode::GotoDirection { direction } = b.mode {
            if direction.length_squared() > 1e-4 {
                direction.normalize()
            } else {
                DVec3::X
            }
        } else {
            DVec3::X
        };
        (b.position, dir)
    }

    /// Executes a Micro Jump Drive (`100 km`) or Capital Micro Jump Drive (`250 km`) jump forward
    /// along the ship's heading (`feat:modules.micro-jump-drive`, `feat:modules.capital-micro-jump-drive`, `feat:navigation.directional-jump-drive`).
    pub fn micro_jump(&mut self, ball_id: BallId, distance_meters: f64) -> Result<DVec3, String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }
        if self.is_ball_scrambled(ball_id)
            || self
                .combat_states
                .get(&ball_id)
                .is_some_and(|cs| cs.is_mwd_mjd_disabled)
        {
            return Err("WarpScrambled: Cannot activate micro jump drive while warp scrambled".into());
        }
        let ship_type_id = self.balls[&ball_id].type_id;
        let ship_group_id = self.ball_group_ids.get(&ball_id).copied().unwrap_or(0);
        let is_capital = matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594)
            || matches!(
                ship_type_id,
                19720 | 19722 | 19724 | 19726 | 23757 | 23911 | 23915 | 24483
            );
        let eff_dist = if is_capital && (distance_meters - 100_000.0).abs() < 1.0 {
            250_000.0
        } else {
            distance_meters
        };
        if self
            .combat_states
            .get(&ball_id)
            .is_some_and(|cs| cs.is_capital_mjfg)
        {
            let _ = self.micro_jump_field(ball_id, eff_dist, 10_000.0, 50)?;
            return Ok(self.balls[&ball_id].position);
        }
        let (current_pos, dir) = self.heading_for_ball(ball_id);
        let target_pos = current_pos + dir * eff_dist;
        self.set_ball_position(ball_id, target_pos);
        let ev_sfx = DestinyEvent::OnSpecialFX {
            ball_id,
            module_id: None,
            guid: "effects.MicroJumpDrive".to_string(),
            is_active: true,
        };
        self.pending_events.push(ev_sfx.clone());
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            self.queue_event_for_grid_observers(gid, None, &ev_sfx);
        }
        Ok(target_pos)
    }

    /// Executes a Micro Jump Field Generator (`100 km`, `6 km` radius, up to `25` ships) or Capital
    /// Micro Jump Field Generator (`250 km`, `10 km` radius, up to `50` ships) group boosh along the
    /// caster's heading, preserving relative formation offsets (`feat:modules.micro-jump-field-generator`,
    /// `feat:modules.micro-jump-field-boosh-limit`, `feat:modules.mjfg-fleet-jump`).
    /// Excludes static objects, structures, invulnerable/tethered/cloaked ships, and warp-scrambled ships.
    pub fn micro_jump_field(
        &mut self,
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
    ) -> Result<Vec<BallId>, String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }
        if self.is_ball_scrambled(ball_id)
            || self
                .combat_states
                .get(&ball_id)
                .is_some_and(|cs| cs.is_mwd_mjd_disabled)
        {
            return Err(
                "WarpScrambled: Cannot activate micro jump field generator while warp scrambled"
                    .into(),
            );
        }
        let (caster_pos, dir) = self.heading_for_ball(ball_id);
        let delta = dir * distance_meters;
        let max_extra = max_jumped_ships.saturating_sub(1);

        let mut candidates: Vec<(BallId, f64)> = self
            .balls
            .iter()
            .filter_map(|(&other_id, b)| {
                if other_id == ball_id
                    || self.is_static_ball(other_id)
                    || matches!(b.mode, MovementMode::Field | MovementMode::Rigid | MovementMode::Warp { .. })
                {
                    return None;
                }
                let grp = self.ball_group_ids.get(&other_id).copied().unwrap_or(25);
                if matches!(grp, 6 | 7 | 8 | 9 | 10 | 15 | 1404 | 1406 | 1657) {
                    return None;
                }
                if self.is_ball_scrambled(other_id) {
                    return None;
                }
                if let Some(cs) = self.combat_states.get(&other_id) {
                    let mut c = cs.clone();
                    c.sync_action_breaks();
                    if c.is_mwd_mjd_disabled || c.is_cloaked || c.is_invulnerable() {
                        return None;
                    }
                }
                let dist = (b.position - caster_pos).length();
                if dist <= field_radius_meters + 1e-3 {
                    Some((other_id, dist))
                } else {
                    None
                }
            })
            .collect();

        candidates.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        candidates.truncate(max_extra);

        let mut jumped_ids = Vec::with_capacity(1 + candidates.len());
        jumped_ids.push(ball_id);
        for (cid, _) in candidates {
            jumped_ids.push(cid);
        }

        for &jid in &jumped_ids {
            if let Some(pos) = self.balls.get(&jid).map(|b| b.position) {
                self.set_ball_position(jid, pos + delta);
            }
        }

        let ev_sfx = DestinyEvent::OnSpecialFX {
            ball_id,
            module_id: None,
            guid: "effects.MicroJumpField".to_string(),
            is_active: true,
        };
        self.pending_events.push(ev_sfx.clone());
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            self.queue_event_for_grid_observers(gid, None, &ev_sfx);
        }
        Ok(jumped_ids)
    }

    /// Activates a ship cloaking device (`Prototype Cloaking Device I` `11370` or `Covert Ops Cloaking Device II` `11578`),
    /// enforcing the `2,000 m` proximity decloak rule, breaking target locks, starting the 15-minute (`900 s`)
    /// Cloak Stabilization timer (`3118`), and emitting `RemoveBalls` to grid observers (`SP-3`).
    pub fn activate_cloak(
        &mut self,
        ball_id: BallId,
        is_covert: bool,
        targeting_delay_s: f64,
        stabilization_s: f64,
    ) -> Result<(), String> {
        let Some((pos_a, rad_a)) = self.balls.get(&ball_id).map(|b| (b.position, b.radius)) else {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        };
        // Cannot cloak if targeted or within 2,000m surface-to-surface of another ball
        for (&id_b, b) in &self.balls {
            if id_b != ball_id && !matches!(b.mode, MovementMode::Field) {
                let surface_dist = (pos_a - b.position).length() - rad_a - b.radius;
                if surface_dist <= DECLOAK_DISTANCE_METERS {
                    return Err("TooCloseToCloak: Within 2000m of another object".into());
                }
            }
        }
        for locks in self.target_locks.values() {
            if locks.iter().any(|l| l.target_id == ball_id) {
                return Err("CannotCloakWhileTargeted: Ship is currently targeted".into());
            }
        }
        self.clear_target_locks(ball_id);
        let cs = self.combat_states.entry(ball_id).or_default();
        cs.is_cloaked = true;
        cs.is_covert_cloak = is_covert;
        cs.cloak_configured_targeting_delay_s = targeting_delay_s.max(0.0);
        cs.cloak_stabilization_remaining_s = stabilization_s.max(0.0);
        cs.decloak_targeting_delay_remaining_s = 0.0;
        if let Some(meta) = self.ball_slim_meta.get_mut(&ball_id) {
            meta.is_cloaked = true;
        }
        self.emit_event(Some(ball_id), DestinyEvent::CloakBall { ball_id, cloaked: 1 });
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            self.queue_event_for_grid_observers(
                gid,
                Some(ball_id),
                &DestinyEvent::RemoveBalls {
                    ball_ids: vec![ball_id],
                },
            );
        }
        Ok(())
    }

    /// Deactivates a ship's cloak, starting its post-decloak sensor recalibration targeting delay (`560`),
    /// clearing its Cloak Stabilization timer (`3118`), and broadcasting `AddBalls2` to grid observers (`SP-3`).
    pub fn deactivate_cloak(&mut self, ball_id: BallId) -> bool {
        let mut was_cloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            if cs.is_cloaked {
                was_cloaked = true;
                let was_gate_cloak = cs.gate_cloak_remaining_ticks > 0;
                cs.is_cloaked = false;
                cs.is_covert_cloak = false;
                cs.gate_cloak_remaining_ticks = 0;
                cs.cloak_stabilization_remaining_s = 0.0;
                if !was_gate_cloak && cs.cloak_configured_targeting_delay_s > 0.0 {
                    cs.decloak_targeting_delay_remaining_s = cs.cloak_configured_targeting_delay_s;
                }
            }
        }
        if was_cloaked {
            self.emit_event(Some(ball_id), DestinyEvent::CloakBall { ball_id, cloaked: 0 });
            if let Some(meta) = self.ball_slim_meta.get_mut(&ball_id) {
                meta.is_cloaked = false;
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
        was_cloaked
    }

    /// Emits an explicit [`DestinyEvent`] into `pending_events` and queues it for grid observers of `ball_id` (`P-7`).
    pub fn emit_event(&mut self, ball_id: Option<BallId>, ev: DestinyEvent) {
        self.pending_events.push(ev.clone());
        if let Some(bid) = ball_id {
            if let Some(gid) = self.bubble_id_for_ball(bid) {
                self.queue_event_for_grid_observers(gid, None, &ev);
                return;
            }
        }
        let gids: Vec<u64> = self.grids.keys().copied().collect();
        for gid in gids {
            self.queue_event_for_grid_observers(gid, None, &ev);
        }
    }

    /// Sets `ball_id`'s movement toward `point` and emits [`DestinyEvent::GotoPoint`] (`destiny:GotoPoint`).
    pub fn goto_point(&mut self, ball_id: BallId, point: DVec3) -> Result<(), String> {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return Err(format!("Ball {ball_id} not found"));
        };
        let dir = (point - ball.position).normalize_or_zero();
        ball.mode = MovementMode::GotoDirection {
            direction: if dir.length_squared() > 1e-6 { dir } else { DVec3::X },
        };
        if ball.speed_fraction <= 0.0 {
            ball.speed_fraction = 1.0;
        }
        self.emit_event(
            Some(ball_id),
            DestinyEvent::GotoPoint {
                ball_id,
                point: point.to_array(),
            },
        );
        Ok(())
    }

    /// Updates `ball_id`'s radius and emits [`DestinyEvent::SetBallRadius`] (`destiny:SetBallRadius`).
    pub fn set_ball_radius(&mut self, ball_id: BallId, radius: f64) -> bool {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        let new_radius = radius.max(1.0);
        ball.radius = new_radius;
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallRadius {
                ball_id,
                radius: new_radius,
            },
        );
        true
    }

    /// Moves `ball_id` to `new_position` and reconciles its [`GridBubble`] membership.
    pub fn set_ball_position(&mut self, ball_id: BallId, new_position: DVec3) -> bool {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        ball.position = new_position;
        if let Some(wb) = self.warp_bubble_mgr.bubbles.get_mut(&ball_id) {
            wb.position = new_position;
        }
        if !self.is_static_ball(ball_id) {
            let old_gid = self.ball_grid_ids.get(&ball_id).copied();
            let stay_in_old = old_gid
                .and_then(|gid| self.grids.get(&gid))
                .is_some_and(|g| g.contains(new_position));
            if !stay_in_old {
                if let Some(gid) = old_gid {
                    if let Some(g) = self.grids.get_mut(&gid) {
                        g.members.remove(&ball_id);
                    }
                    self.contract_or_delete_grid(gid);
                }
                let new_gid = self.find_or_create_grid_for_position(new_position);
                if let Some(g) = self.grids.get_mut(&new_gid) {
                    g.members.insert(ball_id);
                }
                self.ball_grid_ids.insert(ball_id, new_gid);
            }
        }
        true
    }

    pub fn set_mode(&mut self, ball_id: BallId, mut mode: MovementMode) -> Result<(), String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }

        // Reject warp if scrambled or cloaked with a non-covert cloaking device (`feat:modules.cloak-prototype`, `feat:modules.covert-ops-cloak`)
        if matches!(mode, MovementMode::Warp { .. }) {
            if self.is_ball_scrambled(ball_id) {
                return Err(
                    "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect"
                        .to_string(),
                );
            }
            if let Some(cs) = self.combat_states.get(&ball_id) {
                if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 && !cs.is_covert_cloak {
                    return Err(
                        "CloakedCannotWarp: Cannot warp while cloaked with a non-covert cloaking device"
                            .to_string(),
                    );
                }
            }

            // Check bubble interception along the warp path
            if let MovementMode::Warp {
                origin,
                ref mut destination,
                ref mut total_distance,
                ..
            } = mode
            {
                let is_nullified = self
                    .combat_states
                    .get(&ball_id)
                    .map(|cs| cs.is_interdiction_nullified)
                    .unwrap_or(false);
                if !is_nullified {
                    if let Some((_bid, intercept_pos)) =
                        self.warp_bubble_mgr.check_interception(origin, *destination)
                    {
                        *destination = intercept_pos;
                        *total_distance = (*destination - origin).length();
                    }
                }
                if let Some(locks) = self.target_locks.remove(&ball_id) {
                    for l in locks {
                        self.pending_events.push(DestinyEvent::OnTarget {
                            mode: "lost".to_string(),
                            source_id: ball_id,
                            target_id: l.target_id,
                        });
                    }
                }
            }
        }

        let mut uncloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
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
            if let Some(meta) = self.ball_slim_meta.get_mut(&ball_id) {
                meta.is_cloaked = false;
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

        let mut emitted_event: Option<DestinyEvent> = None;
        let mut extra_events: Vec<DestinyEvent> = Vec::new();
        let ball = self.balls.get_mut(&ball_id).unwrap();
        match &mode {
            MovementMode::GotoDirection { direction } => {
                let ev = DestinyEvent::GotoDirection {
                    ball_id,
                    direction: direction.to_array(),
                };
                self.pending_events.push(ev.clone());
                emitted_event = Some(ev);
            }
            MovementMode::Follow { target_id, range } => {
                let ev = DestinyEvent::Follow {
                    ball_id,
                    target_id: *target_id,
                    range: *range,
                };
                self.pending_events.push(ev.clone());
                emitted_event = Some(ev);
            }
            MovementMode::Orbit { target_id, range } => {
                let ev = DestinyEvent::Orbit {
                    ball_id,
                    target_id: *target_id,
                    range: *range,
                };
                self.pending_events.push(ev.clone());
                emitted_event = Some(ev);
            }
            MovementMode::Stop => {
                ball.speed_fraction = 0.0;
                let ev = DestinyEvent::Stop { ball_id };
                self.pending_events.push(ev.clone());
                emitted_event = Some(ev);
            }
            MovementMode::Warp {
                destination,
                warp_speed_au_s,
                ..
            } => {
                ball.speed_fraction = 1.0;
                let ev = DestinyEvent::WarpTo {
                    ball_id,
                    destination: destination.to_array(),
                    warp_speed_au_s: *warp_speed_au_s,
                };
                self.pending_events.push(ev.clone());
                emitted_event = Some(ev);

                // Warp tunnel visual handoff events
                let ev_act = DestinyEvent::OnActivatingWarp { ball_id };
                let ev_sfx = DestinyEvent::OnSpecialFX {
                    ball_id,
                    module_id: None,
                    guid: "effects.Warping".to_string(),
                    is_active: true,
                };
                extra_events.push(ev_act);
                extra_events.push(ev_sfx);
            }
            _ => {}
        }
        ball.mode = mode;
        if let Some(ev) = emitted_event {
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                self.queue_event_for_grid_observers(gid, None, &ev);
            }
        }
        // Warp tunnel visuals (`OnActivatingWarp`, `OnSpecialFX`) reach grid observers too.
        for ev in extra_events {
            self.emit_event(Some(ball_id), ev);
        }
        Ok(())
    }

    pub fn set_speed_fraction(&mut self, ball_id: BallId, fraction: f64) -> bool {
        let clamped = fraction.clamp(0.0, 1.0);
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        ball.speed_fraction = clamped;
        if clamped > 0.0 {
            let mut uncloaked = false;
            if let Some(cs) = self.combat_states.get_mut(&ball_id) {
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
                if let Some(meta) = self.ball_slim_meta.get_mut(&ball_id) {
                    meta.is_cloaked = false;
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
        }
        let ev = DestinyEvent::SetSpeedFraction {
            ball_id,
            fraction: clamped,
        };
        self.pending_events.push(ev.clone());
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            self.queue_event_for_grid_observers(gid, None, &ev);
        }
        true
    }

    pub fn add_target_lock(&mut self, source_id: BallId, lock: TargetLock) {
        let mut uncloaked = false;
        let mut source_blocked = false;
        if let Some(cs) = self.combat_states.get_mut(&source_id) {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.warp_exit_invulnerable_remaining_ticks = 0;
            cs.sync_invuln_with_undock_timer = false;
            if cs.gate_cloak_remaining_ticks > 0 {
                cs.gate_cloak_remaining_ticks = 0;
                cs.is_cloaked = false;
                uncloaked = true;
            }
            if cs.is_cloaked || cs.decloak_targeting_delay_remaining_s > 0.0 {
                source_blocked = true;
            }
        }
        if uncloaked {
            if let Some(meta) = self.ball_slim_meta.get_mut(&source_id) {
                meta.is_cloaked = false;
            }
            if let (Some(&gid), Some(b)) = (
                self.ball_grid_ids.get(&source_id),
                self.balls.get(&source_id).cloned(),
            ) {
                self.queue_event_for_grid_observers(
                    gid,
                    Some(source_id),
                    &DestinyEvent::AddBalls2 { balls: vec![b] },
                );
            }
        }
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
            return;
        }
        self.target_locks.entry(source_id).or_default().push(lock);
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
