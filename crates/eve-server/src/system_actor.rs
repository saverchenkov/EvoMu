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
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall

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
    pub cap_drain_target: f64,
    pub cap_transfer_target: f64,
    pub shield_boost_amount: f64,
    pub armor_repair_amount: f64,
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
}

#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    cmd_tx: mpsc::Sender<SystemCommand>,
    event_tx: broadcast::Sender<TickBroadcast>,
    ball_positions: Arc<RwLock<HashMap<BallId, DVec3>>>,
}

impl SystemHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<TickBroadcast> {
        self.event_tx.subscribe()
    }

    pub fn get_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        self.ball_positions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ball_id)
            .copied()
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

    pub async fn spawn_asteroid(
        &self,
        asteroid: AsteroidState,
        ball: Ball,
    ) -> Result<BallId, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SpawnAsteroid {
                asteroid,
                ball,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn mine_asteroid(
        &self,
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
    ) -> Result<MiningExtractionResult, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::MineAsteroid {
                asteroid_id,
                requested_units,
                waste_probability,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_asteroid_state(
        &self,
        asteroid_id: BallId,
    ) -> Result<Option<AsteroidState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetAsteroidState {
                asteroid_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn break_gate_cloak(&self, ball_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::BreakGateCloak {
                ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn populate_asteroid_belt_entities(&self) -> Result<Vec<AsteroidState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::PopulateAsteroids { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
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

    pub async fn set_warp_scramble_strength(
        &self,
        ball_id: BallId,
        strength: i32,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetWarpScrambleStrength {
                ball_id,
                strength,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_warp_core_strength(
        &self,
        ball_id: BallId,
        strength: i32,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetWarpCoreStrength {
                ball_id,
                strength,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn add_warp_bubble(
        &self,
        id: BallId,
        position: DVec3,
        radius: f64,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::AddWarpBubble {
                id,
                position,
                radius,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_warp_bubble(&self, id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::RemoveWarpBubble { id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
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

    pub async fn set_ball_position(&self, ball_id: BallId, position: glam::DVec3) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SystemCommand::SetBallPosition {
                ball_id,
                position,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
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

    pub async fn heal_combat_state(&self, ball_id: BallId) -> Result<CombatState, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::HealCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_combat_state(&self, ball_id: BallId, combat_state: CombatState) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetCombatState { ball_id, combat_state, reply: tx })
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

    pub async fn set_interdiction_nullified(&self, ball_id: BallId, nullified: bool) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetInterdictionNullified { ball_id, nullified, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn micro_jump(&self, ball_id: BallId, distance_meters: f64) -> Result<DVec3, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::MicroJump { ball_id, distance_meters, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn is_ball_scrambled(&self, ball_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::IsBallScrambled { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_interdiction_sphere(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::AddInterdictionSphere { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_mobile_warp_disruptor(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
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
                let Some(type_rec) = sde.types.get(&st.station_type_id) else {
                    continue;
                };
                let group_id = type_rec.group_id;
                let category_id = type_rec.category_id;
                let owner_id = st.owner_corporation_id;
                let radius = type_rec.radius;
                let mass = type_rec.mass;
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

/// Populates asteroid belt entities (Veldspar, Scordite, Pyroxeres, etc.) around asteroid belt celestials (`SP-4`, `MN-01`).
pub fn populate_asteroid_belt_entities(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) -> Vec<AsteroidState> {
    let mut states = Vec::new();
    let mut ast_id_counter = 750_000_000i64;

    let Some(sys) = sde.solar_systems.get(&solar_system_id) else {
        return states;
    };
    let sec = sys.security_status;

    let mut base_ores: Vec<&eve_sde::InvType> = sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 25
                && t.published
                && (450..=469).contains(&t.group_id)
                && t.group_id != 465
                && sde.groups.get(&t.group_id).is_some_and(|g| g.name == t.name)
        })
        .collect();
    base_ores.sort_by(|a, b| {
        a.volume
            .partial_cmp(&b.volume)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.type_id.cmp(&b.type_id))
    });

    // DERIVED (OF-36 / R57-P1-2): security-band volume thresholds and initial asteroid
    // unit/radius/mass scaling are derived server-side heuristics because belt ore tables
    // are server-only and not present in V24 client ResFiles. `feat:mining.belt-ore-distribution`
    // and `feat:mining.ice-belts` remain open until regional/ice spawn tables are wired.
    for cel in sde.celestials_in_system(solar_system_id) {
        if cel.group_id == 9 {
            let belt_pos = cel.position;
            let band_ores: Vec<&eve_sde::InvType> = base_ores
                .iter()
                .copied()
                .filter(|t| {
                    if sec >= 0.45 {
                        t.volume <= 0.35
                    } else if sec >= 0.05 {
                        t.volume > 0.35 && t.volume < 3.0
                    } else {
                        t.volume >= 3.0 && t.volume <= 16.0
                    }
                })
                .collect();

            for (idx, ore_type) in band_ores.iter().enumerate() {
                let id = ast_id_counter;
                ast_id_counter += 1;

                let unit_vol = ore_type.volume.max(0.01);
                let batch = ore_type.portion_size.max(1);
                let raw_units = ((2_000.0 / unit_vol).round() as u64).max(batch);
                let units = (raw_units / batch).max(1) * batch;
                let total_m3 = (units as f64) * unit_vol;
                let scaled_radius =
                    (ore_type.radius.max(1.0) * 5.0 * total_m3.cbrt()).clamp(60.0, 2_500.0);

                let offset = DVec3::new(
                    (idx as f64 * 600.0) + 200.0,
                    (idx as f64 * 300.0) - 100.0,
                    (idx as f64 * 400.0) + 150.0,
                );
                let pos = belt_pos + offset;

                let ast_state = AsteroidState {
                    asteroid_id: id,
                    belt_id: cel.celestial_id,
                    type_id: ore_type.type_id,
                    initial_units: units,
                    remaining_units: units,
                    unit_volume: unit_vol,
                    depleted: false,
                    respawn_at_tick: None,
                };

                let ball = Ball {
                    id,
                    type_id: ore_type.type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: (total_m3 * 5.0e7).max(ore_type.mass),
                    radius: scaled_radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                };

                ballpark.set_ball_slim_meta(
                    id,
                    SlimItemMeta {
                        group_id: ore_type.group_id,
                        category_id: ore_type.category_id,
                        owner_id: 1,
                        char_id: None,
                        corp_id: -1,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                );
                ballpark.add_ball(ball);
                states.push(ast_state);
            }
        }
    }
    states
}

fn roll_probability(prob: f64) -> bool {
    if prob <= 0.0 {
        return false;
    }
    if prob >= 1.0 {
        return true;
    }
    let mut buf = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut buf).is_ok() {
            let val = u64::from_ne_bytes(buf);
            let frac = (val as f64) / (u64::MAX as f64);
            return frac < prob;
        }
    }
    true
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
    belt_respawn_interval_secs: Arc<std::sync::atomic::AtomicU64>,
    downtime_utc_secs_of_day: Arc<std::sync::atomic::AtomicU64>,
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
        }
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

fn step_cycles_and_capacitor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
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
            // 1. Local shield boost & armor repair healing (`feat:modules.shield-booster`, `DG-4`)
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                if cycle.shield_boost_amount > 0.0 {
                    cs.shield_hp = (cs.shield_hp + cycle.shield_boost_amount).min(cs.max_shield_hp);
                }
                if cycle.armor_repair_amount > 0.0 {
                    cs.armor_hp = (cs.armor_hp + cycle.armor_repair_amount).min(cs.max_armor_hp);
                }
            }

            // 2. Target interactions: damage, cap drain, cap transfer (`C-1`, `C-2`, `DG-4`)
            if let Some(target_id) = cycle.target_id {
                if cycle.damage_vector.total() > 0.0 {
                    ballpark.apply_damage_to_ball(target_id, cycle.damage_vector);
                }
                if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                    if cycle.cap_drain_target > 0.0 {
                        cs.capacitor = (cs.capacitor - cycle.cap_drain_target).max(0.0);
                    }
                    if cycle.cap_transfer_target > 0.0 {
                        let transferred = cycle.cap_transfer_target.min(cs.capacitor);
                        cs.capacitor -= transferred;
                        if let Some(src_cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                            src_cs.capacitor = (src_cs.capacitor + transferred).min(src_cs.max_capacitor);
                        }
                    }
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
                    let mut has_cap = true;
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        if cs.capacitor >= cycle.cap_need_per_cycle {
                            cs.capacitor -= cycle.cap_need_per_cycle;
                        } else {
                            has_cap = false;
                        }
                    }
                    if !has_cap {
                        finished_indices.push(idx);
                    }
                }
            }
        }
    }
    for idx in finished_indices.into_iter().rev() {
        let finished = active_module_cycles.remove(idx);
        cleanup_removed_module_cycle(ballpark, &finished);
    }
    reconcile_tackle_and_bubbles(
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
    );
}

fn cleanup_removed_module_cycle(ballpark: &mut Ballpark, removed: &ActiveModuleCycle) {
    if let Some(orig_v) = removed.original_max_velocity {
        if let Some(ball) = ballpark.balls.get_mut(&removed.ship_id) {
            ball.max_velocity = orig_v;
        }
    }
    if let Some(orig_m) = removed.original_mass {
        if let Some(ball) = ballpark.balls.get_mut(&removed.ship_id) {
            ball.mass = orig_m;
        }
    }
    if let Some(orig_sig) = removed.original_signature_radius {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.signature_radius = orig_sig;
        }
    }
    if removed.hic_bubble_radius_m.is_some() {
        ballpark.remove_warp_bubble(removed.module_id);
    }
}

fn reconcile_tackle_and_bubbles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
) {
    // 1. Collect all target balls currently subjected to MWD/MJD suppression (Warp Scrambler / Scripted HIC WDFG)
    let mut mwd_disabled_targets: Vec<BallId> = Vec::new();
    for cycle in active_module_cycles.iter() {
        if cycle.disables_target_mwd_mjd {
            if let Some(tid) = cycle.target_id {
                if !mwd_disabled_targets.contains(&tid) {
                    mwd_disabled_targets.push(tid);
                }
            }
        }
    }

    // Immediately shut down any active MWD or MJD cycle on scrambled targets
    if !mwd_disabled_targets.is_empty() {
        let mut idx = 0;
        while idx < active_module_cycles.len() {
            if active_module_cycles[idx].is_mwd_or_mjd
                && mwd_disabled_targets.contains(&active_module_cycles[idx].ship_id)
            {
                let shut_down = active_module_cycles.remove(idx);
                cleanup_removed_module_cycle(ballpark, &shut_down);
            } else {
                idx += 1;
            }
        }
    }

    // 2. Sync HIC unscripted warp disruption bubbles to the HIC ship's current position
    for cycle in active_module_cycles.iter() {
        if let Some(radius) = cycle.hic_bubble_radius_m {
            if let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) {
                ballpark.add_warp_bubble(cycle.module_id, ship_ball.position, radius);
            }
        }
    }

    // 3. Recompute warp_scramble_strength, warp_core_strength, is_mwd_mjd_disabled, and disallow_assistance for all balls
    let ball_ids: Vec<BallId> = ballpark.combat_states.keys().copied().collect();
    for ball_id in ball_ids {
        let base_scram = base_scramble_strengths.get(&ball_id).copied().unwrap_or(0);
        let base_core = base_core_strengths.get(&ball_id).copied().unwrap_or(1);

        let mod_scram: i32 = active_module_cycles
            .iter()
            .filter(|c| c.target_id == Some(ball_id))
            .map(|c| c.warp_scramble_points)
            .sum();
        let mod_core: i32 = active_module_cycles
            .iter()
            .filter(|c| c.ship_id == ball_id)
            .map(|c| c.warp_core_stab_bonus)
            .sum();
        let mwd_disabled = active_module_cycles
            .iter()
            .any(|c| c.target_id == Some(ball_id) && c.disables_target_mwd_mjd);
        let disallow_assist = active_module_cycles
            .iter()
            .any(|c| c.ship_id == ball_id && c.hic_bubble_radius_m.is_some());

        if let Some(cs) = ballpark.combat_states.get_mut(&ball_id) {
            cs.warp_scramble_strength = base_scram + mod_scram;
            cs.warp_core_strength = base_core + mod_core;
            cs.is_mwd_mjd_disabled = mwd_disabled;
            cs.disallow_assistance = disallow_assist;
        }
    }
}

fn restore_asteroid_from_template(
    id: BallId,
    orig_ast: &AsteroidState,
    orig_ball: &Ball,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    let mut reset_ast = orig_ast.clone();
    reset_ast.remaining_units = reset_ast.initial_units;
    reset_ast.depleted = false;
    reset_ast.respawn_at_tick = None;
    asteroids.insert(id, reset_ast);
    if !ballpark.balls.contains_key(&id) {
        let (group_id, category_id) = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&orig_ball.type_id))
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((25, 25));
        ballpark.set_ball_slim_meta(
            id,
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
        ballpark.add_ball(orig_ball.clone());
    }
}

fn respawn_due_asteroids(
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &HashMap<BallId, (AsteroidState, Ball)>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) -> Vec<BallId> {
    let Some(db) = db_opt else {
        return Vec::new();
    };
    let now_unix = db.clock().now_unix_secs();
    let mut belt_ids: Vec<BallId> = asteroid_templates
        .values()
        .map(|(ast, _)| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut respawned = Vec::new();
    for belt_id in belt_ids {
        // Per EVE wiki `Asteroids_and_ore.md:69`, a belt respawns after downtime only if
        // there are no asteroids left in the belt (`remaining_units == 0` for every asteroid in the belt).
        let belt_empty_and_due = asteroid_templates
            .iter()
            .filter(|(_, (orig_ast, _))| orig_ast.belt_id == belt_id)
            .all(|(&id, _)| {
                asteroids.get(&id).is_some_and(|a| {
                    a.remaining_units == 0
                        && a.respawn_at_tick.is_some_and(|due_unix| now_unix >= due_unix)
                })
            });
        if belt_empty_and_due {
            for (&id, (orig_ast, orig_ball)) in asteroid_templates {
                if orig_ast.belt_id == belt_id {
                    restore_asteroid_from_template(
                        id,
                        orig_ast,
                        orig_ball,
                        ballpark,
                        asteroids,
                        sde_opt,
                    );
                    respawned.push(id);
                }
            }
        }
    }
    respawned
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
                            let events = ballpark.step(dt);
                            step_cycles_and_capacitor(
                                &mut ballpark,
                                &mut active_module_cycles,
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
                            SystemCommand::SpawnAsteroid { asteroid, ball, reply } => {
                                let ball_id = asteroid.asteroid_id;
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
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
                                let res = if let Some(ast) = asteroids.get_mut(&asteroid_id) {
                                    if ast.remaining_units == 0 {
                                        Err("Asteroid is already depleted".to_string())
                                    } else {
                                        let to_mine = requested_units.min(ast.remaining_units);
                                        ast.remaining_units -= to_mine;

                                        let wasted_units = if waste_probability > 0.0 && ast.remaining_units > 0 {
                                            let rolls = waste_probability >= 1.0 || roll_probability(waste_probability);
                                            if rolls {
                                                to_mine.min(ast.remaining_units)
                                            } else {
                                                0
                                            }
                                        } else {
                                            0
                                        };
                                        ast.remaining_units -= wasted_units;

                                        let depleted = ast.remaining_units == 0;
                                        ast.depleted = depleted;
                                        let remaining_units = ast.remaining_units;
                                        let ore_type_id = ast.type_id;
                                        let belt_id = ast.belt_id;
                                        if depleted {
                                            let _ = ballpark.remove_ball(asteroid_id);
                                            // Per EVE wiki `Asteroids_and_ore.md:69`, belts respawn after downtime
                                            // only if there are no asteroids left in the belt.
                                            let belt_fully_depleted = asteroids
                                                .values()
                                                .filter(|a| a.belt_id == belt_id)
                                                .all(|a| a.remaining_units == 0);
                                            if belt_fully_depleted {
                                                if let Some(db) = &db_opt {
                                                    let interval_secs = belt_interval_atomic
                                                        .load(std::sync::atomic::Ordering::Relaxed);
                                                    let downtime_secs = downtime_utc_atomic
                                                        .load(std::sync::atomic::Ordering::Relaxed);
                                                    let due_unix = next_downtime_unix_secs(
                                                        db.clock().now_unix_secs(),
                                                        interval_secs,
                                                        downtime_secs,
                                                    );
                                                    for belt_ast in asteroids
                                                        .values_mut()
                                                        .filter(|a| a.belt_id == belt_id)
                                                    {
                                                        belt_ast.respawn_at_tick = Some(due_unix);
                                                    }
                                                }
                                            }
                                        }

                                        Ok(MiningExtractionResult {
                                            mined_units: to_mine,
                                            wasted_units,
                                            remaining_units,
                                            ore_type_id,
                                            depleted,
                                        })
                                    }
                                } else {
                                    Err(format!("Asteroid {asteroid_id} not found in system {solar_system_id}"))
                                };
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
                                if let Some(meta) = ballpark.ball_slim_meta.get_mut(&ball_id) {
                                    meta.is_cloaked = false;
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
                                let removed = ballpark.remove_ball(ball_id);
                                sync_ball_positions(&ballpark, &ball_positions_actor);
                                let _ = reply.send(removed);
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
                                if let Some((applied, destroyed, ball_opt)) = ballpark.apply_damage_to_ball(ball_id, damage) {
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
                                let events = ballpark.step(dt);
                                step_cycles_and_capacitor(
                                    &mut ballpark,
                                    &mut active_module_cycles,
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
                            SystemCommand::AddModuleCycle { mut cycle, reply } => {
                                if cycle.is_mwd_or_mjd {
                                    if ballpark
                                        .combat_states
                                        .get(&cycle.ship_id)
                                        .is_some_and(|cs| cs.is_mwd_mjd_disabled)
                                    {
                                        let _ = reply.send(Err(
                                            "WarpScrambled: Propulsion module disabled by warp scrambler or focused warp disruption script".to_string(),
                                        ));
                                        continue;
                                    }
                                }
                                if cycle.cap_need_per_cycle > 0.0 {
                                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                                        if cs.capacitor < cycle.cap_need_per_cycle {
                                            let _ = reply.send(Err("Insufficient capacitor to activate module".to_string()));
                                            continue;
                                        }
                                        cs.capacitor = (cs.capacitor - cycle.cap_need_per_cycle).max(0.0);
                                    }
                                }
                                // Remove any existing cycle for this (ship_id, module_id) before re-adding
                                if let Some(pos) = active_module_cycles
                                    .iter()
                                    .position(|c| c.ship_id == cycle.ship_id && c.module_id == cycle.module_id)
                                {
                                    let prev = active_module_cycles.remove(pos);
                                    cleanup_removed_module_cycle(&mut ballpark, &prev);
                                }
                                if let Some(vel_mult) = cycle.hic_velocity_multiplier {
                                    if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                                        cycle.original_max_velocity = Some(ball.max_velocity);
                                        ball.max_velocity *= vel_mult;
                                    }
                                } else if cycle.speed_boost_factor > 0.0 {
                                    if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                                        if cycle.original_max_velocity.is_none() {
                                            cycle.original_max_velocity = Some(ball.max_velocity);
                                            ball.max_velocity *= 1.0 + cycle.speed_boost_factor;
                                        }
                                    }
                                }
                                if let Some(mass_add) = cycle.hic_mass_addition {
                                    if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                                        cycle.original_mass = Some(ball.mass);
                                        ball.mass += mass_add;
                                    }
                                }
                                if let Some(sig_mult) = cycle.hic_sig_multiplier {
                                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                                        cycle.original_signature_radius = Some(cs.signature_radius);
                                        cs.signature_radius *= sig_mult;
                                    }
                                }
                                active_module_cycles.push(cycle);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    &mut active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                );
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
                                if let Some(pos) = active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                                    let removed = active_module_cycles.remove(pos);
                                    cleanup_removed_module_cycle(&mut ballpark, &removed);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        &mut active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                }
                                let _ = reply.send(Ok(()));
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
                            SystemCommand::IsBallScrambled { ball_id, reply } => {
                                let is_scram = ballpark.is_ball_scrambled(ball_id);
                                let _ = reply.send(is_scram);
                            }
                            SystemCommand::AddInterdictionSphere { id, position, radius, reply } => {
                                let ok = ballpark.add_interdiction_sphere(id, position, radius);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply } => {
                                let ok = ballpark.add_mobile_warp_disruptor(id, position, radius);
                                let _ = reply.send(ok);
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
