//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

use eve_destiny::{Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode};
use eve_sde::SolarSystemId;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::MissedTickBehavior;

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct TickBroadcast {
    pub solar_system_id: SolarSystemId,
    pub tick: u64,
    pub events: Vec<DestinyEvent>,
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
    RemoveBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<Ball>>,
    },
    SetMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    GetBallSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<(u64, Ball)>>,
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

    pub async fn get_ball_snapshot(&self, ball_id: BallId) -> Result<Option<(u64, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SystemCommand::GetBallSnapshot { ball_id, reply: tx })
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
}

pub struct SystemRegistry {
    systems: RwLock<HashMap<SolarSystemId, SystemHandle>>,
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
        }
    }

    /// Spawns or retrieves a running Solar System Actor for `solar_system_id`.
    /// Empty solar systems start with an empty `Ballpark` and hibernate (`LOD 0`) with zero timer
    /// wakes until balls are spawned on grid.
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

        tokio::spawn(async move {
            let mut ballpark = Ballpark::new(solar_system_id);
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut lod_counter: u64 = 0;

            loop {
                tokio::select! {
                    biased;
                    _ = ticker.tick(), if !ballpark.balls.is_empty() => {
                        lod_counter = lod_counter.wrapping_add(1);
                        let has_player_balls = ballpark.balls.keys().any(|&id| id < 700_000_000);
                        let (should_step, dt) = if has_player_balls {
                            (true, 1.0) // Tier 2 Active Grid: 1.0 Hz
                        } else {
                            (lod_counter.is_multiple_of(5), 5.0) // Tier 1 System Presence: 0.2 Hz
                        };
                        if should_step {
                            let events = ballpark.step(dt);
                            let _ = event_tx_actor.send(TickBroadcast {
                                solar_system_id,
                                tick: ballpark.current_tick,
                                events,
                            });
                        }
                    }
                    cmd_opt = cmd_rx.recv() => {
                        let Some(cmd) = cmd_opt else {
                            break;
                        };
                        match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                ballpark.balls.remove(&ball_id);
                                ballpark.add_ball(ball);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SpawnBallWithCombat {
                                ball,
                                combat_state,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                ballpark.balls.remove(&ball_id);
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
                            SystemCommand::GetBallSnapshot { ball_id, reply } => {
                                let res = ballpark.balls.get(&ball_id).cloned().map(|b| (ballpark.current_tick, b));
                                let _ = reply.send(res);
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
                                let broadcast = TickBroadcast {
                                    solar_system_id,
                                    tick: ballpark.current_tick,
                                    events,
                                };
                                let _ = event_tx_actor.send(broadcast.clone());
                                let _ = reply.send(broadcast);
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
