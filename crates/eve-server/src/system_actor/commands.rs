//! Actor mailbox command enum and test access closures for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{CombatState, DamageVector, MovementMode, TargetLock};
use std::collections::HashSet;
use tokio::sync::oneshot;

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
    SpawnBallWithMetaTagged {
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        spawn_token: u64,
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
    /// Atomically removes `ball_id` only when its active spawn token matches `spawn_token`.
    RemoveBallIfToken {
        ball_id: BallId,
        spawn_token: u64,
        reply: oneshot::Sender<Option<Ball>>,
    },
    AcceptDocking {
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
        reply: oneshot::Sender<Option<Ball>>,
    },
    TakeBall {
        ball_id: BallId,
        accept_docking_target: Option<(BallId, DVec3)>,
        preserve_db_active_states: bool,
        reply: oneshot::Sender<Option<TakenBall>>,
    },
    RestoreBall {
        taken: TakenBall,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    /// Atomically restores `taken` only when `taken.ball.id` is absent from `ballpark.balls`.
    RestoreBallIfAbsent {
        taken: TakenBall,
        reply: oneshot::Sender<Result<u64, String>>,
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
    SetWarpMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        provenance: WarpProvenance,
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
    EmitObserverDestinyEvent {
        observer_ball_id: BallId,
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
    TrackModuleReloadSim {
        module_id: i64,
        ship_id: BallId,
        remaining_sim_s: f64,
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
    /// Like [`GetSetState`], and also returns the actor's `TickBroadcast.broadcast_seq`
    /// watermark at the instant the `SetState` snapshot was encoded (`#1461`).
    GetSetStateWithSeq {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<(u64, PyRep, PyRep, u64), String>>,
    },
    CurrentTick {
        reply: oneshot::Sender<u64>,
    },
    GetBubbleIdForBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<u64>>,
    },
    DrainObserverUpdates {
        observer_ball_id: BallId,
        reply: oneshot::Sender<(u64, Vec<DestinyEvent>, PyRep)>,
    },
    DrainObserverUpdatesWithSeq {
        observer_ball_id: BallId,
        reply: oneshot::Sender<(u64, Vec<DestinyEvent>, PyRep, u64)>,
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
        reply: oneshot::Sender<StepOutput>,
    },
    StepManualTickOnly {
        dt: f64,
        reply: oneshot::Sender<StepOutput>,
    },
    FlushConditionCheckpoint {
        reply: oneshot::Sender<usize>,
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
    AddTargetLockBreakingProtection {
        source_id: BallId,
        lock: TargetLock,
        reply: oneshot::Sender<Result<ProtectionBreakEvents, String>>,
    },
    RemoveTargetLock {
        source_id: BallId,
        target_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    RemoveTargetLockQuiet {
        source_id: BallId,
        target_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    ClearTargetLocks {
        source_id: BallId,
        reply: oneshot::Sender<usize>,
    },
    ClearTargetLocksQuiet {
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
    ModifyCombatState {
        ball_id: BallId,
        f: Box<dyn FnOnce(&mut CombatState) + Send>,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    RepairCombatState {
        ball_id: BallId,
        hull_heal: f64,
        armor_heal: f64,
        shield_heal: f64,
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
    RequestCycleDeactivation {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<bool>,
    },
    FireWeaponCycleVolley {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<WeaponVolleyOutput>,
    },
    HasModuleCycle {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<bool>,
    },
    ReconcileModuleCycleBeforeActivate {
        ship_id: BallId,
        module_id: i64,
        now_unix: u64,
        reply: oneshot::Sender<(Option<(i32, Option<BallId>)>, bool)>,
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
    ApplyEcmJamFrom {
        source_id: BallId,
        target_id: BallId,
        duration_s: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmJamFromModule {
        source_id: BallId,
        module_id: i64,
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
    CancelShipNaniteRepairs {
        ship_id: BallId,
        refund_paste: bool,
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
    ApplyFitProjection {
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
        reply: Option<oneshot::Sender<bool>>,
    },
    BreakProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<ProtectionBreakEvents, String>>,
    },
    ForceDecloak {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<ProtectionBreakEvents, String>>,
    },
    GetProtectionSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<ActorShipProtection>>,
    },
    ActivateModuleCycleAtomic {
        request: AtomicActivateCycleRequest,
        reply: oneshot::Sender<Result<AtomicActivateCycleOutcome, String>>,
    },
    CommitLancerActivationAtomic {
        request: AtomicLancerActivationRequest,
        reply: oneshot::Sender<Result<AtomicActivateCycleOutcome, String>>,
    },
    AddCapacitorClamped {
        ball_id: BallId,
        delta: f64,
        reply: oneshot::Sender<Result<f64, String>>,
    },
    SetMovementModeBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    SetWarpMovementModeBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        provenance: WarpProvenance,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    SetSpeedFractionBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        fraction: f64,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    GotoPointBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        point: glam::DVec3,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    ConvertBallToRigidAt {
        ball_id: BallId,
        position: DVec3,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    SetBallTypeId {
        ball_id: BallId,
        type_id: u32,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    SetCombatStateHpBounds {
        ball_id: BallId,
        shield_hp: f64,
        max_shield_hp: f64,
        armor_hp: f64,
        max_armor_hp: f64,
        structure_hp: f64,
        max_structure_hp: f64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    ApplyNpcAiStepEffects {
        mechanic_rng: Arc<crate::rng::MechanicRng>,
        target_sigs: HashMap<BallId, f64>,
        mode_updates: Vec<crate::combat::npc_ai::ModeUpdate>,
        dmg_actions: Vec<crate::combat::npc_ai::AiDamageAction>,
        reply: oneshot::Sender<Result<crate::combat::npc_ai::NpcAiWorldEffectsOutput, String>>,
    },
    ApplyNpcRemoteRepairs {
        peer_ids: Vec<BallId>,
        shield_rep: f64,
        armor_rep: f64,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    ExecuteExtendedNpcAttack {
        input: Box<ExtendedNpcAttackActorInput>,
        reply: oneshot::Sender<Result<ExtendedNpcAttackActorOutput, String>>,
    },
    ClearNpcScramTargets {
        targets: Vec<BallId>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StepExtDeployables {
        sde: Arc<SdeRegistry>,
        db: Arc<eve_db::WorldRepository>,
        mechanic_rng: Arc<crate::rng::MechanicRng>,
        now_unix: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ReconnectLiveShipInSpace {
        request: ReconnectLiveShipRequest,
        reply: oneshot::Sender<Result<Option<DVec3>, String>>,
    },
    SpawnReconnectedShipInSpace {
        request: Box<SpawnReconnectedShipRequest>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StartDisconnectedEmergencyWarp {
        ball_id: BallId,
        current_pos: DVec3,
        dest: DVec3,
        warp_speed_au_s: f64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    PrepareDisconnectedWarp {
        ship_id: BallId,
        warp_speed_au_s: f64,
        current_pos: DVec3,
        dest: DVec3,
        reply: oneshot::Sender<()>,
    },
    ReconcileDisconnectedWarp {
        ship_id: BallId,
        dest: DVec3,
        reply: oneshot::Sender<(DVec3, bool)>,
    },
    /// Transfers pilot observer registration and pending `observer_events` across balls (`#1342`).
    HandoffInSpacePilotObserver {
        from_ball_id: BallId,
        to_ball_id: BallId,
        ensure_add_to_ball: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Replaces a ball's wire dbuff snapshot and notifies observers (`#1342`).
    UpdateBallDbuffs {
        ball_id: BallId,
        dbuffs: Vec<(i64, f64, i64)>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Emits deactivation `OnSpecialFX` events for live `/soak logi` NPCs (`#1342`).
    EmitLogiSoakStopFx {
        npc_ids: Vec<BallId>,
        target_ship_id: BallId,
        mod_type_ids: [u32; 2],
        fx_duration_ms: f64,
        active_parity: u8,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Transitions due `/deadwarp` ships into return warps and updates actor warp tracking (`#1342`).
    LaunchDeadwarpReturns {
        returns: Vec<(BallId, CharacterId, DVec3, DVec3, f64)>,
        reply: oneshot::Sender<Result<(Vec<BallId>, Vec<BallId>), String>>,
    },
    /// Applies one `/soak logi` repair, combat FX, and dbuff update cycle in actor state (`#1342`).
    StepLogiSoakCycle {
        input: Box<LogiSoakCycleActorInput>,
        reply: oneshot::Sender<Result<(f64, f64), String>>,
    },
    UpsertDroneMiner {
        state: super::DroneMiningState,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RescheduleDroneMiner {
        drone_id: BallId,
        now_unix: u64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    SetDroneNextReadyUnix {
        drone_id: BallId,
        next_ready_unix: u64,
        require_ball_in_space: bool,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    ClearDroneRuntime {
        drone_ids: Option<Vec<BallId>>,
        clear_miners: bool,
        clear_timers: bool,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    FlushFighterCheckpoint {
        reply: oneshot::Sender<Option<FighterCheckpointSnapshot>>,
    },
    LoadOrUnloadFighterTube {
        op: super::FighterTubeOp,
        reply: oneshot::Sender<Result<Option<i64>, super::FighterActorError>>,
    },
    FighterLaunchOp {
        op: super::FighterLaunchActorOp,
        reply: oneshot::Sender<Result<super::FighterLaunchActorOut, super::FighterActorError>>,
    },
    FighterRecallOp {
        op: super::FighterRecallActorOp,
        reply: oneshot::Sender<Option<super::FighterRecallClaim>>,
    },
    ApplyDamageToFighterSquadron {
        fighter_ball_id: i64,
        raw_damage: f64,
        sde: Arc<SdeRegistry>,
        reply: oneshot::Sender<Option<super::FighterDamageOutcome>>,
    },
    AbandonOwnedFighters {
        char_id: i64,
        ship_id: i64,
        filter_ids: Option<Vec<i64>>,
        db_in_sys_items: HashSet<i64>,
        clear_tubes: bool,
        only_matching_sys_items: bool,
        reply: oneshot::Sender<super::FighterAbandonActorResult>,
    },
    ClaimOrRestoreAbandonedFighterForScoop {
        op: super::FighterScoopActorOp,
        reply: oneshot::Sender<Result<Option<(i64, super::ActiveFighterSquadron)>, super::FighterActorError>>,
    },
    MutateFighterAbilityState {
        op: super::FighterAbilityActorOp,
        reply: oneshot::Sender<Result<super::FighterAbilityActorOut, super::FighterActorError>>,
    },
    CleanupDestroyedFighter {
        ball_id: i64,
        reply: oneshot::Sender<Option<super::ActiveFighterSquadron>>,
    },
    ExtractShipFighterTubes {
        ship_id: i64,
        reply: oneshot::Sender<super::ShipFighterTubeTransfer>,
    },
    InsertShipFighterTubes {
        ship_id: i64,
        transfer: super::ShipFighterTubeTransfer,
        reply: oneshot::Sender<()>,
    },
    SeedAbandonedFighters {
        squadrons: Vec<super::ActiveFighterSquadron>,
        reply: oneshot::Sender<()>,
    },
    ReloadFighterStateClean {
        state: super::SystemFighterState,
        reply: oneshot::Sender<(u64, u64)>,
    },
    MergeFighterState {
        state: super::SystemFighterState,
        mark_dirty: bool,
        clear_hydration: bool,
        reply: oneshot::Sender<(u64, u64)>,
    },
    #[cfg(any(test, feature = "testkit"))]
    TestHelperFighterOp {
        op: super::FighterTestHelperOp,
        reply: oneshot::Sender<super::FighterTestHelperOut>,
    },
    UpsertActiveDrones {
        drones: Vec<super::DroneSpaceState>,
        only_if_absent: bool,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    ReserveDroneLaunches {
        req: super::ReserveDroneLaunchesReq,
        reply: oneshot::Sender<Result<super::ReserveDroneLaunchesOutcome, String>>,
    },
    FinalizeDroneLaunches {
        mappings: Vec<(BallId, BallId, i64)>,
        rollback_res_ids: Vec<BallId>,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    ReconnectDrones {
        req: super::ReconnectDronesReq,
        reply: oneshot::Sender<Result<super::ReconnectDronesOutcome, String>>,
    },
    MutateActiveDrones {
        op: super::ActiveDroneMutOp,
        reply: oneshot::Sender<Result<(), String>>,
    },
    MutateActiveDronesWithClaims {
        op: super::ActiveDroneMutWithClaimsOp,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ScoopDroneClaimOp {
        ball_id: BallId,
        action: super::DroneScoopClaimAction,
        reply: oneshot::Sender<Result<super::DroneScoopClaimOutcome, String>>,
    },
    HydrateActiveDronesFromSqlite {
        drones: Vec<super::DroneSpaceState>,
        legacy_dirty: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SnapshotDirtyActiveDrones {
        reply: oneshot::Sender<Result<Option<(u64, Vec<(BallId, super::DroneSpaceState)>)>, String>>,
    },
    CommitActiveDronesFlushed {
        seq: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpsertProjectedEwar {
        source_ship_id: i64,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RecordProjectedEwarSource {
        source_module_id: i64,
        source_ship_id: i64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveProjectedEwarByModule {
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
        reply: oneshot::Sender<Result<Vec<i64>, String>>,
    },
    ClearProjectedEwarForTarget {
        target_ship_id: i64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RecordActiveEwarStart {
        source_ship_id: i64,
        module_id: i64,
        target_ship_id: i64,
        jamming_type: &'static str,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ReplaceActiveEwarStarts {
        source_ship_id: i64,
        module_id: i64,
        targets: Vec<(i64, &'static str)>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    FlushModuleEwar {
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
        reply: oneshot::Sender<Result<super::ModuleEwarFlushSnapshot, String>>,
    },
    ClearEwarState {
        reply: oneshot::Sender<Result<(), String>>,
    },
    RecordModuleCooldown {
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
        reply: oneshot::Sender<Result<super::ModuleCooldownPersistPayload, String>>,
    },
    SeedModuleCooldownEntry {
        key: (i64, i64, u32),
        entry: super::ModuleCooldownEntry,
        reply: oneshot::Sender<Result<super::ModuleCooldownPersistPayload, String>>,
    },
    RemoveModuleCooldown {
        key: (i64, i64, u32),
        reply: oneshot::Sender<Result<Option<super::ModuleCooldownPersistPayload>, String>>,
    },
    ClearShipModuleCooldowns {
        ship_id: i64,
        reply: oneshot::Sender<Result<Option<super::ModuleCooldownPersistPayload>, String>>,
    },
    ClearAllModuleCooldowns {
        reply: oneshot::Sender<Result<super::ModuleCooldownPersistPayload, String>>,
    },
    ReplaceModuleCooldowns {
        cooldowns: HashMap<(i64, i64, u32), super::ModuleCooldownEntry>,
        expected_gate_token: Option<(u64, Option<(u64, u64)>)>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    PruneExpiredModuleCooldowns {
        now_unix_f64: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
        force_persist: bool,
        reply: oneshot::Sender<Result<Option<super::ModuleCooldownPersistPayload>, String>>,
    },
    SnapshotModuleCooldownCheckpoint {
        only_if_dirty: bool,
        reply: oneshot::Sender<Result<Option<super::ModuleCooldownPersistPayload>, String>>,
    },
    RecordModuleReloadSimDeadlines {
        module_ids: Vec<i64>,
        sim_start_s: f64,
        sim_deadline_s: f64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ClearModuleReloadSimDeadline {
        module_id: i64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    ApplyLoadingCooldownTransitions {
        tidi_factor: f64,
        has_tidi_history: bool,
        transitions: Vec<super::module_cooldowns::PendingCooldownTransition>,
        reply: oneshot::Sender<Result<(bool, Option<super::ModuleCooldownPersistPayload>), String>>,
    },
    ApplyPersistedCooldownCommit {
        commit: super::module_cooldowns::PersistedCooldownCommit,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    HydrateWeaponEffects {
        spools: Vec<((i64, i64), super::EntropicSpoolState)>,
        missiles: Vec<(i64, super::InFlightMissileState)>,
        breachers: Vec<(i64, super::ActiveBreacherDot)>,
        legacy_prune: Option<super::LegacyWeaponEffectsPruneKeys>,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    UpsertEntropicSpoolActivation {
        spool: super::EntropicSpoolState,
        is_active_in_db: bool,
        reply: oneshot::Sender<Result<Option<(u32, f64)>, String>>,
    },
    AdvanceEntropicSpoolCycle {
        ship_id: i64,
        module_id: i64,
        reply: oneshot::Sender<Result<(u32, f64), String>>,
    },
    UpdateEntropicSpool {
        spool: super::EntropicSpoolState,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveEntropicSpool {
        ship_id: i64,
        module_id: i64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    CollectAndClearEntropicSpoolsForShip {
        ship_id: i64,
        target_id_opt: Option<i64>,
        reply: oneshot::Sender<Result<Vec<i64>, String>>,
    },
    UpsertInFlightMissile {
        missile: super::InFlightMissileState,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveInFlightMissile {
        missile_ball_id: i64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    DamageInFlightMissile {
        missile_ball_id: i64,
        damage: f64,
        reply: oneshot::Sender<Result<Option<bool>, String>>,
    },
    InterceptInFlightMissile {
        target_missile_id_opt: Option<i64>,
        reply: oneshot::Sender<Result<Option<i64>, String>>,
    },
    UpsertActiveBreacherDot {
        dot: super::ActiveBreacherDot,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveActiveBreacherDot {
        target_ball_id: i64,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    FlushWeaponEffectsCheckpoint {
        reply: oneshot::Sender<Result<bool, String>>,
    },
    ShutdownActor {
        reply: oneshot::Sender<()>,
    },
    RefreshStargateActivationStates {
        lazy_enabled: bool,
        loaded_systems: HashSet<SolarSystemId>,
        reply: Option<oneshot::Sender<()>>,
    },
    ActivateStargateTransitionForDest {
        dest_sys_id: SolarSystemId,
        now_ms: u64,
        reply: Option<oneshot::Sender<()>>,
    },
    ActivateStargateTransition {
        gate_id: BallId,
        transition_ms: u64,
        now_ms_opt: Option<u64>,
        reply: oneshot::Sender<bool>,
    },
    SetStargateActivationState {
        gate_id: BallId,
        activation_state: i64,
        transition_remaining_ms: u64,
        activation_transition_at_ms: u64,
        reply: oneshot::Sender<bool>,
    },
    SettleStargateActivationStates {
        now_ms: u64,
        reply: oneshot::Sender<Vec<BallId>>,
    },
    MutateExternalActiveModule {
        op: ExternalActiveModuleOp,
        reply: oneshot::Sender<bool>,
    },
    RepairCombatStateSync {
        ball_id: BallId,
        hull_heal: f64,
        armor_heal: f64,
        shield_heal: f64,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    SetBallPositionSync {
        ball_id: BallId,
        position: DVec3,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<()>,
    },
    StopBall {
        ball_id: BallId,
        reply: oneshot::Sender<()>,
    },
    RemoveBallSync {
        ball_id: BallId,
        emit_destruction_fx: bool,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<()>,
    },
    CancelShipNaniteRepairsSync {
        ship_id: BallId,
        refund_paste: bool,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<()>,
    },
    CancelNaniteRepairsForShip {
        ship_id: BallId,
        reply: oneshot::Sender<Vec<ActiveNaniteRepair>>,
    },
    TrySpawnAsteroid {
        asteroid: AsteroidState,
        ball: Ball,
        reply: oneshot::Sender<()>,
    },
    SyncAsteroidQuantity {
        asteroid_id: BallId,
        remaining_units: u64,
        due_unix_on_belt_empty: u64,
        reply: oneshot::Sender<()>,
    },
    TrySpawnBallWithMeta {
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        reply: oneshot::Sender<()>,
    },
    MarkBallNonFree {
        ball_id: BallId,
        is_free: bool,
        reply: oneshot::Sender<()>,
    },
    TrackModuleReloadSimSync {
        module_id: i64,
        ship_id: BallId,
        remaining_sim_s: f64,
        reply: oneshot::Sender<()>,
    },
    TryHandoffInSpaceObserver {
        old_ball_id: BallId,
        new_ball_id: BallId,
        added_ball_ids: Vec<BallId>,
        reply: oneshot::Sender<bool>,
    },
    TrySpawnBallWithCombat {
        ball: Ball,
        combat_state: CombatState,
        reply: oneshot::Sender<()>,
    },
    UpdateCombatState {
        ball_id: BallId,
        recompute_attrs: bool,
        mutator: Box<dyn FnOnce(&mut CombatState) + Send>,
        reply: oneshot::Sender<bool>,
    },
    ApplyDirectCombatDamage {
        ball_id: BallId,
        damage: f64,
        reply: oneshot::Sender<Option<bool>>,
    },
    SyncShipBallparkStats {
        ball_id: BallId,
        fresh: Box<CombatState>,
        reply: oneshot::Sender<bool>,
    },
    RespawnDepletedBeltsOnDowntime {
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<usize>,
    },
    TryStepManual {
        dt: f64,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<Vec<DestinyEvent>>,
    },
    TakePendingCycleActions {
        reply: oneshot::Sender<Vec<PendingCycleAction>>,
    },
    MutateWarpState {
        op: WarpStateMutOp,
        reply: oneshot::Sender<bool>,
    },
    FinishWarpForDock {
        ball_id: BallId,
        landing_pos: Option<DVec3>,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    FlushDirtyConditionCheckpointSync {
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<usize>,
    },
    StepFullTick {
        dt: f64,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<StepFullTickOutput>,
    },
    TryRemoveModuleCycle {
        ship_id: BallId,
        module_id: i64,
        wait_for_post_unlock: bool,
        reply: oneshot::Sender<()>,
    },
    /// Test-only exclusive access to actor `Ballpark` and `SystemActorState` through the mailbox (`#1342`).
    #[cfg(any(test, feature = "testkit"))]
    TestAccess {
        op: TestAccessOp,
        mutated: bool,
        reply: oneshot::Sender<()>,
    },
}

/// External active-module registry operations dispatched through [`SystemCommand::MutateExternalActiveModule`].
#[derive(Clone, Copy, Debug)]
pub enum ExternalActiveModuleOp {
    Register(BallId, ItemId),
    Unregister(BallId, ItemId),
    ClearAll,
}

/// Actor warp-tracking operations dispatched through [`SystemCommand::MutateWarpState`].
#[derive(Clone, Debug)]
pub enum WarpStateMutOp {
    SetProvenance {
        ball_id: BallId,
        provenance: WarpProvenance,
    },
    ClearProvenance {
        ball_id: BallId,
    },
    TrackPlayerOrFleet {
        ball_id: BallId,
        char_id: CharacterId,
    },
    TrackReturn {
        ball_id: BallId,
    },
    ClearBall {
        ball_id: BallId,
    },
    ClearReturnAndPending {
        ball_id: BallId,
    },
    ConsumeFinished {
        ball_id: BallId,
    },
    InsertActiveForTest {
        ball_id: BallId,
        char_id: CharacterId,
    },
    InsertReturnForTest {
        ball_id: BallId,
    },
    ClearAllForTest,
    RestoreForTest(Box<ActorWarpState>),
}

/// Output returned by [`SystemCommand::StepFullTick`] so the caller spawns hooks and checks runtime
/// availability on its own thread (`#1168`, `#1342`).
pub struct StepFullTickOutput {
    pub(crate) tick: u64,
    pub(crate) dt_sim: f64,
    pub(crate) warp_events: Vec<DestinyEvent>,
    pub(crate) drained_destroyed: Vec<(BallId, Option<CharacterId>)>,
    pub(crate) destroyed_lease: Option<Arc<PendingDestroyedDeliveryGuard>>,
    pub(crate) has_destroyed: bool,
    pub(crate) ops_was_empty: bool,
    pub(crate) waited_sync: bool,
}

/// Opaque test-only closure passed inside [`SystemCommand::TestAccess`] (`#1342`).
#[cfg(any(test, feature = "testkit"))]
pub struct TestAccessOp(pub(crate) Box<dyn FnOnce(&mut Ballpark, &mut SystemActorState) + Send>);

#[cfg(any(test, feature = "testkit"))]
impl TestAccessOp {
    pub(crate) fn new(
        op: impl FnOnce(&mut Ballpark, &mut SystemActorState) + Send + 'static,
    ) -> Self {
        Self(Box::new(op))
    }

    pub(crate) fn run(self, ballpark: &mut Ballpark, actor_st: &mut SystemActorState) {
        (self.0)(ballpark, actor_st);
    }
}

impl SystemCommand {
    /// Returns `true` if executing this command can modify any field mirrored in [`SystemView`].
    ///
    /// Observer-drain commands return `false` here because they only mutate [`SystemView`] when
    /// `sync_player_slim_items` updates player metadata, which the actor checks dynamically (`RT-43`).
    pub(crate) fn may_mutate_system_view(&self) -> bool {
        #[cfg(any(test, feature = "testkit"))]
        if let Self::TestAccess { mutated, .. } = self {
            return *mutated;
        }
        !matches!(
            self,
            Self::GetBallSlimItem { .. }
                | Self::GetAsteroidState { .. }
                | Self::GetBallSnapshot { .. }
                | Self::GetBallGroupId { .. }
                | Self::ListBalls { .. }
                | Self::ListBallsInBubble { .. }
                | Self::GetSetState { .. }
                | Self::GetSetStateWithSeq { .. }
                | Self::CurrentTick { .. }
                | Self::GetBubbleIdForBall { .. }
                | Self::DrainObserverUpdates { .. }
                | Self::DrainObserverUpdatesWithSeq { .. }
                | Self::GetTargetLocks { .. }
                | Self::GetCombatState { .. }
                | Self::HasModuleCycle { .. }
                | Self::IsBallScrambled { .. }
                | Self::FlushPeerObserverUpdates { .. }
                | Self::FlushConditionCheckpoint { .. }
                | Self::FlushFighterCheckpoint { .. }
                | Self::GetProtectionSnapshot { .. }
                | Self::SnapshotDirtyActiveDrones { .. }
                | Self::CommitActiveDronesFlushed { .. }
                | Self::SnapshotModuleCooldownCheckpoint { .. }
                | Self::FlushWeaponEffectsCheckpoint { .. }
                | Self::TakePendingCycleActions { .. }
                | Self::FlushDirtyConditionCheckpointSync { .. }
                | Self::ShutdownActor { .. }
        )
    }
}
