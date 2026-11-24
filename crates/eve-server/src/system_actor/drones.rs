//! Per-system repeating drone mining and drone combat cooldown state owned by `SystemActor` (`#1541`).

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActorExecCtx, BallOpsCtx, SystemActorState, SystemCommand, SystemHandle, SystemRegistry,
    ACTOR_CMD_TIMEOUT,
};
use eve_destiny::BallId;
use std::collections::HashMap;

/// Active repeating drone ore-mining loop state owned per-system by [`SystemActor`] (`#1541`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneMiningState {
    pub drone_id: i64,
    pub asteroid_id: i64,
    pub ship_id: i64,
    pub owner_char_id: i64,
    pub solar_system_id: u32,
    pub yield_units: u64,
    pub duration_secs: u64,
    pub next_cycle_unix: u64,
    pub waste_prob: f64,
    pub ore_type_id: u32,
    pub destination_flag: eve_db::InventoryFlag,
}

#[derive(Debug, Clone)]
struct ClearDroneRuntimeReq {
    drone_ids: Option<Vec<BallId>>,
    clear_miners: bool,
    clear_timers: bool,
}

fn upsert_drone_miner_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    state: DroneMiningState,
) -> Result<(), String> {
    ctx.actor_st.active_drone_miners.insert(state.drone_id, state);
    Ok(())
}

fn reschedule_drone_miner_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    drone_id: BallId,
    now_unix: u64,
) -> Result<bool, String> {
    let Some(miner) = ctx.actor_st.active_drone_miners.get_mut(&drone_id) else {
        return Ok(false);
    };
    miner.next_cycle_unix = now_unix.saturating_add(miner.duration_secs);
    Ok(true)
}

fn set_drone_next_ready_unix_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    drone_id: BallId,
    next_ready_unix: u64,
    require_ball_in_space: bool,
) -> Result<bool, String> {
    if require_ball_in_space && !ctx.ballpark.balls.contains_key(&drone_id) {
        return Ok(false);
    }
    ctx.actor_st
        .drone_next_ready_unix
        .insert(drone_id, next_ready_unix);
    Ok(true)
}

fn clear_drone_runtime_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: ClearDroneRuntimeReq,
) -> Result<usize, String> {
    let mut removed = 0usize;
    match req.drone_ids {
        Some(ids) => {
            for id in ids {
                if req.clear_miners && ctx.actor_st.active_drone_miners.remove(&id).is_some() {
                    removed += 1;
                }
                if req.clear_timers && ctx.actor_st.drone_next_ready_unix.remove(&id).is_some() {
                    removed += 1;
                }
            }
        }
        None => {
            if req.clear_miners {
                removed += ctx.actor_st.active_drone_miners.len();
                ctx.actor_st.active_drone_miners.clear();
            }
            if req.clear_timers {
                removed += ctx.actor_st.drone_next_ready_unix.len();
                ctx.actor_st.drone_next_ready_unix.clear();
            }
        }
    }
    Ok(removed)
}

/// Clears both repeating drone mining state and combat cooldown timer for `ball_id` in actor state.
pub(in crate::system_actor) fn clear_drone_runtime_in_state(
    actor_st: &mut SystemActorState,
    ball_id: BallId,
) {
    actor_st.active_drone_miners.remove(&ball_id);
    actor_st.drone_next_ready_unix.remove(&ball_id);
}

pub(in crate::system_actor) fn handle_drone_runtime_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::UpsertDroneMiner { state, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = upsert_drone_miner_in_actor(&mut ctx, state);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RescheduleDroneMiner {
            drone_id,
            now_unix,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reschedule_drone_miner_in_actor(&mut ctx, drone_id, now_unix);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetDroneNextReadyUnix {
            drone_id,
            next_ready_unix,
            require_ball_in_space,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = set_drone_next_ready_unix_in_actor(
                &mut ctx,
                drone_id,
                next_ready_unix,
                require_ball_in_space,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearDroneRuntime {
            drone_ids,
            clear_miners,
            clear_timers,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_drone_runtime_in_actor(
                &mut ctx,
                ClearDroneRuntimeReq {
                    drone_ids,
                    clear_miners,
                    clear_timers,
                },
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        other => Some(other),
    }
}

impl SystemHandle {
    /// Returns the published repeating mining state for `drone_id` in this solar system, if active.
    pub fn active_drone_miner(&self, drone_id: BallId) -> Option<DroneMiningState> {
        self.view().active_drone_miners.get(&drone_id).cloned()
    }

    /// Returns a snapshot of all published repeating drone mining states in this solar system.
    pub fn active_drone_miners(&self) -> HashMap<BallId, DroneMiningState> {
        self.view().active_drone_miners.clone()
    }

    /// Returns the published next weapon/combat ready UNIX timestamp for `drone_id`, if set.
    pub fn drone_next_ready_unix(&self, drone_id: BallId) -> Option<u64> {
        self.view().drone_next_ready_unix.get(&drone_id).copied()
    }

    /// Returns a snapshot of all published drone combat cooldown timestamps in this solar system.
    pub fn drone_next_ready_unix_map(&self) -> HashMap<BallId, u64> {
        self.view().drone_next_ready_unix.clone()
    }

    /// Inserts or replaces a repeating drone mining state inside this solar system actor (`#1541`).
    pub async fn upsert_drone_miner(&self, state: DroneMiningState) -> Result<(), String> {
        call_actor_result_with_deadline(self, "UpsertDroneMiner", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpsertDroneMiner { state, reply }
        })
        .await
    }

    /// Synchronous bridge for [`Self::upsert_drone_miner`].
    pub fn upsert_drone_miner_sync(&self, state: DroneMiningState) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertDroneMiner",
            ACTOR_CMD_TIMEOUT,
            state,
            |state, reply| SystemCommand::UpsertDroneMiner { state, reply },
            upsert_drone_miner_in_actor,
        )
    }

    /// Advances an existing mining drone's `next_cycle_unix` to `now_unix.saturating_add(duration_secs)`.
    pub async fn reschedule_drone_miner(
        &self,
        drone_id: BallId,
        now_unix: u64,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(self, "RescheduleDroneMiner", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RescheduleDroneMiner {
                drone_id,
                now_unix,
                reply,
            }
        })
        .await
    }

    async fn clear_runtime_scope(&self, req: ClearDroneRuntimeReq) -> Result<usize, String> {
        if req.drone_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(0);
        }
        call_actor_result_with_deadline(self, "ClearDroneRuntime", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ClearDroneRuntime {
                drone_ids: req.drone_ids,
                clear_miners: req.clear_miners,
                clear_timers: req.clear_timers,
                reply,
            }
        })
        .await
    }

    fn clear_runtime_scope_sync(&self, req: ClearDroneRuntimeReq) -> Result<usize, String> {
        if req.drone_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(0);
        }
        dispatch_actor_sync_with_deadline(
            self,
            "ClearDroneRuntime",
            ACTOR_CMD_TIMEOUT,
            req,
            |req, reply| SystemCommand::ClearDroneRuntime {
                drone_ids: req.drone_ids,
                clear_miners: req.clear_miners,
                clear_timers: req.clear_timers,
                reply,
            },
            clear_drone_runtime_in_actor,
        )
    }

    /// Removes active repeating mining state for `drone_id`, leaving combat cooldown intact.
    pub async fn remove_drone_miner(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: false,
        })
        .await
    }

    /// Synchronous bridge for [`Self::remove_drone_miner`].
    pub fn remove_drone_miner_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: false,
        })
    }

    /// Clears all repeating drone mining state in this solar system.
    pub async fn clear_drone_miners(&self) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: true,
            clear_timers: false,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_miners`].
    pub fn clear_drone_miners_sync(&self) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: true,
            clear_timers: false,
        })
    }

    async fn set_drone_next_ready_unix_inner(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
        require_ball_in_space: bool,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "SetDroneNextReadyUnix",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SetDroneNextReadyUnix {
                drone_id,
                next_ready_unix,
                require_ball_in_space,
                reply,
            },
        )
        .await
    }

    /// Sets `drone_next_ready_unix` for `drone_id` unconditionally.
    pub async fn set_drone_next_ready_unix(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        self.set_drone_next_ready_unix_inner(drone_id, next_ready_unix, false)
            .await
    }

    /// Synchronous bridge for [`Self::set_drone_next_ready_unix`].
    pub fn set_drone_next_ready_unix_sync(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SetDroneNextReadyUnix",
            ACTOR_CMD_TIMEOUT,
            (drone_id, next_ready_unix, false),
            |(drone_id, next_ready_unix, require_ball_in_space), reply| {
                SystemCommand::SetDroneNextReadyUnix {
                    drone_id,
                    next_ready_unix,
                    require_ball_in_space,
                    reply,
                }
            },
            |ctx, (drone_id, next_ready_unix, require_ball_in_space)| {
                set_drone_next_ready_unix_in_actor(
                    ctx,
                    drone_id,
                    next_ready_unix,
                    require_ball_in_space,
                )
            },
        )
    }

    /// Sets `drone_next_ready_unix` only if `drone_id` still has a ball in `Ballpark`, preventing
    /// a racing combat step from resurrecting timer state after ball removal (`#1541`).
    pub async fn set_drone_next_ready_unix_if_present(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        self.set_drone_next_ready_unix_inner(drone_id, next_ready_unix, true)
            .await
    }

    /// Clears `drone_next_ready_unix` for `drone_id`.
    pub async fn clear_drone_next_ready_unix(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_next_ready_unix`].
    pub fn clear_drone_next_ready_unix_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: false,
            clear_timers: true,
        })
    }

    /// Clears `drone_next_ready_unix` for each drone in `drone_ids`.
    pub async fn clear_drone_next_ready_unix_batch(
        &self,
        drone_ids: Vec<BallId>,
    ) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Clears all `drone_next_ready_unix` timers in this solar system.
    pub async fn clear_all_drone_next_ready_unix(&self) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Clears both active mining state and combat cooldown timer for `drone_id`.
    pub async fn clear_drone_runtime(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_runtime`].
    pub fn clear_drone_runtime_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: true,
        })
    }

    /// Clears both active mining state and combat cooldown timers for all drones in `drone_ids`.
    pub async fn clear_drone_runtime_batch(
        &self,
        drone_ids: Vec<BallId>,
    ) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: true,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_runtime_batch`].
    pub fn clear_drone_runtime_batch_sync(&self, drone_ids: Vec<BallId>) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: true,
            clear_timers: true,
        })
    }
}

impl SystemRegistry {
    /// Returns total active repeating drone miners across all currently loaded solar systems.
    pub fn active_drone_miners_count(&self) -> usize {
        self.active_system_handles()
            .into_iter()
            .map(|(_, h)| h.view().active_drone_miners.len())
            .sum()
    }

    /// Collects `(SystemHandle, DroneMiningState)` pairs across all currently loaded systems.
    pub fn all_active_drone_miners(&self) -> Vec<(SystemHandle, DroneMiningState)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let miners = handle.active_drone_miners();
            out.extend(miners.into_values().map(|m| (handle.clone(), m)));
        }
        out
    }

    /// Clears active repeating drone miners across all currently loaded solar systems.
    pub async fn clear_all_drone_miners(&self) {
        for (_, handle) in self.active_system_handles() {
            if !handle.view().active_drone_miners.is_empty() {
                let _ = handle.clear_drone_miners().await;
            }
        }
    }
}
