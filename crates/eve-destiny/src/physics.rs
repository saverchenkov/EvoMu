//! 1 Hz `Ballpark::step` physics loop, sub-warp & warp kinematics, collision bounce, and `combat` equations.

use crate::{
    Ball, BallId, Ballpark, DamageVector, DestinyEvent, MovementMode, WarpPhase, AU_METERS,
    BUBBLE_TRANSITION_THRESHOLD_METERS, DECLOAK_DISTANCE_METERS,
};
use glam::DVec3;
use std::collections::BTreeMap;

impl Ballpark {
    /// Advances the entire solar system simulation by `dt` seconds (standard EVE server tick `dt = 1.0`).
    /// Rejects non-positive or non-finite `dt`.
    pub fn step(&mut self, dt: f64) -> Vec<DestinyEvent> {
        if dt <= 0.0 || !dt.is_finite() {
            return Vec::new();
        }
        let dt = dt.min(60.0);
        self.current_tick += 1;

        let dynamic_ids: Vec<BallId> = self
            .balls
            .keys()
            .copied()
            .filter(|&id| !self.is_static_ball(id))
            .collect();

        // Snapshot grid membership before stepping
        let grids_before: BTreeMap<BallId, u64> = dynamic_ids
            .iter()
            .filter_map(|&id| self.ball_grid_ids.get(&id).copied().map(|g| (id, g)))
            .collect();

        let positions: BTreeMap<BallId, (DVec3, f64)> = self
            .balls
            .iter()
            .map(|(&id, b)| (id, (b.position, b.radius)))
            .collect();

        let mut warp_exit_stops = Vec::new();

        for ball in self.balls.values_mut() {
            // evemu_Crucible `DestinyManager::ProcessState` (`DestinyManager.cpp:250-258`) gives these
            // modes no server-side movement: the server never integrates their position.
            if matches!(
                ball.mode,
                MovementMode::Rigid
                    | MovementMode::Field
                    | MovementMode::Mushroom
                    | MovementMode::Boid
                    | MovementMode::Troll
                    | MovementMode::Miniball
                    | MovementMode::Formation
            ) {
                continue;
            }

            if let MovementMode::Missile { target_id } = ball.mode {
                step_missile_ball(ball, positions.get(&target_id).map(|&(p, _)| p), dt);
                continue;
            }

            if let MovementMode::Warp {
                destination,
                warp_speed_au_s,
                mut phase,
                mut elapsed_warp_time,
                origin,
                total_distance,
            } = ball.mode.clone()
            {
                let is_nullified = self
                    .combat_states
                    .get(&ball.id)
                    .map(|cs| cs.is_interdiction_nullified)
                    .unwrap_or(false);
                // In-flight warp bubble interception: if ship enters a bubble during flight, drop out of warp
                if !is_nullified && self.warp_bubble_mgr.is_scrambled_at(ball.position) {
                    ball.velocity = DVec3::ZERO;
                    ball.speed_fraction = 0.0;
                    ball.mode = MovementMode::Stop;
                    warp_exit_stops.push(ball.id);
                    continue;
                }

                let pos_before = ball.position;
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
                if !is_nullified {
                    if let Some((_bid, intercept_pos)) =
                        self.warp_bubble_mgr.check_interception(pos_before, ball.position)
                    {
                        ball.position = intercept_pos;
                        ball.velocity = DVec3::ZERO;
                        ball.speed_fraction = 0.0;
                        ball.mode = MovementMode::Stop;
                        warp_exit_stops.push(ball.id);
                        continue;
                    }
                    if exited {
                        if let Some((_bid, intercept_pos)) =
                            self.warp_bubble_mgr.check_interception(origin, destination)
                        {
                            ball.position = intercept_pos;
                            ball.velocity = DVec3::ZERO;
                        }
                    }
                }
                if exited {
                    warp_exit_stops.push(ball.id);
                }
                continue;
            }

            let goal_velocity = compute_subwarp_goal_velocity(ball, &positions);
            integrate_subwarp(ball, goal_velocity, dt);
        }

        // Collision bounce physics (`Destiny::Ball::Collision`, `SP-4`)
        let ball_keys: Vec<BallId> = self.balls.keys().copied().collect();
        let mut collision_events = Vec::new();
        let restitution = 0.8;

        for i in 0..ball_keys.len() {
            let id_a = ball_keys[i];
            let is_static_a = self.is_static_ball(id_a);

            for j in (i + 1)..ball_keys.len() {
                let id_b = ball_keys[j];
                let is_static_b = self.is_static_ball(id_b);

                // Skip static-static collisions
                if is_static_a && is_static_b {
                    continue;
                }

                // Celestial spheres, stargates, stations, and intangible field bubbles do not have elastic bounce collision physics
                if matches!(self.balls[&id_a].mode, MovementMode::Field)
                    || matches!(self.balls[&id_b].mode, MovementMode::Field)
                {
                    continue;
                }
                let group_a = self.ball_group_ids.get(&id_a).copied().unwrap_or(0);
                let group_b = self.ball_group_ids.get(&id_b).copied().unwrap_or(0);
                if matches!(group_a, 6 | 7 | 8 | 9 | 10 | 15)
                    || matches!(group_b, 6 | 7 | 8 | 9 | 10 | 15)
                {
                    continue;
                }

                // Jump-cloaked / cloaked ships cannot be bumped (`feat:navigation.jump-cloak-immunities`, `SP-4`)
                if self.combat_states.get(&id_a).is_some_and(|cs| {
                    let mut c = cs.clone();
                    c.sync_action_breaks();
                    c.gate_cloak_remaining_ticks > 0 || c.is_cloaked
                }) || self.combat_states.get(&id_b).is_some_and(|cs| {
                    let mut c = cs.clone();
                    c.sync_action_breaks();
                    c.gate_cloak_remaining_ticks > 0 || c.is_cloaked
                }) {
                    continue;
                }

                let (pos_a, rad_a, mass_a, vel_a) = {
                    let b = &self.balls[&id_a];
                    (b.position, b.radius, b.mass.max(1.0), b.velocity)
                };
                let (pos_b, rad_b, mass_b, vel_b) = {
                    let b = &self.balls[&id_b];
                    (b.position, b.radius, b.mass.max(1.0), b.velocity)
                };

                if rad_a > 50_000.0 || rad_b > 50_000.0 {
                    continue;
                }

                let delta = pos_a - pos_b;
                let dist = delta.length();
                let min_dist = rad_a + rad_b;

                if matches!(self.balls[&id_a].mode, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning)
                    || matches!(self.balls[&id_b].mode, MovementMode::Warp { phase, .. } if phase != WarpPhase::Aligning)
                {
                    continue;
                }

                if dist < min_dist {
                    let normal = if dist > 1e-4 {
                        delta / dist
                    } else {
                        DVec3::X
                    };
                    let overlap = min_dist - dist;
                    let v_rel = (vel_a - vel_b).dot(normal);

                    if !is_static_a && !is_static_b {
                        // Dynamic vs dynamic
                        if v_rel < 0.0 {
                            let total_inv_mass = (1.0 / mass_a) + (1.0 / mass_b);
                            let impulse = -(1.0 + restitution) * v_rel / total_inv_mass;
                            if let Some(ba) = self.balls.get_mut(&id_a) {
                                ba.velocity += normal * (impulse / mass_a);
                            }
                            if let Some(bb) = self.balls.get_mut(&id_b) {
                                bb.velocity -= normal * (impulse / mass_b);
                            }
                        }
                        let total_mass = mass_a + mass_b;
                        if let Some(ba) = self.balls.get_mut(&id_a) {
                            ba.position += normal * (overlap * (mass_b / total_mass));
                        }
                        if let Some(bb) = self.balls.get_mut(&id_b) {
                            bb.position -= normal * (overlap * (mass_a / total_mass));
                        }
                    } else if !is_static_a && is_static_b {
                        // Dynamic A vs Static B
                        if v_rel < 0.0 {
                            if let Some(ba) = self.balls.get_mut(&id_a) {
                                ba.velocity -= normal * ((1.0 + restitution) * v_rel);
                            }
                        }
                        if let Some(ba) = self.balls.get_mut(&id_a) {
                            ba.position += normal * overlap;
                        }
                    } else if is_static_a && !is_static_b {
                        // Static A vs Dynamic B
                        if v_rel < 0.0 {
                            if let Some(bb) = self.balls.get_mut(&id_b) {
                                bb.velocity -= normal * ((1.0 + restitution) * v_rel);
                            }
                        }
                        if let Some(bb) = self.balls.get_mut(&id_b) {
                            bb.position -= normal * overlap;
                        }
                    }

                    collision_events.push((id_a, id_b));
                }
            }
        }

        for (id_a, id_b) in collision_events {
            let ev = DestinyEvent::Collision {
                ball_a: id_a,
                ball_b: id_b,
            };
            self.pending_events.push(ev.clone());
            if let Some(&gid) = self
                .ball_grid_ids
                .get(&id_a)
                .or_else(|| self.ball_grid_ids.get(&id_b))
            {
                self.queue_event_for_grid_observers(gid, None, &ev);
            }
        }

        // Gate cloak, post-warp invulnerability, and undock invulnerability decay and break on active movement (`SP-3`, `SP-4`, `SP-5`)
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
                } else {
                    cs.gate_cloak_remaining_ticks =
                        cs.gate_cloak_remaining_ticks.saturating_sub(1);
                    if cs.gate_cloak_remaining_ticks == 0 {
                        cs.is_cloaked = false;
                        cs.sync_invuln_with_undock_timer = false;
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
                        cs.warp_exit_invulnerable_remaining_ticks.saturating_sub(1);
                    if cs.warp_exit_invulnerable_remaining_ticks == 0 {
                        cs.sync_invuln_with_undock_timer = false;
                    }
                }
            }
            if cs.undock_invulnerable_remaining_ticks > 0 {
                cs.undock_invulnerable_remaining_ticks =
                    cs.undock_invulnerable_remaining_ticks.saturating_sub(1);
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
        let updated_positions: Vec<(BallId, DVec3, f64)> = self
            .balls
            .values()
            .map(|b| (b.id, b.position, b.radius))
            .collect();

        let mut proximity_decloaked = Vec::new();
        let mut mjfg_completed_ids = Vec::new();
        let mut dot_updates = Vec::new();
        for &(id_a, pos_a, rad_a) in &updated_positions {
            let is_cmd_destroyer = self
                .balls
                .get(&id_a)
                .is_some_and(|b| (37480..=37483).contains(&b.type_id));
            let is_scrambled = self.is_ball_scrambled(id_a);
            let has_target_locks = self
                .target_locks
                .get(&id_a)
                .is_some_and(|locks| !locks.is_empty());
            if let Some(cs) = self.combat_states.get_mut(&id_a) {
                if cs.is_tethered {
                    if has_target_locks || is_scrambled || cs.is_mwd_mjd_disabled || cs.disallow_assistance {
                        cs.is_tethered = false;
                    } else {
                        cs.shield_hp =
                            (cs.shield_hp + cs.max_shield_hp * 0.10 * dt).min(cs.max_shield_hp);
                        cs.armor_hp =
                            (cs.armor_hp + cs.max_armor_hp * 0.10 * dt).min(cs.max_armor_hp);
                        cs.structure_hp = (cs.structure_hp + cs.max_structure_hp * 0.10 * dt)
                            .min(cs.max_structure_hp);
                    }
                }
                if is_cmd_destroyer {
                    let prev_cap = cs.last_step_capacitor.unwrap_or(cs.max_capacitor);
                    if cs.mjfg_spool_remaining_s <= 0.0
                        && prev_cap - cs.capacitor >= 100.0
                        && !is_scrambled
                        && !cs.is_mwd_mjd_disabled
                    {
                        cs.mjfg_spool_remaining_s = 9.0;
                        if cs.mjfg_original_sig_radius.is_none() {
                            cs.mjfg_original_sig_radius = Some(cs.signature_radius);
                            cs.signature_radius *= 2.5;
                        }
                    }
                    if cs.mjfg_spool_remaining_s > 0.0 {
                        if is_scrambled || cs.is_mwd_mjd_disabled {
                            cs.mjfg_spool_remaining_s = 0.0;
                            if let Some(orig_sig) = cs.mjfg_original_sig_radius.take() {
                                cs.signature_radius = orig_sig;
                            }
                        } else {
                            cs.mjfg_spool_remaining_s =
                                (cs.mjfg_spool_remaining_s - dt).max(0.0);
                            if cs.mjfg_spool_remaining_s <= 1e-6 {
                                cs.mjfg_spool_remaining_s = 0.0;
                                if let Some(orig_sig) = cs.mjfg_original_sig_radius.take() {
                                    cs.signature_radius = orig_sig;
                                }
                                mjfg_completed_ids.push(id_a);
                            }
                        }
                    }
                }
                if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
                    for &(id_b, pos_b, rad_b) in &updated_positions {
                        if id_a != id_b {
                            let surface_dist = (pos_a - pos_b).length() - rad_a - rad_b;
                            if surface_dist <= DECLOAK_DISTANCE_METERS {
                                cs.is_cloaked = false;
                                cs.is_covert_cloak = false;
                                cs.cloak_stabilization_remaining_s = 0.0;
                                if cs.cloak_configured_targeting_delay_s > 0.0 {
                                    cs.decloak_targeting_delay_remaining_s =
                                        cs.cloak_configured_targeting_delay_s;
                                }
                                proximity_decloaked.push(id_a);
                                break;
                            }
                        }
                    }
                }
                if let Some((flat_dps, pct_dps, rem_ticks)) = cs.active_breacher_dot {
                    let max_total = cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp;
                    let tick_dmg = flat_dps.min(pct_dps * max_total) * dt;
                    let _ = cs.apply_damage(DamageVector {
                        em: 0.0,
                        thermal: tick_dmg * 0.5,
                        kinetic: tick_dmg * 0.5,
                        explosive: 0.0,
                    });
                    let next_rem = rem_ticks.saturating_sub(1);
                    dot_updates.push((id_a, flat_dps, pct_dps, next_rem));
                    if rem_ticks <= 1 {
                        cs.active_breacher_dot = None;
                    } else {
                        cs.active_breacher_dot = Some((flat_dps, pct_dps, next_rem));
                    }
                }

                // Passive Shield and Capacitor recharge simulation (C-2)
                if cs.shield_hp > 0.0
                    && cs.shield_hp < cs.max_shield_hp
                    && cs.shield_recharge_time_ms > 0.0
                {
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
        for (dot_id, flat_dps, pct_dps, remaining_ticks) in dot_updates {
            self.emit_event(
                Some(dot_id),
                DestinyEvent::OnDotVictimUpdated {
                    ball_id: dot_id,
                    flat_dps,
                    pct_dps,
                    remaining_ticks,
                },
            );
        }
        for mjfg_id in mjfg_completed_ids {
            let _ = self.micro_jump_field(mjfg_id, 100_000.0, 6_000.0, 25);
        }
        for decloaked_id in proximity_decloaked {
            self.emit_event(
                Some(decloaked_id),
                DestinyEvent::CloakBall {
                    ball_id: decloaked_id,
                    cloaked: 0,
                },
            );
            if let Some(orig_v) = self
                .combat_states
                .get_mut(&decloaked_id)
                .and_then(|c| c.cloak_original_max_velocity.take())
            {
                if let Some(b) = self.balls.get_mut(&decloaked_id) {
                    b.max_velocity = orig_v;
                }
            }
            if let Some(meta) = self.ball_slim_meta.get_mut(&decloaked_id) {
                meta.is_cloaked = false;
            }
            if let (Some(&gid), Some(b)) = (
                self.ball_grid_ids.get(&decloaked_id),
                self.balls.get(&decloaked_id).cloned(),
            ) {
                self.queue_event_for_grid_observers(
                    gid,
                    Some(decloaked_id),
                    &DestinyEvent::AddBalls2 { balls: vec![b] },
                );
            }
        }

        // Reconcile shared dynamic GridBubble membership after movement (`SP-2`, `SP-3`, `Grid.md:13-15`).
        for &ball_id in &dynamic_ids {
            let Some((new_pos, is_warping)) = self
                .balls
                .get(&ball_id)
                .map(|b| (b.position, matches!(b.mode, MovementMode::Warp { .. })))
            else {
                continue;
            };
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
                if !is_warping {
                    if let Some(gid) = old_gid {
                        let _ = self.try_expand_grid(gid, new_pos);
                        self.contract_or_delete_grid(gid);
                    }
                }
                continue;
            }
            // Sub-warp ships pushing a grid boundary expand the grid (up to 10,000 km) if no adjacent grid wall blocks it.
            if !is_warping {
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

        // Emit AddBalls2 and RemoveBalls for grid wall crossings (`SP-2`).
        for &obs_id in &dynamic_ids {
            let Some(&obs_before) = grids_before.get(&obs_id) else {
                continue;
            };
            let Some(&obs_after) = self.ball_grid_ids.get(&obs_id) else {
                continue;
            };
            let mut entered = Vec::new();
            let mut left = Vec::new();
            for &other_id in &dynamic_ids {
                if obs_id == other_id {
                    continue;
                }
                let Some(&other_before) = grids_before.get(&other_id) else {
                    continue;
                };
                let Some(&other_after) = self.ball_grid_ids.get(&other_id) else {
                    continue;
                };
                let other_cloaked = self
                    .combat_states
                    .get(&other_id)
                    .is_some_and(|c| c.is_cloaked);
                let was_same = obs_before == other_before;
                let now_same = obs_after == other_after && !other_cloaked;
                if !was_same && now_same {
                    if let Some(other_ball) = self.balls.get(&other_id) {
                        entered.push(other_ball.clone());
                    }
                } else if was_same && !now_same {
                    left.push(other_id);
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

        for ball_id in warp_exit_stops {
            if let Some(cs) = self.combat_states.get_mut(&ball_id) {
                cs.warp_exit_invulnerable_remaining_ticks = 10;
                cs.undock_invulnerable_remaining_ticks =
                    cs.undock_invulnerable_remaining_ticks.max(10);
                cs.sync_invuln_with_undock_timer = true;
            }
            let ev = DestinyEvent::Stop { ball_id };
            let ev_deact = DestinyEvent::OnDeactivatingWarp { ball_id };
            let ev_sfx = DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: None,
                guid: "effects.Warping".to_string(),
                is_active: false,
            };
            self.pending_events.push(ev.clone());
            self.pending_events.push(ev_deact.clone());
            self.pending_events.push(ev_sfx.clone());
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                // `OnDeactivatingWarp` is raised by the client's own destiny tick; it is kept in
                // `pending_events` for the simulation but never queued onto the wire.
                self.queue_event_for_grid_observers(gid, None, &ev);
                self.queue_event_for_grid_observers(gid, None, &ev_sfx);
            }
        }

        for cs in self.combat_states.values_mut() {
            cs.sync_action_breaks();
            if cs.ecm_jammed_remaining_s > 0.0 {
                cs.ecm_jammed_remaining_s = (cs.ecm_jammed_remaining_s - dt).max(0.0);
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
        (40_000.0 / denom).max(0.1)
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
    positions: &BTreeMap<BallId, (DVec3, f64)>,
) -> DVec3 {
    match &ball.mode {
        MovementMode::GotoDirection { direction } => {
            let dir = direction.normalize_or_zero();
            dir * (ball.max_velocity * ball.speed_fraction)
        }
        MovementMode::Follow { target_id, range } => {
            if let Some(&(target_pos, target_radius)) = positions.get(target_id) {
                let delta = target_pos - ball.position;
                let dist = delta.length();
                let desired_dist = ball.radius + target_radius + *range;
                if *range > 0.0 {
                    let err = dist - desired_dist;
                    if err.abs() > 1.0 {
                        let max_spd = ball.max_velocity * ball.speed_fraction;
                        let decel_dist = (max_spd * ball.time_constant().max(1.0)).max(50.0);
                        let factor = (err / decel_dist).clamp(-1.0, 1.0);
                        delta.normalize_or_zero() * (max_spd * factor)
                    } else {
                        DVec3::ZERO
                    }
                } else if dist > desired_dist + 1.0 {
                    delta.normalize_or_zero() * (ball.max_velocity * ball.speed_fraction)
                } else {
                    DVec3::ZERO
                }
            } else {
                DVec3::ZERO
            }
        }
        MovementMode::Orbit { target_id, range } => {
            if let Some(&(target_pos, target_radius)) = positions.get(target_id) {
                let to_target = target_pos - ball.position;
                let dist = to_target.length().max(1.0);
                let orbit_radius = ball.radius + target_radius + *range;
                let radial_dir = to_target / dist;

                let v_perp = ball.velocity - radial_dir * ball.velocity.dot(radial_dir);
                let tangent = if v_perp.length_squared() > 1e-4 {
                    v_perp.normalize()
                } else {
                    let ref_axis = if radial_dir.y.abs() < 0.9 {
                        DVec3::Y
                    } else {
                        DVec3::Z
                    };
                    radial_dir.cross(ref_axis).normalize_or_zero()
                };

                let max_spd = ball.max_velocity * ball.speed_fraction;
                let tau = ball.time_constant().max(0.1);
                let centripetal_lead =
                    (ball.velocity.length() * (tau + 0.5)) / orbit_radius.max(1.0);
                let radial_error = ((dist - orbit_radius) / 250.0).clamp(-3.0, 3.0);
                let steer =
                    (tangent + radial_dir * (radial_error + centripetal_lead)).normalize_or_zero();
                steer * max_spd
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
    mut remaining_dt: f64,
    origin: DVec3,
    destination: DVec3,
    warp_speed_au_s: f64,
    total_distance: f64,
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
        let align_threshold = ball.max_velocity * 0.75;
        let forward_speed = ball.velocity.dot(warp_dir);
        if forward_speed < align_threshold {
            let goal_vel = warp_dir * ball.max_velocity;
            integrate_subwarp(ball, goal_vel, remaining_dt);
            if ball.velocity.dot(warp_dir) >= align_threshold {
                *phase = WarpPhase::Accelerating;
            }
            ball.mode = MovementMode::Warp {
                origin,
                destination,
                warp_speed_au_s,
                total_distance,
                phase: *phase,
                elapsed_warp_time: *elapsed_warp_time,
            };
            return false;
        } else {
            *phase = WarpPhase::Accelerating;
        }
    }

    let k_a = warp_speed_au_s.max(0.1);
    let k_d = (k_a / 3.0).clamp(0.1, 2.0);
    let v_cruise = k_a * AU_METERS;
    let v_drop = (ball.max_velocity * 0.5).clamp(1.0, 100.0);
    let v_init = (ball.max_velocity * 0.75).max(v_drop);

    let eff_total = total_distance.max((destination - ball.position).length());
    let v_peak = v_cruise
        .min(eff_total / ((1.0 / k_a) + (1.0 / k_d)))
        .max(v_init * 1.1);
    let decel_start_dist = v_peak / k_d;

    let remaining_dist = (destination - ball.position).length();
    if *phase != WarpPhase::Accelerating && remaining_dist <= decel_start_dist {
        *phase = WarpPhase::Decelerating;
    }

    if *phase == WarpPhase::Accelerating {
        let v_start = (v_init * (k_a * *elapsed_warp_time).exp()).min(v_peak);
        let t_to_peak = ((v_peak / v_start).ln() / k_a).max(0.0);
        let dt_accel = remaining_dt.min(t_to_peak);

        *elapsed_warp_time += dt_accel;
        let v_end = (v_init * (k_a * *elapsed_warp_time).exp()).min(v_peak);
        let accel_disp = ((v_end - v_start) / k_a).min(remaining_dist);
        ball.position += warp_dir * accel_disp;
        ball.velocity = warp_dir * v_end;
        remaining_dt -= dt_accel;

        let new_rem = (destination - ball.position).length();
        if new_rem <= decel_start_dist {
            *phase = WarpPhase::Decelerating;
        } else if (v_end - v_peak).abs() < 1.0 {
            if v_peak < v_cruise {
                *phase = WarpPhase::Decelerating;
            } else {
                *phase = WarpPhase::Cruising;
            }
        }
    }

    if *phase == WarpPhase::Cruising && remaining_dt > 0.0 {
        let rem = (destination - ball.position).length();
        let dist_to_decel = (rem - decel_start_dist).max(0.0);
        let time_in_cruise = (dist_to_decel / v_peak).min(remaining_dt);
        ball.position += warp_dir * (v_peak * time_in_cruise);
        ball.velocity = warp_dir * v_peak;
        remaining_dt -= time_in_cruise;

        if remaining_dt > 0.0 || (destination - ball.position).length() <= decel_start_dist {
            *phase = WarpPhase::Decelerating;
        }
    }

    if *phase == WarpPhase::Decelerating && remaining_dt > 0.0 {
        let rem = (destination - ball.position).length();
        let decay = (-k_d * remaining_dt).exp();
        let new_rem = rem * decay;
        let current_warp_speed = (new_rem * k_d).max(v_drop);

        if new_rem <= 2_500.0 || current_warp_speed <= v_drop * 1.05 {
            let stop_offset = new_rem.min(2_500.0);
            ball.position = destination - warp_dir * stop_offset;
            ball.velocity = warp_dir * v_drop;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
            return true;
        } else {
            ball.position = destination - warp_dir * new_rem;
            ball.velocity = warp_dir * current_warp_speed;
        }
    }

    if (destination - ball.position).length() <= 2_500.0 {
        ball.velocity = warp_dir * v_drop;
        ball.speed_fraction = 0.0;
        ball.mode = MovementMode::Stop;
        true
    } else {
        ball.mode = MovementMode::Warp {
            origin,
            destination,
            warp_speed_au_s,
            total_distance,
            phase: *phase,
            elapsed_warp_time: *elapsed_warp_time,
        };
        false
    }
}
