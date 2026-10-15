//! `eve-destiny` — EVE Online 1 Hz Space Simulation & Kinematics Engine ("Destiny").
//!
//! Implements deterministic sphere-kinematics (`Ballpark`), exponential velocity approach,
//! align time formulas, velocity-plane orbit trajectories, and symmetric short/long warp kinematics.

#![forbid(unsafe_code)]

use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type BallId = i64;

/// 1 Astronomical Unit in meters (exact EVE constant).
pub const AU_METERS: f64 = 149_597_870_700.0;

/// Standard EVE grid bubble radius in meters (250 km pre-Citadel, 8,000 km post-Citadel).
pub const BUBBLE_RADIUS_METERS: f64 = 250_000.0;


/// Explicit phase inside a `MovementMode::Warp` trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarpPhase {
    /// Accelerating sub-warp until velocity reaches 75% of `max_velocity` along the warp vector.
    Aligning,
    /// Exponential warp acceleration ($k_a = k_{\text{warp}}$) up to $v_{\text{peak}}$.
    Accelerating,
    /// Constant-speed cruise at $v_{\text{peak}} = v_{\text{cruise}}$ (only reached on long warps).
    Cruising,
    /// Exponential deceleration ($k_d = \min(k_{\text{warp}} / 3, 2.0)\text{ s}^{-1}$) down to sub-warp dropout speed.
    Decelerating,
}

/// EVE Destiny movement modes (`dstBallMode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MovementMode {
    GotoDirection {
        direction: DVec3,
    },
    Follow {
        target_id: BallId,
        range: f64,
    },
    Stop,
    Warp {
        origin: DVec3,
        destination: DVec3,
        warp_speed_au_s: f64,
        total_distance: f64,
        phase: WarpPhase,
        elapsed_warp_time: f64,
    },
    Orbit {
        target_id: BallId,
        range: f64,
    },
    Rigid,
}

impl MovementMode {
    pub fn new_warp(origin: DVec3, destination: DVec3, warp_speed_au_s: f64) -> Self {
        let total_distance = (destination - origin).length();
        Self::Warp {
            origin,
            destination,
            warp_speed_au_s,
            total_distance,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
        }
    }
}

/// Physical & kinematic state of a single entity ("Ball") inside a solar system's `Ballpark`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ball {
    pub id: BallId,
    pub type_id: u32,
    pub position: DVec3,
    pub velocity: DVec3,
    pub max_velocity: f64,
    pub inertia: f64,
    pub mass: f64,
    pub radius: f64,
    pub speed_fraction: f64,
    pub mode: MovementMode,
}

impl Ball {
    /// Computes the ship's friction/alignment time constant $\tau = \frac{I \cdot M}{10^6}$ in seconds.
    #[inline]
    pub fn time_constant(&self) -> f64 {
        (self.inertia * self.mass) / 1_000_000.0
    }

    /// Computes the time in seconds required to reach 75% of max velocity from rest (warp align time):
    /// $$t_{\text{align}} = -\ln(0.25) \cdot \tau \approx 1.38629436 \cdot \tau$$
    #[inline]
    pub fn align_time_seconds(&self) -> f64 {
        -0.25_f64.ln() * self.time_constant()
    }
}

/// High-level state synchronization events emitted by the `Ballpark` each 1 Hz server tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum DestinyEvent {
    AddBall {
        ball: Ball,
    },
    RemoveBall {
        ball_id: BallId,
    },
    GotoDirection {
        ball_id: BallId,
        direction: [f64; 3],
    },
    Follow {
        ball_id: BallId,
        target_id: BallId,
        range: f64,
    },
    SetSpeedFraction {
        ball_id: BallId,
        fraction: f64,
    },
    Orbit {
        ball_id: BallId,
        target_id: BallId,
        range: f64,
    },
    Stop {
        ball_id: BallId,
    },
    WarpTo {
        ball_id: BallId,
        destination: [f64; 3],
        warp_speed_au_s: f64,
    },
}

/// Represents a solar system's active space simulation instance (`Destiny.Ballpark`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ballpark {
    pub solar_system_id: u32,
    pub current_tick: u64,
    pub balls: BTreeMap<BallId, Ball>,
    pub pending_events: Vec<DestinyEvent>,
}

impl Ballpark {
    pub fn new(solar_system_id: u32) -> Self {
        Self {
            solar_system_id,
            current_tick: 1,
            balls: BTreeMap::new(),
            pending_events: Vec::new(),
        }
    }

    /// Inserts a new `Ball` into the solar system, returning `false` if `ball.id` already exists.
    pub fn add_ball(&mut self, ball: Ball) -> bool {
        if self.balls.contains_key(&ball.id) {
            return false;
        }
        self.pending_events
            .push(DestinyEvent::AddBall { ball: ball.clone() });
        self.balls.insert(ball.id, ball);
        true
    }

    pub fn remove_ball(&mut self, ball_id: BallId) -> Option<Ball> {
        let removed = self.balls.remove(&ball_id)?;
        self.pending_events
            .push(DestinyEvent::RemoveBall { ball_id });
        Some(removed)
    }

    pub fn set_mode(&mut self, ball_id: BallId, mode: MovementMode) {
        if let Some(ball) = self.balls.get_mut(&ball_id) {
            match &mode {
                MovementMode::GotoDirection { direction } => {
                    self.pending_events.push(DestinyEvent::GotoDirection {
                        ball_id,
                        direction: direction.to_array(),
                    });
                }
                MovementMode::Follow { target_id, range } => {
                    self.pending_events.push(DestinyEvent::Follow {
                        ball_id,
                        target_id: *target_id,
                        range: *range,
                    });
                }
                MovementMode::Orbit { target_id, range } => {
                    self.pending_events.push(DestinyEvent::Orbit {
                        ball_id,
                        target_id: *target_id,
                        range: *range,
                    });
                }
                MovementMode::Stop => {
                    ball.speed_fraction = 0.0;
                    self.pending_events.push(DestinyEvent::Stop { ball_id });
                }
                MovementMode::Warp {
                    destination,
                    warp_speed_au_s,
                    ..
                } => {
                    ball.speed_fraction = 1.0;
                    self.pending_events.push(DestinyEvent::WarpTo {
                        ball_id,
                        destination: destination.to_array(),
                        warp_speed_au_s: *warp_speed_au_s,
                    });
                }
                MovementMode::Rigid => {}
            }
            ball.mode = mode;
        }
    }

    /// Advances the entire solar system simulation by `dt` seconds (standard EVE server tick `dt = 1.0`).
    pub fn step(&mut self, dt: f64) -> Vec<DestinyEvent> {
        self.current_tick += 1;

        let positions: BTreeMap<BallId, (DVec3, f64)> = self
            .balls
            .iter()
            .map(|(&id, b)| (id, (b.position, b.radius)))
            .collect();

        let mut warp_exit_stops = Vec::new();

        for ball in self.balls.values_mut() {
            if matches!(ball.mode, MovementMode::Rigid) {
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
                if exited {
                    warp_exit_stops.push(ball.id);
                }
                continue;
            }

            let goal_velocity = compute_subwarp_goal_velocity(ball, &positions);
            integrate_subwarp(ball, goal_velocity, dt);
        }

        for ball_id in warp_exit_stops {
            self.pending_events.push(DestinyEvent::Stop { ball_id });
        }

        std::mem::take(&mut self.pending_events)
    }

    /// Returns all balls within `radius_meters` (default `BUBBLE_RADIUS_METERS`) of `observer_ball_id`,
    /// implementing spatial grid bubble interest management.
    pub fn balls_in_bubble(&self, observer_ball_id: BallId, radius_meters: f64) -> Vec<&Ball> {
        let Some(observer) = self.balls.get(&observer_ball_id) else {
            return Vec::new();
        };
        let r2 = radius_meters * radius_meters;
        self.balls
            .values()
            .filter(|b| {
                matches!(b.mode, MovementMode::Rigid)
                    || (b.position - observer.position).length_squared() <= r2
            })
            .collect()
    }
}

/// Binary `Destiny` wire-format encoder (`AddBalls` state buffer) per `DestinyStructs.h`.
pub mod wire {
    use super::{Ball, MovementMode};

    /// Ball mode byte values (`Destiny::BallMode` in `DestinyStructs.h`).
    pub const DST_BALL_GOTO: u8 = 0;
    pub const DST_BALL_FOLLOW: u8 = 1;
    pub const DST_BALL_STOP: u8 = 2;
    pub const DST_BALL_WARP: u8 = 3;
    pub const DST_BALL_ORBIT: u8 = 4;
    pub const DST_BALL_RIGID: u8 = 6;

    /// Ball flags (`Destiny::BallFlag` in `DestinyStructs.h`).
    pub const BALL_FLAG_IS_FREE: u8 = 0x01;
    pub const BALL_FLAG_IS_GLOBAL: u8 = 0x02;
    pub const BALL_FLAG_IS_MASSIVE: u8 = 0x04;
    pub const BALL_FLAG_IS_INTERACTIVE: u8 = 0x08;

    /// Encodes a binary `AddBalls` buffer (`Destiny_Header` + per-ball `BallHeader` + `MassSector` + `ShipSector` + mode struct).
    pub fn encode_add_balls(stamp: u32, balls: &[&Ball]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24 + balls.len() * 128);
        // Destiny_Header: packet_type = 0 (AddBalls), stamp (u32 LE)
        buf.push(0u8);
        buf.extend_from_slice(&stamp.to_le_bytes());

        for ball in balls {
            let is_rigid = matches!(ball.mode, MovementMode::Rigid);
            let (mode_byte, flags) = match ball.mode {
                MovementMode::GotoDirection { .. } => (
                    DST_BALL_GOTO,
                    BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
                ),
                MovementMode::Follow { .. } => (
                    DST_BALL_FOLLOW,
                    BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
                ),
                MovementMode::Stop => (
                    DST_BALL_STOP,
                    BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
                ),
                MovementMode::Warp { .. } => (
                    DST_BALL_WARP,
                    BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
                ),
                MovementMode::Orbit { .. } => (
                    DST_BALL_ORBIT,
                    BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
                ),
                MovementMode::Rigid => (DST_BALL_RIGID, BALL_FLAG_IS_GLOBAL | BALL_FLAG_IS_MASSIVE),
            };

            // BallHeader (39 bytes packed): entityID (i64), mode (u8), radius (f32), x/y/z (f64 * 3), sub_type (u8)
            buf.extend_from_slice(&ball.id.to_le_bytes());
            buf.push(mode_byte);
            buf.extend_from_slice(&(ball.radius as f32).to_le_bytes());
            buf.extend_from_slice(&ball.position.x.to_le_bytes());
            buf.extend_from_slice(&ball.position.y.to_le_bytes());
            buf.extend_from_slice(&ball.position.z.to_le_bytes());
            buf.push(flags);

            // MassSector (21 bytes packed): mass (f64), cloak (u8), allianceID (i64), corpID (u32), harmonic (f32)
            if (flags & BALL_FLAG_IS_MASSIVE) != 0 {
                buf.extend_from_slice(&ball.mass.to_le_bytes());
                buf.push(0u8); // cloak = 0
                buf.extend_from_slice(&(-1i64).to_le_bytes()); // allianceID = -1
                buf.extend_from_slice(&0u32.to_le_bytes()); // corpID = 0
                buf.extend_from_slice(&(-1.0f32).to_le_bytes()); // harmonic = -1.0
            }

            // ShipSector (36 bytes packed): max_speed (f32), velocity_x/y/z (f64 * 3), agility (f32), speed_fraction (f32)
            if !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0 {
                buf.extend_from_slice(&(ball.max_velocity as f32).to_le_bytes());
                buf.extend_from_slice(&ball.velocity.x.to_le_bytes());
                buf.extend_from_slice(&ball.velocity.y.to_le_bytes());
                buf.extend_from_slice(&ball.velocity.z.to_le_bytes());
                buf.extend_from_slice(&(ball.inertia as f32).to_le_bytes());
                buf.extend_from_slice(&(ball.speed_fraction as f32).to_le_bytes());
            }

            // Mode-specific tail
            match &ball.mode {
                MovementMode::GotoDirection { direction } => {
                    buf.push(1u8); // formationID
                    buf.extend_from_slice(&direction.x.to_le_bytes());
                    buf.extend_from_slice(&direction.y.to_le_bytes());
                    buf.extend_from_slice(&direction.z.to_le_bytes());
                }
                MovementMode::Follow { target_id, range }
                | MovementMode::Orbit { target_id, range } => {
                    buf.push(1u8); // formationID
                    buf.extend_from_slice(&target_id.to_le_bytes());
                    buf.extend_from_slice(&(*range as f32).to_le_bytes());
                }
                MovementMode::Stop => {
                    buf.push(1u8); // formationID
                }
                MovementMode::Warp {
                    destination,
                    warp_speed_au_s,
                    ..
                } => {
                    buf.push(1u8); // formationID
                    buf.extend_from_slice(&destination.x.to_le_bytes());
                    buf.extend_from_slice(&destination.y.to_le_bytes());
                    buf.extend_from_slice(&destination.z.to_le_bytes());
                    buf.extend_from_slice(&0u32.to_le_bytes()); // effectStamp
                    buf.extend_from_slice(&15000.0f64.to_le_bytes()); // followRange
                    buf.extend_from_slice(&0i64.to_le_bytes()); // followID
                    buf.extend_from_slice(&((*warp_speed_au_s * 10.0) as u32).to_le_bytes()); // warpSpeed (10x AU/s)
                }
                MovementMode::Rigid => {
                    buf.push(1u8); // formationID
                }
            }
        }

        buf
    }
}

fn compute_subwarp_goal_velocity(ball: &Ball, positions: &BTreeMap<BallId, (DVec3, f64)>) -> DVec3 {
    match &ball.mode {
        MovementMode::Stop | MovementMode::Rigid | MovementMode::Warp { .. } => DVec3::ZERO,
        MovementMode::GotoDirection { direction } => {
            let dir = direction.normalize_or_zero();
            dir * (ball.max_velocity * ball.speed_fraction)
        }
        MovementMode::Follow { target_id, range } => {
            if let Some(&(target_pos, target_radius)) = positions.get(target_id) {
                let delta = target_pos - ball.position;
                let dist = delta.length();
                let desired_dist = ball.radius + target_radius + *range;
                if dist > desired_dist + 1.0 {
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

                // Build tangent in the plane of the ship's current velocity vector when moving,
                // avoiding arbitrary horizontal-plane pinning or pole flips.
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

                let radial_error = (dist - orbit_radius) / orbit_radius.max(1.0);
                let steer =
                    (tangent + radial_dir * radial_error.clamp(-1.0, 1.0)).normalize_or_zero();
                steer * (ball.max_velocity * ball.speed_fraction)
            } else {
                DVec3::ZERO
            }
        }
    }
}

/// Integrates exponential sub-warp movement over `dt` seconds:
/// $$\vec{v}(t + \Delta t) = \vec{v}_{\text{goal}} + (\vec{v}(t) - \vec{v}_{\text{goal}}) e^{-\Delta t / \tau}$$
fn integrate_subwarp(ball: &mut Ball, goal_velocity: DVec3, dt: f64) {
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

/// Advances a warping ship across Align -> Accelerate -> Cruise -> Decelerate -> Exit phases.
/// Uses symmetric peak velocity $v_{\text{peak}} = \min(v_{\text{cruise}}, d_{\text{total}} / (1/k_a + 1/k_d))$
/// so short warps (< 3 AU) accelerate smoothly before decelerating.
/// Returns `true` if the ship exited warp into `MovementMode::Stop` on this tick.
#[allow(clippy::too_many_arguments)]
fn step_warp_ball(
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

    // Phase 1: Sub-warp Alignment until velocity along warp_dir reaches 75% of max_velocity
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

    // Phases 2-4: Active Warp Tunnel
    let k_a = warp_speed_au_s.max(0.1);
    let k_d = (k_a / 3.0).clamp(0.1, 2.0);
    let v_cruise = k_a * AU_METERS;
    let v_drop = (ball.max_velocity * 0.5).clamp(1.0, 100.0);
    let v_init = (ball.max_velocity * 0.75).max(v_drop);

    // Symmetric peak velocity for short warps vs long warps:
    // d_accel + d_decel = v_peak / k_a + v_peak / k_d
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
        // Analytic integral of exponential velocity v(t) = v_init * e^(k_a * t):
        // dx = (v_end - v_start) / k_a
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rifter_align_time_and_exponential_acceleration() {
        let mut park = Ballpark::new(30000142);
        let rifter = Ball {
            id: 1001,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection {
                direction: DVec3::X,
            },
        };

        assert!((rifter.align_time_seconds() - 5.103).abs() < 0.01);

        assert!(park.add_ball(rifter));
        for _ in 0..6 {
            park.step(1.0);
        }

        let updated = park.balls.get(&1001).unwrap();
        assert!(updated.velocity.x > 365.0 * 0.75);
        assert!(updated.position.x > 0.0);
    }

    #[test]
    fn test_short_and_long_warp_exit_without_overshoot() {
        let mut park = Ballpark::new(30000142);
        // Short 0.05 AU warp (tests symmetric peak speed acceleration before deceleration)
        let short_dest = DVec3::new(0.05 * AU_METERS, 0.0, 0.0);

        let ship = Ball {
            id: 1002,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(365.0, 0.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, short_dest, 3.0),
        };

        assert!(park.add_ball(ship));

        // First tick of short warp must accelerate smoothly (not jump 63% of distance immediately)
        park.step(1.0);
        let tick1 = park.balls.get(&1002).unwrap();
        assert!(
            tick1.position.x < 0.05 * AU_METERS * 0.1,
            "Short warp jumped too far on tick 1: {}",
            tick1.position.x
        );

        for _ in 0..60 {
            park.step(1.0);
        }

        let arrived = park.balls.get(&1002).unwrap();
        assert_eq!(arrived.mode, MovementMode::Stop);
        let dist_from_dest = (arrived.position - short_dest).length();
        assert!(
            dist_from_dest <= 2_500.0,
            "Ship overshot destination by {dist_from_dest} meters!"
        );
        assert!(arrived.velocity.length() < 1.0);
    }
}
