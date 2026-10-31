//! Celestial & asteroid belt population and `SystemHandle` client methods for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    build_tick_broadcast, ActiveModuleCycle, ActiveNaniteRepair, AsteroidState, CharacterId,
    MiningExtractionResult, SystemCommand, TickBroadcast,
};
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    pub(crate) cmd_tx: mpsc::Sender<SystemCommand>,
    pub(crate) event_tx: broadcast::Sender<TickBroadcast>,
    pub(crate) ball_positions: Arc<RwLock<HashMap<BallId, DVec3>>>,
    pub(crate) scrambled_balls: Arc<RwLock<std::collections::HashSet<BallId>>>,
    pub(crate) active_module_ships: Arc<RwLock<std::collections::HashSet<BallId>>>,
    pub ballpark: Arc<RwLock<Ballpark>>,
    pub(crate) actor_state: Arc<RwLock<super::SystemActorState>>,
    pub(crate) last_auto_stepped_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) npc_death_hook: Arc<RwLock<Option<super::NpcDeathHook>>>,
    pub(crate) module_deactivation_hook: Arc<RwLock<Option<super::ModuleDeactivationHook>>>,
    pub(crate) target_lock_completed_hook: Arc<RwLock<Option<super::TargetLockCompletedHook>>>,
    pub(crate) system_step_hook: Arc<RwLock<Option<super::SystemStepHook>>>,
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

    pub fn is_ball_scrambled_sync(&self, ball_id: BallId) -> bool {
        self.scrambled_balls
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&ball_id)
    }

    pub fn try_set_ball_position(&self, ball_id: BallId, position: DVec3) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(ball_id, position);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn try_stop_ball(&self, ball_id: BallId) {
        if let Ok(mut bp) = self.ballpark.write() {
            let _ = bp.set_mode(ball_id, MovementMode::Stop);
            if let Some(b) = bp.balls.get_mut(&ball_id) {
                b.speed_fraction = 0.0;
                b.velocity = DVec3::ZERO;
            }
        }
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

    pub async fn set_ball_wormhole_stages(
        &self,
        ball_id: BallId,
        wormhole_age: i64,
        wormhole_size: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::SetBallWormholeStages {
                ball_id,
                wormhole_age,
                wormhole_size,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_slim_item(
        &self,
        ball_id: BallId,
    ) -> Result<Option<eve_proto::PyRep>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetBallSlimItem { ball_id, reply: tx })
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

    pub async fn accept_docking(
        &self,
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
    ) -> Option<Ball> {
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SystemCommand::AcceptDocking {
                ship_id,
                station_id,
                station_pos,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    pub async fn emit_packaged_actions(
        &self,
        ball_id: BallId,
        events: Vec<DestinyEvent>,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SystemCommand::EmitPackagedActions {
                ball_id,
                events,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub fn try_remove_ball(&self, ball_id: BallId) {
        let (removed_cycles, db_opt, deact_guard) = {
            let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            let deact_guard = self.module_deactivation_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
            let super::SystemActorState {
                active_module_cycles,
                base_scramble_strengths,
                base_core_strengths,
                db_opt,
                ..
            } = &mut *st;
            let removed_cycles = super::cycles::remove_all_ship_module_cycles(
                &mut bp,
                active_module_cycles,
                base_scramble_strengths,
                base_core_strengths,
                &None,
                ball_id,
                None,
            );
            base_scramble_strengths.remove(&ball_id);
            base_core_strengths.remove(&ball_id);
            let _ = bp.remove_ball(ball_id);
            super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
            (removed_cycles, db_opt.clone(), deact_guard)
        };
        if let Some(db_ref) = db_opt {
            if !db_ref.is_write_locked_by_current_thread() {
                db_ref.with_mut_snapshot(|snap| {
                    let ship_mod_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|it| it.location_id == ball_id)
                        .map(|it| it.item_id)
                        .collect();
                    for mid in ship_mod_ids {
                        snap.module_active_states.remove(&mid);
                        snap.module_active_charges.remove(&mid);
                    }
                    for (removed, _) in &removed_cycles {
                        snap.module_active_states.remove(&removed.module_id);
                        snap.module_active_charges.remove(&removed.module_id);
                        if removed.adc_saved_resists.is_some() {
                            if let Some(tr) = snap.transient_item_attributes.get_mut(&removed.ship_id) {
                                for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112, 113] {
                                    tr.remove(&aid);
                                }
                            }
                        }
                        if removed.mutadaptive_spool_step > 0.0 {
                            if let Some(tr) = snap.transient_item_attributes.get_mut(&removed.module_id) {
                                tr.remove(&84);
                            }
                        }
                        if removed.is_rah {
                            if let Some(tr) = snap.transient_item_attributes.get_mut(&removed.module_id) {
                                for aid in 267..=270 {
                                    tr.remove(&aid);
                                }
                            }
                            if let Some(tr) = snap.transient_item_attributes.get_mut(&removed.ship_id) {
                                for aid in 267..=270 {
                                    tr.remove(&aid);
                                }
                            }
                        }
                    }
                });
                if let Some(hook) = deact_guard.as_ref() {
                    for (removed, char_id_opt) in &removed_cycles {
                        let cid = (*char_id_opt).or_else(|| {
                            db_ref.with_snapshot(|s| {
                                s.characters
                                    .values()
                                    .find(|c| c.active_ship_id == removed.ship_id)
                                    .map(|c| c.character_id)
                                    .or_else(|| s.items.get(&removed.ship_id).map(|it| it.owner_id))
                            })
                        });
                        if let Some(cid) = cid {
                            hook(cid, removed);
                        }
                    }
                }
            }
        }
    }

    pub fn try_spawn_asteroid(&self, asteroid: AsteroidState, ball: Ball) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let ball_id = ball.id;
        super::ensure_default_slim_meta(&mut bp, &ball, &st.sde_opt);
        let _ = bp.add_ball(ball.clone());
        st.asteroid_templates.insert(ball_id, (asteroid.clone(), ball));
        st.asteroids.insert(ball_id, asteroid);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn sync_asteroid_quantity(&self, asteroid_id: BallId, remaining_units: u64, due_unix_on_belt_empty: u64) {
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let mut belt_id_opt = None;
        if let Some(ast) = st.asteroids.get_mut(&asteroid_id) {
            ast.remaining_units = remaining_units;
            ast.depleted = remaining_units == 0;
            if remaining_units == 0 {
                belt_id_opt = Some(ast.belt_id);
            }
        }
        if let Some(belt_id) = belt_id_opt {
            let belt_fully_depleted = st
                .asteroids
                .values()
                .filter(|a| a.belt_id == belt_id)
                .all(|a| a.remaining_units == 0);
            if belt_fully_depleted {
                for belt_ast in st.asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
                    belt_ast.respawn_at_tick = Some(due_unix_on_belt_empty);
                }
            }
        }
    }

    pub fn try_spawn_ball_with_meta(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
    ) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let ball_id = ball.id;
        if bp.balls.contains_key(&ball_id) {
            let _ = bp.remove_ball(ball_id);
        }
        bp.set_ball_slim_meta(ball_id, slim_meta);
        bp.add_ball(ball);
        bp.combat_states.insert(ball_id, combat_state);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn try_spawn_ball_with_combat(&self, ball: Ball, combat_state: CombatState) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let ball_id = ball.id;
        if bp.balls.contains_key(&ball_id) {
            let _ = bp.remove_ball(ball_id);
        }
        bp.add_ball(ball);
        bp.combat_states.insert(ball_id, combat_state);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn try_update_combat_state<F: FnOnce(&mut CombatState)>(&self, ball_id: BallId, f: F) -> bool {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cs) = bp.combat_states.get_mut(&ball_id) {
            f(cs);
            true
        } else {
            false
        }
    }

    pub fn try_add_mobile_warp_disruptor(&self, id: BallId, position: DVec3, radius: f64) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.add_mobile_warp_disruptor(id, position, radius);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn try_remove_warp_bubble(&self, id: BallId) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let _ = bp.remove_warp_bubble(id);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn try_step_manual(&self, dt: f64) {
        if dt > 0.0 && dt.is_finite() {
            self.actor_state.write().unwrap_or_else(|e| e.into_inner()).sim_elapsed_secs += dt;
        }
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let events = bp.step(dt);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        let broadcast = build_tick_broadcast(self.solar_system_id, &mut bp, events);
        let _ = self.event_tx.send(broadcast);
    }

    pub fn sim_elapsed_secs(&self) -> f64 {
        self.actor_state.read().unwrap_or_else(|e| e.into_inner()).sim_elapsed_secs
    }

    pub fn take_pending_cycle_actions(&self) -> Vec<super::PendingCycleAction> {
        std::mem::take(&mut self.actor_state.write().unwrap_or_else(|e| e.into_inner()).pending_cycle_actions)
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

    /// Queues `event` for all grid observers of `ball_id`; returns `false` if the ball is unknown.
    pub async fn emit_destiny_event(&self, ball_id: BallId, event: DestinyEvent) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SystemCommand::EmitDestinyEvent { ball_id, event, reply: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Steers `ball_id` toward `point` (`beyonce.CmdGotoPoint`) and returns the current tick.
    pub async fn goto_point(&self, ball_id: BallId, point: glam::DVec3) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GotoPoint { ball_id, point, reply: tx })
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

    pub async fn apply_damage(&self, ball_id: BallId, damage: DamageVector) -> Result<Option<(f64, bool, Ball)>, String> {
        self.apply_damage_with_attacker(ball_id, damage, None).await
    }

    pub async fn apply_damage_with_attacker(
        &self,
        ball_id: BallId,
        damage: DamageVector,
        attacker_id: Option<CharacterId>,
    ) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ApplyDamage { ball_id, damage, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let res = rx.await.map_err(|_| "Solar system reply dropped".to_string())?;
        if let Some((_, true, _)) = &res {
            let hook = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(fut) = super::on_ball_destroyed_by_damage(
                self.solar_system_id,
                ball_id,
                attacker_id,
                hook.as_ref(),
            ) {
                let _ = fut.await;
            }
        }
        Ok(res)
    }

    pub async fn step_manual(&self, dt: f64) -> Result<TickBroadcast, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(SystemCommand::StepManual { dt, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        let (broadcast, destroyed_balls) = rx.await.map_err(|_| "Solar system reply dropped".to_string())?;
        let hook = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        for &(target_ball_id, owner_char_id) in &destroyed_balls {
            if let Some(fut) = super::on_ball_destroyed_by_damage(self.solar_system_id, target_ball_id, owner_char_id, hook.as_ref()) {
                fut.await;
            }
        }
        let step_hook = self.system_step_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(hook) = step_hook {
            hook(self.solar_system_id, dt, destroyed_balls).await;
        }
        Ok(broadcast)
    }

    pub async fn get_target_locks(&self, ball_id: BallId) -> Result<Vec<TargetLock>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(SystemCommand::GetTargetLocks { ball_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_target_lock(&self, source_id: BallId, lock: TargetLock) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(SystemCommand::AddTargetLock { source_id, lock, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_target_lock(&self, source_id: BallId, target_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(SystemCommand::RemoveTargetLock { source_id, target_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn clear_target_locks(&self, source_id: BallId) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(SystemCommand::ClearTargetLocks { source_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
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

    pub async fn remove_module_cycle(&self, ship_id: BallId, module_id: i64) -> Result<Option<f64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::RemoveModuleCycle { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn has_module_cycle(&self, ship_id: BallId, module_id: i64) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SystemCommand::HasModuleCycle {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Parity: [plan:DG-4] Entering warp halts active non-warp module cycles.
    pub async fn stop_non_warp_module_cycles(&self, ship_id: BallId) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::StopNonWarpModuleCycles { ship_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
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

    pub async fn micro_jump_field(
        &self,
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
    ) -> Result<Vec<BallId>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::MicroJumpField {
                ball_id,
                distance_meters,
                field_radius_meters,
                max_jumped_ships,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn tick(&self) -> Result<TickBroadcast, String> {
        self.step_manual(1.0).await
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
        self.apply_ecm_burst_ships(source_id, burst_radius_m, jam_strengths, duration_s, roll, None)
            .await
    }

    pub async fn apply_ecm_burst_ships(
        &self,
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
        ship_eval_overrides: Option<HashMap<BallId, (usize, f64, f64)>>,
    ) -> Result<Vec<BallId>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::ApplyEcmBurst {
                source_id,
                burst_radius_m,
                jam_strengths,
                duration_s,
                roll,
                ship_eval_overrides,
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

#[allow(clippy::type_complexity)]
fn db_ballpark_map() -> &'static std::sync::Mutex<
    HashMap<
        (u64, SolarSystemId),
        (
            std::sync::Weak<RwLock<Ballpark>>,
            std::sync::Weak<RwLock<HashMap<BallId, DVec3>>>,
            std::sync::Weak<RwLock<std::collections::HashSet<BallId>>>,
        ),
    >,
> {
    static MAP: std::sync::OnceLock<
        std::sync::Mutex<
            HashMap<
                (u64, SolarSystemId),
                (
                    std::sync::Weak<RwLock<Ballpark>>,
                    std::sync::Weak<RwLock<HashMap<BallId, DVec3>>>,
                    std::sync::Weak<RwLock<std::collections::HashSet<BallId>>>,
                ),
            >,
        >,
    > = std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn register_db_system_ballpark(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    handle: &SystemHandle,
) {
    let mut g = db_ballpark_map().lock().unwrap_or_else(|e| e.into_inner());
    if g.len() > 256 {
        g.retain(|_, (bp, _, _)| bp.strong_count() > 0);
    }
    g.insert(
        (db.instance_id(), solar_system_id),
        (
            Arc::downgrade(&handle.ballpark),
            Arc::downgrade(&handle.ball_positions),
            Arc::downgrade(&handle.scrambled_balls),
        ),
    );
}

/// Synchronously removes `ball_id` from `solar_system_id`'s `Ballpark` if spawned for `db`.
pub fn try_remove_ball_for_db(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    ball_id: BallId,
) {
    let entry = db_ballpark_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(db.instance_id(), solar_system_id))
        .cloned();
    if let Some((bp_w, pos_w, scram_w)) = entry {
        if let (Some(bp_arc), Some(pos_arc), Some(scram_arc)) =
            (bp_w.upgrade(), pos_w.upgrade(), scram_w.upgrade())
        {
            let mut bp = bp_arc.write().unwrap_or_else(|e| e.into_inner());
            let _ = bp.remove_ball(ball_id);
            super::sync_ball_positions(&bp, &pos_arc, &scram_arc);
        }
    }
}

