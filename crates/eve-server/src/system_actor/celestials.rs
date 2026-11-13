//! Celestial & asteroid belt population and `SystemHandle` client methods for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    build_tick_broadcast, ActiveModuleCycle, ActiveNaniteRepair, AsteroidState, CharacterId,
    MiningExtractionResult, SystemCommand, TickBroadcast,
};
use eve_db::lock_rank::{
    assert_no_locks_held, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK, RANK_LEAF,
};
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    pub(crate) cmd_tx: mpsc::Sender<SystemCommand>,
    pub(crate) event_tx: broadcast::Sender<TickBroadcast>,
    pub(crate) ball_positions: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(crate) scrambled_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub(crate) committed_warp_balls: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(crate) active_module_ships: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub ballpark: Arc<RankedRwLock<Ballpark, RANK_BALLPARK>>,
    pub(crate) actor_state: Arc<RankedRwLock<super::SystemActorState, RANK_ACTOR_STATE>>,
    pub(crate) last_auto_stepped_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) tidi_factor_bits: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) hibernated: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) hibernated_since_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) idle_ticks: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) dynamic_ball_count: Arc<std::sync::atomic::AtomicUsize>,
    pub command_panics: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    pub(crate) panic_next_command_for_test: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) npc_death_hook: Arc<RankedRwLock<Option<super::NpcDeathHook>, RANK_LEAF>>,
    pub(crate) module_deactivation_hook: Arc<RankedRwLock<Option<super::ModuleDeactivationHook>, RANK_LEAF>>,
    pub(crate) target_lock_completed_hook: Arc<RankedRwLock<Option<super::TargetLockCompletedHook>, RANK_LEAF>>,
    pub(crate) system_step_hook: Arc<RankedRwLock<Option<super::SystemStepHook>, RANK_LEAF>>,
}

impl SystemHandle {
    #[track_caller]
    pub(crate) fn cmd_sender(&self) -> &mpsc::Sender<SystemCommand> {
        assert_no_locks_held();
        &self.cmd_tx
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TickBroadcast> {
        self.hibernated
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.hibernated_since_unix
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.idle_ticks
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.event_tx.subscribe()
    }

    /// Returns `true` when this solar system has no active session subscribers, no dynamic balls,
    /// and no active module/repair/anchoring timers (`#919`, `#866`).
    pub fn is_hibernated(&self) -> bool {
        if self.event_tx.receiver_count() > 0 {
            return false;
        }
        if self
            .dynamic_ball_count
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            return false;
        }
        if !self.hibernated.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        if let Ok(bp) = self.ballpark.try_read() {
            if bp.balls.keys().any(|&bid| !bp.is_static_ball(bid)) {
                return false;
            }
        }
        true
    }

    pub fn hibernated_since_unix(&self) -> u64 {
        self.hibernated_since_unix
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn idle_ticks(&self) -> u64 {
        self.idle_ticks.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn record_idle_tick(&self) -> u64 {
        self.idle_ticks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    pub fn get_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_LEAF)
        {
            self.ball_positions
                .read_unranked()
                .unwrap_or_else(|e| e.into_inner())
                .get(&ball_id)
                .copied()
        } else {
            self.ball_positions
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&ball_id)
                .copied()
        }
    }

    pub fn is_ball_scrambled_sync(&self, ball_id: BallId) -> bool {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_LEAF)
        {
            self.scrambled_balls
                .read_unranked()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&ball_id)
        } else {
            self.scrambled_balls
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&ball_id)
        }
    }

    pub fn try_set_ball_position(&self, ball_id: BallId, position: DVec3) {
        let updated = {
            let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
            let ok = bp.set_ball_position(ball_id, position);
            super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
            ok
        };
        if updated {
            let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
            let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
            persist_ball_positions_in_db(&bp, &[ball_id], &st.db_opt, &st.ball_space_entry_epochs);
        }
    }

    pub fn try_stop_ball(&self, ball_id: BallId) {
        if let Ok(mut bp) = self.ballpark.write() {
            let _ = bp.force_stop_warp(ball_id);
            super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        }
    }

    pub async fn spawn_ball(&self, ball: Ball) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SpawnBall { ball, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_ball_max_velocity(
        &self,
        ball_id: BallId,
        max_velocity: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetBallMaxVelocity {
                ball_id,
                max_velocity,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_ball_max_velocity_cap(
        &self,
        ball_id: BallId,
        cap: Option<f64>,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetBallMaxVelocityCap {
                ball_id,
                cap,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn spawn_ball_with_combat(
        &self,
        ball: Ball,
        combat_state: CombatState,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.mine_asteroid_with_db_sync(asteroid_id, requested_units, waste_probability, true)
            .await
    }

    pub async fn mine_asteroid_with_db_sync(
        &self,
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
        persist_db: bool,
    ) -> Result<MiningExtractionResult, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::MineAsteroid {
                asteroid_id,
                requested_units,
                waste_probability,
                persist_db,
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
            .send(SystemCommand::PopulateAsteroids { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn remove_ball(&self, ball_id: BallId) -> Option<Ball> {
        self.cancel_ship_nanite_repairs_sync(ball_id, true);
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
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
        self.cancel_ship_nanite_repairs_sync(ship_id, true);
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::AcceptDocking {
                ship_id,
                station_id,
                station_pos,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Atomically removes `ball_id` from this solar system (`#842`, `#917` E-2),
    /// clearing its active module cycles and target locks and returning its final
    /// `Ball`, `CombatState`, and `SlimItemMeta`. Returns `None` if `ball_id` is
    /// not present in space.
    pub async fn take_ball(&self, ball_id: BallId) -> Option<super::TakenBall> {
        self.cancel_ship_nanite_repairs_sync(ball_id, true);
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::TakeBall {
                ball_id,
                accept_docking_target: None,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Atomically removes `ship_id` for docking at `station_id` (`#842`, `#917` E-2),
    /// emitting `OnDockingAccepted` and returning its final `TakenBall` snapshot,
    /// or `None` if `ship_id` is not present in space.
    pub async fn take_ball_for_docking(
        &self,
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
    ) -> Option<super::TakenBall> {
        self.cancel_ship_nanite_repairs_sync(ship_id, true);
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::TakeBall {
                ball_id: ship_id,
                accept_docking_target: Some((station_id, station_pos)),
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Re-inserts a previously [`Self::take_ball`] snapshot into this solar system
    /// when a handoff's subsequent DB transaction fails (`#842`, `#917` E-2).
    pub async fn restore_ball(&self, taken: super::TakenBall) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RestoreBall { taken, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn emit_packaged_actions(
        &self,
        ball_id: BallId,
        events: Vec<DestinyEvent>,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
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
        self.cancel_ship_nanite_repairs_sync(ball_id, true);
        let remove_from_bp_and_st =
            |bp: &mut Ballpark, st: &mut super::SystemActorState| {
                let deact_guard = if eve_db::lock_rank::held_lock_ranks()
                    .iter()
                    .any(|&r| r >= eve_db::lock_rank::RANK_LEAF)
                {
                    self.module_deactivation_hook
                        .read_unranked()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                } else {
                    self.module_deactivation_hook
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                };
                let (removed_cycles, fallback_slim_owner, db_opt_cloned) = {
                    let super::SystemActorState {
                        active_module_cycles,
                        base_scramble_strengths,
                        base_core_strengths,
                        db_opt,
                        ..
                    } = &mut *st;
                    let removed_cycles = super::cycles::remove_all_ship_module_cycles(
                        bp,
                        active_module_cycles,
                        base_scramble_strengths,
                        base_core_strengths,
                        &None,
                        ball_id,
                        None,
                    );
                    let fallback_slim_owner = bp
                        .ball_slim_meta
                        .get(&ball_id)
                        .and_then(|m| (m.owner_id > 0).then_some(m.owner_id));
                    base_scramble_strengths.remove(&ball_id);
                    base_core_strengths.remove(&ball_id);
                    let _ = bp.remove_ball(ball_id);
                    (removed_cycles, fallback_slim_owner, db_opt.clone())
                };
                celestials_helpers::remove_ball_checkpoint_tracking(st, ball_id);
                celestials_helpers::update_system_hibernation_flag(
                    st,
                    bp,
                    self.event_tx.receiver_count(),
                );
                super::sync_ball_positions(bp, &self.ball_positions, &self.scrambled_balls);
                (removed_cycles, fallback_slim_owner, db_opt_cloned, deact_guard)
            };
        let (removed_cycles, fallback_slim_owner, db_opt, deact_guard) =
            if eve_db::lock_rank::held_lock_ranks()
                .iter()
                .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
            {
                let mut bp = match self.ballpark.try_write_unranked() {
                    Ok(g) => g,
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return,
                };
                let mut st = match self.actor_state.try_write_unranked() {
                    Ok(g) => g,
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return,
                };
                remove_from_bp_and_st(&mut bp, &mut st)
            } else {
                let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
                let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
                remove_from_bp_and_st(&mut bp, &mut st)
            };
        if let Some(db_ref) = db_opt {
            if !db_ref.is_write_locked_by_current_thread()
                && !eve_db::lock_rank::held_lock_ranks()
                    .iter()
                    .any(|&r| r >= eve_db::lock_rank::RANK_WORLD_INNER)
            {
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
                        if removed.is_adc || removed.ehe_hull_resists.is_some() {
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
                        let cid = (*char_id_opt)
                            .or_else(|| {
                                db_ref.with_snapshot(|s| {
                                    s.characters
                                        .values()
                                        .find(|c| c.active_ship_id == removed.ship_id)
                                        .map(|c| c.character_id)
                                        .or_else(|| s.items.get(&removed.ship_id).map(|it| it.owner_id))
                                })
                            })
                            .or(fallback_slim_owner);
                        if let Some(cid) = cid {
                            hook(cid, removed);
                        }
                    }
                }
            }
        }
    }

    pub fn has_active_nanite_repair_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_nanite_repairs
            .iter()
            .any(|r| r.ship_id == ship_id && r.module_id == module_id)
    }

    pub fn has_active_module_cycle_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.module_id == module_id)
    }

    pub fn has_active_mjd_or_mjfg_spool_sync(&self, ship_id: BallId) -> bool {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.mjd_jump_distance_m.is_some())
    }

    pub fn is_module_mjd_or_mjfg_spooling_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.module_id == module_id && c.mjd_jump_distance_m.is_some())
    }

    pub fn cancel_ship_nanite_repairs_sync(&self, ship_id: BallId, refund_paste: bool) {
        let drain_fn = |st: &mut super::SystemActorState| {
            let mut cancelled = Vec::new();
            st.active_nanite_repairs.retain(|r| {
                if r.ship_id == ship_id {
                    let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                    let rem_dmg = (r.initial_damage_hp * (1.0 - frac)).max(0.0);
                    let paste_used = ((r.paste_total_needed as f64) * frac).ceil() as i32;
                    let refund = (r.paste_consumed_so_far
                        - paste_used.clamp(0, r.paste_consumed_so_far))
                    .max(0);
                    cancelled.push((r.char_id, r.module_id, rem_dmg, refund));
                    false
                } else {
                    true
                }
            });
            (cancelled, st.db_opt.clone(), st.sde_opt.clone())
        };
        let (cancelled, db_opt, sde_opt) = if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_ACTOR_STATE)
        {
            match self.actor_state.try_write_unranked() {
                Ok(mut st) => drain_fn(&mut st),
                Err(std::sync::TryLockError::Poisoned(e)) => drain_fn(&mut e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => return,
            }
        } else {
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            drain_fn(&mut st)
        };
        if !cancelled.is_empty() {
            if let Some(db) = db_opt {
                if !db.is_write_locked_by_current_thread() {
                    db.with_mut_snapshot(|snap| {
                        for (char_id, module_id, rem_dmg, refund) in cancelled {
                            if let Some(m_item) = snap.items.get_mut(&module_id) {
                                if rem_dmg <= 1e-6 {
                                    m_item.dynamic_attributes.remove(&eve_sde::attr_ids::DAMAGE);
                                } else {
                                    m_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, rem_dmg);
                                }
                            }
                            if refund_paste && refund > 0 {
                                if let Some(ref sde) = sde_opt {
                                    crate::services::tactical_module_mechanics::refund_paste_to_cargo_capped_in_snap(
                                        snap,
                                        sde,
                                        char_id,
                                        ship_id,
                                        refund,
                                    );
                                }
                            }
                        }
                    });
                }
            }
        }
    }

    pub fn try_spawn_asteroid(&self, asteroid: AsteroidState, ball: Ball) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let ball_id = ball.id;
        let mut ast_ball = ball;
        ast_ball.mode = eve_destiny::MovementMode::Rigid;
        let (group_id, category_id, name) = st
            .sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&ast_ball.type_id))
            .map(|t| {
                (
                    t.group_id,
                    t.category_id,
                    (!t.name.is_empty()).then(|| t.name.clone()),
                )
            })
            .unwrap_or((25, 25, None));
        let _ = bp.add_static_ball_with_meta(
            ast_ball.clone(),
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        );
        bp.queue_event_for_all_grid_observers(
            Some(ball_id),
            &eve_destiny::DestinyEvent::AddBalls2 {
                balls: vec![ast_ball.clone()],
            },
        );
        st.asteroid_templates
            .insert(ball_id, (asteroid.clone(), ast_ball));
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
        let apply_spawn = |bp: &mut Ballpark| {
            let ball_id = ball.id;
            if bp.balls.contains_key(&ball_id) {
                let _ = bp.remove_ball(ball_id);
            }
            if matches!(ball.mode, eve_destiny::MovementMode::Rigid) {
                bp.add_static_ball_with_meta(ball.clone(), slim_meta);
                bp.combat_states.insert(ball_id, combat_state);
                bp.pending_events
                    .push(eve_destiny::DestinyEvent::AddBall { ball: ball.clone() });
                bp.queue_event_for_all_grid_observers(
                    Some(ball_id),
                    &eve_destiny::DestinyEvent::AddBalls2 { balls: vec![ball] },
                );
            } else {
                if super::should_pin_stationary_ball(&ball, Some(&slim_meta)) {
                    bp.mark_ball_non_free(ball_id, false);
                }
                bp.set_ball_slim_meta(ball_id, slim_meta);
                bp.add_ball(ball);
                bp.combat_states.insert(ball_id, combat_state);
            }
            super::sync_ball_positions(bp, &self.ball_positions, &self.scrambled_balls);
            let dyn_count = bp.balls.keys().filter(|&&bid| !bp.is_static_ball(bid)).count();
            self.dynamic_ball_count
                .store(dyn_count, std::sync::atomic::Ordering::Relaxed);
            if dyn_count > 0 {
                self.hibernated
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                self.hibernated_since_unix
                    .store(0, std::sync::atomic::Ordering::Relaxed);
                self.idle_ticks
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
        };
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
        {
            match self.ballpark.try_write_unranked() {
                Ok(mut bp) => apply_spawn(&mut bp),
                Err(std::sync::TryLockError::Poisoned(e)) => apply_spawn(&mut e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
        } else {
            let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
            apply_spawn(&mut bp);
        }
    }

    pub fn try_push_set_state(&self, observer_ball_id: BallId) -> bool {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.queue_set_state(observer_ball_id)
    }

    pub fn try_handoff_in_space_observer(
        &self,
        old_ball_id: BallId,
        new_ball_id: BallId,
        added_ball_ids: &[BallId],
    ) -> bool {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.queue_in_space_ship_handoff(old_ball_id, new_ball_id, added_ball_ids)
    }

    pub fn try_spawn_ball_with_combat(&self, ball: Ball, combat_state: CombatState) {
        let sde_opt = self
            .actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sde_opt
            .clone();
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let ball_id = ball.id;
        let existing_meta = bp.ball_slim_meta.get(&ball_id).cloned();
        if bp.balls.contains_key(&ball_id) {
            let _ = bp.remove_ball(ball_id);
        }
        if let Some(meta) = existing_meta {
            bp.set_ball_slim_meta(ball_id, meta);
        } else {
            super::ensure_default_slim_meta(&mut bp, &ball, &sde_opt);
        }
        if super::should_pin_stationary_ball(&ball, bp.ball_slim_meta.get(&ball_id)) {
            bp.mark_ball_non_free(ball_id, false);
        }
        bp.add_ball(ball);
        bp.combat_states.insert(ball_id, combat_state);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        let dyn_count = bp.balls.keys().filter(|&&bid| !bp.is_static_ball(bid)).count();
        self.dynamic_ball_count
            .store(dyn_count, std::sync::atomic::Ordering::Relaxed);
        if dyn_count > 0 {
            self.hibernated
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.hibernated_since_unix
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.idle_ticks
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
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

    /// Atomically mutates declarative modifiers on `ball_id`'s [`CombatState`] and recomputes
    /// its derived `Ball` and `CombatState` attributes from immutable baselines (`#584`).
    pub fn try_recompute_combat_attributes(
        &self,
        ball_id: BallId,
        f: impl FnOnce(&mut CombatState),
    ) -> bool {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let Some(cs) = bp.combat_states.get_mut(&ball_id) else {
            return false;
        };
        f(cs);
        bp.recompute_ball_attributes(ball_id);
        true
    }

    /// Pushes recomputed ship kinematics & `CombatState` baselines from `ServerContext`
    /// after skill completions, booster expirations, or expert system expirations (`#736`).
    pub fn try_sync_ship_ballpark_stats(&self, ball_id: BallId, fresh: &CombatState) -> bool {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let prev_max_vel = bp.balls.get(&ball_id).map(|b| b.max_velocity);
        let Some(cs) = bp.combat_states.get_mut(&ball_id) else {
            return false;
        };
        cs.base_max_velocity = fresh.base_max_velocity;
        cs.base_mass = fresh.base_mass;
        cs.base_inertia = fresh.base_inertia;
        cs.base_signature_radius = fresh.base_signature_radius;
        cs.base_max_shield_hp = fresh.base_max_shield_hp;
        cs.base_max_armor_hp = fresh.base_max_armor_hp;
        cs.base_max_structure_hp = fresh.base_max_structure_hp;
        cs.base_max_capacitor = fresh.base_max_capacitor;
        cs.shield_recharge_time_ms = fresh.shield_recharge_time_ms;
        cs.base_capacitor_recharge_time_ms = fresh.base_capacitor_recharge_time_ms;
        cs.base_shield_resonance = fresh.base_shield_resonance;
        cs.base_armor_resonance = fresh.base_armor_resonance;
        cs.base_structure_resonance = fresh.base_structure_resonance;
        bp.recompute_ball_attributes(ball_id);
        if let (Some(prev), Some(new_v)) = (prev_max_vel, bp.balls.get(&ball_id).map(|b| b.max_velocity)) {
            if (prev - new_v).abs() > 1e-6 {
                bp.emit_event(
                    Some(ball_id),
                    eve_destiny::DestinyEvent::SetMaxSpeed {
                        ball_id,
                        max_speed: new_v,
                    },
                );
            }
        }
        true
    }

    pub fn try_remove_warp_bubble(&self, id: BallId) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let _ = bp.remove_warp_bubble(id);
        super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn tidi_factor(&self) -> f64 {
        let raw = f64::from_bits(self.tidi_factor_bits.load(std::sync::atomic::Ordering::Relaxed));
        if raw.is_finite() {
            raw.clamp(0.10, 1.0)
        } else {
            1.0
        }
    }

    pub fn set_tidi_factor_raw(&self, factor: f64) -> f64 {
        let clamped = if factor.is_finite() {
            factor.clamp(0.10, 1.0)
        } else {
            1.0
        };
        let prev_bits = self
            .tidi_factor_bits
            .swap(clamped.to_bits(), std::sync::atomic::Ordering::Relaxed);
        let prev = f64::from_bits(prev_bits);
        if prev.is_finite() {
            prev.clamp(0.10, 1.0)
        } else {
            1.0
        }
    }

    /// Computes live simulation load in this solar system (dynamic balls + weighted active module/repair/anchoring cycles).
    pub fn active_entity_and_cycle_load(&self) -> usize {
        let dyn_balls = {
            let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
            bp.balls.keys().filter(|&&bid| !bp.is_static_ball(bid)).count()
        };
        let cycle_load = {
            let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
            st.active_module_cycles.len().saturating_mul(2)
                + st.pending_anchoring_disruptors.len()
                + st.active_nanite_repairs.len()
        };
        dyn_balls.saturating_add(cycle_load)
    }

    /// Synchronously respawns fully depleted SDE ore belts and depleted ice blocks during 11:00 UTC downtime.
    pub fn respawn_depleted_belts_on_downtime(&self) -> usize {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let super::SystemActorState {
            asteroids,
            asteroid_templates,
            sde_opt,
            db_opt,
            ..
        } = &mut *st;
        let count = super::belts::respawn_depleted_belts_on_downtime_inner(
            &mut bp,
            asteroids,
            asteroid_templates,
            sde_opt,
            db_opt,
        );
        if count > 0 {
            super::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        }
        count
    }

    pub fn try_step_manual(&self, dt: f64) {
        let dt_sim = if dt > 0.0 && dt.is_finite() {
            dt * self.tidi_factor()
        } else {
            dt
        };
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        if dt_sim > 0.0 && dt_sim.is_finite() {
            st.sim_elapsed_secs += dt_sim;
        }
        let events = bp.step(dt_sim);
        persist_landed_warps_in_db(&bp, &events, &st.db_opt, &st.ball_space_entry_epochs);
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
            .send(SystemCommand::RemoveWarpBubble { id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_speed_fraction(&self, ball_id: BallId, fraction: f64) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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
            .cmd_sender()
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
            .cmd_sender()
            .send(SystemCommand::EmitDestinyEvent { ball_id, event, reply: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Queues `event` directly into `observer_ball_id`'s observer queue (returns `false` if absent).
    pub async fn emit_observer_destiny_event(
        &self,
        observer_ball_id: BallId,
        event: DestinyEvent,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::EmitObserverDestinyEvent {
                observer_ball_id,
                event,
                reply: tx,
            })
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
        self.cmd_sender()
            .send(SystemCommand::GotoPoint { ball_id, point, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Queues a `SetState` destiny update for `observer_ball_id` (`P-7`).
    pub async fn push_set_state(&self, observer_ball_id: BallId) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::PushSetState {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Immediately flushes queued `DoDestinyUpdate` payloads over `event_tx` for all active
    /// peer observers (excluding `exclude_ball_id` and any observer whose queue still awaits
    /// initial `SetState` consumption), avoiding up to `1.0s` of next-tick latency.
    pub async fn flush_peer_observer_updates(
        &self,
        exclude_ball_id: Option<BallId>,
    ) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FlushPeerObserverUpdates {
                exclude_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_snapshot(&self, ball_id: BallId) -> Result<Option<(u64, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBallSnapshot { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_group_id(&self, ball_id: BallId) -> Result<Option<u32>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBallGroupId { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn list_balls(&self) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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

    pub async fn apply_true_damage(&self, ball_id: BallId, amount: f64) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyTrueDamage { ball_id, amount, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_damage_with_attacker(
        &self,
        ball_id: BallId,
        damage: DamageVector,
        attacker_id: Option<CharacterId>,
    ) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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

    /// Synchronously collects dirty condition checkpoint entries and updates the checkpoint baseline (`#919` `T-5`).
    pub fn collect_dirty_condition_checkpoint_sync(&self) -> Vec<super::ConditionCheckpointEntry> {
        let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        celestials_helpers::collect_dirty_condition_checkpoint_entries(&mut st, &bp)
    }

    /// Asks the actor to flush all dirty condition checkpoint entries and commits them to `WorldRepository` (`#919` `T-2`, `T-5`).
    pub async fn flush_condition_checkpoint(&self) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FlushConditionCheckpoint { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let entries = rx
            .await
            .map_err(|_| "Solar system reply dropped".to_string())?;
        let db_opt = self
            .actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .db_opt
            .clone();
        if let Some(ref db) = db_opt {
            Ok(crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
                db,
                self.solar_system_id,
                &entries,
            ))
        } else {
            Ok(0)
        }
    }

    /// Commits a [`super::StepOutput`] produced by this system's actor (`#919` E-4, `#839` B1):
    /// applies any staggered dirty-only condition checkpoint (`T-2`, `T-5`), invokes `npc_death_hook`
    /// for destroyed balls, and executes `system_step_hook` (`T-4` inventory-visible step outputs
    /// and player ship destruction).
    pub async fn apply_step_output(&self, output: &super::StepOutput) {
        if let Some(ref entries) = output.condition_checkpoint {
            if !entries.is_empty() {
                let db_opt = self
                    .actor_state
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .db_opt
                    .clone();
                if let Some(ref db) = db_opt {
                    let _ = crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
                        db,
                        self.solar_system_id,
                        entries,
                    );
                }
            }
        }
        let hook = self
            .npc_death_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for &(target_ball_id, owner_char_id) in &output.destroyed_balls {
            if let Some(fut) = super::on_ball_destroyed_by_damage(
                self.solar_system_id,
                target_ball_id,
                owner_char_id,
                hook.as_ref(),
            ) {
                fut.await;
            }
        }
        let step_hook = self
            .system_step_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = step_hook {
            hook(
                self.solar_system_id,
                output.dt_sim,
                output.destroyed_balls.clone(),
            )
            .await;
        }
    }

    /// Steps this system by `dt` seconds and returns the full [`super::StepOutput`] after driver commit (`#919` E-4).
    pub async fn step_with_output(&self, dt: f64) -> Result<super::StepOutput, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StepManual { dt, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let output = rx
            .await
            .map_err(|_| "Solar system reply dropped".to_string())?;
        self.apply_step_output(&output).await;
        Ok(output)
    }

    pub async fn step_manual(&self, dt: f64) -> Result<TickBroadcast, String> {
        let output = self.step_with_output(dt).await?;
        Ok(output.broadcast.unwrap_or_else(|| TickBroadcast {
            solar_system_id: self.solar_system_id,
            tick: output.tick,
            events: Vec::new(),
            observer_events: std::collections::BTreeMap::new(),
            observer_updates_pyrep: std::collections::BTreeMap::new(),
        }))
    }

    pub(crate) async fn step_manual_tick_only(&self, dt: f64) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StepManualTickOnly { dt, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let output = rx
            .await
            .map_err(|_| "Solar system reply dropped".to_string())?;
        self.apply_step_output(&output).await;
        Ok(output.tick)
    }

    pub async fn fast_forward_clock(&self, ship_id: BallId, secs: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FastForwardClock { ship_id, secs, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_target_locks(&self, ball_id: BallId) -> Result<Vec<TargetLock>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::GetTargetLocks { ball_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_target_lock(&self, source_id: BallId, lock: TargetLock) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::AddTargetLock { source_id, lock, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_target_lock(&self, source_id: BallId, target_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::RemoveTargetLock { source_id, target_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn clear_target_locks(&self, source_id: BallId) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::ClearTargetLocks { source_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_capacitor(&self, ball_id: BallId, capacitor: f64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetCapacitor { ball_id, capacitor, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn heal_combat_state(&self, ball_id: BallId) -> Result<CombatState, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::HealCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_combat_state(&self, ball_id: BallId, combat_state: CombatState) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetCombatState { ball_id, combat_state, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_combat_state(&self, ball_id: BallId) -> Result<Option<CombatState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Atomically mutates `ball_id`'s [`CombatState`] inside the actor loop without a
    /// stale get-modify-set round-trip (`#846`, `#917` E-2). Returns `Ok(None)` if
    /// `ball_id` is not present in space.
    pub async fn modify_combat_state(
        &self,
        ball_id: BallId,
        f: impl FnOnce(&mut CombatState) + Send + 'static,
    ) -> Result<Option<CombatState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ModifyCombatState {
                ball_id,
                f: Box::new(f),
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_module_cycle(&self, cycle: ActiveModuleCycle) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddModuleCycle { cycle, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_module_cycle(&self, ship_id: BallId, module_id: i64) -> Result<Option<f64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RemoveModuleCycle { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn request_cycle_deactivation(&self, ship_id: BallId, module_id: i64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RequestCycleDeactivation { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn fire_weapon_cycle_volley(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<Vec<super::PendingCycleAction>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FireWeaponCycleVolley { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let (destroyed, actions) = rx.await.map_err(|_| "Solar system reply dropped".to_string())?;
        if !destroyed.is_empty() {
            let hook = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
            for (target_ball_id, owner_char_id) in destroyed {
                if let Some(fut) = super::on_ball_destroyed_by_damage(
                    self.solar_system_id,
                    target_ball_id,
                    owner_char_id,
                    hook.as_ref(),
                ) {
                    let _ = fut.await;
                }
            }
        }
        Ok(actions)
    }

    pub async fn has_module_cycle(&self, ship_id: BallId, module_id: i64) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
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
        self.cmd_sender()
            .send(SystemCommand::StopNonWarpModuleCycles { ship_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_interdiction_nullified(&self, ball_id: BallId, nullified: bool) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetInterdictionNullified { ball_id, nullified, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn micro_jump(&self, ball_id: BallId, distance_meters: f64) -> Result<DVec3, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
            .send(SystemCommand::IsBallScrambled { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_interdiction_sphere(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddInterdictionSphere { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_mobile_warp_disruptor(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_ecm_jam(&self, target_id: BallId, duration_s: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
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
        self.cmd_sender()
            .send(SystemCommand::StartNaniteRepair { repair, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn stop_nanite_repair(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<Option<(f64, i32)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StopNaniteRepair {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub fn has_any_active_nanite_repair_sync(&self, ship_id: BallId) -> bool {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        st.active_nanite_repairs.iter().any(|r| r.ship_id == ship_id)
    }

    pub fn is_module_being_nanite_repaired_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        st.active_nanite_repairs
            .iter()
            .any(|r| r.ship_id == ship_id && r.module_id == module_id)
    }

    pub fn has_active_overloaded_cycle_sync(&self, ship_id: BallId) -> bool {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        st.active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.is_overloaded)
    }

    pub fn active_offensive_aggressors_on_sync(
        &self,
        target_ship_id: BallId,
        sde: Option<&eve_sde::SdeRegistry>,
    ) -> Vec<BallId> {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        st.active_module_cycles
            .iter()
            .filter(|c| {
                c.target_id == Some(target_ship_id)
                    && c.ship_id != target_ship_id
                    && crate::services::mining_phased_autotarget::is_offensive_aggressor_cycle(c, sde)
            })
            .map(|c| c.ship_id)
            .collect()
    }

    pub fn has_active_industrial_core_sync(&self, ship_id: BallId) -> bool {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        st.active_module_cycles.iter().any(|c| {
            c.ship_id == ship_id
                && matches!(c.module_type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
        })
    }

    pub fn cancel_nanite_repairs_for_ship_sync(&self, ship_id: BallId) -> Vec<ActiveNaniteRepair> {
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let mut cancelled = Vec::new();
        let all = std::mem::take(&mut st.active_nanite_repairs);
        for job in all {
            if job.ship_id == ship_id {
                cancelled.push(job);
            } else {
                st.active_nanite_repairs.push(job);
            }
        }
        cancelled
    }

    pub async fn record_incoming_armor_damage(
        &self,
        ship_id: BallId,
        damage: [f64; 4],
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
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

#[path = "celestials_helpers.rs"]
mod celestials_helpers;
pub use celestials_helpers::try_remove_ball_for_db;
pub(crate) use celestials_helpers::{
    collect_dirty_condition_checkpoint_entries, collect_immediate_armor_hull_checkpoint_entries,
    collect_landed_warp_positions, fast_forward_clock_in_actor_with_epochs,
    persist_ball_positions_in_db, persist_landed_warps_in_db,
    record_spawned_ball_checkpoint_baseline, register_committed_warp_cache,
    register_db_system_ballpark, remove_ball_checkpoint_tracking, sync_ball_positions,
    update_system_hibernation_flag,
};

#[cfg(test)] #[path = "celestials_unit_tests.rs"] mod celestials_unit_tests;

