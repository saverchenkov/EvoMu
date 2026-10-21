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
}

fn default_recharge_time_ms() -> f64 {
    120_000.0
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
            shield_resonance: [1.0, 1.0, 1.0, 1.0],
            armor_resonance: [1.0, 1.0, 1.0, 1.0],
            structure_resonance: [1.0, 1.0, 1.0, 1.0],
            shield_recharge_time_ms: 120_000.0,
            capacitor_recharge_time_ms: 120_000.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            signature_radius: 35.0,
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
        }
    }
}

impl CombatState {
    pub fn new() -> Self {
        Self::default()
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
}
