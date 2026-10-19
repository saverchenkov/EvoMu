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
use std::collections::{BTreeMap, BTreeSet};

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
    pub gate_cloak_remaining_ticks: u32,
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
        }
    }
}

impl CombatState {
    pub fn is_warp_scrambled(&self) -> bool {
        self.warp_scramble_strength > 0 && self.warp_scramble_strength >= self.warp_core_strength
    }
    /// Applies a [`DamageVector`] through Shield -> Armor -> Structure layers using each layer's
    /// 4-channel `[EM, Thermal, Kinetic, Explosive]` resonance profile.
    /// Returns `(total_applied_damage, destroyed)`.
    pub fn apply_damage(&mut self, dmg: DamageVector) -> (f64, bool) {
        if self.is_tethered || self.gate_cloak_remaining_ticks > 0 {
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
    TerminalExplosion {
        ball_id: BallId,
        wreck_ball_id: BallId,
    },
    Collision {
        ball_a: BallId,
        ball_b: BallId,
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
        self.position.distance(point) <= self.radius
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
    pub warp_bubble_mgr: WarpBubbleMgr,
}

fn default_next_grid_id() -> u64 {
    1
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

    fn queue_event_for_grid_observers(
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
        if let Some(cs) = self.combat_states.get(&ball_id) {
            if cs.is_warp_scrambled() {
                return true;
            }
        }
        if let Some(ball) = self.balls.get(&ball_id) {
            if self.warp_bubble_mgr.is_scrambled_at(ball.position) {
                return true;
            }
        }
        false
    }

    /// Moves `ball_id` to `new_position` and reconciles its [`GridBubble`] membership.
    pub fn set_ball_position(&mut self, ball_id: BallId, new_position: DVec3) -> bool {
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        ball.position = new_position;
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

        // Reject warp if scrambled
        if matches!(mode, MovementMode::Warp { .. }) {
            if self.is_ball_scrambled(ball_id) {
                return Err(
                    "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect"
                        .to_string(),
                );
            }

            // Check bubble interception along the warp path
            if let MovementMode::Warp {
                origin,
                ref mut destination,
                ref mut total_distance,
                ..
            } = mode
            {
                if let Some((_bid, intercept_pos)) =
                    self.warp_bubble_mgr.check_interception(origin, *destination)
                {
                    *destination = intercept_pos;
                    *total_distance = (*destination - origin).length();
                }
            }
        }

        let mut emitted_event: Option<DestinyEvent> = None;
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
                        guid: "effects.WarpDrive".to_string(),
                        is_active: true,
                    };
                    self.pending_events.push(ev_act);
                    self.pending_events.push(ev_sfx);
                }
                _ => {}
            }
            ball.mode = mode;
        if let Some(ev) = emitted_event {
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                self.queue_event_for_grid_observers(gid, None, &ev);
            }
        }
        Ok(())
    }

    pub fn set_speed_fraction(&mut self, ball_id: BallId, fraction: f64) -> bool {
        let clamped = fraction.clamp(0.0, 1.0);
        let Some(ball) = self.balls.get_mut(&ball_id) else {
            return false;
        };
        ball.speed_fraction = clamped;
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
                // In-flight warp bubble interception: if ship enters a bubble during flight, drop out of warp
                if self.warp_bubble_mgr.is_scrambled_at(ball.position) {
                    ball.velocity = DVec3::ZERO;
                    ball.speed_fraction = 0.0;
                    ball.mode = MovementMode::Stop;
                    warp_exit_stops.push(ball.id);
                    continue;
                }

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

                // Celestial spheres, stargates, and stations do not have elastic bounce collision physics
                let group_a = self.ball_group_ids.get(&id_a).copied().unwrap_or(0);
                let group_b = self.ball_group_ids.get(&id_b).copied().unwrap_or(0);
                if matches!(group_a, 6 | 7 | 8 | 9 | 10 | 15) || matches!(group_b, 6 | 7 | 8 | 9 | 10 | 15) {
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
                            if matches!(ba.mode, MovementMode::Warp { .. }) {
                                ba.mode = MovementMode::Stop;
                                ba.speed_fraction = 0.0;
                            }
                        }
                        if let Some(bb) = self.balls.get_mut(&id_b) {
                            bb.position -= normal * (overlap * (mass_a / total_mass));
                            if matches!(bb.mode, MovementMode::Warp { .. }) {
                                bb.mode = MovementMode::Stop;
                                bb.speed_fraction = 0.0;
                            }
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
                            if matches!(ba.mode, MovementMode::Warp { .. }) {
                                ba.mode = MovementMode::Stop;
                                ba.speed_fraction = 0.0;
                            }
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
                            if matches!(bb.mode, MovementMode::Warp { .. }) {
                                bb.mode = MovementMode::Stop;
                                bb.speed_fraction = 0.0;
                            }
                        }
                    }

                    collision_events.push((id_a, id_b));
                }
            }
        }

        for (id_a, id_b) in collision_events {
            let ev = DestinyEvent::Collision { ball_a: id_a, ball_b: id_b };
            self.pending_events.push(ev.clone());
            if let Some(&gid) = self.ball_grid_ids.get(&id_a).or_else(|| self.ball_grid_ids.get(&id_b)) {
                self.queue_event_for_grid_observers(gid, None, &ev);
            }
        }

        // Gate cloak decay and break on active movement (`SP-5`)
        for (&id, cs) in &mut self.combat_states {
            if cs.gate_cloak_remaining_ticks > 0 {
                let has_moved = self.balls.get(&id).is_some_and(|b| {
                    !matches!(b.mode, MovementMode::Stop) || b.speed_fraction > 0.0 || b.velocity.length_squared() > 1.0
                });
                if has_moved {
                    cs.gate_cloak_remaining_ticks = 0;
                    cs.is_cloaked = false;
                } else {
                    cs.gate_cloak_remaining_ticks = cs.gate_cloak_remaining_ticks.saturating_sub(1);
                    if cs.gate_cloak_remaining_ticks == 0 {
                        cs.is_cloaked = false;
                    } else {
                        cs.is_cloaked = true;
                    }
                }
            }
        }

        // Check 2,000m surface-to-surface proximity decloak and active Breacher Pod DoT
        let updated_positions: Vec<(BallId, DVec3, f64)> = self
            .balls
            .values()
            .map(|b| (b.id, b.position, b.radius))
            .collect();

        for &(id_a, pos_a, rad_a) in &updated_positions {
            if let Some(cs) = self.combat_states.get_mut(&id_a) {
                if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
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
                self.observer_events
                    .entry(obs_id)
                    .or_default()
                    .push(DestinyEvent::AddBalls2 { balls: entered });
            }
            if !left.is_empty() {
                self.observer_events
                    .entry(obs_id)
                    .or_default()
                    .push(DestinyEvent::RemoveBalls { ball_ids: left });
            }
        }

        for ball_id in warp_exit_stops {
            let ev = DestinyEvent::Stop { ball_id };
            let ev_deact = DestinyEvent::OnDeactivatingWarp { ball_id };
            let ev_sfx = DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: None,
                guid: "effects.WarpDrive".to_string(),
                is_active: false,
            };
            self.pending_events.push(ev.clone());
            self.pending_events.push(ev_deact.clone());
            self.pending_events.push(ev_sfx.clone());
            if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
                self.queue_event_for_grid_observers(gid, None, &ev);
                self.queue_event_for_grid_observers(gid, None, &ev_deact);
                self.queue_event_for_grid_observers(gid, None, &ev_sfx);
            }
        }

        std::mem::take(&mut self.pending_events)
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

/// Binary `Destiny` wire-format encoder/decoder (`AddBalls` state buffer) and `DoDestinyUpdate` marshaling (`P-7`).
pub mod wire;

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
        MovementMode::Boid | MovementMode::Formation => {
            let dir = ball.velocity.normalize_or_zero();
            let heading = if dir == DVec3::ZERO { DVec3::X } else { dir };
            heading * (ball.max_velocity * ball.speed_fraction.max(0.5))
        }
        MovementMode::Mushroom | MovementMode::Troll | MovementMode::Miniball => {
            ball.velocity * 0.25
        }
        MovementMode::Stop | MovementMode::Warp { .. } | MovementMode::Field | MovementMode::Rigid => {
            DVec3::ZERO
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

    #[test]
    // parity: plan:SP-2, feat:navigation.bubbles-grid, destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
    fn test_spatial_bubbles_10000km_isolation_and_warp_transitions() {
        let mut park = Ballpark::new(30_000_142);
        assert_eq!(park.bubble_radius_meters, 8_000_000.0);

        // Static star at origin (system-wide)
        park.add_static_ball(
            Ball {
                id: 40_009_076,
                type_id: 3802,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e30,
                radius: 513_100_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            6,
        );

        // Ship A at origin creates Grid 1 (fixed bounds [-8,000 km, +8,000 km]).
        // Ship B at +10,000 km is outside Grid 1 -> creates Grid 2 clamped against Grid 1's wall at x = +8,000 km.
        // Ship C at +5,000 km and Ship D at -5,000 km are both inside Grid 1 (even though C and D are 10,000 km apart!).
        let ship_a = Ball {
            id: 100_000_001,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(365.0, 0.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let ship_b = Ball {
            id: 100_000_002,
            type_id: 587,
            position: DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let ship_c = Ball {
            id: 100_000_003,
            type_id: 587,
            position: DVec3::new(5_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let ship_d = Ball {
            id: 100_000_004,
            type_id: 587,
            position: DVec3::new(-5_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        park.add_ball(ship_a);
        park.add_ball(ship_b);
        park.add_ball(ship_c);
        park.add_ball(ship_d);

        // Verify fixed shared GridBubbles (not observer-relative spheres):
        let gid_a = park.bubble_id_for_ball(100_000_001).unwrap();
        let gid_b = park.bubble_id_for_ball(100_000_002).unwrap();
        let gid_c = park.bubble_id_for_ball(100_000_003).unwrap();
        let gid_d = park.bubble_id_for_ball(100_000_004).unwrap();
        assert_eq!(gid_a, gid_c);
        assert_eq!(gid_a, gid_d);
        assert_ne!(gid_a, gid_b);

        // Transitivity: Ship C (+5,000 km) and Ship D (-5,000 km) are 10,000 km apart from each other,
        // yet both are inside Grid 1 ([-8,000 km, +8,000 km]) so they see each other!
        assert!(park.in_same_bubble(100_000_003, 100_000_004));
        // Grid wall: Ship C (+5,000 km, in Grid 1) and Ship B (+10,000 km, in Grid 2) are only 5,000 km apart,
        // yet separated by the fixed grid wall at x = +8,000 km so they do NOT see each other!
        assert!(!park.in_same_bubble(100_000_003, 100_000_002));
        assert!(!park.in_same_bubble(100_000_001, 100_000_002));

        let vis_a: Vec<BallId> = park
            .visible_balls_for_observer(100_000_001)
            .iter()
            .map(|b| b.id)
            .collect();
        assert!(vis_a.contains(&40_009_076), "Static star must be visible");
        assert!(vis_a.contains(&100_000_001), "Self must be visible");
        assert!(vis_a.contains(&100_000_003), "Ship C (+5,000 km) must be visible");
        assert!(vis_a.contains(&100_000_004), "Ship D (-5,000 km) must be visible");
        assert!(
            !vis_a.contains(&100_000_002),
            "Ship B (+10,000 km, Grid 2) must NOT be visible in Grid 1"
        );

        let _ = park.drain_all_observer_events();

        // Removing Ship D from Grid 1 emits RemoveBall to remaining members of Grid 1 (A and C), not Grid 2 (B)
        park.remove_ball(100_000_004);
        let ev_a_rem = park.drain_observer_events(100_000_001);
        let ev_b_rem = park.drain_observer_events(100_000_002);
        assert!(ev_a_rem
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id: 100_000_004 })));
        assert!(ev_b_rem.is_empty());
        let _ = park.drain_all_observer_events();

        // Ship A issues GotoDirection -> Ship C receives it, Ship B (in Grid 2) does NOT
        park.set_mode(
            100_000_001,
            MovementMode::GotoDirection {
                direction: DVec3::X,
            },
        )
        .expect("set GotoDirection");
        let ev_b = park.drain_observer_events(100_000_002);
        let ev_c = park.drain_observer_events(100_000_003);
        assert!(
            ev_b.is_empty(),
            "Ship B in Grid 2 must not receive Ship A's local movement update"
        );
        assert!(
            ev_c.iter()
                .any(|e| matches!(e, DestinyEvent::GotoDirection { ball_id: 100_000_001, .. })),
            "Ship C in same grid must receive Ship A's movement update"
        );

        // Now Ship A warps across the grid wall into Grid 2 (to 20,000 km)
        let _ = park.drain_all_observer_events();
        let far_dest = DVec3::new(20_000_000.0, 0.0, 0.0);
        park.set_ball_position(100_000_002, far_dest);
        park.set_mode(
            100_000_001,
            MovementMode::new_warp(DVec3::ZERO, far_dest, 3.0),
        )
        .expect("set warp");
        let _ = park.drain_all_observer_events();

        let mut b_saw_add = false;
        let mut c_saw_remove = false;
        for _ in 0..30 {
            park.step(1.0);
            for ev in park.drain_observer_events(100_000_002) {
                if let DestinyEvent::AddBalls2 { balls } = ev {
                    if balls.iter().any(|b| b.id == 100_000_001) {
                        b_saw_add = true;
                    }
                }
            }
            for ev in park.drain_observer_events(100_000_003) {
                if let DestinyEvent::RemoveBalls { ball_ids } = ev {
                    if ball_ids.contains(&100_000_001) {
                        c_saw_remove = true;
                    }
                }
            }
        }
        assert!(
            b_saw_add,
            "Ship B must receive AddBalls2 when Ship A warps into Grid 2"
        );
        assert!(
            c_saw_remove,
            "Ship C must receive RemoveBalls when Ship A warps out of Grid 1"
        );
        assert_eq!(
            park.bubble_id_for_ball(100_000_001),
            park.bubble_id_for_ball(100_000_002)
        );

        // Approaching the +x wall (7,950 km) where Grid 2 starts at 8,000,001 m must NOT overlap Grid 2 (`Grid.md:15`)
        if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
            c_ball.position = DVec3::new(7_950_000.0, 0.0, 0.0);
        }
        park.step(1.0);
        assert_eq!(
            park.grids[&gid_a].max_bounds.x,
            8_000_000.0,
            "Grid 1 +x wall adjacent to Grid 2 must not overlap Grid 2 (Grid.md:15)"
        );

        // Sub-warp proximity within 100 km of Grid 1's unobstructed +y (+8,000 km) wall expands max_bounds.y to pos.y + 100 km (`Grid.md:13`)
        if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
            c_ball.position = DVec3::new(0.0, 7_950_000.0, 0.0);
        }
        park.step(1.0);
        assert_eq!(
            park.grids[&gid_a].max_bounds.y,
            8_050_000.0,
            "Approaching within 100 km of the +8,000 km +y grid wall must expand max_bounds.y to 8,050 km"
        );
        // Moving back inside contracts max_bounds.y back to base_radius (8,000 km)
        if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
            c_ball.position = DVec3::new(0.0, 5_000_000.0, 0.0);
        }
        park.step(1.0);
        assert_eq!(
            park.grids[&gid_a].max_bounds.y,
            8_000_000.0,
            "Retreating from the expanded wall must contract max_bounds.y back to 8,000 km"
        );

        // Removing the last ship in Grid 1 (Ship C) deletes Grid 1 (`RemoveEmpty`)
        park.remove_ball(100_000_003);
        assert!(!park.grids.contains_key(&gid_a), "Empty grid must be deleted");
    }

    #[test]
    // parity: plan:SP-4, destiny:Ball::Collision
    fn test_destiny_collision_bounce_physics_and_gate_cloak() {
        let mut park = Ballpark::new(30_000_142);

        // Dynamic ship 1 moving along +X towards static asteroid
        let ship1 = Ball {
            id: 2001,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(15.0, 0.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.1,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        };
        // Static asteroid at x = 80m, radius 50m (overlap: 50 + 50 - 80 = 20m)
        let asteroid = Ball {
            id: 9001,
            type_id: 1230,
            position: DVec3::new(80.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 10_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        };

        park.add_ball(ship1);
        park.add_static_ball(asteroid, 25);

        // Step physics -> collision bounce
        let events = park.step(1.0);
        let has_collision = events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if (*ball_a == 2001 && *ball_b == 9001) || (*ball_a == 9001 && *ball_b == 2001)));
        assert!(has_collision, "Must emit DestinyEvent::Collision upon collision");

        let b1 = park.balls.get(&2001).unwrap();
        assert!(b1.velocity.x < 0.0, "Velocity must bounce back after colliding with static asteroid, got {}", b1.velocity.x);

        // Gate cloak test (SP-5): ship with gate cloak is invulnerable and cloaked for 60s
        let mut cs = CombatState::default();
        cs.gate_cloak_remaining_ticks = 60;
        let (applied, destroyed) = cs.apply_damage(DamageVector { em: 100.0, thermal: 100.0, kinetic: 100.0, explosive: 100.0 });
        assert_eq!(applied, 0.0);
        assert!(!destroyed);
    }
}

