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

pub use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod ballpark;
mod physics;
pub mod wire;
pub use physics::combat;

#[cfg(test)]
mod tests;

pub type BallId = i64;

/// 1 Astronomical Unit in meters (exact EVE constant).
pub const AU_METERS: f64 = 149_597_870_700.0;

/// Standard EVE V24 grid bubble radius in meters (`8,000 km` = `8,000,000 m`).
pub const BUBBLE_RADIUS_METERS: f64 = 8_000_000.0;

/// Bubble transition threshold in meters (`10,000 km` = `10,000,000 m`, `SP-3`).
pub const BUBBLE_TRANSITION_THRESHOLD_METERS: f64 = 10_000_000.0;

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

/// State for a `DSTBALL_MUSHROOM` (`MovementMode::Mushroom`) expanding shockwave ball (`_destiny.so` `AddMushroom` / `Ballpark::Evolve`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MushroomState {
    /// Target maximum radius (`followRange` in `MUSHROOM_Struct`, `_destiny.so` `0x370c6`).
    pub max_radius: f64,
    /// Expansion duration in seconds (`unknown125` in `MUSHROOM_Struct`, `_destiny.so` `0x370d6`).
    pub duration_s: f64,
    /// Simulation tick at which the mushroom started (`effectStamp` in `MUSHROOM_Struct`, `_destiny.so` `0x370d2`).
    pub effect_stamp: u64,
    /// Owner entity ID (`ownerID` in `MUSHROOM_Struct`, `_destiny.so` `0x370e5`).
    pub owner_id: i64,
}

/// State for a `DSTBALL_TROLL` (`MovementMode::Troll`) free coasting ball that freezes into `DSTBALL_RIGID`
/// at `effect_stamp` (`_destiny.so` `SetBallTroll` `0x3cea0`, `Ballpark::Evolve` `0x2d103`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrollState {
    /// Simulation tick (`effectStamp` in `TROLL_Struct`) at which the troll ball becomes `MovementMode::Rigid`.
    pub effect_stamp: u64,
}

/// State for a `DSTBALL_MINIBALL` (`MovementMode::Miniball`) collision sub-sphere attached to a parent ball
/// (`_destiny.so` `AddMiniball` `0x36130`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MiniballState {
    /// Parent ball ID (`owner_id` at `+0x60` in `_destiny.so` `AddMiniball` `0x36243`).
    pub parent_id: BallId,
    /// Offset from the parent ball's center (`+0x120..+0x130` in `_destiny.so` `AddMiniball` `0x36222`).
    pub relative_offset: DVec3,
}

/// State for a `DSTBALL_FORMATION` (`MovementMode::Formation`) formation-following ball
/// (`_destiny.so` `FormationFollow` `0x37a90`, `Ballpark::Evolve` `0x275e5`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FormationState {
    /// Leader ball ID (`followID` in `FORMATION_Struct`, `_destiny.so` `0x37c0e`).
    pub leader_id: BallId,
    /// Slot offset `(right, up, forward)` in the leader's local heading basis (`_destiny.so` `0x27620..0x27739`).
    pub slot_offset: DVec3,
    /// Follow range parameter (`followRange` in `FORMATION_Struct`).
    pub follow_range: f64,
    /// Simulation tick stamp (`effectStamp` in `FORMATION_Struct`).
    pub effect_stamp: u64,
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
    pub const fn new(em: f64, thermal: f64, kinetic: f64, explosive: f64) -> Self {
        Self {
            em,
            thermal,
            kinetic,
            explosive,
        }
    }

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
    /// Shield damage resonance `[EM, Thermal, Kinetic, Explosive]` (`1.0` = 0% resistance).
    pub shield_resonance: [f64; 4],
    /// Armor damage resonance `[EM, Thermal, Kinetic, Explosive]` (`1.0` = 0% resistance).
    pub armor_resonance: [f64; 4],
    /// Structure damage resonance `[EM, Thermal, Kinetic, Explosive]` (`1.0` = 0% resistance).
    pub structure_resonance: [f64; 4],
    #[serde(default = "default_recharge_time_ms")]
    pub shield_recharge_time_ms: f64,
    #[serde(default = "default_recharge_time_ms")]
    pub capacitor_recharge_time_ms: f64,
    pub capacitor: f64,
    pub max_capacitor: f64,
    pub signature_radius: f64,
    #[serde(default)]
    pub base_signature_radius: Option<f64>,
    pub is_cloaked: bool,
    pub is_tethered: bool,
    pub warp_scramble_strength: i32,
    pub warp_core_strength: i32,
    pub entropic_spool_cycles: u32,
    /// Active Breacher Pod DoT `(flat_dmg_per_sec, pct_max_hp_per_sec, remaining_ticks)`.
    pub active_breacher_dot: Option<(f64, f64, u32)>,
    /// Remaining ticks of gate cloak invulnerability (SP-5).
    #[serde(default)]
    pub gate_cloak_remaining_ticks: u32,
    /// Remaining ticks of 30s station undock invulnerability (`feat:navigation.undock`).
    #[serde(default)]
    pub undock_invulnerable_remaining_ticks: u32,
    /// Remaining ticks of 10s post-warp exit invulnerability (`feat:navigation.warp-exit-invulnerability`, `SP-3`).
    #[serde(default)]
    pub warp_exit_invulnerable_remaining_ticks: u32,
    /// Synchronizes external action breaks (`AddModuleCycle` / `AddTargetLock` clearing `undock_invulnerable_remaining_ticks`)
    /// with `warp_exit_invulnerable_remaining_ticks` and `gate_cloak_remaining_ticks`.
    #[serde(default)]
    pub sync_invuln_with_undock_timer: bool,
    #[serde(default)]
    pub is_interdiction_nullified: bool,
    /// Remaining seconds of active ECM jam suppression (`feat:modules.ecm`, `DG-5`).
    #[serde(default)]
    pub ecm_jammed_remaining_s: f64,
    /// True when an active Warp Scrambler or Scripted HIC WDFG disables MWD and MJD (`feat:modules.warp-disruptor`, `feat:modules.warp-disruption-field`).
    #[serde(default)]
    pub is_mwd_mjd_disabled: bool,
    /// True when remote assistance is disallowed, e.g. active unscripted HIC WDFG (`disallowAssistance` attr 854).
    #[serde(default)]
    pub disallow_assistance: bool,
    /// True when an active Covert Ops Cloaking Device II (`11578`) allows warping while cloaked (`feat:modules.covert-ops-cloak`).
    #[serde(default)]
    pub is_covert_cloak: bool,
    /// Remaining seconds of the 15-minute (`900 s`) Cloak Stabilization timer (`3118`, `feat:modules.cloak-stabilization-timer`).
    #[serde(default)]
    pub cloak_stabilization_remaining_s: f64,
    /// Remaining seconds of post-decloak sensor recalibration targeting delay (`560`, `feat:modules.cloak-prototype`, `feat:modules.covert-ops-cloak`).
    #[serde(default)]
    pub decloak_targeting_delay_remaining_s: f64,
    /// Configured sensor recalibration targeting delay in seconds for the currently active cloak (`560`).
    #[serde(default)]
    pub cloak_configured_targeting_delay_s: f64,
    /// Original `max_velocity` before cloak speed penalty was applied.
    #[serde(default)]
    pub cloak_original_max_velocity: Option<f64>,
    /// Remaining seconds of Micro Jump Field Generator (`37479`) spool-up (`feat:modules.micro-jump-field-generator`).
    #[serde(default)]
    pub mjfg_spool_remaining_s: f64,
    /// Original `signature_radius` before MJFG `+150%` signature bloom was applied.
    #[serde(default)]
    pub mjfg_original_sig_radius: Option<f64>,
    /// Capacitor recorded on the previous 1 Hz step to detect MJFG activation on Command Destroyers.
    #[serde(default)]
    pub last_step_capacitor: Option<f64>,
    /// True when a Capital Micro Jump Field Generator (`83485`) is active on a Capital ship.
    #[serde(default)]
    pub is_capital_mjfg: bool,
    /// Active turret tracking speed multiplier (e.g., Abyssal Multibody Tracking Pylon `1.8`).
    #[serde(default = "default_one_f64")]
    pub tracking_speed_multiplier: f64,
    /// Active local shield boost amount multiplier (e.g., Abyssal Filament Cloud `0.6`).
    #[serde(default = "default_one_f64")]
    pub shield_boost_multiplier: f64,
    /// Per-entity deterministic PRNG state (`OF-31`).
    #[serde(default = "default_combat_rng_state")]
    pub rng_state: u64,
}

fn default_recharge_time_ms() -> f64 { 120_000.0 }
fn default_one_f64() -> f64 { 1.0 }
fn default_combat_rng_state() -> u64 { 0x9E37_79B9_7F4A_7C15 }

impl Default for CombatState {
    fn default() -> Self {
        Self {
            shield_hp: 450.0,
            max_shield_hp: 450.0,
            armor_hp: 450.0,
            max_armor_hp: 450.0,
            structure_hp: 350.0,
            max_structure_hp: 350.0,
            shield_resonance: [1.0, 1.0, 1.0, 1.0],
            armor_resonance: [1.0, 1.0, 1.0, 1.0],
            structure_resonance: [1.0, 1.0, 1.0, 1.0],
            shield_recharge_time_ms: 120_000.0,
            capacitor_recharge_time_ms: 120_000.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            signature_radius: 35.0,
            base_signature_radius: None,
            is_cloaked: false,
            is_tethered: false,
            warp_scramble_strength: 0,
            warp_core_strength: 1,
            entropic_spool_cycles: 0,
            active_breacher_dot: None,
            gate_cloak_remaining_ticks: 0,
            undock_invulnerable_remaining_ticks: 0,
            warp_exit_invulnerable_remaining_ticks: 0,
            sync_invuln_with_undock_timer: false,
            is_interdiction_nullified: false,
            ecm_jammed_remaining_s: 0.0,
            is_mwd_mjd_disabled: false,
            disallow_assistance: false,
            is_covert_cloak: false,
            cloak_stabilization_remaining_s: 0.0,
            decloak_targeting_delay_remaining_s: 0.0,
            cloak_configured_targeting_delay_s: 0.0,
            cloak_original_max_velocity: None,
            mjfg_spool_remaining_s: 0.0,
            mjfg_original_sig_radius: None,
            last_step_capacitor: None,
            is_capital_mjfg: false,
            tracking_speed_multiplier: 1.0,
            shield_boost_multiplier: 1.0,
            rng_state: default_combat_rng_state(),
        }
    }
}

impl CombatState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mixes an entity/type seed with `0x9E37_79B9_7F4A_7C15` via SplitMix64, guaranteeing a non-zero `rng_state`.
    pub const fn seed_rng_from_u64(raw: u64) -> u64 {
        let mut z = raw.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let out = z ^ (z >> 31);
        if out == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            out
        }
    }

    pub fn set_interdiction_nullified(&mut self, nullified: bool) {
        self.is_interdiction_nullified = nullified;
    }

    pub fn is_warp_scrambled(&self) -> bool {
        self.warp_scramble_strength > 0 && self.warp_scramble_strength >= self.warp_core_strength
    }

    /// Reconciles external module activation or target lock initiation that cleared `undock_invulnerable_remaining_ticks`.
    pub fn sync_action_breaks(&mut self) {
        if self.sync_invuln_with_undock_timer && self.undock_invulnerable_remaining_ticks == 0 {
            self.warp_exit_invulnerable_remaining_ticks = 0;
            if self.gate_cloak_remaining_ticks > 0 {
                self.gate_cloak_remaining_ticks = 0;
                self.is_cloaked = false;
            }
            self.sync_invuln_with_undock_timer = false;
        }
    }

    /// Returns `true` if the ship is currently inside its 10s post-warp invulnerability window.
    pub fn is_warp_exit_invulnerable(&self) -> bool {
        if self.sync_invuln_with_undock_timer && self.undock_invulnerable_remaining_ticks == 0 {
            return false;
        }
        self.warp_exit_invulnerable_remaining_ticks > 0
    }

    /// Returns `true` if the ship is currently invulnerable due to tether, gate cloak, undock, or post-warp invulnerability.
    pub fn is_invulnerable(&self) -> bool {
        if self.sync_invuln_with_undock_timer && self.undock_invulnerable_remaining_ticks == 0 {
            return self.is_tethered;
        }
        self.is_tethered
            || self.gate_cloak_remaining_ticks > 0
            || self.undock_invulnerable_remaining_ticks > 0
            || self.warp_exit_invulnerable_remaining_ticks > 0
    }

    /// Applies a [`DamageVector`] through Shield -> Armor -> Structure layers using each layer's
    /// 4-channel `[EM, Thermal, Kinetic, Explosive]` resonance profile.
    /// Returns `(total_applied_damage, destroyed)`.
    pub fn apply_damage(&mut self, dmg: DamageVector) -> (f64, bool) {
        self.sync_action_breaks();
        if self.is_tethered
            || self.gate_cloak_remaining_ticks > 0
            || self.undock_invulnerable_remaining_ticks > 0
            || self.warp_exit_invulnerable_remaining_ticks > 0
        {
            return (0.0, false);
        }
        let mut rem_vec = DamageVector {
            em: dmg.em.max(0.0),
            thermal: dmg.thermal.max(0.0),
            kinetic: dmg.kinetic.max(0.0),
            explosive: dmg.explosive.max(0.0),
        };
        if rem_vec.total() <= 0.0 {
            return (0.0, self.structure_hp <= 1e-6);
        }

        let mut total_applied = 0.0;

        // 1. Shield layer
        if self.shield_hp > 0.0 && rem_vec.total() > 0.0 {
            let eff = rem_vec.em * self.shield_resonance[0]
                + rem_vec.thermal * self.shield_resonance[1]
                + rem_vec.kinetic * self.shield_resonance[2]
                + rem_vec.explosive * self.shield_resonance[3];
            if eff > 0.0 {
                if self.shield_hp >= eff {
                    self.shield_hp -= eff;
                    total_applied += eff;
                    rem_vec = DamageVector::default();
                } else {
                    let absorbed_fraction = (self.shield_hp / eff).clamp(0.0, 1.0);
                    total_applied += self.shield_hp;
                    self.shield_hp = 0.0;
                    rem_vec = rem_vec.scaled(1.0 - absorbed_fraction);
                }
            } else {
                rem_vec = DamageVector::default();
            }
        }

        // 2. Armor layer
        if self.armor_hp > 0.0 && rem_vec.total() > 0.0 {
            let eff = rem_vec.em * self.armor_resonance[0]
                + rem_vec.thermal * self.armor_resonance[1]
                + rem_vec.kinetic * self.armor_resonance[2]
                + rem_vec.explosive * self.armor_resonance[3];
            if eff > 0.0 {
                if self.armor_hp >= eff {
                    self.armor_hp -= eff;
                    total_applied += eff;
                    rem_vec = DamageVector::default();
                } else {
                    let absorbed_fraction = (self.armor_hp / eff).clamp(0.0, 1.0);
                    total_applied += self.armor_hp;
                    self.armor_hp = 0.0;
                    rem_vec = rem_vec.scaled(1.0 - absorbed_fraction);
                }
            } else {
                rem_vec = DamageVector::default();
            }
        }

        // 3. Structure layer
        if self.structure_hp > 0.0 && rem_vec.total() > 0.0 {
            let eff = rem_vec.em * self.structure_resonance[0]
                + rem_vec.thermal * self.structure_resonance[1]
                + rem_vec.kinetic * self.structure_resonance[2]
                + rem_vec.explosive * self.structure_resonance[3];
            if eff > 0.0 {
                let take = eff.min(self.structure_hp);
                self.structure_hp -= take;
                total_applied += take;
            }
        }

        let destroyed = self.structure_hp <= 1e-6;
        (total_applied, destroyed)
    }

    /// Applies resistance-ignoring true damage (e.g. Breacher Pod DoT) across Shield -> Armor -> Structure.
    /// Returns `(total_applied_damage, destroyed)`.
    pub fn apply_true_damage(&mut self, amount: f64) -> (f64, bool) {
        self.sync_action_breaks();
        if self.is_tethered
            || self.gate_cloak_remaining_ticks > 0
            || self.undock_invulnerable_remaining_ticks > 0
            || self.warp_exit_invulnerable_remaining_ticks > 0
        {
            return (0.0, false);
        }
        let mut rem = amount.max(0.0);
        if rem <= 0.0 {
            return (0.0, self.structure_hp <= 1e-6);
        }
        let mut total_applied = 0.0;
        if self.shield_hp > 0.0 && rem > 0.0 {
            let take = rem.min(self.shield_hp);
            self.shield_hp -= take;
            total_applied += take;
            rem -= take;
        }
        if self.armor_hp > 0.0 && rem > 0.0 {
            let take = rem.min(self.armor_hp);
            self.armor_hp -= take;
            total_applied += take;
            rem -= take;
        }
        if self.structure_hp > 0.0 && rem > 0.0 {
            let take = rem.min(self.structure_hp);
            self.structure_hp -= take;
            total_applied += take;
        }
        let destroyed = self.structure_hp <= 1e-6;
        (total_applied, destroyed)
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
    SetState {
        observer_ball_id: BallId,
    },
    AddBall {
        ball: Ball,
    },
    AddBalls2 {
        balls: Vec<Ball>,
    },
    RemoveBall {
        ball_id: BallId,
    },
    RemoveBalls {
        ball_ids: Vec<BallId>,
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
    /// Wire action `TerminalPlayDestructionEffect(shipID, destructionEffectId)` (`michelle.pyj:1596`).
    /// `None` makes the client fall back to `destructionEffectType.EXPLOSION` (`michelle.pyj:1609`).
    TerminalExplosion {
        ball_id: BallId,
        destruction_effect_id: Option<i64>,
    },
    Collision {
        ball_a: BallId,
        ball_b: BallId,
    },
    OnTarget {
        mode: String,
        source_id: BallId,
        target_id: BallId,
    },
    OnActivatingWarp {
        ball_id: BallId,
    },
    OnDeactivatingWarp {
        ball_id: BallId,
    },
    OnSpecialFX {
        ball_id: BallId,
        module_id: Option<i64>,
        guid: String,
        is_active: bool,
    },
    CloakBall {
        ball_id: BallId,
        cloaked: u8,
    },
    GotoPoint {
        ball_id: BallId,
        point: [f64; 3],
    },
    OnClientControllerEvent {
        ball_id: BallId,
        target_id: Option<BallId>,
        event_name: String,
        value: i64,
        delay: Option<f64>,
    },
    OnDbuffUpdated {
        ball_id: BallId,
        dbuffs: Vec<(i64, f64, i64)>,
    },
    OnDotVictimUpdated {
        ball_id: BallId,
        flat_dps: f64,
        pct_dps: f64,
        remaining_ticks: u32,
    },
    OnDroneStateChange {
        drone_id: BallId,
        owner_id: i64,
        controller_id: i64,
        activity_state: i64,
        type_id: u32,
        controller_owner_id: i64,
        target_id: Option<BallId>,
    },
    OnFleetDamageStateChange {
        ball_id: BallId,
        shield_ratio: f64,
        armor_ratio: f64,
        structure_ratio: f64,
    },
    OnShipStateUpdate {
        module_states: Vec<(i64, i64, u32, i64, f64, f64)>,
    },
    OnSlimItemChange {
        ball_id: BallId,
    },
    OnSovereigntyChanged {
        solar_system_id: u32,
        alliance_id: i64,
    },
    PackagedAction {
        actions_bytes: Vec<u8>,
    },
    SetBallFree {
        ball_id: BallId,
        is_free: bool,
    },
    SetBallHarmonic {
        ball_id: BallId,
        harmonic: i32,
    },
    SetBallInteractive {
        ball_id: BallId,
        is_interactive: bool,
    },
    SetBallRadius {
        ball_id: BallId,
        radius: f64,
    },
    AddBalls {
        balls: Vec<Ball>,
    },
    OnAudioActivated,
    OnDockingAccepted {
        start_pos: [f64; 3],
        end_pos: [f64; 3],
        station_id: i64,
    },
    OnDroneActivityChange {
        drone_id: BallId,
        activity_state: i64,
    },
    OnDroneControlLost {
        drone_id: BallId,
    },
    OnDroneStateChange2 {
        drone_id: BallId,
        owner_id: i64,
        controller_id: i64,
        activity_state: i64,
        type_id: u32,
        controller_owner_id: i64,
        target_id: Option<BallId>,
    },
    OnFleetStateChange {
        fleet_id: i64,
        member_id: i64,
        fleet_state: i64,
    },
    ScatterEwars {
        ball_id: BallId,
        effect_states: Vec<u8>,
    },
    SetControllerVariableOnBall {
        ball_id: BallId,
        var_name: String,
        value: i64,
    },
}

/// Combat target lock state (`#43`–`#45`, `DG-4`, `DG-5`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetLock {
    pub target_id: BallId,
    pub lock_time_ms: f64,
    pub remaining_lock_ms: f64,
    pub locked: bool,
    #[serde(default)]
    pub max_range_m: Option<f64>,
}

// parity: plan:SP-1, feat:navigation.celestials-balls, plan:SP-2, feat:navigation.bubbles-grid
// parity: plan:P-7, feat:navigation.destiny-updates

/// Metadata used to populate `foo.SlimItem` and `MassSector` in `SetState` and `AddBalls2` (`P-7`, `SP-1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlimItemMeta {
    pub group_id: u32,
    pub category_id: u32,
    pub owner_id: i64,
    pub char_id: Option<i64>,
    pub corp_id: i64,
    pub alliance_id: i64,
    pub security_status: f64,
    pub is_cloaked: bool,
}

impl Default for SlimItemMeta {
    fn default() -> Self {
        Self {
            group_id: 25,
            category_id: 6,
            owner_id: 1,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
        }
    }
}

/// Maximum extent from a grid's original center when expanded by moving entities (`20,000 km`, `Grid.md:13`).
pub const MAX_GRID_EXTENT_METERS: f64 = 20_000_000.0;

/// Proximity threshold (`100 km`) to a grid wall at which non-warping dynamic balls trigger boundary expansion (`Grid.md:13`).
pub const GRID_WALL_EXPANSION_THRESHOLD_METERS: f64 = 100_000.0;

/// Shared spatial grid ("bubble") in a solar system (`SP-2`, `Grid.md`).
///
/// Grids are fixed in space (NOT relative to individual observers), have definite
/// boundaries ("grid walls") at `min_bounds..=max_bounds` (default `8,000 km` in each
/// cardinal direction from `center`, i.e., a `16,000 km` edge cube), and are
/// dynamically created, expanded, contracted, and deleted as dynamic entities move.
/// Two pre-existing grids never merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridBubble {
    pub id: u64,
    pub center: DVec3,
    pub min_bounds: DVec3,
    pub max_bounds: DVec3,
    pub base_radius: f64,
    pub members: BTreeSet<BallId>,
}

impl GridBubble {
    pub fn new(id: u64, center: DVec3, radius: f64) -> Self {
        let half = DVec3::splat(radius);
        Self {
            id,
            center,
            min_bounds: center - half,
            max_bounds: center + half,
            base_radius: radius,
            members: BTreeSet::new(),
        }
    }

    /// Returns `true` if `pos` lies within this grid's fixed 3D boundaries (`min_bounds..=max_bounds`).
    #[inline]
    pub fn contains(&self, pos: DVec3) -> bool {
        pos.x >= self.min_bounds.x
            && pos.x <= self.max_bounds.x
            && pos.y >= self.min_bounds.y
            && pos.y <= self.max_bounds.y
            && pos.z >= self.min_bounds.z
            && pos.z <= self.max_bounds.z
    }

    /// Returns `true` if `[min_b, max_b]` overlaps this grid's 3D bounding box.
    #[inline]
    pub fn overlaps_bounds(&self, min_b: DVec3, max_b: DVec3) -> bool {
        self.min_bounds.x < max_b.x
            && self.max_bounds.x > min_b.x
            && self.min_bounds.y < max_b.y
            && self.max_bounds.y > min_b.y
            && self.min_bounds.z < max_b.z
            && self.max_bounds.z > min_b.z
    }
}

/// A warp disruption bubble in space (e.g. mobile warp disruptor, interdictor probe, or bubble field) (`SP-6`, `Z-3`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarpBubble {
    pub id: BallId,
    pub position: DVec3,
    pub radius: f64,
}

impl WarpBubble {
    pub fn new(id: BallId, position: DVec3, radius: f64) -> Self {
        Self {
            id,
            position,
            radius: radius.max(1.0),
        }
    }

    /// Checks if a point is inside the bubble.
    pub fn contains(&self, point: DVec3) -> bool {
        self.position.distance(point) <= self.radius + 1e-3
    }

    /// Checks if a warp path from `origin` to `destination` intersects this bubble.
    /// If it does, returns the intercept point where the warp path enters the bubble perimeter.
    pub fn intercept_warp(&self, origin: DVec3, destination: DVec3) -> Option<DVec3> {
        let dir = destination - origin;
        let dist = dir.length();
        if dist < 1.0 {
            return None;
        }
        let dir_norm = dir / dist;

        // Vector from origin to bubble center
        let to_center = self.position - origin;
        let proj = to_center.dot(dir_norm);

        // Closest approach distance squared to bubble center
        let d2 = to_center.length_squared() - proj * proj;
        let r2 = self.radius * self.radius;
        if d2 > r2 {
            // Ray does not intersect sphere
            return None;
        }

        // Distance from closest point to sphere perimeter along ray
        let thc = (r2 - d2).max(0.0).sqrt();
        let t0 = proj - thc; // entry point along ray
        let t1 = proj + thc; // exit point along ray

        // If origin is already inside the bubble
        if t0 <= 0.0 && t1 >= 0.0 {
            return Some(origin);
        }

        // Stop bubble (in front of destination): t0 > 0 and t0 <= dist
        // Drag bubble (behind destination inline, within 500 km = 500,000 m): t0 > dist and (t0 - dist) <= 500_000.0
        if t0 > 0.0 && (t0 <= dist || (t0 - dist) <= 500_000.0) {
            Some(origin + dir_norm * t0)
        } else {
            None
        }
    }
}

/// Manages warp disruption bubbles in a solar system / ballpark (`SP-6`, `Z-3`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WarpBubbleMgr {
    pub bubbles: BTreeMap<BallId, WarpBubble>,
}

impl WarpBubbleMgr {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_bubble(&mut self, bubble: WarpBubble) {
        self.bubbles.insert(bubble.id, bubble);
    }

    pub fn remove_bubble(&mut self, id: BallId) -> Option<WarpBubble> {
        self.bubbles.remove(&id)
    }

    pub fn is_scrambled_at(&self, position: DVec3) -> bool {
        self.bubbles.values().any(|b| b.contains(position))
    }

    /// Evaluates if a warp from `origin` to `destination` is intercepted by any bubble.
    /// Returns the earliest intercept point if any bubble intersects the warp path.
    pub fn check_interception(&self, origin: DVec3, destination: DVec3) -> Option<(BallId, DVec3)> {
        let mut earliest: Option<(BallId, DVec3, f64)> = None;

        for bubble in self.bubbles.values() {
            if let Some(intercept) = bubble.intercept_warp(origin, destination) {
                let dist = origin.distance(intercept);
                match &earliest {
                    Some((_, _, min_dist)) if dist >= *min_dist => {}
                    _ => {
                        earliest = Some((bubble.id, intercept, dist));
                    }
                }
            }
        }

        earliest.map(|(id, pos, _)| (id, pos))
    }
}

fn default_constellation_id() -> u32 {
    20_000_020
}

fn default_next_grid_id() -> u64 {
    1
}

/// Represents a solar system's active space simulation instance (`Destiny.Ballpark`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ballpark {
    pub solar_system_id: u32,
    #[serde(default = "default_constellation_id")]
    pub constellation_id: u32,
    pub current_tick: u64,
    pub bubble_radius_meters: f64,
    pub balls: BTreeMap<BallId, Ball>,
    pub combat_states: BTreeMap<BallId, CombatState>,
    pub pending_events: Vec<DestinyEvent>,
    #[serde(default)]
    pub static_ball_ids: BTreeSet<BallId>,
    #[serde(default)]
    pub ball_group_ids: BTreeMap<BallId, u32>,
    #[serde(default)]
    pub ball_slim_meta: BTreeMap<BallId, SlimItemMeta>,
    #[serde(default)]
    pub grids: BTreeMap<u64, GridBubble>,
    #[serde(default)]
    pub ball_grid_ids: BTreeMap<BallId, u64>,
    #[serde(default = "default_next_grid_id")]
    pub next_grid_id: u64,
    #[serde(default)]
    pub observer_events: BTreeMap<BallId, Vec<DestinyEvent>>,
    #[serde(default)]
    pub target_locks: BTreeMap<BallId, Vec<TargetLock>>,
    #[serde(default)]
    pub warp_bubble_mgr: WarpBubbleMgr,
    #[serde(default)]
    pub mushroom_states: BTreeMap<BallId, MushroomState>,
    #[serde(default)]
    pub troll_states: BTreeMap<BallId, TrollState>,
    #[serde(default)]
    pub miniball_states: BTreeMap<BallId, MiniballState>,
    #[serde(default)]
    pub formation_states: BTreeMap<BallId, FormationState>,
    #[serde(default)]
    pub ball_harmonics: BTreeMap<BallId, i32>,
}

impl Ballpark {
    /// Spawns a `DSTBALL_MUSHROOM` (`MovementMode::Mushroom`) expanding shockwave ball (`_destiny.so` `Ballpark::AddMushroom` `0x36fd0`).
    /// Starts at `radius = 0.0` with `velocity = DVec3::ZERO` and expands by $R(t) = R_{\max} (t / T)^{0.25}$ until expiring at $t \ge T$.
    pub fn add_mushroom_ball(&mut self, id: BallId, type_id: u32, position: DVec3, max_radius: f64, duration_s: f64, mass: f64, owner_id: i64) -> bool {
        if duration_s <= 0.0 || !duration_s.is_finite() || self.balls.contains_key(&id) {
            return false;
        }
        let ball = Ball {
            id,
            type_id,
            position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: mass.max(1.0),
            radius: 0.0,
            speed_fraction: 0.0,
            mode: MovementMode::Mushroom,
        };
        self.mushroom_states.insert(
            id,
            MushroomState {
                max_radius: max_radius.max(0.0),
                duration_s,
                effect_stamp: self.current_tick,
                owner_id,
            },
        );
        let added = self.add_ball(ball);
        if let Some(meta) = self.ball_slim_meta.get_mut(&id) {
            meta.owner_id = owner_id;
        }
        added
    }

    /// Configures `ball_id` into `DSTBALL_TROLL` (`MovementMode::Troll`, `_destiny.so` `Ballpark::SetBallTroll` `0x3cea0`):
    /// a free sub-warp coasting ball (`speed_fraction = 0.0`) that freezes into `MovementMode::Rigid` after `duration_ticks`.
    pub fn set_ball_troll(&mut self, ball_id: BallId, duration_ticks: u64) -> Result<u64, String> {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        };
        let effect_stamp = self.current_tick + duration_ticks.max(1);
        ball.mode = MovementMode::Troll;
        ball.speed_fraction = 0.0;
        self.static_ball_ids.remove(&ball_id);
        self.troll_states
            .insert(ball_id, TrollState { effect_stamp });
        Ok(effect_stamp)
    }

    /// Turns `DSTBALL_BOID` (`MovementMode::Boid`) mode on or off for `ball_id` (`_destiny.so` `Ballpark::SetBoid` `0x71870`).
    /// When `enabled` is `false`, transitions `ball_id` to `MovementMode::Stop` (`speed_fraction = 0.0`).
    pub fn set_boid(&mut self, ball_id: BallId, enabled: bool) -> Result<(), String> {
        if !self.balls.contains_key(&ball_id) {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        }
        if enabled {
            if let Some(ball) = self.balls.get_mut(&ball_id) {
                ball.mode = MovementMode::Boid;
            }
            Ok(())
        } else {
            self.set_mode(ball_id, MovementMode::Stop)
        }
    }

    /// Attaches a `DSTBALL_MINIBALL` (`MovementMode::Miniball`) collision sub-sphere to `parent_id` at `relative_offset`
    /// (`_destiny.so` `Ballpark::AddMiniball` `0x36130`).
    pub fn add_miniball(
        &mut self,
        id: BallId,
        parent_id: BallId,
        relative_offset: DVec3,
        radius: f64,
    ) -> Result<(), String> {
        if radius <= 0.0 || !radius.is_finite() {
            return Err("Miniball radius must be positive".into());
        }
        if self.balls.contains_key(&id) {
            return Err(format!("Ball {id} already exists in ballpark"));
        }
        let Some((parent_pos, parent_vel, parent_mass, parent_type)) = self
            .balls
            .get(&parent_id)
            .map(|b| (b.position, b.velocity, b.mass, b.type_id))
        else {
            return Err(format!("Parent ball {parent_id} not found in ballpark"));
        };
        let ball = Ball {
            id,
            type_id: parent_type,
            position: parent_pos + relative_offset,
            velocity: parent_vel,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: parent_mass.max(1.0),
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Miniball,
        };
        self.miniball_states.insert(
            id,
            MiniballState {
                parent_id,
                relative_offset,
            },
        );
        self.add_ball(ball);
        Ok(())
    }

    /// Places `ball_id` into `DSTBALL_FORMATION` (`MovementMode::Formation`) tracking `leader_id` at local `slot_offset`
    /// `(right, up, forward)` in the leader's heading basis (`_destiny.so` `Ballpark::FormationFollow` `0x37a90`).
    pub fn formation_follow(
        &mut self,
        ball_id: BallId,
        leader_id: BallId,
        slot_offset: DVec3,
        follow_range: f64,
    ) -> Result<(), String> {
        if ball_id == leader_id {
            return Err("Ball cannot formation-follow itself".into());
        }
        if !self.balls.contains_key(&leader_id) {
            return Err(format!("Leader ball {leader_id} not found in ballpark"));
        }
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return Err(format!("Ball {ball_id} not found in ballpark"));
        };
        ball.mode = MovementMode::Formation;
        if ball.speed_fraction <= 0.0 {
            ball.speed_fraction = 1.0;
        }
        self.formation_states.insert(
            ball_id,
            FormationState {
                leader_id,
                slot_offset,
                follow_range,
                effect_stamp: self.current_tick,
            },
        );
        Ok(())
    }

    /// Accepts docking of `ship_id` into `station_id` (`destiny:OnDockingAccepted`), emitting
    /// `OnDockingAccepted([ship_x, ship_y, ship_z], [station_x, station_y, station_z], stationID)`
    /// (`Destiny.xmlp:697-715`) to `ship_id` and its grid observers before removing `ship_id` from space.
    pub fn accept_docking(&mut self, ship_id: BallId, station_id: BallId) -> Option<Ball> {
        let station_pos = self
            .balls
            .get(&station_id)
            .map(|b| b.position)
            .unwrap_or(DVec3::ZERO);
        self.accept_docking_at(ship_id, station_id, station_pos)
    }

    /// Accepts docking of `ship_id` into `station_id` at explicit `station_pos` (`destiny:OnDockingAccepted`).
    pub fn accept_docking_at(
        &mut self,
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
    ) -> Option<Ball> {
        let ship_pos = self.balls.get(&ship_id)?.position.to_array();
        let ev = DestinyEvent::OnDockingAccepted {
            start_pos: ship_pos,
            end_pos: station_pos.to_array(),
            station_id,
        };
        self.emit_event(Some(ship_id), ev);
        let saved_obs = self.observer_events.remove(&ship_id).unwrap_or_default();
        let removed = self.remove_ball(ship_id);
        if !saved_obs.is_empty() {
            self.observer_events.insert(ship_id, saved_obs);
        }
        removed
    }

    /// Updates `ball_id`'s force-field harmonic state and emits [`DestinyEvent::SetBallHarmonic`] (`destiny:SetBallHarmonic`).
    pub fn set_ball_harmonic(&mut self, ball_id: BallId, harmonic: i32) -> bool {
        if !self.balls.contains_key(&ball_id) {
            return false;
        }
        self.ball_harmonics.insert(ball_id, harmonic);
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallHarmonic { ball_id, harmonic },
        );
        true
    }

    /// Updates `ball_id`'s free-movement flag and emits [`DestinyEvent::SetBallFree`] (`destiny:SetBallFree`).
    pub fn set_ball_free(&mut self, ball_id: BallId, is_free: bool) -> bool {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        if !is_free {
            ball.velocity = DVec3::ZERO;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
        }
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallFree { ball_id, is_free },
        );
        true
    }

    /// Updates `ball_id`'s interactive flag and emits [`DestinyEvent::SetBallInteractive`] (`destiny:SetBallInteractive`).
    pub fn set_ball_interactive(&mut self, ball_id: BallId, is_interactive: bool) -> bool {
        if !self.balls.contains_key(&ball_id) {
            return false;
        }
        self.emit_event(
            Some(ball_id),
            DestinyEvent::SetBallInteractive {
                ball_id,
                is_interactive,
            },
        );
        true
    }

    /// Packages a batch of inner [`DestinyEvent`]s into marshaled `DoDestinyUpdate` bytes and emits [`DestinyEvent::PackagedAction`] (`destiny:PackagedAction`).
    pub fn emit_packaged_actions(&mut self, ball_id: Option<BallId>, events: &[DestinyEvent]) {
        let inner = crate::wire::encode_destiny_updates(self.current_tick as u32, events, Some(self));
        let actions_bytes = eve_proto::marshal::encode(&inner);
        self.emit_event(ball_id, DestinyEvent::PackagedAction { actions_bytes });
    }

    /// Broadcasts a solar-system sovereignty change (`destiny:OnSovereigntyChanged`).
    pub fn set_sovereignty(&mut self, alliance_id: i64) {
        self.emit_event(
            None,
            DestinyEvent::OnSovereigntyChanged {
                solar_system_id: self.solar_system_id,
                alliance_id,
            },
        );
    }

    /// Emits a client scene controller event on `ball_id` (`destiny:OnClientControllerEvent`).
    pub fn emit_client_controller_event(
        &mut self,
        ball_id: BallId,
        target_id: Option<BallId>,
        event_name: impl Into<String>,
        value: i64,
        delay: Option<f64>,
    ) -> bool {
        if !self.balls.contains_key(&ball_id) {
            return false;
        }
        self.emit_event(
            Some(ball_id),
            DestinyEvent::OnClientControllerEvent {
                ball_id,
                target_id,
                event_name: event_name.into(),
                value,
                delay,
            },
        );
        true
    }

    /// Broadcasts module attribute state updates for `ship_id` (`destiny:OnShipStateUpdate`).
    pub fn update_ship_module_states(
        &mut self,
        ship_id: BallId,
        module_states: Vec<(i64, i64, u32, i64, f64, f64)>,
    ) -> bool {
        if !self.balls.contains_key(&ship_id) {
            return false;
        }
        self.emit_event(
            Some(ship_id),
            DestinyEvent::OnShipStateUpdate { module_states },
        );
        true
    }

    /// Updates dynamic dogma buffs on `ball_id` and emits [`DestinyEvent::OnDbuffUpdated`] (`destiny:OnDbuffUpdated`).
    pub fn update_dbuffs(&mut self, ball_id: BallId, dbuffs: Vec<(i64, f64, i64)>) -> bool {
        if !self.balls.contains_key(&ball_id) {
            return false;
        }
        self.emit_event(
            Some(ball_id),
            DestinyEvent::OnDbuffUpdated { ball_id, dbuffs },
        );
        true
    }

    /// Applies or updates a Breacher Pod DoT on `ball_id` and emits [`DestinyEvent::OnDotVictimUpdated`] (`destiny:OnDotVictimUpdated`).
    pub fn apply_breacher_dot(
        &mut self,
        ball_id: BallId,
        flat_dps: f64,
        pct_dps: f64,
        duration_ticks: u32,
    ) -> bool {
        if !self.balls.contains_key(&ball_id) {
            return false;
        }
        self.combat_states
            .entry(ball_id)
            .or_default()
            .active_breacher_dot = if duration_ticks > 0 {
            Some((flat_dps, pct_dps, duration_ticks))
        } else {
            None
        };
        self.emit_event(
            Some(ball_id),
            DestinyEvent::OnDotVictimUpdated {
                ball_id,
                flat_dps,
                pct_dps,
                remaining_ticks: duration_ticks,
            },
        );
        true
    }

    /// Emits a drone state transition (`destiny:OnDroneStateChange`).
    #[allow(clippy::too_many_arguments)]
    pub fn update_drone_state(
        &mut self,
        drone_id: BallId,
        owner_id: i64,
        controller_id: i64,
        activity_state: i64,
        type_id: u32,
        controller_owner_id: i64,
        target_id: Option<BallId>,
    ) {
        self.emit_event(
            Some(drone_id),
            DestinyEvent::OnDroneStateChange {
                drone_id,
                owner_id,
                controller_id,
                activity_state,
                type_id,
                controller_owner_id,
                target_id,
            },
        );
    }
}
