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
    pub ehe_hull_resists: Option<[f64; 4]>,
    pub target_velocity_multiplier: Option<f64>,
    pub immobilize_self: bool,
    pub self_disallow_assistance: bool,
    pub self_ewar_immune: bool,
    pub self_disable_mwd_mjd: bool,
    pub self_mass_multiplier: Option<f64>,
    pub is_auto_targeter: bool,
    pub auto_target_max_range: f64,
    pub auto_target_max_locks: usize,
    pub auto_target_scan_res: f64,
    pub crystal_volatility_chance: f64,
    pub crystal_volatility_damage: f64,
    pub is_overloaded: bool,
    pub pending_overloaded: Option<bool>,
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
    pub mjd_jump_distance_m: Option<f64>,
    pub mjfg_field_params: Option<(f64, usize, usize)>,
    pub grants_interdiction_nullification: bool,
    pub effective_max_capacitor: Option<f64>,
    /// Maximum effective range in meters for targeted modules (`plan:DG-4`).
    /// Deactivates module cycle if target moves outside this range.
    pub max_effective_range_m: Option<f64>,
    pub lance_beam_dir: Option<DVec3>,
    pub grappler_optimal_m: f64,
    pub grappler_falloff_m: f64,
    pub grappler_speed_factor_pct: f64,
    pub weapon_is_turret: bool,
    pub weapon_is_missile: bool,
    pub weapon_optimal_m: f64,
    pub weapon_falloff_m: f64,
    pub weapon_tracking_speed: f64,
    pub weapon_sig_resolution_m: f64,
    pub missile_explosion_radius_m: f64,
    pub missile_explosion_velocity_ms: f64,
    pub missile_drf: f64,
    /// Unix timestamp (seconds) when this module cycle was added or began its current cycle (`#498`).
    pub started_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct ActiveNaniteRepair {
    pub char_id: CharacterId,
    pub ship_id: BallId,
    pub module_id: i64,
    pub initial_damage_hp: f64,
    pub paste_total_needed: i32,
    pub paste_consumed_so_far: i32,
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
    LanceBeamFired {
        attacker_char_id: CharacterId,
        attacker_ship_id: BallId,
        attacker_ship_type_id: u32,
        module_id: i64,
        module_type_id: u32,
        hits: Vec<(BallId, f64)>,
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
    SyncOverloadState {
        char_id: Option<CharacterId>,
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
    },
    InvalidateShipFitting {
        char_id: Option<CharacterId>,
        ship_id: BallId,
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
    pub pending_notifications_opt: Option<Arc<std::sync::RwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>>,
    pub notify_tx_opt: Option<tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
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
    SetBallWormholeStages {
        ball_id: BallId,
        wormhole_age: i64,
        wormhole_size: f64,
        reply: oneshot::Sender<bool>,
    },
    SetBallMaxVelocity {
        ball_id: BallId,
        max_velocity: f64,
        reply: oneshot::Sender<bool>,
    },
    SetBallMaxVelocityCap {
        ball_id: BallId,
        cap: Option<f64>,
        reply: oneshot::Sender<bool>,
    },
    GetBallSlimItem {
        ball_id: BallId,
        reply: oneshot::Sender<Option<eve_proto::PyRep>>,
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
        persist_db: bool,
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
    ApplyTrueDamage {
        ball_id: BallId,
        amount: f64,
        reply: oneshot::Sender<Option<(f64, bool, Ball)>>,
    },
    StepManual {
        dt: f64,
        reply: oneshot::Sender<(TickBroadcast, Vec<(BallId, Option<CharacterId>)>)>,
    },
    FastForwardClock {
        ship_id: BallId,
        secs: u64,
        reply: oneshot::Sender<()>,
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
        reply: oneshot::Sender<Option<(f64, i32)>>,
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
    FlushPeerObserverUpdates {
        exclude_ball_id: Option<BallId>,
        reply: oneshot::Sender<usize>,
    },
}

pub type CharacterId = i64;
pub type NpcDeathFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type NpcDeathHook = Arc<dyn Fn(SolarSystemId, BallId, Option<CharacterId>) -> NpcDeathFuture + Send + Sync>;
pub type ModuleDeactivationHook = Arc<dyn Fn(CharacterId, &ActiveModuleCycle) + Send + Sync>;
pub type TargetLockCompletedHook = Arc<dyn Fn(CharacterId, BallId) + Send + Sync>;
pub type SystemStepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type SystemStepHook =
    Arc<dyn Fn(SolarSystemId, f64, Vec<(BallId, Option<CharacterId>)>) -> SystemStepFuture + Send + Sync>;

/// DERIVED: Default active-entity + module-cycle load units per worker node before load-induced Time Dilation engages.
pub const DEFAULT_NODE_LOAD_TIDI_THRESHOLD: usize = 80;

/// DERIVED: Runtime worker-node assignment, fleet-fight reinforcement, and simulation clock rebase tracking
/// (`feat:serverops.node-balancing`, `feat:serverops.time-dilation`, `notify:DoSimClockRebase`).
/// Automatic load-induced TiDi is disabled by default (`auto_tidi_enabled = false`) and must be explicitly
/// enabled by operator/GM command (`set_auto_cluster_tidi_enabled`).
#[derive(Debug, Clone)]
pub struct ClusterNodeRegistry {
    /// DERIVED: Default general-purpose worker node pool (`[1, 2, 3, 4]`).
    pub worker_nodes: Vec<u32>,
    pub system_node_overrides: HashMap<SolarSystemId, u32>,
    /// Dedicated reinforced nodes (`node_id -> exclusive solar_system_id`).
    pub reinforced_nodes: HashMap<u32, SolarSystemId>,
    /// Explicit GM/operator Time Dilation overrides (`solar_system_id -> factor`).
    pub manual_tidi_overrides: HashMap<SolarSystemId, f64>,
    /// Last emitted authoritative destiny `(anchor_wall_ft, anchor_sim_ft, tidi_factor)` per system.
    pub sim_clock_anchoring: HashMap<SolarSystemId, (i64, i64, f64)>,
    pub node_load_tidi_threshold: usize,
    pub next_dedicated_node_id: u32,
    pub auto_tidi_enabled: bool,
    pub scheduled_downtime_enabled: bool,
}

impl Default for ClusterNodeRegistry {
    fn default() -> Self {
        Self {
            worker_nodes: vec![1, 2, 3, 4],
            system_node_overrides: HashMap::new(),
            reinforced_nodes: HashMap::new(),
            manual_tidi_overrides: HashMap::new(),
            sim_clock_anchoring: HashMap::new(),
            node_load_tidi_threshold: DEFAULT_NODE_LOAD_TIDI_THRESHOLD,
            next_dedicated_node_id: 100,
            auto_tidi_enabled: false,
            scheduled_downtime_enabled: true,
        }
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;


