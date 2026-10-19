//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

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
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::MissedTickBehavior;

// parity: plan:P-7, plan:SP-1, plan:SP-2
// parity: feat:navigation.celestials-balls, feat:navigation.bubbles-grid
// parity: feat:navigation.destiny-updates, feat:serverops.server-tick
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct TickBroadcast {
    pub solar_system_id: SolarSystemId,
    pub tick: u64,
    pub events: Vec<DestinyEvent>,
    pub observer_events: BTreeMap<BallId, Vec<DestinyEvent>>,
    pub observer_updates_pyrep: BTreeMap<BallId, PyRep>,
}

#[derive(Debug, Clone)]
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
    RemoveBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<Ball>>,
    },
    SetMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetSpeedFraction {
        ball_id: BallId,
        fraction: f64,
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
}

#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    cmd_tx: mpsc::Sender<SystemCommand>,
    event_tx: broadcast::Sender<TickBroadcast>,
}

impl SystemHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<TickBroadcast> {
        self.event_tx.subscribe()
    }

    pub async fn spawn_ball(&self, ball: Ball) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SpawnBall { ball, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn spawn_ball_with_combat(
        &self,
        ball: Ball,
        combat_state: CombatState,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SpawnBallWithCombat {
                ball,
                combat_state,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn spawn_ball_with_meta(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SpawnBallWithMeta {
                ball,
                combat_state,
                slim_meta,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_ball(&self, ball_id: BallId) -> Option<Ball> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SystemCommand::RemoveBall {
                ball_id,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    pub async fn set_movement_mode(
        &self,
        ball_id: BallId,
        mode: MovementMode,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetMovementMode {
                ball_id,
                mode,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_speed_fraction(&self, ball_id: BallId, fraction: f64) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetSpeedFraction {
                ball_id,
                fraction,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Queues a `SetState` destiny update for `observer_ball_id` (`P-7`).
    pub async fn push_set_state(&self, observer_ball_id: BallId) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::PushSetState {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_ball_snapshot(&self, ball_id: BallId) -> Result<Option<(u64, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetBallSnapshot { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_group_id(&self, ball_id: BallId) -> Result<Option<u32>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetBallGroupId { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn list_balls(&self) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ListBalls { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Returns all balls visible to `observer_ball_id` (all static system celestials/gates/stations
    /// plus dynamic balls within the same shared `GridBubble`).
    pub async fn list_balls_in_bubble(&self, observer_ball_id: BallId) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ListBallsInBubble {
                observer_ball_id,
                dynamic_only: false,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Returns only the dynamic balls within the shared `GridBubble` of `observer_ball_id`.
    pub async fn list_dynamic_balls_in_bubble(
        &self,
        observer_ball_id: BallId,
    ) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ListBallsInBubble {
                observer_ball_id,
                dynamic_only: true,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Builds the marshaled `SetState` `KeyVal` and `DoDestinyUpdate` list for `observer_ball_id` (`P-7`, `SP-1`, `SP-2`).
    pub async fn get_set_state(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, PyRep, PyRep), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetSetState {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_bubble_id_for_ball(&self, ball_id: BallId) -> Result<Option<u64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetBubbleIdForBall { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Drains pending bubble-scoped `DestinyEvent`s for `observer_ball_id` and returns `(tick, events, do_destiny_update_pyrep)`.
    pub async fn drain_observer_updates(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, Vec<DestinyEvent>, PyRep), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::DrainObserverUpdates {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_damage(
        &self,
        ball_id: BallId,
        damage: DamageVector,
    ) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ApplyDamage {
                ball_id,
                damage,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn step_manual(&self, dt: f64) -> Result<TickBroadcast, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::StepManual { dt, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_target_locks(&self, ball_id: BallId) -> Result<Vec<TargetLock>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetTargetLocks { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_target_lock(&self, source_id: BallId, lock: TargetLock) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::AddTargetLock { source_id, lock, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_target_lock(&self, source_id: BallId, target_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::RemoveTargetLock { source_id, target_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn clear_target_locks(&self, source_id: BallId) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ClearTargetLocks { source_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_capacitor(&self, ball_id: BallId, capacitor: f64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetCapacitor { ball_id, capacitor, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_combat_state(&self, ball_id: BallId) -> Result<Option<CombatState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_module_cycle(&self, cycle: ActiveModuleCycle) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::AddModuleCycle { cycle, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_module_cycle(&self, ship_id: BallId, module_id: i64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::RemoveModuleCycle { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }
}

/// Populates a solar system's `Ballpark` with its real SDE stars, planets, moons, asteroid belts,
/// stargates, and stations as static `MovementMode::Rigid` balls (`SP-1`).
pub fn populate_system_celestials(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) {
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        ballpark.constellation_id = sys.constellation_id;
    }
    // 1. Stars (group 6), Planets (group 7), Moons (group 8), Asteroid Belts (group 9), Stargates (group 10)
    for cel in sde.celestials_in_system(solar_system_id) {
        let type_rec = sde.types.get(&cel.type_id);
        let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
        let radius = if cel.radius > 0.0 {
            cel.radius
        } else {
            type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(1_000.0)
        };
        let mass = type_rec
            .map(|t| t.mass)
            .filter(|&m| m > 0.0)
            .unwrap_or(1.0e18);
        ballpark.add_static_ball_with_meta(
            Ball {
                id: cel.celestial_id,
                type_id: cel.type_id,
                position: cel.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            SlimItemMeta {
                group_id: cel.group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        );
    }

    // 2. Ensure all stargates in `sde.jumps` originating in `solar_system_id` are present (`group_id = 10`)
    for jump in sde.jumps.values() {
        if jump.from_solar_system_id == solar_system_id
            && !ballpark.balls.contains_key(&jump.stargate_id)
        {
            let type_rec = sde.types.get(&16);
            let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
            let radius = type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0);
            ballpark.add_static_ball_with_meta(
                Ball {
                    id: jump.stargate_id,
                    type_id: 16,
                    position: jump.position,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e12,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
                SlimItemMeta {
                    group_id: 10,
                    category_id,
                    owner_id: 1,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                },
            );
        }
    }

    // 3. Stations (`group_id = 15`) from `sde.solar_systems[solar_system_id].stations`
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        for st in &sys.stations {
            if !ballpark.balls.contains_key(&st.station_id) {
                let type_rec = sde.types.get(&st.station_type_id);
                let group_id = type_rec.map(|t| t.group_id).unwrap_or(15);
                let category_id = type_rec.map(|t| t.category_id).unwrap_or(3);
                let owner_id = if st.owner_corporation_id > 0 {
                    st.owner_corporation_id
                } else {
                    1_000_035
                };
                let radius = type_rec
                    .map(|t| t.radius)
                    .filter(|&r| r > 0.0)
                    .unwrap_or(30_000.0);
                let mass = type_rec
                    .map(|t| t.mass)
                    .filter(|&m| m > 0.0)
                    .unwrap_or(1.0e12);
                ballpark.add_static_ball_with_meta(
                    Ball {
                        id: st.station_id,
                        type_id: st.station_type_id,
                        position: st.position,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass,
                        radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    },
                    SlimItemMeta {
                        group_id,
                        category_id,
                        owner_id,
                        char_id: None,
                        corp_id: owner_id,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                );
            }
        }
    }
}

fn ensure_default_slim_meta(
    ballpark: &mut Ballpark,
    ball: &Ball,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    if ballpark.ball_slim_meta.contains_key(&ball.id) {
        return;
    }
    let (group_id, category_id) = sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&ball.type_id))
        .map(|t| (t.group_id, t.category_id))
        .unwrap_or((25, 6));
    ballpark.set_ball_slim_meta(
        ball.id,
        SlimItemMeta {
            group_id,
            category_id,
            owner_id: if ball.id >= 700_000_000 { 1 } else { ball.id },
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
        },
    );
}

fn build_tick_broadcast(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    events: Vec<DestinyEvent>,
) -> TickBroadcast {
    let observer_events = ballpark.drain_all_observer_events();
    let stamp = ballpark.current_tick as u32;
    let mut observer_updates_pyrep = BTreeMap::new();
    for (&obs_id, obs_evs) in &observer_events {
        observer_updates_pyrep.insert(
            obs_id,
            encode_destiny_updates(stamp, obs_evs, Some(ballpark)),
        );
    }
    TickBroadcast {
        solar_system_id,
        tick: ballpark.current_tick,
        events,
        observer_events,
        observer_updates_pyrep,
    }
}

pub struct SystemRegistry {
    systems: RwLock<HashMap<SolarSystemId, SystemHandle>>,
    sde: Option<Arc<SdeRegistry>>,
    pub db: Option<Arc<eve_db::WorldRepository>>,
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
        }
    }

    pub fn with_sde(sde: Arc<SdeRegistry>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: None,
        }
    }

    pub fn with_sde_and_db(sde: Arc<SdeRegistry>, db: Arc<eve_db::WorldRepository>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: Some(db),
        }
    }
}

fn step_cycles_and_capacitor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    dt: f64,
) {
    for cs in ballpark.combat_states.values_mut() {
        cs.capacitor = eve_dogma::simulate_capacitor(cs.capacitor, cs.max_capacitor, 250.0, dt);
    }

    let mut finished_indices = Vec::new();
    for (idx, cycle) in active_module_cycles.iter_mut().enumerate() {
        cycle.elapsed_s += dt;
        if cycle.elapsed_s >= cycle.duration_s {
            if let Some(target_id) = cycle.target_id {
                if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                    cs.apply_damage(cycle.damage_vector);
                }
            }
            if let Some(db_ref) = db_opt {
                if !cycle.linked_turrets.is_empty() {
                    db_ref.with_mut_snapshot(|snap| {
                        for &turret_id in &cycle.linked_turrets {
                            let turret_flag = snap.items.get(&turret_id).map(|i| i.flag);
                            if let Some(t_flag) = turret_flag {
                                if let Some(charge_item) = snap.items.values_mut().find(|i| {
                                    i.location_id == cycle.ship_id
                                        && i.flag == t_flag
                                        && (cycle.ammo_type_id.is_none() || Some(i.type_id) == cycle.ammo_type_id)
                                        && i.quantity > 0
                                }) {
                                    charge_item.quantity = charge_item.quantity.saturating_sub(1);
                                }
                            }
                        }
                    });
                }
            }
            if cycle.repeat == 0 {
                finished_indices.push(idx);
            } else {
                cycle.elapsed_s -= cycle.duration_s;
                if cycle.cap_need_per_cycle > 0.0 {
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        cs.capacitor = (cs.capacitor - cycle.cap_need_per_cycle).max(0.0);
                    }
                }
            }
        }
    }
    for idx in finished_indices.into_iter().rev() {
        let finished = active_module_cycles.remove(idx);
        if let Some(orig_v) = finished.original_max_velocity {
            if let Some(ball) = ballpark.balls.get_mut(&finished.ship_id) {
                ball.max_velocity = orig_v;
            }
        }
    }
}

impl SystemRegistry {
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
        let mut ballpark = Ballpark::new(solar_system_id);
        if let Some(sde) = &sde_opt {
            populate_system_celestials(&mut ballpark, solar_system_id, sde);
        }

        tokio::spawn(async move {
            let mut active_module_cycles: Vec<ActiveModuleCycle> = Vec::new();
            let mut ticker = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut lod_counter: u64 = 0;

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
                            let events = ballpark.step(dt);
                            step_cycles_and_capacitor(&mut ballpark, &mut active_module_cycles, &db_opt, dt);
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
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, combat_state);
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
                                ballpark.set_ball_slim_meta(ball_id, slim_meta);
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, combat_state);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::RemoveBall { ball_id, reply } => {
                                let removed = ballpark.remove_ball(ball_id);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::SetMovementMode { ball_id, mode, reply } => {
                                if ballpark.balls.contains_key(&ball_id) {
                                    ballpark.set_mode(ball_id, mode);
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
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
                                let events = ballpark.drain_observer_events(observer_ball_id);
                                let stamp = ballpark.current_tick as u32;
                                let pyrep = encode_destiny_updates(stamp, &events, Some(&ballpark));
                                let _ = reply.send((ballpark.current_tick, events, pyrep));
                            }
                            SystemCommand::ApplyDamage { ball_id, damage, reply } => {
                                if let Some(ball_snap) = ballpark.balls.get(&ball_id).cloned() {
                                    let cs = ballpark.combat_states.entry(ball_id).or_default();
                                    let (applied, destroyed) = cs.apply_damage(damage);
                                    if destroyed {
                                        let _ = ballpark.remove_ball(ball_id);
                                    }
                                    let _ = reply.send(Some((applied, destroyed, ball_snap)));
                                } else {
                                    let _ = reply.send(None);
                                }
                            }
                            SystemCommand::StepManual { dt, reply } => {
                                let events = ballpark.step(dt);
                                step_cycles_and_capacitor(&mut ballpark, &mut active_module_cycles, &db_opt, dt);
                                let broadcast = build_tick_broadcast(solar_system_id, &mut ballpark, events);
                                let _ = event_tx_actor.send(broadcast.clone());
                                let _ = reply.send(broadcast);
                            }
                            SystemCommand::GetTargetLocks { ball_id, reply } => {
                                let locks = ballpark.get_target_locks(ball_id);
                                let _ = reply.send(locks);
                            }
                            SystemCommand::AddTargetLock { source_id, lock, reply } => {
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
                            SystemCommand::GetCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.get(&ball_id).cloned();
                                let _ = reply.send(cs);
                            }
                            SystemCommand::AddModuleCycle { mut cycle, reply } => {
                                if cycle.cap_need_per_cycle > 0.0 {
                                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                                        cs.capacitor = (cs.capacitor - cycle.cap_need_per_cycle).max(0.0);
                                    }
                                }
                                if cycle.speed_boost_factor > 0.0 {
                                    if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                                        if cycle.original_max_velocity.is_none() {
                                            cycle.original_max_velocity = Some(ball.max_velocity);
                                            ball.max_velocity *= 1.0 + cycle.speed_boost_factor;
                                        }
                                    }
                                }
                                active_module_cycles.push(cycle);
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
                                if let Some(pos) = active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                                    let removed = active_module_cycles.remove(pos);
                                    if let Some(orig_v) = removed.original_max_velocity {
                                        if let Some(ball) = ballpark.balls.get_mut(&ship_id) {
                                            ball.max_velocity = orig_v;
                                        }
                                    }
                                }
                                let _ = reply.send(Ok(()));
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
        };
        guard.insert(solar_system_id, handle.clone());
        handle
    }
}
