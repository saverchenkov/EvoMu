//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

mod celestials;
pub use celestials::*;
mod cycles;
pub(crate) use cycles::*;

use celestials::{
    build_tick_broadcast, ensure_default_slim_meta, handle_mine_asteroid, respawn_due_asteroids,
};
use eve_destiny::wire::{
    encode_destiny_updates, encode_set_state_payload, encode_set_state_update,
};
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::MissedTickBehavior;

// parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-4, plan:SP-5
// parity: feat:navigation.celestials-balls, feat:navigation.bubbles-grid
// parity: feat:navigation.destiny-updates, feat:serverops.server-tick
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall, destiny:OnSlimItemChange

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

/// DERIVED default maintenance/downtime belt respawn period in seconds (86,400s = 1 day),
/// configurable via `ServerConfig::belt_respawn_interval_secs` (`D-5`, `MN-03`).
/// Per EVE wiki `Asteroids_and_ore.md:69`, belts respawn after downtime only if there are no asteroids left in the belt.
pub const DEFAULT_BELT_RESPAWN_INTERVAL_SECS: u64 = 86_400;

/// Default daily server downtime UTC time of day in seconds (11:00 UTC = 39,600s per EVE wiki `Evergreen_Daily_Login_Rewards.md:21`, `Skillbook_Program.md:43`),
/// configurable via `ServerConfig::downtime_utc_secs_of_day` (`D-5`, `MN-03`).
pub const DEFAULT_DOWNTIME_UTC_SECS_OF_DAY: u64 = 39_600;

/// Computes the strictly next downtime Unix timestamp (`> now_unix`) aligned to `downtime_utc_secs_of_day`
/// across recurring maintenance cycles of `interval_secs` (`MN-03`).
pub fn next_downtime_unix_secs(
    now_unix: u64,
    interval_secs: u64,
    downtime_utc_secs_of_day: u64,
) -> u64 {
    let period = interval_secs.max(1);
    let phase = downtime_utc_secs_of_day % period;
    let rem = now_unix % period;
    let delta = if rem < phase {
        phase - rem
    } else {
        period - (rem - phase)
    };
    now_unix.saturating_add(delta)
}

/// State of an individual harvestable asteroid entity in space (`SP-4`, `MN-01`, `MN-02`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsteroidState {
    pub asteroid_id: BallId,
    pub belt_id: BallId,
    pub type_id: u32,
    pub initial_units: u64,
    pub remaining_units: u64,
    pub unit_volume: f64,
    pub depleted: bool,
    pub respawn_at_tick: Option<u64>,
}

/// Result of an individual mining/harvesting extraction cycle (`MN-01`, `MN-02`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningExtractionResult {
    pub mined_units: u64,
    pub wasted_units: u64,
    pub remaining_units: u64,
    pub ore_type_id: u32,
    pub depleted: bool,
}

#[derive(Debug, Clone)]
pub struct TickBroadcast {
    pub solar_system_id: SolarSystemId,
    pub tick: u64,
    pub events: Vec<DestinyEvent>,
    pub observer_events: BTreeMap<BallId, Vec<DestinyEvent>>,
    pub observer_updates_pyrep: BTreeMap<BallId, PyRep>,
}

#[derive(Debug, Clone, Default)]
pub struct ActiveModuleCycle {
    pub ship_id: BallId,
    pub module_id: i64,
    pub module_type_id: u32,
    pub effect_name: String,
    pub target_id: Option<BallId>,
    pub repeat: i32,
    pub duration_s: f64,
    pub elapsed_s: f64,
    pub cap_need_per_cycle: f64,
    pub speed_boost_factor: f64,
    pub original_max_velocity: Option<f64>,
    pub linked_turrets: Vec<i64>,
    pub ammo_type_id: Option<u32>,
    pub damage_vector: DamageVector,
    pub cap_drain_target: f64,
    pub cap_transfer_target: f64,
    pub shield_boost_amount: f64,
    pub armor_repair_amount: f64,
    pub hull_repair_amount: f64,
    pub remote_shield_amount: f64,
    pub remote_armor_amount: f64,
    pub remote_hull_amount: f64,
    pub remote_cap_amount: f64,
    pub mutadaptive_spool_step: f64,
    pub mutadaptive_spool_max: f64,
    pub current_spool_cycles: u32,
    pub cap_booster_inject_amount: f64,
    pub is_asb: bool,
    pub asb_fallback_cap_need: f64,
    pub is_aar: bool,
    pub aar_paste_rate: i32,
    pub aar_base_armor_repair: f64,
    pub is_rah: bool,
    pub rah_shift_amount: f64,
    pub is_adc: bool,
    pub adc_saved_resists: Option<([f64; 4], [f64; 4], [f64; 4])>,
    pub target_velocity_multiplier: Option<f64>,
    pub target_original_max_velocity: Option<f64>,
    pub immobilize_self: bool,
    pub self_disallow_assistance: bool,
    pub self_ewar_immune: bool,
    pub self_disable_mwd_mjd: bool,
    pub self_mass_multiplier: Option<f64>,
    pub is_auto_targeter: bool,
    pub auto_target_max_range: f64,
    pub auto_target_max_locks: usize,
    pub crystal_volatility_chance: f64,
    pub crystal_volatility_damage: f64,
    pub is_overloaded: bool,
    pub rack_idx: usize,
    pub heat_damage_per_cycle: f64,
    pub reactivation_delay_s: f64,
    pub is_mwd_or_mjd: bool,
    pub warp_scramble_points: i32,
    pub disables_target_mwd_mjd: bool,
    pub warp_core_stab_bonus: i32,
    pub hic_bubble_radius_m: Option<f64>,
    pub hic_velocity_multiplier: Option<f64>,
    pub hic_sig_multiplier: Option<f64>,
    pub hic_mass_addition: Option<f64>,
    pub original_signature_radius: Option<f64>,
    pub original_mass: Option<f64>,
    pub mjd_jump_distance_m: Option<f64>,
    pub grants_interdiction_nullification: bool,
    pub effective_max_capacitor: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ActiveNaniteRepair {
    pub ship_id: BallId,
    pub module_id: i64,
    pub initial_damage_hp: f64,
    pub duration_s: f64,
    pub elapsed_s: f64,
}

#[derive(Debug, Clone)]
pub struct PendingAnchoringDisruptor {
    pub id: BallId,
    pub type_id: u32,
    pub position: DVec3,
    pub radius: f64,
    pub remaining_s: f64,
}

pub enum SystemCommand {
    SpawnBall {
        ball: Ball,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SpawnBallWithCombat {
        ball: Ball,
        combat_state: CombatState,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SpawnBallWithMeta {
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    ChangeSlimItem {
        ball_id: BallId,
        slim_meta: SlimItemMeta,
        reply: oneshot::Sender<bool>,
    },
    SpawnAsteroid {
        asteroid: AsteroidState,
        ball: Ball,
        reply: oneshot::Sender<Result<BallId, String>>,
    },
    MineAsteroid {
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
        reply: oneshot::Sender<Result<MiningExtractionResult, String>>,
    },
    GetAsteroidState {
        asteroid_id: BallId,
        reply: oneshot::Sender<Option<AsteroidState>>,
    },
    BreakGateCloak {
        ball_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    PopulateAsteroids {
        reply: oneshot::Sender<Vec<AsteroidState>>,
    },
    RemoveBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<Ball>>,
    },
    AcceptDocking {
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
        reply: oneshot::Sender<Option<Ball>>,
    },
    EmitPackagedActions {
        ball_id: BallId,
        events: Vec<DestinyEvent>,
        reply: oneshot::Sender<bool>,
    },
    SetMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetWarpScrambleStrength {
        ball_id: BallId,
        strength: i32,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetWarpCoreStrength {
        ball_id: BallId,
        strength: i32,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    AddWarpBubble {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    RemoveWarpBubble {
        id: BallId,
        reply: oneshot::Sender<bool>,
    },
    SetSpeedFraction {
        ball_id: BallId,
        fraction: f64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetBallPosition {
        ball_id: BallId,
        position: glam::DVec3,
        reply: oneshot::Sender<bool>,
    },
    EmitDestinyEvent {
        ball_id: BallId,
        event: DestinyEvent,
        reply: oneshot::Sender<bool>,
    },
    GotoPoint {
        ball_id: BallId,
        point: glam::DVec3,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    PushSetState {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    GetBallSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<(u64, Ball)>>,
    },
    GetBallGroupId {
        ball_id: BallId,
        reply: oneshot::Sender<Option<u32>>,
    },
    ListBalls {
        reply: oneshot::Sender<Vec<Ball>>,
    },
    ListBallsInBubble {
        observer_ball_id: BallId,
        dynamic_only: bool,
        reply: oneshot::Sender<Vec<Ball>>,
    },
    GetSetState {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<(u64, PyRep, PyRep), String>>,
    },
    GetBubbleIdForBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<u64>>,
    },
    DrainObserverUpdates {
        observer_ball_id: BallId,
        reply: oneshot::Sender<(u64, Vec<DestinyEvent>, PyRep)>,
    },
    ApplyDamage {
        ball_id: BallId,
        damage: DamageVector,
        reply: oneshot::Sender<Option<(f64, bool, Ball)>>,
    },
    StepManual {
        dt: f64,
        reply: oneshot::Sender<TickBroadcast>,
    },
    GetTargetLocks {
        ball_id: BallId,
        reply: oneshot::Sender<Vec<TargetLock>>,
    },
    AddTargetLock {
        source_id: BallId,
        lock: TargetLock,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveTargetLock {
        source_id: BallId,
        target_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    ClearTargetLocks {
        source_id: BallId,
        reply: oneshot::Sender<usize>,
    },
    SetCapacitor {
        ball_id: BallId,
        capacitor: f64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    HealCombatState {
        ball_id: BallId,
        reply: oneshot::Sender<Result<CombatState, String>>,
    },
    SetCombatState {
        ball_id: BallId,
        combat_state: CombatState,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetCombatState {
        ball_id: BallId,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    AddModuleCycle {
        cycle: ActiveModuleCycle,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveModuleCycle {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetInterdictionNullified {
        ball_id: BallId,
        nullified: bool,
        reply: oneshot::Sender<bool>,
    },
    MicroJump {
        ball_id: BallId,
        distance_meters: f64,
        reply: oneshot::Sender<Result<DVec3, String>>,
    },
    MicroJumpField {
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
        reply: oneshot::Sender<Result<Vec<BallId>, String>>,
    },
    IsBallScrambled {
        ball_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    AddInterdictionSphere {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<bool>,
    },
    AddMobileWarpDisruptor {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmJam {
        target_id: BallId,
        duration_s: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmBurst {
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
        reply: oneshot::Sender<Vec<BallId>>,
    },
    ScheduleMobileWarpDisruptor {
        id: BallId,
        type_id: u32,
        position: DVec3,
        radius: f64,
        anchoring_delay_s: f64,
        reply: oneshot::Sender<bool>,
    },
    UpdateModuleOverload {
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
        reply: oneshot::Sender<bool>,
    },
    StartNaniteRepair {
        repair: ActiveNaniteRepair,
        reply: oneshot::Sender<()>,
    },
    StopNaniteRepair {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<Option<f64>>,
    },
    RecordIncomingArmorDamage {
        ship_id: BallId,
        damage: [f64; 4],
        reply: oneshot::Sender<()>,
    },
}

pub struct SystemRegistry {
    systems: RwLock<HashMap<SolarSystemId, SystemHandle>>,
    sde: Option<Arc<SdeRegistry>>,
    pub db: Option<Arc<eve_db::WorldRepository>>,
    belt_respawn_interval_secs: Arc<std::sync::atomic::AtomicU64>,
    downtime_utc_secs_of_day: Arc<std::sync::atomic::AtomicU64>,
    /// Game-mechanic RNG (seeded test RNG): OS CSPRNG by default, shared with `ServerContext::mechanic_rng`.
    mechanic_rng: Arc<crate::rng::MechanicRng>,
}

impl Default for SystemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRegistry {
    pub fn new() -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: None,
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_BELT_RESPAWN_INTERVAL_SECS,
            )),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
            )),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
        }
    }

    pub fn with_sde(sde: Arc<SdeRegistry>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_BELT_RESPAWN_INTERVAL_SECS,
            )),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
            )),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
        }
    }

    pub fn with_sde_and_db(sde: Arc<SdeRegistry>, db: Arc<eve_db::WorldRepository>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: Some(db),
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_BELT_RESPAWN_INTERVAL_SECS,
            )),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(
                DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
            )),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
        }
    }

    /// Shares `rng` as this registry's game-mechanic RNG (seeded test RNG).
    pub fn with_mechanic_rng(mut self, rng: Arc<crate::rng::MechanicRng>) -> Self {
        self.mechanic_rng = rng;
        self
    }

    pub fn belt_respawn_interval_secs(&self) -> u64 {
        self.belt_respawn_interval_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_belt_respawn_interval_secs(&self, secs: u64) {
        self.belt_respawn_interval_secs
            .store(secs.clamp(1, 604_800), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn downtime_utc_secs_of_day(&self) -> u64 {
        self.downtime_utc_secs_of_day
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_downtime_utc_secs_of_day(&self, secs: u64) {
        self.downtime_utc_secs_of_day
            .store(secs % 86_400, std::sync::atomic::Ordering::Relaxed);
    }
}

fn sync_ball_positions(ballpark: &Ballpark, cache: &RwLock<HashMap<BallId, DVec3>>) {
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|&id, _| ballpark.balls.contains_key(&id));
    for (&id, ball) in &ballpark.balls {
        guard.insert(id, ball.position);
    }
}

impl SystemRegistry {
    pub fn get_ball_position_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> Option<DVec3> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .and_then(|h| h.get_ball_position_sync(ball_id))
    }

    pub fn find_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        for handle in guard.values() {
            if let Some(pos) = handle.get_ball_position_sync(ball_id) {
                return Some(pos);
            }
        }
        None
    }

    pub fn active_systems_count(&self) -> usize {
        self.systems.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Spawns or retrieves a running Solar System Actor for `solar_system_id`.
    /// Populates static SDE celestials, stargates, and stations (`SP-1`), and hibernates (`LOD 0`)
    /// with zero timer wakes until dynamic balls are spawned on grid.
    pub fn get_or_spawn(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        if let Some(existing) = self
            .systems
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&solar_system_id)
        {
            return existing.clone();
        }

        let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&solar_system_id) {
            return existing.clone();
        }

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SystemCommand>(SYSTEM_MAILBOX_CAPACITY);
        let (event_tx, _) = broadcast::channel::<TickBroadcast>(64);
        let event_tx_actor = event_tx.clone();
        let sde_opt = self.sde.clone();
        let db_opt = self.db.clone();
        let belt_interval_atomic = self.belt_respawn_interval_secs.clone();
        let downtime_utc_atomic = self.downtime_utc_secs_of_day.clone();
        let mechanic_rng = self.mechanic_rng.clone();
        let mut ballpark = Ballpark::new(solar_system_id);
        if let Some(sde) = &sde_opt {
            populate_system_celestials(&mut ballpark, solar_system_id, sde);
        }
        let initial_positions: HashMap<BallId, DVec3> = ballpark
            .balls
            .iter()
            .map(|(&id, b)| (id, b.position))
            .collect();
        let ball_positions = Arc::new(RwLock::new(initial_positions));
        let ball_positions_actor = ball_positions.clone();

        tokio::spawn(async move {
            let mut active_module_cycles: Vec<ActiveModuleCycle> = Vec::new();
            let mut pending_anchoring_disruptors: Vec<PendingAnchoringDisruptor> = Vec::new();
            let mut active_nanite_repairs: Vec<ActiveNaniteRepair> = Vec::new();
            let mut ship_rack_heat: HashMap<BallId, [f64; 3]> = HashMap::new();
            let mut rah_incoming_damage: HashMap<BallId, [f64; 4]> = HashMap::new();
            let mut base_scramble_strengths: HashMap<BallId, i32> = HashMap::new();
            let mut base_core_strengths: HashMap<BallId, i32> = HashMap::new();
            let mut ticker = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut lod_counter: u64 = 0;
            let mut asteroids: HashMap<BallId, AsteroidState> = HashMap::new();
            let mut asteroid_templates: HashMap<BallId, (AsteroidState, Ball)> = HashMap::new();

            loop {
                tokio::select! {
                    biased;
                    _ = ticker.tick(), if ballpark.has_dynamic_balls() && event_tx_actor.receiver_count() > 0 => {
                        lod_counter = lod_counter.wrapping_add(1);
                        let has_player_balls = ballpark
                            .balls
                            .keys()
                            .any(|&id| !ballpark.is_static_ball(id) && id < 700_000_000);
                        let (should_step, dt) = if has_player_balls {
                            (true, 1.0) // Tier 2 Active Grid: 1.0 Hz
                        } else {
                            (lod_counter.is_multiple_of(5), 5.0) // Tier 1 System Presence: 0.2 Hz
                        };
                        if should_step {
                            respawn_due_asteroids(&mut ballpark, &mut asteroids, &asteroid_templates, &sde_opt, &db_opt);
                            sync_player_slim_items(&mut ballpark, &db_opt);
                            let events = ballpark.step(dt);
                            step_cycles_and_capacitor(
                                &mut ballpark,
                                &mut active_module_cycles,
                                &mut pending_anchoring_disruptors,
                                &mut active_nanite_repairs,
                                &mut ship_rack_heat,
                                &mut rah_incoming_damage,
                                &base_scramble_strengths,
                                &base_core_strengths,
                                &db_opt,
                                dt,
                            );
                            sync_ball_positions(&ballpark, &ball_positions_actor);
                            let broadcast = build_tick_broadcast(solar_system_id, &mut ballpark, events);
                            let _ = event_tx_actor.send(broadcast);
                        }
                    }
                    cmd_opt = cmd_rx.recv() => {
                        let Some(cmd) = cmd_opt else {
                            break;
                        };
                        match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                ensure_default_slim_meta(&mut ballpark, &ball, &sde_opt);
                                ballpark.add_ball(ball);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SpawnBallWithCombat {
                                ball,
                                combat_state,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                ensure_default_slim_meta(&mut ballpark, &ball, &sde_opt);
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, combat_state);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SpawnBallWithMeta {
                                ball,
                                combat_state,
                                slim_meta,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                ballpark.set_ball_slim_meta(ball_id, slim_meta);
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, combat_state);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::ChangeSlimItem {
                                ball_id,
                                slim_meta,
                                reply,
                            } => {
                                let changed = ballpark.change_slim_item(ball_id, slim_meta);
                                let _ = reply.send(changed);
                            }
                            SystemCommand::SpawnAsteroid { asteroid, ball, reply } => {
                                let ball_id = asteroid.asteroid_id;
                                if asteroids.contains_key(&ball_id) {
                                    let _ = reply.send(Ok(ball_id));
                                    continue;
                                }
                                let (group_id, category_id) = sde_opt
                                    .as_ref()
                                    .and_then(|s| s.types.get(&ball.type_id))
                                    .map(|t| (t.group_id, t.category_id))
                                    .unwrap_or((25, 25));
                                ballpark.set_ball_slim_meta(
                                    ball_id,
                                    SlimItemMeta {
                                        group_id,
                                        category_id,
                                        owner_id: 1,
                                        char_id: None,
                                        corp_id: -1,
                                        alliance_id: -1,
                                        security_status: 0.0,
                                        is_cloaked: false,
                                    },
                                );
                                ballpark.add_ball(ball.clone());
                                asteroid_templates.insert(ball_id, (asteroid.clone(), ball));
                                asteroids.insert(ball_id, asteroid);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(Ok(ball_id));
                            }
                            SystemCommand::MineAsteroid {
                                asteroid_id,
                                requested_units,
                                waste_probability,
                                reply,
                            } => {
                                let res = handle_mine_asteroid(
                                    solar_system_id,
                                    asteroid_id,
                                    requested_units,
                                    waste_probability,
                                    &mut ballpark,
                                    &mut asteroids,
                                    &mechanic_rng,
                                    &db_opt,
                                    &belt_interval_atomic,
                                    &downtime_utc_atomic,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetAsteroidState { asteroid_id, reply } => {
                                let _ = reply.send(asteroids.get(&asteroid_id).cloned());
                            }
                            SystemCommand::BreakGateCloak { ball_id, reply } => {
                                let mut broken = false;
                                if let Some(cs) = ballpark.combat_states.get_mut(&ball_id) {
                                    if cs.gate_cloak_remaining_ticks > 0 || cs.is_cloaked {
                                        cs.gate_cloak_remaining_ticks = 0;
                                        cs.is_cloaked = false;
                                        broken = true;
                                    }
                                }
                                if let Some(mut meta) = ballpark.ball_slim_meta.get(&ball_id).cloned() {
                                    meta.is_cloaked = false;
                                    ballpark.change_slim_item(ball_id, meta);
                                }
                                let _ = reply.send(broken);
                            }
                            SystemCommand::PopulateAsteroids { reply } => {
                                let mut populated = Vec::new();
                                if let Some(sde) = &sde_opt {
                                    let spawned = populate_asteroid_belt_entities(&mut ballpark, solar_system_id, sde);
                                    for ast in spawned {
                                        if let Some(ball) = ballpark.balls.get(&ast.asteroid_id).cloned() {
                                            asteroid_templates.insert(ast.asteroid_id, (ast.clone(), ball));
                                            asteroids.insert(ast.asteroid_id, ast.clone());
                                            populated.push(ast);
                                        }
                                    }
                                }
                                for ast in asteroids.values() {
                                    if !populated.iter().any(|p| p.asteroid_id == ast.asteroid_id) {
                                        populated.push(ast.clone());
                                    }
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(populated);
                            }
                            SystemCommand::RemoveBall { ball_id, reply } => {
                                base_scramble_strengths.remove(&ball_id);
                                base_core_strengths.remove(&ball_id);
                                asteroids.remove(&ball_id);
                                let docking_evs: Vec<DestinyEvent> = ballpark
                                    .observer_events
                                    .get(&ball_id)
                                    .map(|evs| {
                                        evs.iter()
                                            .filter(|e| matches!(e, DestinyEvent::OnDockingAccepted { .. }))
                                            .cloned()
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let removed = ballpark.remove_ball(ball_id);
                                if !docking_evs.is_empty() {
                                    ballpark.observer_events.insert(ball_id, docking_evs);
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::AcceptDocking {
                                ship_id,
                                station_id,
                                station_pos,
                                reply,
                            } => {
                                base_scramble_strengths.remove(&ship_id);
                                base_core_strengths.remove(&ship_id);
                                asteroids.remove(&ship_id);
                                let removed = if ballpark.balls.contains_key(&station_id) {
                                    ballpark.accept_docking(ship_id, station_id)
                                } else {
                                    ballpark.accept_docking_at(ship_id, station_id, station_pos)
                                };
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::EmitPackagedActions {
                                ball_id,
                                events,
                                reply,
                            } => {
                                let known = ballpark.balls.contains_key(&ball_id);
                                if known {
                                    ballpark.emit_packaged_actions(Some(ball_id), &events);
                                }
                                let _ = reply.send(known);
                            }
                            SystemCommand::SetMovementMode { ball_id, mode, reply } => {
                                if ballpark.balls.contains_key(&ball_id) {
                                    match ballpark.set_mode(ball_id, mode) {
                                        Ok(()) => {
                                            let _ = reply.send(Ok(ballpark.current_tick));
                                        }
                                        Err(e) => {
                                            let _ = reply.send(Err(e));
                                        }
                                    }
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetWarpScrambleStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_scramble_strength(ball_id, strength) {
                                    base_scramble_strengths.insert(ball_id, strength);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        &mut active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::SetWarpCoreStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_core_strength(ball_id, strength) {
                                    base_core_strengths.insert(ball_id, strength);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        &mut active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::AddWarpBubble { id, position, radius, reply } => {
                                ballpark.add_warp_bubble(id, position, radius);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::RemoveWarpBubble { id, reply } => {
                                let removed = ballpark.remove_warp_bubble(id).is_some();
                                let _ = reply.send(removed);
                            }
                            SystemCommand::SetSpeedFraction {
                                ball_id,
                                fraction,
                                reply,
                            } => {
                                if ballpark.set_speed_fraction(ball_id, fraction) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetBallPosition {
                                ball_id,
                                position,
                                reply,
                            } => {
                                let updated = ballpark.set_ball_position(ball_id, position);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(updated);
                            }
                            SystemCommand::EmitDestinyEvent { ball_id, event, reply } => {
                                let known = ballpark.balls.contains_key(&ball_id)
                                    || matches!(event, DestinyEvent::OnSovereigntyChanged { .. });
                                if known {
                                    let scope = ballpark.balls.contains_key(&ball_id).then_some(ball_id);
                                    match event {
                                        DestinyEvent::SetBallFree { ball_id: id, is_free } if id == ball_id => {
                                            ballpark.set_ball_free(id, is_free);
                                        }
                                        DestinyEvent::SetBallHarmonic { ball_id: id, harmonic } if id == ball_id => {
                                            ballpark.set_ball_harmonic(id, harmonic);
                                        }
                                        DestinyEvent::SetBallInteractive { ball_id: id, is_interactive } if id == ball_id => {
                                            ballpark.set_ball_interactive(id, is_interactive);
                                        }
                                        DestinyEvent::SetBallRadius { ball_id: id, radius } if id == ball_id => {
                                            ballpark.set_ball_radius(id, radius);
                                        }
                                        DestinyEvent::OnSovereigntyChanged { alliance_id, .. } => {
                                            ballpark.set_sovereignty(alliance_id);
                                        }
                                        DestinyEvent::OnClientControllerEvent {
                                            ball_id: id,
                                            target_id,
                                            event_name,
                                            value,
                                            delay,
                                        } if id == ball_id => {
                                            ballpark.emit_client_controller_event(
                                                id,
                                                target_id,
                                                event_name,
                                                value,
                                                delay,
                                            );
                                        }
                                        DestinyEvent::OnShipStateUpdate { module_states } => {
                                            ballpark.update_ship_module_states(ball_id, module_states);
                                        }
                                        DestinyEvent::OnDbuffUpdated { ball_id: id, dbuffs } if id == ball_id => {
                                            ballpark.update_dbuffs(id, dbuffs);
                                        }
                                        DestinyEvent::OnDotVictimUpdated {
                                            ball_id: id,
                                            flat_dps,
                                            pct_dps,
                                            remaining_ticks,
                                        } if id == ball_id => {
                                            ballpark.apply_breacher_dot(id, flat_dps, pct_dps, remaining_ticks);
                                        }
                                        DestinyEvent::OnDroneStateChange {
                                            drone_id,
                                            owner_id,
                                            controller_id,
                                            activity_state,
                                            type_id,
                                            controller_owner_id,
                                            target_id,
                                        } if drone_id == ball_id => {
                                            ballpark.update_drone_state(
                                                drone_id,
                                                owner_id,
                                                controller_id,
                                                activity_state,
                                                type_id,
                                                controller_owner_id,
                                                target_id,
                                            );
                                        }
                                        other => {
                                            if let DestinyEvent::OnSlimItemChange { ball_id: slim_id } = &other {
                                                if let Some(meta) = ballpark.ball_slim_meta.get_mut(slim_id) {
                                                    meta.char_id = (*slim_id == ball_id).then_some(meta.owner_id);
                                                }
                                            }
                                            ballpark.emit_event(scope, other);
                                        }
                                    }
                                }
                                let _ = reply.send(known);
                            }
                            SystemCommand::GotoPoint { ball_id, point, reply } => {
                                let res = ballpark
                                    .goto_point(ball_id, point)
                                    .map(|()| ballpark.current_tick);
                                let _ = reply.send(res);
                            }
                            SystemCommand::PushSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.queue_set_state(observer_ball_id) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBallSnapshot { ball_id, reply } => {
                                let res = ballpark.balls.get(&ball_id).cloned().map(|b| (ballpark.current_tick, b));
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetBallGroupId { ball_id, reply } => {
                                let _ = reply.send(ballpark.ball_group_id(ball_id));
                            }
                            SystemCommand::ListBalls { reply } => {
                                let balls: Vec<Ball> = ballpark.balls.values().cloned().collect();
                                let _ = reply.send(balls);
                            }
                            SystemCommand::ListBallsInBubble {
                                observer_ball_id,
                                dynamic_only,
                                reply,
                            } => {
                                let balls: Vec<Ball> = if dynamic_only {
                                    ballpark
                                        .dynamic_balls_in_bubble(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                } else {
                                    ballpark
                                        .visible_balls_for_observer(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                };
                                let _ = reply.send(balls);
                            }
                            SystemCommand::GetSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.balls.contains_key(&observer_ball_id) {
                                    let payload = encode_set_state_payload(&ballpark, observer_ball_id);
                                    let update_list = PyRep::List(vec![encode_set_state_update(
                                        &ballpark,
                                        observer_ball_id,
                                    )]);
                                    let _ = reply.send(Ok((ballpark.current_tick, payload, update_list)));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBubbleIdForBall { ball_id, reply } => {
                                let bid = ballpark.bubble_id_for_ball(ball_id);
                                let _ = reply.send(bid);
                            }
                            SystemCommand::DrainObserverUpdates {
                                observer_ball_id,
                                reply,
                            } => {
                                sync_player_slim_items(&mut ballpark, &db_opt);
                                let events = ballpark.drain_observer_events(observer_ball_id);
                                let stamp = ballpark.current_tick as u32;
                                let pyrep = encode_destiny_updates(stamp, &events, Some(&ballpark));
                                let _ = reply.send((ballpark.current_tick, events, pyrep));
                            }
                            SystemCommand::ApplyDamage { ball_id, damage, reply } => {
                                if let Some((applied, destroyed, ball_opt)) = ballpark.apply_damage_to_ball(ball_id, damage) {
                                    if applied > 0.0 || damage.total() > 0.0 {
                                        let entry = rah_incoming_damage.entry(ball_id).or_insert([0.0; 4]);
                                        entry[0] += damage.em;
                                        entry[1] += damage.thermal;
                                        entry[2] += damage.kinetic;
                                        entry[3] += damage.explosive;
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor);
                                    if let Some(ball_snap) = ball_opt {
                                        let _ = reply.send(Some((applied, destroyed, ball_snap)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else {
                                    let _ = reply.send(None);
                                }
                            }
                            SystemCommand::StepManual { dt, reply } => {
                                respawn_due_asteroids(
                                    &mut ballpark,
                                    &mut asteroids,
                                    &asteroid_templates,
                                    &sde_opt,
                                    &db_opt,
                                );
                                sync_player_slim_items(&mut ballpark, &db_opt);
                                let events = ballpark.step(dt);
                                step_cycles_and_capacitor(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &mut pending_anchoring_disruptors,
                                    &mut active_nanite_repairs,
                                    &mut ship_rack_heat,
                                    &mut rah_incoming_damage,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    dt,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let broadcast = build_tick_broadcast(solar_system_id, &mut ballpark, events);
                                let _ = event_tx_actor.send(broadcast.clone());
                                let _ = reply.send(broadcast);
                            }
                            SystemCommand::GetTargetLocks { ball_id, reply } => {
                                let locks = ballpark.get_target_locks(ball_id);
                                let _ = reply.send(locks);
                            }
                            SystemCommand::AddTargetLock { source_id, lock, reply } => {
                                if let Some(cs) = ballpark.combat_states.get_mut(&source_id) {
                                    cs.undock_invulnerable_remaining_ticks = 0;
                                }
                                ballpark.add_target_lock(source_id, lock);
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::RemoveTargetLock { source_id, target_id, reply } => {
                                let removed = ballpark.remove_target_lock(source_id, target_id);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::ClearTargetLocks { source_id, reply } => {
                                let count = ballpark.clear_target_locks(source_id);
                                let _ = reply.send(count);
                            }
                            SystemCommand::SetCapacitor { ball_id, capacitor, reply } => {
                                let cs = ballpark.combat_states.entry(ball_id).or_default();
                                cs.capacitor = capacitor;
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::HealCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.entry(ball_id).or_default();
                                cs.shield_hp = cs.max_shield_hp;
                                cs.armor_hp = cs.max_armor_hp;
                                cs.structure_hp = cs.max_structure_hp;
                                cs.capacitor = cs.max_capacitor;
                                let _ = reply.send(Ok(cs.clone()));
                            }
                            SystemCommand::SetCombatState { ball_id, combat_state, reply } => {
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                ballpark.combat_states.insert(ball_id, combat_state);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                );
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::GetCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.get(&ball_id).cloned();
                                let _ = reply.send(cs);
                            }
                            SystemCommand::AddModuleCycle { cycle, reply } => {
                                let res = handle_add_module_cycle(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    cycle,
                                );
                                let _ = reply.send(res);
                            }
                            SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
                                if let Some(pos) = active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                                    let removed = active_module_cycles.remove(pos);
                                    cleanup_removed_module_cycle(&mut ballpark, &removed, &db_opt);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        &mut active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                }
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::UpdateModuleOverload {
                                ship_id,
                                module_id,
                                is_overloaded,
                                reply,
                            } => {
                                let mut updated = false;
                                for c in active_module_cycles.iter_mut() {
                                    if c.ship_id == ship_id && c.module_id == module_id {
                                        c.is_overloaded = is_overloaded;
                                        updated = true;
                                    }
                                }
                                let _ = reply.send(updated);
                            }
                            SystemCommand::StartNaniteRepair { repair, reply } => {
                                active_nanite_repairs.retain(|r| {
                                    !(r.ship_id == repair.ship_id && r.module_id == repair.module_id)
                                });
                                active_nanite_repairs.push(repair);
                                let _ = reply.send(());
                            }
                            SystemCommand::StopNaniteRepair {
                                ship_id,
                                module_id,
                                reply,
                            } => {
                                let mut rem = None;
                                if let Some(pos) = active_nanite_repairs
                                    .iter()
                                    .position(|r| r.ship_id == ship_id && r.module_id == module_id)
                                {
                                    let r = active_nanite_repairs.remove(pos);
                                    let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                                    rem = Some(r.initial_damage_hp * (1.0 - frac));
                                }
                                let _ = reply.send(rem);
                            }
                            SystemCommand::RecordIncomingArmorDamage {
                                ship_id,
                                damage,
                                reply,
                            } => {
                                let entry = rah_incoming_damage.entry(ship_id).or_insert([0.0; 4]);
                                entry[0] += damage[0];
                                entry[1] += damage[1];
                                entry[2] += damage[2];
                                entry[3] += damage[3];
                                let _ = reply.send(());
                            }
                            SystemCommand::SetInterdictionNullified { ball_id, nullified, reply } => {
                                let ok = ballpark.set_interdiction_nullified(ball_id, nullified);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::MicroJump { ball_id, distance_meters, reply } => {
                                let res = ballpark.micro_jump(ball_id, distance_meters);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::MicroJumpField {
                                ball_id,
                                distance_meters,
                                field_radius_meters,
                                max_jumped_ships,
                                reply,
                            } => {
                                let res = ballpark.micro_jump_field(
                                    ball_id,
                                    distance_meters,
                                    field_radius_meters,
                                    max_jumped_ships,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::IsBallScrambled { ball_id, reply } => {
                                let is_scram = ballpark.is_ball_scrambled(ball_id);
                                let _ = reply.send(is_scram);
                            }
                            SystemCommand::AddInterdictionSphere { id, position, radius, reply } => {
                                let ok = ballpark.add_interdiction_sphere(id, position, radius);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply } => {
                                let ok = ballpark.add_mobile_warp_disruptor(id, position, radius);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ScheduleMobileWarpDisruptor {
                                id,
                                type_id,
                                position,
                                radius,
                                anchoring_delay_s,
                                reply,
                            } => {
                                let ball = Ball {
                                    id,
                                    type_id,
                                    position,
                                    velocity: DVec3::ZERO,
                                    max_velocity: 0.0,
                                    inertia: 1.0,
                                    mass: 50_000.0,
                                    radius: 70.0,
                                    speed_fraction: 0.0,
                                    mode: MovementMode::Stop,
                                };
                                let ok = ballpark.add_ball(ball);
                                if ok {
                                    ballpark.set_ball_free(id, false);
                                    ballpark.set_ball_radius(id, radius);
                                    ballpark.set_ball_harmonic(id, 1);
                                }
                                pending_anchoring_disruptors.push(PendingAnchoringDisruptor {
                                    id,
                                    type_id,
                                    position,
                                    radius,
                                    remaining_s: anchoring_delay_s.max(1.0),
                                });
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ApplyEcmJam {
                                target_id,
                                duration_s,
                                reply,
                            } => {
                                let target_ewar_immune = active_module_cycles
                                    .iter()
                                    .any(|c| c.ship_id == target_id && c.self_ewar_immune);
                                let exists = ballpark.combat_states.contains_key(&target_id)
                                    && !target_ewar_immune;
                                if exists {
                                    ballpark.apply_ecm_jam(target_id, duration_s);
                                }
                                let _ = reply.send(exists);
                            }
                            SystemCommand::ApplyEcmBurst {
                                source_id,
                                burst_radius_m,
                                jam_strengths,
                                duration_s,
                                roll,
                                reply,
                            } => {
                                let source_pos = ballpark
                                    .balls
                                    .get(&source_id)
                                    .map(|b| b.position)
                                    .unwrap_or(DVec3::ZERO);
                                let ball_meta: HashMap<BallId, (u32, f64)> = ballpark
                                    .balls
                                    .iter()
                                    .map(|(&id, b)| (id, (b.type_id, (b.position - source_pos).length())))
                                    .collect();
                                let ewar_immune_ships: Vec<BallId> = active_module_cycles
                                    .iter()
                                    .filter(|c| c.self_ewar_immune)
                                    .map(|c| c.ship_id)
                                    .collect();
                                let jammed = ballpark.apply_ecm_burst(
                                    source_id,
                                    burst_radius_m,
                                    |cid| {
                                        if ewar_immune_ships.contains(&cid) {
                                            return false;
                                        }
                                        let Some(&(target_type_id, dist)) = ball_meta.get(&cid) else {
                                            return false;
                                        };
                                        if dist > burst_radius_m {
                                            return false;
                                        }
                                        let (sensor_idx, sensor_str) = sde_opt
                                            .as_ref()
                                            .and_then(|sde| sde.types.get(&target_type_id))
                                            .map(|t| {
                                                let grav = t.default_attributes.get(&211).copied().unwrap_or(0.0);
                                                let ladar = t.default_attributes.get(&209).copied().unwrap_or(0.0);
                                                let mag = t.default_attributes.get(&210).copied().unwrap_or(0.0);
                                                let radar = t.default_attributes.get(&208).copied().unwrap_or(0.0);
                                                if grav >= ladar && grav >= mag && grav >= radar && grav > 0.0 {
                                                    (0usize, grav)
                                                } else if ladar >= mag && ladar >= radar && ladar > 0.0 {
                                                    (1usize, ladar)
                                                } else if mag >= radar && mag > 0.0 {
                                                    (2usize, mag)
                                                } else if radar > 0.0 {
                                                    (3usize, radar)
                                                } else {
                                                    (0usize, 10.0)
                                                }
                                            })
                                            .unwrap_or((0usize, 10.0));
                                        let jam_str = jam_strengths[sensor_idx];
                                        let chance = eve_dogma::compute_ecm_jam_chance(
                                            jam_str,
                                            sensor_str,
                                            dist,
                                            burst_radius_m,
                                            0.0,
                                        );
                                        chance > 0.0 && roll < chance
                                    },
                                );
                                for &cid in &jammed {
                                    if let Some(cs) = ballpark.combat_states.get_mut(&cid) {
                                        cs.ecm_jammed_remaining_s = duration_s;
                                    }
                                }
                                let _ = reply.send(jammed);
                            }
                        }
                    }
                }
            }
        });

        let handle = SystemHandle {
            solar_system_id,
            cmd_tx,
            event_tx,
            ball_positions,
        };
        guard.insert(solar_system_id, handle.clone());
        handle
    }
}
