//! Types, command enum, and state definitions for `system_actor`.

#![forbid(unsafe_code)]

use eve_destiny::{
    Ball, BallId, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::oneshot;

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
    pub owner_char_id: Option<CharacterId>,
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
    pub nosferatu_blood_raider_override: bool,
    pub shield_boost_amount: f64,
    pub armor_repair_amount: f64,
    pub hull_repair_amount: f64,
    pub remote_shield_amount: f64,
    pub remote_armor_amount: f64,
    pub remote_hull_amount: f64,
    pub remote_cap_amount: f64,
    pub remote_optimal: f64,
    pub remote_falloff: f64,
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
    pub ehe_hull_resists: Option<[f64; 4]>,
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
    /// Maximum effective range in meters for targeted modules (`plan:DG-4`).
    /// Deactivates module cycle if target moves outside this range.
    pub max_effective_range_m: Option<f64>,
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

#[derive(Debug, Clone)]
pub enum PendingCycleAction {
    SalvageEnd {
        char_id: CharacterId,
        ship_id: BallId,
        module_id: i64,
        module_type_id: u32,
        wreck_id: BallId,
        repeat: i32,
        effect_name: String,
        next_cycle_cap_deducted: bool,
        deducted_cap_gj: f64,
    },
    RepeatCycleStart {
        char_id: CharacterId,
        ship_id: BallId,
        module_id: i64,
        module_type_id: u32,
        target_id: Option<BallId>,
        duration_s: f64,
        is_overloaded: bool,
        effect_name: String,
    },
}

#[derive(Default)]
pub(crate) struct SystemActorState {
    pub active_module_cycles: Vec<ActiveModuleCycle>,
    pub pending_anchoring_disruptors: Vec<PendingAnchoringDisruptor>,
    pub active_nanite_repairs: Vec<ActiveNaniteRepair>,
    pub ship_rack_heat: HashMap<BallId, [f64; 3]>,
    pub rah_incoming_damage: HashMap<BallId, [f64; 4]>,
    pub base_scramble_strengths: HashMap<BallId, i32>,
    pub base_core_strengths: HashMap<BallId, i32>,
    pub asteroids: HashMap<BallId, AsteroidState>,
    pub asteroid_templates: HashMap<BallId, (AsteroidState, Ball)>,
    pub sde_opt: Option<Arc<SdeRegistry>>,
    pub db_opt: Option<Arc<eve_db::WorldRepository>>,
    pub mechanic_rng_opt: Option<Arc<crate::rng::MechanicRng>>,
    pub sim_elapsed_secs: f64,
    pub pending_cycle_actions: Vec<PendingCycleAction>,
    pub tactical_state: Arc<std::sync::RwLock<crate::services::tactical_modules::TacticalModulesState>>,
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
        reply: oneshot::Sender<(TickBroadcast, Vec<(BallId, Option<CharacterId>)>)>,
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
        reply: oneshot::Sender<Result<Option<f64>, String>>,
    },
    HasModuleCycle {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<bool>,
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
        ship_eval_overrides: Option<HashMap<BallId, (usize, f64, f64)>>,
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
    StopNonWarpModuleCycles {
        ship_id: BallId,
        reply: oneshot::Sender<()>,
    },
}

pub type CharacterId = i64;
pub type NpcDeathFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type NpcDeathHook = Arc<dyn Fn(SolarSystemId, BallId, Option<CharacterId>) -> NpcDeathFuture + Send + Sync>;
pub type ModuleDeactivationHook = Arc<dyn Fn(CharacterId, &ActiveModuleCycle) + Send + Sync>;
pub type SystemStepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type SystemStepHook =
    Arc<dyn Fn(SolarSystemId, f64, Vec<(BallId, Option<CharacterId>)>) -> SystemStepFuture + Send + Sync>;
