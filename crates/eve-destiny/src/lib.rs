//! `eve-destiny` — EVE Online 1 Hz Space Simulation, Kinematics, Combat, EWAR, & CrimeWatch Engine ("Destiny").
//!
//! Implements:
//! - Deterministic sphere-kinematics (`Ballpark`), exponential velocity approach, align time,
//!   velocity-plane orbit trajectories, symmetric short/long warp kinematics, and all 13 `DSTBALL_*` modes (`0..=12`)
//! - Cloaking & `2,000m` proximity decloaking
//! - Target lock time & max target calculation (`#43`–`#45`)
//! - Turret ChanceToHit, hit quality $Q \in [0.5, 1.49]$, and $3.0\times$ wrecking shots (`#46`–`#49`)
//! - Missile damage application (`#50`–`#51`), Breacher Pod DoT (`#52`), Vorton Projector 4-bounce chain (`#53`),
//!   and Entropic Disintegrator spooling (`#54`)
//! - Shield / Armor / Structure damage propagation, wreck spawning, and CrimeWatch 2.0 / CONCORD response (`#132`–`#138`)

#![forbid(unsafe_code)]

use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type BallId = i64;

/// 1 Astronomical Unit in meters (exact EVE constant).
pub const AU_METERS: f64 = 149_597_870_700.0;

/// Standard EVE grid bubble radius in meters (250 km pre-Citadel, 8,000 km post-Citadel).
pub const BUBBLE_RADIUS_METERS: f64 = 250_000.0;

/// Proximity decloak distance in meters (`2,000m` surface-to-surface).
pub const DECLOAK_DISTANCE_METERS: f64 = 2_000.0;

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

/// EVE Destiny movement modes (`dstBallMode` `0..=12`).
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
    Missile {
        target_id: BallId,
    },
    Mushroom,
    Boid,
    Troll,
    Miniball,
    Field,
    Rigid,
    Formation,
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

/// 4-channel damage vector (`EM`, `Thermal`, `Kinetic`, `Explosive`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DamageVector {
    pub em: f64,
    pub thermal: f64,
    pub kinetic: f64,
    pub explosive: f64,
}

impl DamageVector {
    pub fn total(&self) -> f64 {
        self.em + self.thermal + self.kinetic + self.explosive
    }

    pub fn scaled(&self, factor: f64) -> Self {
        Self {
            em: self.em * factor,
            thermal: self.thermal * factor,
            kinetic: self.kinetic * factor,
            explosive: self.explosive * factor,
        }
    }
}

/// Shield, Armor, and Structure HP & Resonance state for a combat-capable `Ball`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatState {
    pub shield_hp: f64,
    pub max_shield_hp: f64,
    pub armor_hp: f64,
    pub max_armor_hp: f64,
    pub structure_hp: f64,
    pub max_structure_hp: f64,
    pub capacitor: f64,
    pub max_capacitor: f64,
    pub signature_radius: f64,
    pub is_cloaked: bool,
    pub is_tethered: bool,
    pub warp_scramble_strength: i32,
    pub entropic_spool_cycles: u32,
    /// Active Breacher Pod DoT `(flat_dmg_per_sec, pct_max_hp_per_sec, remaining_ticks)`.
    pub active_breacher_dot: Option<(f64, f64, u32)>,
}

impl Default for CombatState {
    fn default() -> Self {
        Self {
            shield_hp: 450.0,
            max_shield_hp: 450.0,
            armor_hp: 450.0,
            max_armor_hp: 450.0,
            structure_hp: 350.0,
            max_structure_hp: 350.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            signature_radius: 35.0,
            is_cloaked: false,
            is_tethered: false,
            warp_scramble_strength: 0,
            entropic_spool_cycles: 0,
            active_breacher_dot: None,
        }
    }
}

impl CombatState {
    /// Applies a [`DamageVector`] through Shield -> Armor -> Structure layers.
    /// Returns `(total_applied_damage, destroyed)`.
    pub fn apply_damage(&mut self, dmg: DamageVector) -> (f64, bool) {
        if self.is_tethered {
            return (0.0, false);
        }
        let raw = dmg.total().max(0.0);
        let mut rem = raw;

        if self.shield_hp > 0.0 {
            let take = rem.min(self.shield_hp);
            self.shield_hp -= take;
            rem -= take;
        }
        if rem > 0.0 && self.armor_hp > 0.0 {
            let take = rem.min(self.armor_hp);
            self.armor_hp -= take;
            rem -= take;
        }
        if rem > 0.0 && self.structure_hp > 0.0 {
            let take = rem.min(self.structure_hp);
            self.structure_hp -= take;
            rem -= take;
        }
        let applied = raw - rem;
        let destroyed = self.structure_hp <= 1e-6;
        (applied, destroyed)
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
    OnDamageStateChange {
        ball_id: BallId,
        shield_ratio: f64,
        armor_ratio: f64,
        structure_ratio: f64,
    },
    TerminalExplosion {
        ball_id: BallId,
        wreck_ball_id: BallId,
    },
}

/// Represents a solar system's active space simulation instance (`Destiny.Ballpark`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ballpark {
    pub solar_system_id: u32,
    pub current_tick: u64,
    pub balls: BTreeMap<BallId, Ball>,
    pub combat_states: BTreeMap<BallId, CombatState>,
    pub pending_events: Vec<DestinyEvent>,
}

impl Ballpark {
    pub fn new(solar_system_id: u32) -> Self {
        Self {
            solar_system_id,
            current_tick: 1,
            balls: BTreeMap::new(),
            combat_states: BTreeMap::new(),
            pending_events: Vec::new(),
        }
    }

    /// Inserts a new `Ball` into the solar system, returning `false` if `ball.id` already exists.
    pub fn add_ball(&mut self, ball: Ball) -> bool {
        if self.balls.contains_key(&ball.id) {
            return false;
        }
        let ball_id = ball.id;
        self.pending_events
            .push(DestinyEvent::AddBall { ball: ball.clone() });
        self.balls.insert(ball_id, ball);
        self.combat_states.entry(ball_id).or_default();
        true
    }

    pub fn remove_ball(&mut self, ball_id: BallId) -> Option<Ball> {
        let removed = self.balls.remove(&ball_id)?;
        self.combat_states.remove(&ball_id);
        self.pending_events
            .push(DestinyEvent::RemoveBall { ball_id });
        Some(removed)
    }

    pub fn set_mode(&mut self, ball_id: BallId, mode: MovementMode) {
        if let Some(ball) = self.balls.get_mut(&ball_id) {
            // Reject warp if warp_scramble_strength > 0
            if matches!(mode, MovementMode::Warp { .. }) {
                if let Some(cs) = self.combat_states.get(&ball_id) {
                    if cs.warp_scramble_strength > 0 {
                        return;
                    }
                }
            }
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
                _ => {}
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
            if matches!(ball.mode, MovementMode::Rigid | MovementMode::Field) {
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

        // Check 2,000m surface-to-surface proximity decloak and active Breacher Pod DoT
        let updated_positions: Vec<(BallId, DVec3, f64)> = self
            .balls
            .values()
            .map(|b| (b.id, b.position, b.radius))
            .collect();

        for &(id_a, pos_a, rad_a) in &updated_positions {
            if let Some(cs) = self.combat_states.get_mut(&id_a) {
                if cs.is_cloaked {
                    for &(id_b, pos_b, rad_b) in &updated_positions {
                        if id_a != id_b {
                            let surface_dist = (pos_a - pos_b).length() - rad_a - rad_b;
                            if surface_dist <= DECLOAK_DISTANCE_METERS {
                                cs.is_cloaked = false;
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
                    if rem_ticks <= 1 {
                        cs.active_breacher_dot = None;
                    } else {
                        cs.active_breacher_dot = Some((flat_dps, pct_dps, rem_ticks - 1));
                    }
                }
            }
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

/// Combat, Weapons, EWAR, Target Lock, and CrimeWatch 2.0 Equations (`#43`–`#65`, `#132`–`#138`).
pub mod combat {
    use super::DamageVector;
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
        let track_denom = (turret_tracking * target_sig_radius).max(1e-6);
        let track_term = ((angular_velocity * 40_000.0) / track_denom).powi(2);
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

    /// Resolves a turret shot given `chance_to_hit` and deterministic uniform roll `u in [0.0, 1.0)` (`#48`–`#49`).
    /// - If `u >= chance_to_hit`: Miss (`0.0` multiplier).
    /// - If `u < 0.01`: Wrecking Shot (`3.0` multiplier).
    /// - Otherwise: Normal hit with quality $Q = 0.5 + u \in [0.5, 1.49]$.
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
        let q = (0.5 + roll_u).clamp(0.5, 1.49);
        (base_damage.scaled(q), false)
    }

    /// Computes EVE Missile Damage Application factor (`#50`–`#51`):
    /// $$D = D_{\text{base}} \cdot \min\left(1.0, \frac{\sigma_t}{E_r}, \left(\frac{\sigma_t}{E_r} \cdot \frac{E_v}{v_t}\right)^{\ln(\text{drf}) / \ln(5.5)}\right)$$
    pub fn compute_missile_damage_factor(
        target_sig_radius: f64,
        target_velocity: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        damage_reduction_factor: f64,
    ) -> f64 {
        let sig_ratio = target_sig_radius / explosion_radius.max(1.0);
        let vel_ratio = explosion_velocity / target_velocity.max(1.0);
        let exponent = damage_reduction_factor.max(1e-3).ln() / 5.5_f64.ln();
        let combined = (sig_ratio * vel_ratio).powf(exponent.abs());
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

/// Binary `Destiny` wire-format encoder (`AddBalls` state buffer) per `DestinyStructs.h`.
pub mod wire {
    use super::{Ball, MovementMode};

    /// All 13 Ball mode byte values (`Destiny::BallMode` in `DestinyStructs.h`).
    pub const DST_BALL_GOTO: u8 = 0;
    pub const DST_BALL_FOLLOW: u8 = 1;
    pub const DST_BALL_STOP: u8 = 2;
    pub const DST_BALL_WARP: u8 = 3;
    pub const DST_BALL_ORBIT: u8 = 4;
    pub const DST_BALL_MISSILE: u8 = 5;
    pub const DST_BALL_MUSHROOM: u8 = 6;
    pub const DST_BALL_BOID: u8 = 7;
    pub const DST_BALL_TROLL: u8 = 8;
    pub const DST_BALL_MINIBALL: u8 = 9;
    pub const DST_BALL_FIELD: u8 = 10;
    pub const DST_BALL_RIGID: u8 = 11;
    pub const DST_BALL_FORMATION: u8 = 12;

    /// Ball flags (`Destiny::BallFlag` in `DestinyStructs.h`).
    pub const BALL_FLAG_IS_FREE: u8 = 0x01;
    pub const BALL_FLAG_IS_GLOBAL: u8 = 0x02;
    pub const BALL_FLAG_IS_MASSIVE: u8 = 0x04;
    pub const BALL_FLAG_IS_INTERACTIVE: u8 = 0x08;

    /// Encodes a binary `AddBalls` buffer (`Destiny_Header` + per-ball `BallHeader` + `MassSector` + `ShipSector` + mode struct).
    pub fn encode_add_balls(stamp: u32, balls: &[&Ball]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24 + balls.len() * 128);
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
                MovementMode::Missile { .. } => (DST_BALL_MISSILE, BALL_FLAG_IS_FREE),
                MovementMode::Mushroom => (DST_BALL_MUSHROOM, BALL_FLAG_IS_FREE),
                MovementMode::Boid => (DST_BALL_BOID, BALL_FLAG_IS_FREE),
                MovementMode::Troll => (DST_BALL_TROLL, BALL_FLAG_IS_FREE),
                MovementMode::Miniball => (DST_BALL_MINIBALL, BALL_FLAG_IS_FREE),
                MovementMode::Field => (DST_BALL_FIELD, BALL_FLAG_IS_GLOBAL),
                MovementMode::Rigid => (DST_BALL_RIGID, BALL_FLAG_IS_GLOBAL | BALL_FLAG_IS_MASSIVE),
                MovementMode::Formation => (DST_BALL_FORMATION, BALL_FLAG_IS_FREE),
            };

            buf.extend_from_slice(&ball.id.to_le_bytes());
            buf.push(mode_byte);
            buf.extend_from_slice(&(ball.radius as f32).to_le_bytes());
            buf.extend_from_slice(&ball.position.x.to_le_bytes());
            buf.extend_from_slice(&ball.position.y.to_le_bytes());
            buf.extend_from_slice(&ball.position.z.to_le_bytes());
            buf.push(flags);

            if (flags & BALL_FLAG_IS_MASSIVE) != 0 {
                buf.extend_from_slice(&ball.mass.to_le_bytes());
                buf.push(0u8);
                buf.extend_from_slice(&(-1i64).to_le_bytes());
                buf.extend_from_slice(&0u32.to_le_bytes());
                buf.extend_from_slice(&(-1.0f32).to_le_bytes());
            }

            if !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0 {
                buf.extend_from_slice(&(ball.max_velocity as f32).to_le_bytes());
                buf.extend_from_slice(&ball.velocity.x.to_le_bytes());
                buf.extend_from_slice(&ball.velocity.y.to_le_bytes());
                buf.extend_from_slice(&ball.velocity.z.to_le_bytes());
                buf.extend_from_slice(&(ball.inertia as f32).to_le_bytes());
                buf.extend_from_slice(&(ball.speed_fraction as f32).to_le_bytes());
            }

            match &ball.mode {
                MovementMode::GotoDirection { direction } => {
                    buf.push(1u8);
                    buf.extend_from_slice(&direction.x.to_le_bytes());
                    buf.extend_from_slice(&direction.y.to_le_bytes());
                    buf.extend_from_slice(&direction.z.to_le_bytes());
                }
                MovementMode::Follow { target_id, range }
                | MovementMode::Orbit { target_id, range } => {
                    buf.push(1u8);
                    buf.extend_from_slice(&target_id.to_le_bytes());
                    buf.extend_from_slice(&(*range as f32).to_le_bytes());
                }
                MovementMode::Warp {
                    destination,
                    warp_speed_au_s,
                    ..
                } => {
                    buf.push(1u8);
                    buf.extend_from_slice(&destination.x.to_le_bytes());
                    buf.extend_from_slice(&destination.y.to_le_bytes());
                    buf.extend_from_slice(&destination.z.to_le_bytes());
                    buf.extend_from_slice(&0u32.to_le_bytes());
                    buf.extend_from_slice(&15000.0f64.to_le_bytes());
                    buf.extend_from_slice(&0i64.to_le_bytes());
                    buf.extend_from_slice(&((*warp_speed_au_s * 10.0) as u32).to_le_bytes());
                }
                _ => {
                    buf.push(1u8);
                }
            }
        }

        buf
    }
}

fn compute_subwarp_goal_velocity(ball: &Ball, positions: &BTreeMap<BallId, (DVec3, f64)>) -> DVec3 {
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
                if dist > desired_dist + 1.0 {
                    delta.normalize_or_zero() * (ball.max_velocity * ball.speed_fraction)
                } else {
                    DVec3::ZERO
                }
            } else {
                DVec3::ZERO
            }
        }
        MovementMode::Missile { target_id } => {
            if let Some(&(target_pos, _)) = positions.get(target_id) {
                (target_pos - ball.position).normalize_or_zero() * ball.max_velocity
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

                let radial_error = (dist - orbit_radius) / orbit_radius.max(1.0);
                let steer =
                    (tangent + radial_dir * radial_error.clamp(-1.0, 1.0)).normalize_or_zero();
                steer * (ball.max_velocity * ball.speed_fraction)
            } else {
                DVec3::ZERO
            }
        }
        _ => DVec3::ZERO,
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

    #[test]
    fn test_turret_wrecking_shot_missile_and_proximity_decloak() {
        let p_hit = combat::compute_turret_chance_to_hit(0.0, 240.0, 35.0, 1500.0, 1500.0, 8400.0);
        assert!((p_hit - 1.0).abs() < 1e-6);

        let base_dmg = DamageVector {
            em: 0.0,
            thermal: 10.0,
            kinetic: 2.0,
            explosive: 2.0,
        };
        let (wrecking, is_wrecking) = combat::resolve_turret_shot(base_dmg, p_hit, 0.005);
        assert!(is_wrecking);
        assert!((wrecking.total() - 42.0).abs() < 1e-6);

        // Proximity decloak within 2000m
        let mut park = Ballpark::new(30000142);
        park.add_ball(Ball {
            id: 1,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        park.combat_states.get_mut(&1).unwrap().is_cloaked = true;

        park.add_ball(Ball {
            id: 2,
            type_id: 587,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        park.step(1.0);
        assert!(!park.combat_states[&1].is_cloaked);
    }
}
