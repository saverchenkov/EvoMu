//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, MovementMode};
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
    SetMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    GetBallSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<(u64, Ball)>>,
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

    /// Spawns or retrieves a running 1 Hz Solar System Actor for `solar_system_id`.
    pub fn get_or_spawn(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        if let Some(existing) = self.systems.read().unwrap().get(&solar_system_id) {
            return existing.clone();
        }

        let mut guard = self.systems.write().unwrap();
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

            loop {
                tokio::select! {
                    biased;
                    _ = ticker.tick() => {
                        let events = ballpark.step(1.0);
                        let _ = event_tx_actor.send(TickBroadcast {
                            solar_system_id,
                            tick: ballpark.current_tick,
                            events,
                        });
                    }
                    cmd_opt = cmd_rx.recv() => {
                        let Some(cmd) = cmd_opt else {
                            break;
                        };
                        match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                if ballpark.add_ball(ball) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Ball {ball_id} is already spawned in solar system {solar_system_id}"
                                    )));
                                }
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
