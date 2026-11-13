//! `Ballpark` spatial grid management, ball lifecycle, warp bubbles, cloaking, MJD/MJFG, and target locking.

use crate::{
    Ball, BallId, Ballpark, DamageVector, DestinyEvent, GridBubble, MovementMode, SlimItemMeta,
    WarpBubble, WarpBubbleMgr, WarpPhase, BUBBLE_RADIUS_METERS, BUBBLE_TRANSITION_THRESHOLD_METERS,
    DECLOAK_DISTANCE_METERS, GRID_WALL_EXPANSION_THRESHOLD_METERS, MAX_GRID_EXTENT_METERS,
};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

#[path = "ballpark_targeting.rs"]
mod ballpark_targeting;
#[path = "ballpark_navigation.rs"]
mod ballpark_navigation;

fn default_constellation_id() -> u32 {
    20_000_020
}

fn is_capital_ship_ball(ship_type_id: u32, ship_group_id: u32) -> bool {
    static CACHE: std::sync::OnceLock<(
        std::collections::HashSet<u32>,
        std::collections::HashMap<u32, u32>,
    )> = std::sync::OnceLock::new();
    let (cap_groups, type_to_group) = CACHE.get_or_init(|| {
        let mut cap_groups = std::collections::HashSet::from([
            30, 485, 513, 547, 659, 883, 902, 1538, 4594, 5120,
        ]);
        let mut type_to_group = std::collections::HashMap::new();
        if let Ok(sde) = eve_sde::SdeRegistry::open(eve_sde::SdeRegistry::default_db_path()) {
            for (&tid, ty) in &sde.types {
                if ty.category_id == 6 {
                    type_to_group.insert(tid, ty.group_id);
                    let rig_size = ty.default_attributes.get(&1547).copied().unwrap_or(0.0);
                    let is_cap_size = ty.default_attributes.get(&1785).copied().unwrap_or(0.0);
                    if rig_size >= 4.0 || is_cap_size > 0.0 {
                        cap_groups.insert(ty.group_id);
                    }
                }
            }
        }
        (cap_groups, type_to_group)
    });
    let grp = if ship_group_id != 0 {
        ship_group_id
    } else {
        type_to_group.get(&ship_type_id).copied().unwrap_or(0)
    };
    cap_groups.contains(&grp)
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
            non_free_ball_ids: BTreeSet::new(),
            ball_group_ids: BTreeMap::new(),
            ball_slim_meta: BTreeMap::new(),
            grids: BTreeMap::new(),
            ball_grid_ids: BTreeMap::new(),
            next_grid_id: 1,
            observer_events: BTreeMap::new(),
            target_locks: BTreeMap::new(),
            warp_bubble_mgr: WarpBubbleMgr::default(),
            mushroom_states: BTreeMap::new(),
            troll_states: BTreeMap::new(),
            miniball_states: BTreeMap::new(),
            formation_states: BTreeMap::new(),
            ball_harmonics: BTreeMap::new(),
            ball_interactive_overrides: BTreeMap::new(),
            ball_wormhole_stages: BTreeMap::new(),
            active_wubble_multipliers: BTreeMap::new(),
            sim_unix_secs: 0,
            registered_observers: None,
            static_epoch: 0,
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
            ..Default::default()
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
        self.static_epoch = self.static_epoch.wrapping_add(1);
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

    /// Replaces the slim item of a ball that is already in space and tells every observer on its
    /// grid with `OnSlimItemChange(itemID, foo.SlimItem)`, as evemu_Crucible
    /// `DestinyManager::UpdateNewShip` / `UpdateOldShip` do (`DestinyManager.cpp:2725-2770`).
    /// Returns `false` (and emits nothing) when the ball is unknown or the slim item is unchanged.
    pub fn change_slim_item(&mut self, ball_id: BallId, meta: SlimItemMeta) -> bool {
        if !self.balls.contains_key(&ball_id) || self.ball_slim_meta.get(&ball_id) == Some(&meta) {
            return false;
        }
        self.set_ball_slim_meta(ball_id, meta);
        self.emit_event(Some(ball_id), DestinyEvent::OnSlimItemChange { ball_id });
        true
    }

    /// Sets or updates `wormholeAge` and `wormholeSize` on a wormhole ball's slim item,
    /// emitting `OnSlimItemChange` when the stage values change.
    pub fn set_ball_wormhole_stages(
        &mut self,
        ball_id: BallId,
        wormhole_age: i64,
        wormhole_size: f64,
    ) -> bool {
        let next = (wormhole_age, wormhole_size);
        if self.ball_wormhole_stages.get(&ball_id) == Some(&next) {
            return false;
        }
        self.ball_wormhole_stages.insert(ball_id, next);
        if self.balls.contains_key(&ball_id) {
            self.emit_event(Some(ball_id), DestinyEvent::OnSlimItemChange { ball_id });
        }
        true
    }

    /// Returns `true` if `ball_id` is a system-wide static celestial/gate/station (`MovementMode::Rigid` / `static_ball_ids`).
    pub fn is_static_ball(&self, ball_id: BallId) -> bool {
        self.static_ball_ids.contains(&ball_id)
            || self
                .balls
                .get(&ball_id)
                .is_some_and(|b| matches!(b.mode, MovementMode::Rigid))
    }

    /// Returns `true` if `ball_id` is an immobile static celestial/station or an anchored structure (`category_id` in structure/station/deployable/orbital categories, or `max_velocity <= 0.0` when slim metadata is absent).
    pub fn is_immobile_structure_or_static(&self, ball_id: BallId) -> bool {
        if self.is_static_ball(ball_id) {
            return true;
        }
        if let Some(m) = self.ball_slim_meta.get(&ball_id) {
            return matches!(m.category_id, 2 | 3 | 22 | 23 | 40 | 46 | 65);
        }
        self.balls
            .get(&ball_id)
            .is_some_and(|b| b.max_velocity <= 0.0)
    }

    /// Returns `true` if any dynamic (non-static) ball is currently in the `Ballpark`.
    pub fn has_dynamic_balls(&self) -> bool {
        self.balls.keys().any(|&id| !self.is_static_ball(id))
    }

    /// Computes the clamped `(min_bounds, max_bounds)` that a newly created [`GridBubble`]
    /// centered at `pos` would occupy without mutating `self.grids` (`Grid.md:13-15`).
    pub fn prospective_grid_bounds(&self, pos: DVec3) -> (DVec3, DVec3) {
        let r = self.bubble_radius_meters;
        let mut min_b = pos - DVec3::splat(r);
        let mut max_b = pos + DVec3::splat(r);

        // Clamp against any pre-existing grids so adjacent grids never overlap or merge (`Grid.md:15`).
        for existing in self.grids.values() {
            if existing.overlaps_bounds(min_b, max_b) {
                if pos.x > existing.max_bounds.x {
                    min_b.x = min_b.x.max(existing.max_bounds.x);
                } else if pos.x < existing.min_bounds.x {
                    max_b.x = max_b.x.min(existing.min_bounds.x);
                } else if pos.y > existing.max_bounds.y {
                    min_b.y = min_b.y.max(existing.max_bounds.y);
                } else if pos.y < existing.min_bounds.y {
                    max_b.y = max_b.y.min(existing.min_bounds.y);
                } else if pos.z > existing.max_bounds.z {
                    min_b.z = min_b.z.max(existing.max_bounds.z);
                } else if pos.z < existing.min_bounds.z {
                    max_b.z = max_b.z.min(existing.min_bounds.z);
                }
            }
        }
        min_b = min_b.min(pos);
        max_b = max_b.max(pos);
        (min_b, max_b)
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
        let (min_b, max_b) = self.prospective_grid_bounds(pos);

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
        let Some(grid) = self.grids.get(&grid_id) else {
            return;
        };
        if grid.members.is_empty() {
            self.grids.remove(&grid_id);
            return;
        }
        let base_min = grid.center - DVec3::splat(grid.base_radius);
        let base_max = grid.center + DVec3::splat(grid.base_radius);
        if grid.min_bounds.x >= base_min.x
            && grid.min_bounds.y >= base_min.y
            && grid.min_bounds.z >= base_min.z
            && grid.max_bounds.x <= base_max.x
            && grid.max_bounds.y <= base_max.y
            && grid.max_bounds.z <= base_max.z
        {
            return;
        }
        let member_positions: Vec<DVec3> = grid
            .members
            .iter()
            .filter_map(|id| self.balls.get(id).map(|b| b.position))
            .collect();
        if let Some(g) = self.grids.get_mut(&grid_id) {
            let thresh = DVec3::splat(GRID_WALL_EXPANSION_THRESHOLD_METERS);
            let max_ext = MAX_GRID_EXTENT_METERS.max(g.base_radius);
            let max_min = g.center - DVec3::splat(max_ext);
            let max_max = g.center + DVec3::splat(max_ext);
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

    /// Enables or disables registered-observer filtering (`#868`).
    /// When enabled, only registered observer balls (e.g. active client sessions) accumulate
    /// per-observer `DestinyEvent` queues; NPCs, drones, and wrecks skip queue allocation.
    pub fn set_observer_filtering(&mut self, enabled: bool) {
        if enabled {
            self.registered_observers.get_or_insert_with(BTreeSet::new);
        } else {
            self.registered_observers = None;
        }
    }

    /// Registers `ball_id` as an active observer and enables registered-observer filtering.
    pub fn register_observer(&mut self, ball_id: BallId) {
        self.registered_observers
            .get_or_insert_with(BTreeSet::new)
            .insert(ball_id);
    }

    /// Unregisters `ball_id` from active observer filtering (if enabled).
    pub fn unregister_observer(&mut self, ball_id: BallId) {
        if let Some(ref mut set) = self.registered_observers {
            set.remove(&ball_id);
        }
    }

    /// Returns `true` when `ball_id` should accumulate per-observer `DestinyEvent` queues.
    #[inline]
    pub fn is_registered_observer(&self, ball_id: BallId) -> bool {
        self.registered_observers
            .as_ref()
            .is_none_or(|set| set.contains(&ball_id))
    }

    #[inline]
    pub(crate) fn push_observer_event(&mut self, observer_ball_id: BallId, event: DestinyEvent) {
        if self.is_registered_observer(observer_ball_id) {
            self.observer_events
                .entry(observer_ball_id)
                .or_default()
                .push(event);
        }
    }

    pub(crate) fn queue_event_for_grid_observers(
        &mut self,
        grid_id: u64,
        exclude_ball_id: Option<BallId>,
        event: &DestinyEvent,
    ) {
        let Some(grid) = self.grids.get(&grid_id) else {
            return;
        };
        let observer_events = &mut self.observer_events;
        let registered_observers = &self.registered_observers;
        for &obs_id in &grid.members {
            if Some(obs_id) == exclude_ball_id {
                continue;
            }
            if !registered_observers
                .as_ref()
                .is_none_or(|set| set.contains(&obs_id))
            {
                continue;
            }
            observer_events
                .entry(obs_id)
                .or_default()
                .push(event.clone());
        }
    }

    /// Queues `event` for every active observer across all currently populated [`GridBubble`]s
    /// (used when system-wide `Rigid` static balls such as belt asteroids or structures are
    /// dynamically spawned, respawned, or removed while pilots are in space).
    pub fn queue_event_for_all_grid_observers(
        &mut self,
        exclude_ball_id: Option<BallId>,
        event: &DestinyEvent,
    ) {
        let grid_ids: Vec<u64> = self.grids.keys().copied().collect();
        for gid in grid_ids {
            self.queue_event_for_grid_observers(gid, exclude_ball_id, event);
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
        self.balls.insert(ball_id, ball.clone());
        let is_cloaked = {
            let cs = self.combat_states.entry(ball_id).or_default();
            cs.was_cloaked_last_tick = cs.is_cloaked;
            cs.is_cloaked
        };
        if !is_cloaked {
            self.pending_events
                .push(DestinyEvent::AddBall { ball: ball.clone() });
        }
        if self.is_registered_observer(ball_id) {
            self.observer_events.insert(ball_id, Vec::new());
        }
        let grid_id = self.find_or_create_grid_for_position(ball_pos);
        if let Some(g) = self.grids.get_mut(&grid_id) {
            g.members.insert(ball_id);
        }
        self.ball_grid_ids.insert(ball_id, grid_id);
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
    ///
    /// DERIVED: V24.01 `eve/client/script/remote/michelle.pyj` (`Park.FlushState` & `Park.RealFlushState`)
    /// only checks `if state[0][1][0] == 'SetState':` at index `0` of the `DoDestinyUpdate` action list
    /// and explicitly skips `'SetState'` at any non-zero index (`if funcName == 'SetState': continue`).
    /// Therefore `SetState` must always be placed at index `0` of the observer's event queue.
    pub fn queue_set_state(&mut self, observer_ball_id: BallId) -> bool {
        if !self.balls.contains_key(&observer_ball_id) {
            return false;
        }
        if let Some(ref mut set) = self.registered_observers {
            set.insert(observer_ball_id);
        }
        let events = self.observer_events.entry(observer_ball_id).or_default();
        events.retain(|e| {
            !matches!(
                e,
                DestinyEvent::SetState {
                    observer_ball_id: id
                } if *id == observer_ball_id
            )
        });
        events.insert(0, DestinyEvent::SetState { observer_ball_id });
        true
    }

    pub fn remove_ball(&mut self, ball_id: BallId) -> Option<Ball> {
        let Some(removed) = self.balls.remove(&ball_id) else {
            self.combat_states.remove(&ball_id);
            return None;
        };
        self.warp_bubble_mgr.remove_bubble(ball_id);
        let old_grid_id = self.ball_grid_ids.remove(&ball_id);
        let was_static = self.static_ball_ids.remove(&ball_id)
            || matches!(removed.mode, MovementMode::Rigid);
        if was_static {
            self.static_epoch = self.static_epoch.wrapping_add(1);
        }
        if let Some(ref mut set) = self.registered_observers {
            set.remove(&ball_id);
        }
        self.non_free_ball_ids.remove(&ball_id);
        self.ball_group_ids.remove(&ball_id);
        self.ball_slim_meta.remove(&ball_id);
        self.combat_states.remove(&ball_id);
        self.mushroom_states.remove(&ball_id);
        self.troll_states.remove(&ball_id);
        self.miniball_states.remove(&ball_id);
        self.formation_states.remove(&ball_id);
        self.ball_harmonics.remove(&ball_id);
        self.ball_interactive_overrides.remove(&ball_id);
        self.active_wubble_multipliers.remove(&ball_id);
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
        } else if was_static {
            self.queue_event_for_all_grid_observers(
                Some(ball_id),
                &DestinyEvent::RemoveBall { ball_id },
            );
        }
        Some(removed)
    }

    /// Recomputes `ball_id`'s effective attributes in-place, returning `Some(new_max_velocity)`
    /// when a non-static ball's `max_velocity` changed so callers can emit `SetMaxSpeed`.
    pub(crate) fn recompute_ball_attributes_quiet(&mut self, ball_id: BallId) -> Option<f64> {
        let is_static = self.is_static_ball(ball_id);
        match (
            self.balls.get_mut(&ball_id),
            self.combat_states.get_mut(&ball_id),
        ) {
            (Some(ball), Some(cs)) => {
                let prev_max_v = ball.max_velocity;
                cs.recompute_attributes(ball);
                let next_max_v = ball.max_velocity;
                (!is_static && (next_max_v - prev_max_v).abs() > 1e-6).then_some(next_max_v)
            }
            (None, Some(cs)) => {
                cs.recompute_combat_attributes();
                None
            }
            _ => None,
        }
    }

    /// Declaratively recomputes a ball's effective kinematics and combat attributes
    /// from its immutable baselines and currently active modifiers (`#584`), emitting
    /// [`DestinyEvent::SetMaxSpeed`] whenever a non-static ball's `max_velocity` changes (`#741`).
    pub fn recompute_ball_attributes(&mut self, ball_id: BallId) {
        if let Some(next_max_v) = self.recompute_ball_attributes_quiet(ball_id) {
            self.set_ball_max_velocity(ball_id, next_max_v);
        }
    }

    /// Updates `ball_id`'s `max_velocity` in-place and emits [`DestinyEvent::SetMaxSpeed`]
    /// without removing the ball, preserving `CombatState` and all target locks (`#513`).
    pub fn set_ball_max_velocity(&mut self, ball_id: BallId, max_velocity: f64) -> bool {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        let clamped = max_velocity.max(0.0);
        ball.max_velocity = clamped;
        self.emit_event(Some(ball_id), DestinyEvent::SetMaxSpeed { ball_id, max_speed: clamped });
        true
    }

    /// Sets or clears a declarative hard `max_velocity_cap` (e.g. Orbital Skyhook silo link) on
    /// `ball_id`'s `CombatState` and recomputes effective `max_velocity`, emitting
    /// [`DestinyEvent::SetMaxSpeed`] when the effective speed changes (`#584`).
    pub fn set_ball_max_velocity_cap(&mut self, ball_id: BallId, cap: Option<f64>) -> bool {
        let Some(prev_vel) = self.balls.get(&ball_id).map(|b| b.max_velocity) else {
            return false;
        };
        let new_vel = {
            let ball = self.balls.get_mut(&ball_id).unwrap();
            let cs = self.combat_states.entry(ball_id).or_default();
            cs.capture_missing_baselines(ball);
            cs.max_velocity_cap = cap.map(|v| v.max(0.0));
            cs.recompute_attributes(ball);
            ball.max_velocity
        };
        if (new_vel - prev_vel).abs() > 1e-6 {
            self.emit_event(Some(ball_id), DestinyEvent::SetMaxSpeed { ball_id, max_speed: new_vel });
        }
        true
    }

    /// Emits `OnDamageStateChange` (and `TerminalExplosion` + ball removal when `destroyed`).
    pub fn emit_damage_and_destruction_events(
        &mut self,
        ball_id: BallId,
        grid_id: Option<u64>,
        s_ratio: f64,
        a_ratio: f64,
        h_ratio: f64,
        destroyed: bool,
    ) {
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
                destruction_effect_id: None,
            };
            if let Some(gid) = grid_id {
                self.queue_event_for_grid_observers(gid, None, &term_ev);
            } else {
                self.observer_events.entry(ball_id).or_default().push(term_ev);
            }
            let _ = self.remove_ball_preserving_observer_events(ball_id);
        }
    }

    /// Applies incoming damage to `ball_id`, updates its [`CombatState`], emits
    /// `DestinyEvent::OnDamageStateChange` to all observers in the entity's grid,
    /// and if destroyed, queues `DestinyEvent::TerminalExplosion` and removes the ball.
    pub fn apply_damage_to_ball(
        &mut self,
        ball_id: BallId,
        dmg: DamageVector,
    ) -> Option<(f64, bool, Option<Ball>)> {
        let ball_snap = self.balls.get(&ball_id).cloned()?;
        let grid_id = self.ball_grid_ids.get(&ball_id).copied();
        let (applied, destroyed, s_ratio, a_ratio, h_ratio) = {
            let cs = self.combat_states.get_mut(&ball_id)?;
            let (applied, destroyed) = cs.apply_damage(dmg);
            let s = if cs.max_shield_hp > 0.0 { (cs.shield_hp / cs.max_shield_hp).clamp(0.0, 1.0) } else { 0.0 };
            let a = if cs.max_armor_hp > 0.0 { (cs.armor_hp / cs.max_armor_hp).clamp(0.0, 1.0) } else { 0.0 };
            let h = if cs.max_structure_hp > 0.0 { (cs.structure_hp / cs.max_structure_hp).clamp(0.0, 1.0) } else { 0.0 };
            (applied, destroyed, s, a, h)
        };
        self.emit_damage_and_destruction_events(ball_id, grid_id, s_ratio, a_ratio, h_ratio, destroyed);
        Some((applied, destroyed, Some(ball_snap)))
    }

    /// Applies resistance-ignoring true damage (e.g., Breacher Pod DoT, Arena hazard boundary) to `ball_id`
    /// while enforcing tether/gate-cloak/invulnerability guards, emitting `OnDamageStateChange` /
    /// `TerminalExplosion`, and preserving `observer_events` upon destruction (`#585`).
    pub fn apply_true_damage_to_ball(
        &mut self,
        ball_id: BallId,
        amount: f64,
    ) -> Option<(f64, bool, Option<Ball>)> {
        let ball_snap = self.balls.get(&ball_id).cloned()?;
        let grid_id = self.ball_grid_ids.get(&ball_id).copied();
        let (applied, destroyed, s_ratio, a_ratio, h_ratio) = {
            let cs = self.combat_states.get_mut(&ball_id)?;
            let (applied, destroyed) = cs.apply_true_damage(amount);
            let s = if cs.max_shield_hp > 0.0 { (cs.shield_hp / cs.max_shield_hp).clamp(0.0, 1.0) } else { 0.0 };
            let a = if cs.max_armor_hp > 0.0 { (cs.armor_hp / cs.max_armor_hp).clamp(0.0, 1.0) } else { 0.0 };
            let h = if cs.max_structure_hp > 0.0 { (cs.structure_hp / cs.max_structure_hp).clamp(0.0, 1.0) } else { 0.0 };
            (applied, destroyed, s, a, h)
        };
        self.emit_damage_and_destruction_events(ball_id, grid_id, s_ratio, a_ratio, h_ratio, destroyed);
        Some((applied, destroyed, Some(ball_snap)))
    }

    /// Applies unresisted direct structure/hull HP damage to `ball_id`, leaving shield and armor
    /// completely untouched (`feat:navigation.gravity-hazard-safe-zones`).
    pub fn apply_direct_structure_damage_to_ball(
        &mut self,
        ball_id: BallId,
        raw_hull_damage: f64,
    ) -> Option<(f64, bool, Option<Ball>)> {
        let ball_snap = self.balls.get(&ball_id).cloned()?;
        let grid_id = self.ball_grid_ids.get(&ball_id).copied();
        let (applied, destroyed, s_ratio, a_ratio, h_ratio) = {
            let cs = self.combat_states.get_mut(&ball_id)?;
            if cs.is_tethered
                || cs.gate_cloak_remaining_ticks > 0
                || cs.undock_invulnerable_remaining_ticks > 0
                || cs.warp_exit_invulnerable_remaining_ticks > 0
            {
                return Some((0.0, false, Some(ball_snap)));
            }
            let dmg = raw_hull_damage.max(0.0);
            let applied = dmg.min(cs.structure_hp.max(0.0));
            cs.structure_hp = (cs.structure_hp - dmg).max(0.0);
            let destroyed = cs.structure_hp <= 1e-6;
            let s = if cs.max_shield_hp > 0.0 { (cs.shield_hp / cs.max_shield_hp).clamp(0.0, 1.0) } else { 0.0 };
            let a = if cs.max_armor_hp > 0.0 { (cs.armor_hp / cs.max_armor_hp).clamp(0.0, 1.0) } else { 0.0 };
            let h = if cs.max_structure_hp > 0.0 { (cs.structure_hp / cs.max_structure_hp).clamp(0.0, 1.0) } else { 0.0 };
            (applied, destroyed, s, a, h)
        };
        self.emit_damage_and_destruction_events(ball_id, grid_id, s_ratio, a_ratio, h_ratio, destroyed);
        Some((applied, destroyed, Some(ball_snap)))
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

    /// Deploys an interdiction sphere (e.g. from an interdictor probe or HIC bubble) (`feat:modules.interdiction-sphere`).
    pub fn add_interdiction_sphere(&mut self, id: BallId, position: DVec3, radius: f64) -> bool {
        let r = radius.max(1.0);
        if let Some(existing) = self.balls.get_mut(&id) {
            existing.position = position;
            existing.radius = r;
            existing.mode = MovementMode::Field;
            self.warp_bubble_mgr
                .add_bubble(WarpBubble::new(id, position, r));
            return true;
        }
        let ball = Ball {
            id,
            type_id: 22634, // Warp Disruption Probe / Interdiction Sphere
            position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: r,
            speed_fraction: 0.0,
            mode: MovementMode::Field,
        };
        self.add_ball(ball)
    }

    /// Deploys an anchorable mobile warp disruptor (`feat:modules.mobile-warp-disruptor`).
    pub fn add_mobile_warp_disruptor(&mut self, id: BallId, position: DVec3, radius: f64) -> bool {
        let r = radius.max(1.0);
        if let Some(existing) = self.balls.get_mut(&id) {
            existing.position = position;
            existing.radius = r;
            existing.mode = MovementMode::Field;
            self.warp_bubble_mgr
                .add_bubble(WarpBubble::new(id, position, r));
            return true;
        }
        let ball = Ball {
            id,
            type_id: 26888, // Mobile Warp Disruptor
            position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: r,
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
        if matches!(
            self.balls[&ball_id].mode,
            MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning
        ) {
            return Err("CantActivateInWarp: Cannot micro jump while in warp".into());
        }
        if self.is_immobile_structure_or_static(ball_id) {
            return Err("StructureCannotNavigate: Static or Upwell structure balls cannot micro jump".into());
        }
        if self
            .combat_states
            .get(&ball_id)
            .is_some_and(|cs| cs.is_mwd_mjd_disabled)
        {
            return Err("WarpScrambled: Cannot activate micro jump drive while warp scrambled".into());
        }
        let ship_type_id = self.balls[&ball_id].type_id;
        let ship_group_id = self.ball_group_ids.get(&ball_id).copied().unwrap_or(0);
        let is_capital = is_capital_ship_ball(ship_type_id, ship_group_id);
        let eff_dist = if is_capital && (distance_meters - 100_000.0).abs() < 1.0 {
            250_000.0
        } else {
            distance_meters
        };
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
        self.micro_jump_field_with_capitals(
            ball_id,
            distance_meters,
            field_radius_meters,
            max_jumped_ships,
            0,
        )
    }

    pub fn micro_jump_field_with_capitals(
        &mut self,
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
        max_jumped_capitals: usize,
    ) -> Result<Vec<BallId>, String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }
        if matches!(
            self.balls[&ball_id].mode,
            MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning
        ) {
            return Err("CantActivateInWarp: Cannot activate micro jump field while in warp".into());
        }
        if self.is_immobile_structure_or_static(ball_id) {
            return Err("StructureCannotNavigate: Static or Upwell structure balls cannot activate micro jump field".into());
        }
        if self
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

        let mut candidates: Vec<(BallId, f64, bool)> = self
            .balls
            .iter()
            .filter_map(|(&other_id, b)| {
                if other_id == ball_id
                    || self.is_immobile_structure_or_static(other_id)
                    || matches!(b.mode, MovementMode::Field | MovementMode::Rigid | MovementMode::Warp { .. })
                {
                    return None;
                }
                let grp = self.ball_group_ids.get(&other_id).copied().unwrap_or(25);
                if matches!(grp, 6 | 7 | 8 | 9 | 10 | 15 | 1404 | 1406 | 1657 | 1971 | 1980 | 1981) {
                    return None;
                }
                let is_cap = is_capital_ship_ball(b.type_id, grp);
                if is_cap && max_jumped_capitals == 0 {
                    return None;
                }
                let is_marauder = grp == 900 || matches!(b.type_id, 28659 | 28661 | 28665 | 28710);
                let is_bastioned_marauder = is_marauder
                    && self
                        .combat_states
                        .get(&other_id)
                        .map(|cs| cs.disallow_assistance)
                        .unwrap_or(false);
                if let Some(cs) = self.combat_states.get(&other_id) {
                    let mut c = cs.clone();
                    c.sync_action_breaks();
                    if (c.is_mwd_mjd_disabled && !is_bastioned_marauder)
                        || c.is_cloaked
                        || c.is_invulnerable()
                    {
                        return None;
                    }
                }
                let dist = (b.position - caster_pos).length();
                if dist <= field_radius_meters + 1e-3 {
                    Some((other_id, dist, is_cap))
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

        let mut jumped_ids = Vec::with_capacity(1 + max_extra.min(candidates.len()));
        jumped_ids.push(ball_id);
        let mut cap_count = 0usize;
        for (cid, _, is_cap) in candidates {
            if jumped_ids.len() > max_extra {
                break;
            }
            if is_cap {
                if cap_count >= max_jumped_capitals {
                    continue;
                }
                cap_count += 1;
            }
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
        let ball_snapshot = self.balls.get(&ball_id).cloned();
        let cs = self.combat_states.entry(ball_id).or_default();
        if let Some(ref b) = ball_snapshot {
            cs.capture_missing_baselines(b);
        }
        cs.is_cloaked = true;
        cs.was_cloaked_last_tick = true;
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
        let mut was_gate_cloak = false;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            if cs.is_cloaked || cs.gate_cloak_remaining_ticks > 0 {
                was_cloaked = true;
                was_gate_cloak = cs.gate_cloak_remaining_ticks > 0;
                cs.is_cloaked = false;
                cs.was_cloaked_last_tick = false;
                cs.is_covert_cloak = false;
                cs.gate_cloak_remaining_ticks = 0;
                cs.cloak_stabilization_remaining_s = 0.0;
                cs.cloak_velocity_multiplier = None;
                if !was_gate_cloak && cs.cloak_configured_targeting_delay_s > 0.0 {
                    cs.decloak_targeting_delay_remaining_s = cs.cloak_configured_targeting_delay_s;
                }
            }
        }
        if was_cloaked {
            let speed_change = self.recompute_ball_attributes_quiet(ball_id);
            self.emit_event(Some(ball_id), DestinyEvent::CloakBall { ball_id, cloaked: 0 });
            if was_gate_cloak {
                self.emit_event(
                    Some(ball_id),
                    DestinyEvent::OnSpecialFX {
                        ball_id,
                        module_id: None,
                        guid: "effects.Cloak".to_string(),
                        is_active: false,
                    },
                );
            }
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
            if let Some(next_max_v) = speed_change {
                self.set_ball_max_velocity(ball_id, next_max_v);
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

    /// Updates `ball_id`'s radius and emits [`DestinyEvent::SetBallRadius`] (`destiny:SetBallRadius`).
    pub fn set_ball_radius(&mut self, ball_id: BallId, radius: f64) -> bool {
        if !radius.is_finite() || radius <= 0.0 {
            return false;
        }
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
        if !new_position.is_finite() {
            return false;
        }
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
            let pos_ev = DestinyEvent::SetBallPosition {
                ball_id,
                position: new_position.to_array(),
            };
            self.pending_events.push(pos_ev.clone());
            if self.is_static_ball(ball_id) {
                self.static_epoch = self.static_epoch.wrapping_add(1);
            }
            if stay_in_old {
                if let Some(gid) = old_gid {
                    self.queue_event_for_grid_observers(gid, None, &pos_ev);
                }
            } else {
                self.push_observer_event(ball_id, pos_ev);
                let old_members: Vec<BallId> = old_gid
                    .and_then(|gid| self.grids.get(&gid))
                    .map(|g| {
                        g.members
                            .iter()
                            .copied()
                            .filter(|&id| id != ball_id && !self.is_static_ball(id))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(gid) = old_gid {
                    if let Some(g) = self.grids.get_mut(&gid) {
                        g.members.remove(&ball_id);
                    }
                    self.contract_or_delete_grid(gid);
                }
                let new_gid = self.find_or_create_grid_for_position(new_position);
                let new_members: Vec<BallId> = self
                    .grids
                    .get(&new_gid)
                    .map(|g| {
                        g.members
                            .iter()
                            .copied()
                            .filter(|&id| id != ball_id && !self.is_static_ball(id))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(g) = self.grids.get_mut(&new_gid) {
                    g.members.insert(ball_id);
                }
                self.ball_grid_ids.insert(ball_id, new_gid);

                if old_gid != Some(new_gid) {
                    let is_cloaked = self
                        .combat_states
                        .get(&ball_id)
                        .is_some_and(|c| c.is_cloaked);
                    if !old_members.is_empty() {
                        for &other_id in &old_members {
                            self.push_observer_event(
                                other_id,
                                DestinyEvent::RemoveBalls {
                                    ball_ids: vec![ball_id],
                                },
                            );
                        }
                        self.push_observer_event(
                            ball_id,
                            DestinyEvent::RemoveBalls {
                                ball_ids: old_members,
                            },
                        );
                    }
                    if !new_members.is_empty() {
                        if !is_cloaked {
                            if let Some(moved_ball) = self.balls.get(&ball_id).cloned() {
                                for &other_id in &new_members {
                                    self.push_observer_event(
                                        other_id,
                                        DestinyEvent::AddBalls2 {
                                            balls: vec![moved_ball.clone()],
                                        },
                                    );
                                }
                            }
                        }
                        let mut entered_for_mover = Vec::new();
                        for &other_id in &new_members {
                            let other_cloaked = self
                                .combat_states
                                .get(&other_id)
                                .is_some_and(|c| c.is_cloaked);
                            if !other_cloaked {
                                if let Some(b) = self.balls.get(&other_id).cloned() {
                                    entered_for_mover.push(b);
                                }
                            }
                        }
                        if !entered_for_mover.is_empty() {
                            self.push_observer_event(
                                ball_id,
                                DestinyEvent::AddBalls {
                                    balls: entered_for_mover,
                                },
                            );
                        }
                    }
                }
            }
        }
        true
    }
}
#[cfg(test)] #[path = "ballpark_unit_tests.rs"] mod ballpark_unit_tests;

