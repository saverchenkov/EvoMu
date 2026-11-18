//! Sub-warp steering, movement mode transitions, warp-tunnel immutability guards, and forced warp
//! termination for [`Ballpark`].

use crate::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, WarpPhase};
use glam::DVec3;

#[inline]
pub(crate) fn is_in_active_warp(mode: &MovementMode) -> bool {
    matches!(mode, MovementMode::Warp { phase, .. } if *phase != WarpPhase::Aligning)
}

#[inline]
pub(crate) fn clamp_ball_subwarp_velocity(ball: &mut Ball) {
    let max_v = ball.max_velocity.max(0.0);
    if !ball.velocity.is_finite() {
        ball.velocity = DVec3::ZERO;
    } else {
        let speed = ball.velocity.length();
        if speed > max_v {
            ball.velocity = if max_v > 0.0 {
                (ball.velocity / speed) * max_v
            } else {
                DVec3::ZERO
            };
        }
    }
}

impl Ballpark {
    fn emit_warp_cancelled_events(&mut self, ball_id: BallId) {
        self.pending_events.push(DestinyEvent::OnDeactivatingWarp { ball_id });
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallMassive {
                ball_id,
                is_massive: true,
            },
        );
        self.emit_event(
            Some(ball_id),
            DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: None,
                guid: "effects.Warping".to_string(),
                is_active: false,
            },
        );
    }

    /// Sets `ball_id`'s movement toward `point` and emits [`DestinyEvent::GotoPoint`] (`destiny:GotoPoint`).
    pub fn goto_point(&mut self, ball_id: BallId, point: DVec3) -> Result<(), String> {
        if !point.is_finite() {
            return Err("InvalidPoint: GotoPoint coordinates must be finite".to_string());
        }
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found"));
        }
        if self.is_immobile_structure_or_static(ball_id) {
            return Err(
                "StructureCannotNavigate: Static or Upwell structure balls cannot navigate or warp"
                    .to_string(),
            );
        }
        if is_in_active_warp(&self.balls[&ball_id].mode) {
            return Err("ShipAlreadyInWarp: Cannot steer ship while in active warp".to_string());
        }
        let was_aligning_warp = matches!(
            self.balls[&ball_id].mode,
            MovementMode::Warp {
                phase: WarpPhase::Aligning,
                ..
            }
        );
        let mut uncloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            if was_aligning_warp {
                cs.is_emergency_warping = false;
            }
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
            self.finish_gate_decloak(ball_id);
        }
        let ball = self.balls.get_mut(&ball_id).unwrap();
        if was_aligning_warp {
            clamp_ball_subwarp_velocity(ball);
        }
        let dir = (point - ball.position).normalize_or_zero();
        ball.mode = MovementMode::GotoDirection {
            direction: if dir.length_squared() > 1e-6 { dir } else { DVec3::X },
        };
        if ball.speed_fraction <= 0.0 {
            ball.speed_fraction = 1.0;
        }
        if was_aligning_warp {
            self.emit_warp_cancelled_events(ball_id);
        }
        self.same_tick_owner_goto_tick.remove(&ball_id);
        self.emit_event(
            Some(ball_id),
            DestinyEvent::GotoPoint {
                ball_id,
                point: point.to_array(),
            },
        );
        Ok(())
    }

    pub fn set_mode(&mut self, ball_id: BallId, mode: MovementMode) -> Result<(), String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }
        if self.is_immobile_structure_or_static(ball_id) {
            return Err(
                "StructureCannotNavigate: Static or Upwell structure balls cannot navigate or warp"
                    .to_string(),
            );
        }
        if is_in_active_warp(&self.balls[&ball_id].mode) {
            return Err(
                "ShipAlreadyInWarp: Cannot change movement mode while in active warp".to_string(),
            );
        }

        match &mode {
            MovementMode::GotoDirection { direction } => {
                if !direction.is_finite() {
                    return Err(
                        "InvalidDirection: GotoDirection vector must be finite".to_string()
                    );
                }
            }
            MovementMode::Follow { range, .. } | MovementMode::Orbit { range, .. } => {
                if !range.is_finite() || *range < 0.0 {
                    return Err(
                        "InvalidRange: Follow/Orbit range must be finite and non-negative"
                            .to_string(),
                    );
                }
            }
            MovementMode::Warp {
                origin,
                destination,
                warp_speed_au_s,
                total_distance,
                elapsed_warp_time,
                ..
            } => {
                if !origin.is_finite()
                    || !destination.is_finite()
                    || !warp_speed_au_s.is_finite()
                    || *warp_speed_au_s <= 0.0
                    || !total_distance.is_finite()
                    || *total_distance < 0.0
                    || !elapsed_warp_time.is_finite()
                    || *elapsed_warp_time < 0.0
                {
                    return Err(
                        "InvalidWarpParameters: Warp coordinates, distance, and speed must be finite"
                            .to_string(),
                    );
                }
            }
            _ => {}
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

            if let Some(locks) = self.target_locks.remove(&ball_id) {
                for l in locks {
                    if l.locked {
                        self.pending_events.push(DestinyEvent::OnTarget {
                            mode: "lost".to_string(),
                            source_id: ball_id,
                            target_id: l.target_id,
                        });
                    } else {
                        self.pending_events.push(DestinyEvent::TargetLockFailed {
                            source_id: ball_id,
                            target_id: l.target_id,
                        });
                    }
                }
            }
        }

        let cancelling_align_warp = matches!(
            self.balls[&ball_id].mode,
            MovementMode::Warp {
                phase: WarpPhase::Aligning,
                ..
            }
        ) && !matches!(mode, MovementMode::Warp { .. });

        let mut uncloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            if !matches!(mode, MovementMode::Warp { .. }) {
                cs.is_emergency_warping = false;
            }
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
            self.finish_gate_decloak(ball_id);
        }

        if !cancelling_align_warp {
            let cur_ball = &self.balls[&ball_id];
            if let MovementMode::GotoDirection { direction: new_dir } = &mode {
                if cur_ball.speed_fraction > 0.0 {
                    if let MovementMode::GotoDirection { direction: cur_dir } = &cur_ball.mode {
                        let c_len_sq = cur_dir.length_squared();
                        let n_len_sq = new_dir.length_squared();
                        if c_len_sq.is_finite()
                            && n_len_sq.is_finite()
                            && c_len_sq > 1e-18
                            && n_len_sq > 1e-18
                        {
                            let cos_sim =
                                (*cur_dir / c_len_sq.sqrt()).dot(*new_dir / n_len_sq.sqrt());
                            if cos_sim >= crate::DESTINY_GOTO_DUPLICATE_COSINE_THRESHOLD {
                                return Ok(());
                            }
                        }
                    }
                }
            } else if matches!(mode, MovementMode::Stop)
                && matches!(cur_ball.mode, MovementMode::Stop)
                && cur_ball.speed_fraction <= 0.0
            {
                return Ok(());
            }
        }

        let mut goto_event: Option<(DestinyEvent, crate::ballpark::OwnerGotoDelivery)> = None;
        let mut mode_events: Vec<DestinyEvent> = Vec::new();
        let mut extra_events: Vec<DestinyEvent> = Vec::new();
        let current_tick = self.current_tick;
        let already_echoed_goto_this_tick =
            self.same_tick_owner_goto_tick.get(&ball_id) == Some(&current_tick);
        let owner_has_undrained_goto =
            self.has_undrained_goto_for_observer(ball_id, ball_id);
        let sim_tick_accum = self.sim_tick_accum;
        let ball = self.balls.get_mut(&ball_id).unwrap();
        if cancelling_align_warp {
            clamp_ball_subwarp_velocity(ball);
        }
        match &mode {
            MovementMode::GotoDirection { direction } => {
                let ev = DestinyEvent::GotoDirection {
                    ball_id,
                    direction: direction.to_array(),
                };
                let owner_delivery =
                    if !already_echoed_goto_this_tick || !owner_has_undrained_goto {
                        crate::ballpark::OwnerGotoDelivery::Push
                    } else if sim_tick_accum > 0.0 {
                        crate::ballpark::OwnerGotoDelivery::Coalesce
                    } else {
                        crate::ballpark::OwnerGotoDelivery::Suppress
                    };
                match owner_delivery {
                    crate::ballpark::OwnerGotoDelivery::Suppress => {}
                    crate::ballpark::OwnerGotoDelivery::Push => {
                        self.pending_events.push(ev.clone());
                    }
                    crate::ballpark::OwnerGotoDelivery::Coalesce => {
                        crate::ballpark::Ballpark::coalesce_or_push_goto(
                            &mut self.pending_events,
                            ball_id,
                            &ev,
                        );
                    }
                }
                goto_event = Some((ev, owner_delivery));
            }
            MovementMode::Follow { target_id, range } => {
                if ball.speed_fraction <= 0.0 {
                    ball.speed_fraction = 1.0;
                }
                let ev = DestinyEvent::Follow {
                    ball_id,
                    target_id: *target_id,
                    range: *range,
                };
                mode_events.push(ev);
                extra_events.push(DestinyEvent::SetSpeedFraction {
                    ball_id,
                    fraction: ball.speed_fraction,
                });
            }
            MovementMode::Orbit { target_id, range } => {
                let ev = DestinyEvent::Orbit {
                    ball_id,
                    target_id: *target_id,
                    range: *range,
                };
                mode_events.push(ev);
            }
            MovementMode::Stop => {
                let pre_stop_velocity = ball.velocity;
                ball.speed_fraction = 0.0;
                mode_events.push(DestinyEvent::SetSpeedFraction {
                    ball_id,
                    fraction: 0.0,
                });
                mode_events.push(DestinyEvent::Stop { ball_id });
                mode_events.push(DestinyEvent::SetBallVelocity {
                    ball_id,
                    velocity: pre_stop_velocity.to_array(),
                });
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
                mode_events.push(ev);

                let ev_act = DestinyEvent::OnActivatingWarp { ball_id };
                let ev_sfx = DestinyEvent::OnSpecialFX {
                    ball_id,
                    module_id: None,
                    guid: "effects.Warping".to_string(),
                    is_active: true,
                };
                let ev_massive = DestinyEvent::SetBallMassive {
                    ball_id,
                    is_massive: false,
                };
                let ev_speed = DestinyEvent::SetSpeedFraction {
                    ball_id,
                    fraction: 1.0,
                };
                extra_events.push(ev_act);
                extra_events.push(ev_sfx);
                extra_events.push(ev_massive);
                extra_events.push(ev_speed);
            }
            _ => {}
        }
        ball.mode = mode;
        self.warp_align_ticks.remove(&ball_id);
        if let Some((_, owner_delivery)) = &goto_event {
            if *owner_delivery != crate::ballpark::OwnerGotoDelivery::Suppress {
                self.same_tick_owner_goto_tick.insert(ball_id, current_tick);
            }
        } else {
            self.same_tick_owner_goto_tick.remove(&ball_id);
        }
        if cancelling_align_warp {
            self.emit_warp_cancelled_events(ball_id);
        }
        if let Some((ev, owner_delivery)) = goto_event {
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                self.queue_or_coalesce_goto_for_grid_observers(gid, ball_id, &ev, owner_delivery);
            }
        }
        for ev in mode_events {
            self.pending_events.push(ev.clone());
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                self.queue_event_for_grid_observers(gid, None, &ev);
            }
        }
        for ev in extra_events {
            if matches!(ev, DestinyEvent::OnActivatingWarp { .. }) {
                self.pending_events.push(ev);
            } else {
                self.emit_event(Some(ball_id), ev);
            }
        }
        Ok(())
    }

    pub fn set_speed_fraction(&mut self, ball_id: BallId, fraction: f64) -> bool {
        let clamped = fraction.clamp(0.0, 1.0);
        if clamped > 0.0 && self.is_immobile_structure_or_static(ball_id) {
            return false;
        }
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        if is_in_active_warp(&ball.mode) {
            return false;
        }
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
                self.finish_gate_decloak(ball_id);
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

    /// Server-authoritative warp termination (reconnect recovery / admin reset) that resets
    /// velocity to zero, restores collision mass (`SetBallMassive { is_massive: true }`), and clears
    /// active warp visual state even if `ball_id` was mid-warp.
    pub fn force_stop_warp(&mut self, ball_id: BallId) -> Result<(), String> {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        };
        let was_warping = matches!(ball.mode, MovementMode::Warp { .. });
        let landing_pos = if was_warping && ball.position.is_finite() {
            Some(ball.position.to_array())
        } else {
            None
        };
        ball.mode = MovementMode::Stop;
        ball.speed_fraction = 0.0;
        ball.velocity = DVec3::ZERO;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            cs.is_emergency_warping = false;
        }
        if was_warping {
            self.emit_warp_cancelled_events(ball_id);
        }
        self.same_tick_owner_goto_tick.remove(&ball_id);
        self.warp_align_ticks.remove(&ball_id);
        self.emit_event(Some(ball_id), DestinyEvent::Stop { ball_id });
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallVelocity {
                ball_id,
                velocity: [0.0, 0.0, 0.0],
            },
        );
        if let Some(position) = landing_pos {
            self.emit_event(Some(ball_id), DestinyEvent::SetBallPosition { ball_id, position });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ballpark_navigation_tests.rs"]
mod ballpark_navigation_tests;
