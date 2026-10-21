//! Celestial & asteroid belt population and `SystemHandle` client methods for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    ActiveModuleCycle, ActiveNaniteRepair, AsteroidState, MiningExtractionResult, SystemCommand,
    TickBroadcast, next_downtime_unix_secs,
};
use eve_destiny::wire::encode_destiny_updates;
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    pub(super) cmd_tx: mpsc::Sender<SystemCommand>,
    pub(super) event_tx: broadcast::Sender<TickBroadcast>,
    pub(super) ball_positions: Arc<RwLock<HashMap<BallId, DVec3>>>,
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

    /// Updates the slim item metadata of an in-space ball and emits `DestinyEvent::OnSlimItemChange`
    /// to every observer on its grid via `Ballpark::change_slim_item`.
    pub async fn change_slim_item(
        &self,
        ball_id: BallId,
        slim_meta: SlimItemMeta,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ChangeSlimItem {
                ball_id,
                slim_meta,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
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
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
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

    pub fn try_remove_ball(&self, ball_id: BallId) {
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
        let (tx, _rx) = oneshot::channel();
        let _ = self.cmd_tx.try_send(SystemCommand::RemoveBall {
            ball_id,
            reply: tx,
        });
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

    pub async fn apply_ecm_jam(&self, target_id: BallId, duration_s: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ApplyEcmJam {
                target_id,
                duration_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_ecm_burst(
        &self,
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
    ) -> Result<Vec<BallId>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ApplyEcmBurst {
                source_id,
                burst_radius_m,
                jam_strengths,
                duration_s,
                roll,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn schedule_mobile_warp_disruptor(
        &self,
        id: BallId,
        type_id: u32,
        position: DVec3,
        radius: f64,
        anchoring_delay_s: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ScheduleMobileWarpDisruptor {
                id,
                type_id,
                position,
                radius,
                anchoring_delay_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn emit_destiny_event(
        &self,
        ball_id: BallId,
        event: DestinyEvent,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::EmitDestinyEvent {
                ball_id,
                event,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn goto_point(&self, ball_id: BallId, point: DVec3) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GotoPoint {
                ball_id,
                point,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn update_module_overload(
        &self,
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::UpdateModuleOverload {
                ship_id,
                module_id,
                is_overloaded,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn start_nanite_repair(&self, repair: ActiveNaniteRepair) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::StartNaniteRepair { repair, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn stop_nanite_repair(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<Option<f64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::StopNaniteRepair {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn record_incoming_armor_damage(
        &self,
        ship_id: BallId,
        damage: [f64; 4],
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::RecordIncomingArmorDamage {
                ship_id,
                damage,
                reply: tx,
            })
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

pub(super) fn ensure_default_slim_meta(
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

pub(super) fn build_tick_broadcast(
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

pub(super) fn restore_asteroid_from_template(
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

pub(super) fn respawn_due_asteroids(
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

pub(super) fn handle_mine_asteroid(
    solar_system_id: SolarSystemId,
    asteroid_id: BallId,
    requested_units: u64,
    waste_probability: f64,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    mechanic_rng: &crate::rng::MechanicRng,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    belt_interval_atomic: &std::sync::atomic::AtomicU64,
    downtime_utc_atomic: &std::sync::atomic::AtomicU64,
) -> Result<MiningExtractionResult, String> {
    if let Some(ast) = asteroids.get_mut(&asteroid_id) {
        if ast.remaining_units == 0 {
            Err("Asteroid is already depleted".to_string())
        } else {
            let to_mine = requested_units.min(ast.remaining_units);
            ast.remaining_units -= to_mine;

            let wasted_units = if waste_probability > 0.0 && ast.remaining_units > 0 {
                let rolls = waste_probability >= 1.0 || mechanic_rng.roll(waste_probability);
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
                    if let Some(db) = db_opt {
                        let interval_secs =
                            belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed);
                        let downtime_secs =
                            downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed);
                        let due_unix = next_downtime_unix_secs(
                            db.clock().now_unix_secs(),
                            interval_secs,
                            downtime_secs,
                        );
                        for belt_ast in asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
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
        Err(format!(
            "Asteroid {asteroid_id} not found in system {solar_system_id}"
        ))
    }
}

/// Synchronizes in-space player ship slim item metadata (`corp_id`, `alliance_id`, `security_status`)
/// with `WorldRepository` character state, calling `Ballpark::change_slim_item` to emit
/// `DestinyEvent::OnSlimItemChange` to all grid observers whenever a pilot's slim item changes.
// parity: destiny:OnSlimItemChange
pub(super) fn sync_player_slim_items(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let Some(db) = db_opt else {
        return;
    };
    let updates: Vec<(BallId, SlimItemMeta)> = ballpark
        .ball_slim_meta
        .iter()
        .filter_map(|(&ball_id, meta)| {
            if meta.category_id != 6 {
                return None;
            }
            let char_id = meta.char_id?;
            let ch = db.get_character(char_id).ok()?;
            let new_corp_id = ch.corporation_id;
            let new_alliance_id = ch.alliance_id.unwrap_or(0);
            let new_sec = ch.security_status;
            if meta.corp_id != new_corp_id
                || meta.alliance_id != new_alliance_id
                || (meta.security_status - new_sec).abs() > 1e-9
            {
                let mut updated = meta.clone();
                updated.corp_id = new_corp_id;
                updated.alliance_id = new_alliance_id;
                updated.security_status = new_sec;
                Some((ball_id, updated))
            } else {
                None
            }
        })
        .collect();
    for (ball_id, updated_meta) in updates {
        ballpark.change_slim_item(ball_id, updated_meta);
    }
}
