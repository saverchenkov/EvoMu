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
                    self.pending_events.push(DestinyEvent::OnTarget {
                        mode: "lost".to_string(),
                        source_id: ball_id,
                        target_id: l.target_id,
                    });
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

        let mut emitted_event: Option<DestinyEvent> = None;
        let mut extra_events: Vec<DestinyEvent> = Vec::new();
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
        if cancelling_align_warp {
            self.emit_warp_cancelled_events(ball_id);
        }
        if let Some(ev) = emitted_event {
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
        ball.mode = MovementMode::Stop;
        ball.speed_fraction = 0.0;
        ball.velocity = DVec3::ZERO;
        if let Some(cs) = self.combat_states.get_mut(&ball_id) {
            cs.is_emergency_warping = false;
        }
        if was_warping {
            self.emit_warp_cancelled_events(ball_id);
        }
        self.emit_event(Some(ball_id), DestinyEvent::Stop { ball_id });
        Ok(())
    }
}

#[cfg(test)]
#[path = "ballpark_navigation_tests.rs"]
mod ballpark_navigation_tests;
