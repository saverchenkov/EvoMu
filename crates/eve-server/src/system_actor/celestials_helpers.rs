//! DB-backed ballpark lookup, ball removal, and warp landing persistence helpers for `celestials`.

#![forbid(unsafe_code)]

use super::{BallparkHandle, SystemHandle};
use crate::system_actor::post_unlock;
use eve_db::lock_rank::{held_lock_ranks, RankedRwLock, RANK_LEAF};
use eve_destiny::{BallId, Ballpark};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

/// Atomically assigns the next monotonic `TickBroadcast.broadcast_seq` and sends `broadcast`
/// onto `event_tx` while holding `broadcast_seq_gate`, preserving strict FIFO seq ordering (`GH #1172`).
pub(crate) fn send_tick_broadcast_sequenced_owned(
    event_tx: &super::broadcast::Sender<super::TickBroadcast>,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
    mut broadcast: super::TickBroadcast,
) {
    let mut seq = broadcast_seq_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *seq = seq.saturating_add(1);
    broadcast.broadcast_seq = *seq;
    let _ = event_tx.send(broadcast);
}

/// Assigns `broadcast.broadcast_seq` in-place under `broadcast_seq_gate` and sends a clone onto
/// `event_tx` (`GH #1172`).
pub(crate) fn send_tick_broadcast_sequenced(
    event_tx: &super::broadcast::Sender<super::TickBroadcast>,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
    broadcast: &mut super::TickBroadcast,
) {
    let mut seq = broadcast_seq_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *seq = seq.saturating_add(1);
    broadcast.broadcast_seq = *seq;
    let _ = event_tx.send(broadcast.clone());
}

#[derive(Clone)]
struct DbBallparkEntry {
    ballpark: std::sync::Weak<BallparkHandle>,
    cmd_tx: tokio::sync::mpsc::WeakSender<crate::system_actor::SystemCommand>,
    handle: Option<std::sync::Weak<SystemHandle>>,
}

const DB_BALLPARK_REGISTRY_SLOT: &str = "eve_server.system_actor.db_ballpark_registry";

struct DbBallparkRegistry(std::sync::Mutex<HashMap<SolarSystemId, DbBallparkEntry>>);

fn db_ballpark_registry(db: &eve_db::WorldRepository) -> Arc<DbBallparkRegistry> {
    db.get_or_init_transient_runtime_slot(DB_BALLPARK_REGISTRY_SLOT, || {
        DbBallparkRegistry(std::sync::Mutex::new(HashMap::new()))
    })
}

fn insert_db_system_ballpark(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    handle: &SystemHandle,
    weak_handle: Option<std::sync::Weak<SystemHandle>>,
) {
    let reg = db_ballpark_registry(db);
    let mut g = reg.0.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() > 256 {
        g.retain(|_, entry| entry.ballpark.strong_count() > 0);
    }
    g.insert(
        solar_system_id,
        DbBallparkEntry {
            ballpark: Arc::downgrade(&handle.ballpark),
            cmd_tx: handle.cmd_tx.downgrade(),
            handle: weak_handle,
        },
    );
}

#[cfg(test)]
pub(crate) fn register_db_system_ballpark(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    handle: &SystemHandle,
) {
    insert_db_system_ballpark(db, solar_system_id, handle, None);
}

pub(crate) fn register_db_system_ballpark_arc(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    handle: &Arc<SystemHandle>,
) {
    insert_db_system_ballpark(db, solar_system_id, handle, Some(Arc::downgrade(handle)));
}

/// Clears the DB ballpark entry for `(db, solar_system_id)` only if it still points at `handle`.
pub(crate) fn clear_db_system_ballpark_if_matching(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    handle: &SystemHandle,
) {
    let Some(Some(reg)) =
        db.get_transient_runtime_slot::<DbBallparkRegistry>(DB_BALLPARK_REGISTRY_SLOT)
    else {
        return;
    };
    let mut g = reg.0.lock().unwrap_or_else(|e| e.into_inner());
    let target = Arc::downgrade(&handle.ballpark);
    if g.get(&solar_system_id)
        .is_some_and(|entry| std::sync::Weak::ptr_eq(&entry.ballpark, &target))
    {
        g.remove(&solar_system_id);
    }
}

#[cfg(test)]
pub(crate) fn db_ballpark_entry_for_test(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
) -> Option<std::sync::Weak<BallparkHandle>> {
    let reg = db
        .get_transient_runtime_slot::<DbBallparkRegistry>(DB_BALLPARK_REGISTRY_SLOT)
        .flatten()?;
    let g = reg.0.lock().unwrap_or_else(|e| e.into_inner());
    g.get(&solar_system_id).map(|entry| entry.ballpark.clone())
}

fn leaf_write_value<T>(lock: &RankedRwLock<T, RANK_LEAF>, leaf_held: bool, value: T) {
    if leaf_held {
        if let Ok(mut g) = lock.try_write_unranked() {
            *g = value;
        }
    } else {
        *lock.write().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

/// Per-system synchronized ball position, velocity, committed-warp, and tether cache (`#1583`).
///
/// Owned directly by [`SystemHandle`] so stopping and restarting a system actor discards all
/// prior-run kinematics, tether, and warp cache state with zero pointer-address maps or
/// process-wide eviction wipes.
pub(crate) struct BallPositionSyncCache {
    positions: RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>,
    committed_warps: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    tethered_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    static_sync_meta: std::sync::Mutex<Option<(u64, usize)>>,
    velocities: std::sync::Mutex<HashMap<BallId, [f64; 3]>>,
}

impl BallPositionSyncCache {
    pub(crate) fn new(
        committed_warps: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
        tethered_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    ) -> Self {
        Self {
            positions: RankedRwLock::new(HashMap::new()),
            committed_warps,
            tethered_balls,
            static_sync_meta: std::sync::Mutex::new(None),
            velocities: std::sync::Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_standalone() -> Self {
        Self::new(
            Arc::new(RankedRwLock::new(HashMap::new())),
            Arc::new(RankedRwLock::new(std::collections::HashSet::new())),
        )
    }

    pub(crate) fn get_velocity(&self, ball_id: BallId) -> Option<[f64; 3]> {
        self.velocities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ball_id)
            .copied()
    }
}

impl std::ops::Deref for BallPositionSyncCache {
    type Target = RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>;

    fn deref(&self) -> &Self::Target {
        &self.positions
    }
}

pub(crate) fn sync_ball_positions(
    ballpark: &Ballpark,
    cache: &BallPositionSyncCache,
    scrambled: &RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>,
) {
    let cur_static_epoch = ballpark.static_epoch();
    let cur_static_count = ballpark.static_ball_ids.len();
    let prev_static = *cache
        .static_sync_meta
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let statics_unchanged = prev_static == Some((cur_static_epoch, cur_static_count));

    let leaf_held = eve_db::lock_rank::held_lock_ranks()
        .iter()
        .any(|&r| r >= RANK_LEAF);

    let update_cache = |guard: &mut HashMap<BallId, DVec3>| {
        if guard.is_empty() || !statics_unchanged {
            guard.clear();
            guard.reserve(ballpark.balls.len());
            for (&id, ball) in &ballpark.balls {
                guard.insert(id, ball.position);
            }
        } else {
            let mut inserted_new = false;
            if ballpark.balls.len() != cur_static_count {
                for (&id, ball) in &ballpark.balls {
                    if !ballpark.is_static_ball(id) && guard.insert(id, ball.position).is_none() {
                        inserted_new = true;
                    }
                }
            }
            if inserted_new || guard.len() != ballpark.balls.len() {
                guard.retain(|id, _| ballpark.balls.contains_key(id));
                if guard.len() < ballpark.balls.len() {
                    for (&id, ball) in &ballpark.balls {
                        guard.entry(id).or_insert(ball.position);
                    }
                }
            }
        }
    };

    let mut cache_updated = false;
    if leaf_held {
        if let Ok(mut g) = cache.positions.try_write_unranked() {
            update_cache(&mut g);
            cache_updated = true;
        }
    } else {
        let mut g = cache.positions.write().unwrap_or_else(|e| e.into_inner());
        update_cache(&mut g);
        cache_updated = true;
    }
    if cache_updated {
        *cache
            .static_sync_meta
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((cur_static_epoch, cur_static_count));

        let mut dyn_vel = HashMap::new();
        if ballpark.balls.len() > cur_static_count {
            for (&id, ball) in &ballpark.balls {
                if !ballpark.is_static_ball(id) {
                    dyn_vel.insert(id, [ball.velocity.x, ball.velocity.y, ball.velocity.z]);
                }
            }
        }
        *cache
            .velocities
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = dyn_vel;
    }

    let mut scram = std::collections::HashSet::new();
    if ballpark.warp_bubble_mgr.bubbles.is_empty() {
        for (&id, cs) in &ballpark.combat_states {
            if cs.is_warp_scrambled() && ballpark.balls.contains_key(&id) {
                scram.insert(id);
            }
        }
    } else {
        for (&id, ball) in &ballpark.balls {
            if !ballpark.is_static_ball(id)
                && (ballpark.combat_states.get(&id).is_some_and(|cs| cs.is_warp_scrambled())
                    || (!ballpark.is_ball_bubble_immune(id)
                        && ballpark.warp_bubble_mgr.is_scrambled_at(ball.position)))
            {
                scram.insert(id);
            }
        }
    }

    let mut teth = std::collections::HashSet::new();
    for (&id, cs) in &ballpark.combat_states {
        if cs.is_tethered && ballpark.balls.contains_key(&id) {
            teth.insert(id);
        }
    }

    let mut warps = HashMap::new();
    if ballpark.balls.len() > cur_static_count {
        for (&id, ball) in &ballpark.balls {
            if !ballpark.is_static_ball(id) {
                if let eve_destiny::MovementMode::Warp {
                    destination, phase, ..
                } = ball.mode
                {
                    if phase != eve_destiny::WarpPhase::Aligning {
                        warps.insert(id, destination);
                    }
                }
            }
        }
    }

    leaf_write_value(scrambled, leaf_held, scram);
    leaf_write_value(&cache.committed_warps, leaf_held, warps);
    leaf_write_value(&cache.tethered_balls, leaf_held, teth);
}

/// Removes `ball_id` from `solar_system_id`'s actor via mailbox if registered for `db` (`RT-101`, `#1342`).
pub fn try_remove_ball_for_db(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    ball_id: BallId,
) {
    let entry = db
        .get_transient_runtime_slot::<DbBallparkRegistry>(DB_BALLPARK_REGISTRY_SLOT)
        .flatten()
        .and_then(|reg| {
            reg.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&solar_system_id)
                .cloned()
        });
    let Some(entry) = entry else {
        return;
    };
    if let Some(handle) = entry.handle.as_ref().and_then(std::sync::Weak::upgrade) {
        handle.try_remove_ball(ball_id);
        return;
    }
    let Some(cmd_tx) = entry.cmd_tx.upgrade() else {
        return;
    };
    if !held_lock_ranks().is_empty() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let _ = cmd_tx.try_send(crate::system_actor::SystemCommand::RemoveBallSync {
            ball_id,
            emit_destruction_fx: false,
            wait_for_post_unlock: false,
            reply: tx,
        });
        return;
    }
    let _ = crate::system_actor::handle_cmds::dispatch_cmd_tx_sync_reply(
        &cmd_tx,
        solar_system_id,
        "RemoveBallForDb",
        crate::system_actor::ball_ops::ACTOR_CMD_TIMEOUT,
        |reply| crate::system_actor::SystemCommand::RemoveBallSync {
            ball_id,
            emit_destruction_fx: false,
            wait_for_post_unlock: true,
            reply,
        },
    );
}

/// Non-blocking mailbox enqueue for `post_unlock_worker` (`#1170` W6, `RT-101`).
pub(crate) fn try_enqueue_remove_ball_for_db(
    db: &Arc<eve_db::WorldRepository>,
    solar_system_id: SolarSystemId,
    ball_id: BallId,
) {
    let cmd_tx_opt = db
        .get_transient_runtime_slot::<DbBallparkRegistry>(DB_BALLPARK_REGISTRY_SLOT)
        .flatten()
        .and_then(|reg| {
            reg.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&solar_system_id)
                .and_then(|entry| entry.cmd_tx.upgrade())
        });
    if let Some(cmd_tx) = cmd_tx_opt {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let _ = cmd_tx.try_send(crate::system_actor::SystemCommand::RemoveBallSync {
            ball_id,
            emit_destruction_fx: false,
            wait_for_post_unlock: false,
            reply: tx,
        });
    }
}

/// Minimum sub-warp displacement squared (`100 m`) triggering an immediate `snap.item_positions` flush
/// between periodic 5-tick flushes.
const SUBWARP_FLUSH_DISPLACEMENT_SQ_M2: f64 = 100.0 * 100.0;
/// Periodic sub-warp position flush cadence in 1 Hz ticks when moving slowly (`< 100 m/s`).
const SUBWARP_FLUSH_INTERVAL_TICKS: u64 = 5;

/// Collects explicit changed `ball_ids` coordinates from `ballpark` into `post_unlock.item_positions`
/// when backed by `snap.items` and not stale (`#741`, `#1170`).
pub(crate) fn collect_ball_positions_for_post_unlock(
    ballpark: &Ballpark,
    ball_ids: &[BallId],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    post_unlock: &mut post_unlock::PostUnlockOps,
) {
    if db_opt.is_none() || ball_ids.is_empty() {
        return;
    }
    let solar_system_id = ballpark.solar_system_id;
    let mut updates = Vec::new();
    for &bid in ball_ids {
        if ballpark.is_static_ball(bid)
            || ballpark.ball_slim_meta.get(&bid).is_some_and(|m| m.category_id == 11)
        {
            continue;
        }
        let Some(b) = ballpark.balls.get(&bid).filter(|b| b.position.is_finite()) else {
            continue;
        };
        updates.push((bid, b.position.to_array(), solar_system_id, ball_space_entry_epochs.get(&bid).copied()));
    }
    post_unlock.push_item_positions(updates);
}

/// Persists explicit `ball_ids` coordinates from `ballpark` into `snap.item_positions`
/// when backed by `snap.items` and changed (`#741`). Unit tests only (`W2`, `W6`).
#[cfg(test)]
pub(crate) fn persist_ball_positions_in_db(
    ballpark: &Ballpark,
    ball_ids: &[BallId],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
) {
    let mut ops = post_unlock::PostUnlockOps::default();
    collect_ball_positions_for_post_unlock(
        ballpark,
        ball_ids,
        db_opt,
        ball_space_entry_epochs,
        &mut ops,
    );
    if !ops.is_empty() {
        let _ = post_unlock::execute_post_unlock_ops(db_opt.as_ref(), None, ops, None, None);
    }
}

/// Collects dynamic ship/item coordinates into `post_unlock`, on warp exit,
/// explicit `SetBallPosition` / `Stop` events, and periodic / resting sub-warp movement flushes (`#741`, `#1170`).
pub(crate) fn collect_landed_warps_for_post_unlock_with_in_flight(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    has_in_flight: bool,
    post_unlock: &mut post_unlock::PostUnlockOps,
) {
    let Some(db) = db_opt.as_ref() else {
        return;
    };
    let solar_system_id = ballpark.solar_system_id;
    let mut immediate_ids = std::collections::BTreeSet::new();
    for ev in events {
        match ev {
            eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id }
            | eve_destiny::DestinyEvent::SetBallPosition { ball_id, .. }
            | eve_destiny::DestinyEvent::Stop { ball_id } => {
                if !ballpark.is_static_ball(*ball_id) {
                    immediate_ids.insert(*ball_id);
                }
            }
            _ => {}
        }
    }
    let collect_unfiltered = || {
        let mut out = Vec::new();
        for (&bid, b) in &ballpark.balls {
            if ballpark.is_static_ball(bid)
                || ballpark
                    .ball_slim_meta
                    .get(&bid)
                    .is_some_and(|m| m.category_id == 11)
                || !b.position.is_finite()
            {
                continue;
            }
            let is_immediate = immediate_ids.contains(&bid);
            if !is_immediate
                && matches!(
                    b.mode,
                    eve_destiny::MovementMode::Warp { phase, .. }
                        if phase != eve_destiny::WarpPhase::Aligning
                )
            {
                continue;
            }
            if is_immediate
                || ballpark.current_tick % SUBWARP_FLUSH_INTERVAL_TICKS == 0
                || !matches!(b.mode, eve_destiny::MovementMode::Stop)
                || b.velocity.length_squared() > 1e-6
            {
                let epoch_opt = ball_space_entry_epochs.get(&bid).copied();
                out.push((bid, b.position.to_array(), solar_system_id, epoch_opt));
            }
        }
        out
    };
    let updates: Vec<(BallId, [f64; 3], SolarSystemId, Option<u64>)> =
        if db.is_write_locked_by_current_thread() || has_in_flight {
            collect_unfiltered()
        } else if let Some(filtered) = db.try_with_snapshot(|snap| {
            let mut out = Vec::new();
            for (&bid, b) in &ballpark.balls {
                if ballpark.is_static_ball(bid)
                    || ballpark
                        .ball_slim_meta
                        .get(&bid)
                        .is_some_and(|m| m.category_id == 11)
                    || !b.position.is_finite()
                {
                    continue;
                }
                let is_immediate = immediate_ids.contains(&bid);
                if !is_immediate
                    && matches!(
                        b.mode,
                        eve_destiny::MovementMode::Warp { phase, .. }
                            if phase != eve_destiny::WarpPhase::Aligning
                    )
                {
                    continue;
                }
                let should_write = match snap.item_positions.get(&bid) {
                    None => {
                        is_immediate
                            || !matches!(b.mode, eve_destiny::MovementMode::Stop)
                            || b.velocity.length_squared() > 1e-6
                    }
                    Some(&prev) => {
                        let delta_sq = (DVec3::from_array(prev) - b.position).length_squared();
                        if is_immediate {
                            delta_sq > 1e-6
                        } else {
                            delta_sq > 1e-4
                                && (delta_sq >= SUBWARP_FLUSH_DISPLACEMENT_SQ_M2
                                    || ballpark.current_tick % SUBWARP_FLUSH_INTERVAL_TICKS == 0
                                    || (matches!(b.mode, eve_destiny::MovementMode::Stop)
                                        && b.velocity.length_squared() <= 1.0))
                        }
                    }
                };
                let epoch_opt = ball_space_entry_epochs.get(&bid).copied();
                if should_write
                    && snap.items.contains_key(&bid)
                    && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                        snap,
                        bid,
                        Some(solar_system_id),
                        epoch_opt,
                    )
                {
                    out.push((bid, b.position.to_array(), solar_system_id, epoch_opt));
                }
            }
            out
        }) {
            filtered
        } else {
            collect_unfiltered()
        };
    post_unlock.push_item_positions(updates);
}

/// Persists dynamic ship/item coordinates into `snap.item_positions` on warp exit (`OnDeactivatingWarp`),
/// explicit `SetBallPosition` / `Stop` events, and periodic / resting sub-warp movement flushes,
/// while performing zero snapshot writes when persisted items are stationary (`#741`). Unit tests only (`W2`, `W6`).
#[cfg(test)]
pub(crate) fn persist_landed_warps_in_db(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
) {
    let mut ops = post_unlock::PostUnlockOps::default();
    collect_landed_warps_for_post_unlock_with_in_flight(
        ballpark,
        events,
        db_opt,
        ball_space_entry_epochs,
        false,
        &mut ops,
    );
    if !ops.is_empty() {
        let _ = post_unlock::execute_post_unlock_ops(db_opt.as_ref(), None, ops, None, None);
    }
}

/// Records the initial checkpoint baseline and `space_entry_epoch` when a dynamic ball enters the actor (`T-2`, `T-5`).
pub(crate) fn record_spawned_ball_checkpoint_baseline(
    state: &mut crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
    ball_id: BallId,
) {
    if ballpark.is_static_ball(ball_id) {
        return;
    }
    let epoch = state
        .db_opt
        .as_ref()
        .filter(|db| !db.is_write_locked_by_current_thread())
        .map(|db| crate::sp6_lifecycle::current_ship_space_entry_epoch(db, ball_id))
        .unwrap_or(0);
    state.ball_space_entry_epochs.insert(ball_id, epoch);
    if let Some(b) = ballpark.balls.get(&ball_id) {
        let cs = ballpark.combat_states.get(&ball_id);
        state.last_checkpointed_balls.insert(
            ball_id,
            crate::system_actor::types::LastCheckpointedBallState {
                space_entry_epoch: epoch,
                shield_hp: cs.map(|c| c.shield_hp).unwrap_or(0.0),
                armor_hp: cs.map(|c| c.armor_hp).unwrap_or(0.0),
                structure_hp: cs.map(|c| c.structure_hp).unwrap_or(0.0),
                capacitor: cs.map(|c| c.capacitor).unwrap_or(0.0),
                position: b.position.to_array(),
            },
        );
    }
}

/// Removes checkpoint tracking for a ball leaving the actor (`T-2`, `T-5`).
pub(crate) fn remove_ball_checkpoint_tracking(
    state: &mut crate::system_actor::types::SystemActorState,
    ball_id: BallId,
) {
    state.ball_space_entry_epochs.remove(&ball_id);
    state.last_checkpointed_balls.remove(&ball_id);
    state.warp_state.clear_ball(ball_id, true);
    let _ = crate::system_actor::weapon_effects::clear_removed_ball_weapon_effects_in_state(state, ball_id);
}

/// Prunes landed or deactivated warps from actor `warp_state`, deferring `OnWarpFinished`
/// delivery to `warp_state.pending_warp_finished` when a player, fleet, or return warp
/// completes (`#1175`, `#1541`).
pub(crate) fn clear_landed_warp_tracking_from_events(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    warp_state: &mut crate::system_actor::types::ActorWarpState,
) {
    let is_currently_warping = |ball_id: BallId| {
        matches!(
            ballpark.balls.get(&ball_id).map(|b| &b.mode),
            Some(eve_destiny::MovementMode::Warp { .. })
        )
    };
    let mut deactivated_ids: std::collections::HashSet<BallId> = std::collections::HashSet::new();
    let mut stopped_ids: std::collections::HashSet<BallId> = std::collections::HashSet::new();
    let mut removed_ids: std::collections::HashSet<BallId> = std::collections::HashSet::new();
    for ev in events {
        match ev {
            eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id }
                if !ballpark.is_static_ball(*ball_id) && !is_currently_warping(*ball_id) =>
            {
                deactivated_ids.insert(*ball_id);
            }
            eve_destiny::DestinyEvent::Stop { ball_id }
                if !ballpark.is_static_ball(*ball_id) && !is_currently_warping(*ball_id) =>
            {
                stopped_ids.insert(*ball_id);
            }
            eve_destiny::DestinyEvent::RemoveBall { ball_id }
                if !ballpark.is_static_ball(*ball_id) && !ballpark.balls.contains_key(ball_id) =>
            {
                removed_ids.insert(*ball_id);
            }
            eve_destiny::DestinyEvent::RemoveBalls { ball_ids } => {
                for &ball_id in ball_ids {
                    if !ballpark.is_static_ball(ball_id) && !ballpark.balls.contains_key(&ball_id)
                    {
                        removed_ids.insert(ball_id);
                    }
                }
            }
            _ => {}
        }
    }
    if deactivated_ids.is_empty() && removed_ids.is_empty() {
        return;
    }
    for &ball_id in &deactivated_ids {
        let prev_prov = warp_state.provenance.remove(&ball_id);
        let removed_active = warp_state.active_warps.remove(&ball_id).is_some();
        let removed_return = warp_state.return_warps.remove(&ball_id);
        if stopped_ids.contains(&ball_id)
            && (removed_active
                || removed_return
                || matches!(
                    prev_prov,
                    Some(
                        crate::system_actor::types::WarpProvenance::PlayerOrFleetWarp
                            | crate::system_actor::types::WarpProvenance::ReconnectReturnWarp
                    )
                ))
        {
            warp_state.pending_warp_finished.insert(ball_id);
        }
    }
    for &ball_id in &removed_ids {
        warp_state.clear_ball(ball_id, true);
    }
}

/// Collects landed warp / explicit stop positions from step events for [`crate::system_actor::types::StepOutput`].
pub(crate) fn collect_landed_warp_positions(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
) -> Vec<(BallId, [f64; 3])> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for ev in events {
        match ev {
            eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id }
            | eve_destiny::DestinyEvent::SetBallPosition { ball_id, .. }
            | eve_destiny::DestinyEvent::Stop { ball_id } => {
                if !ballpark.is_static_ball(*ball_id) && seen.insert(*ball_id) {
                    if let Some(b) = ballpark.balls.get(ball_id) {
                        if b.position.is_finite() {
                            out.push((*ball_id, b.position.to_array()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Collects dirty-only condition checkpoint entries for dynamic balls whose HP, capacitor, or position
/// changed since the last checkpoint (`RFC-1 E-4`, `T-2`, `T-5`, `#919`).
pub(crate) fn collect_dirty_condition_checkpoint_entries(
    state: &mut crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
) -> Vec<crate::system_actor::types::ConditionCheckpointEntry> {
    collect_condition_checkpoint_entries_inner(state, ballpark, false)
}

/// Collects condition checkpoint entries for every non-static, non-NPC dynamic ball in `ballpark`,
/// updating `last_checkpointed_balls` so shutdown/eviction flushes always persist a complete checkpoint (`#920`).
pub(crate) fn collect_all_condition_checkpoint_entries(
    state: &mut crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
) -> Vec<crate::system_actor::types::ConditionCheckpointEntry> {
    collect_condition_checkpoint_entries_inner(state, ballpark, true)
}

fn collect_condition_checkpoint_entries_inner(
    state: &mut crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
    force_all: bool,
) -> Vec<crate::system_actor::types::ConditionCheckpointEntry> {
    state
        .last_checkpointed_balls
        .retain(|bid, _| ballpark.balls.contains_key(bid));
    state
        .ball_space_entry_epochs
        .retain(|bid, _| ballpark.balls.contains_key(bid));

    let mut dirty = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if ballpark.is_static_ball(bid)
            || ballpark
                .ball_slim_meta
                .get(&bid)
                .is_some_and(|m| m.category_id == 11)
            || !b.position.is_finite()
        {
            continue;
        }
        let in_tunnel_warp = matches!(
            b.mode,
            eve_destiny::MovementMode::Warp { phase, .. }
                if phase != eve_destiny::WarpPhase::Aligning
        );
        let epoch = match state.ball_space_entry_epochs.get(&bid).copied() {
            Some(ep) => ep,
            None => {
                let ep = state
                    .db_opt
                    .as_ref()
                    .filter(|db| !db.is_write_locked_by_current_thread())
                    .map(|db| crate::sp6_lifecycle::current_ship_space_entry_epoch(db, bid))
                    .unwrap_or(0);
                state.ball_space_entry_epochs.insert(bid, ep);
                ep
            }
        };
        let cs_opt = ballpark.combat_states.get(&bid);
        let shield_hp = cs_opt.map(|c| c.shield_hp).unwrap_or(0.0);
        let armor_hp = cs_opt.map(|c| c.armor_hp).unwrap_or(0.0);
        let structure_hp = cs_opt.map(|c| c.structure_hp).unwrap_or(0.0);
        let capacitor = cs_opt.map(|c| c.capacitor).unwrap_or(0.0);
        let pos = if in_tunnel_warp {
            state
                .last_checkpointed_balls
                .get(&bid)
                .map(|prev| prev.position)
                .unwrap_or_else(|| b.position.to_array())
        } else {
            b.position.to_array()
        };
        let next_state = crate::system_actor::types::LastCheckpointedBallState {
            space_entry_epoch: epoch,
            shield_hp,
            armor_hp,
            structure_hp,
            capacitor,
            position: pos,
        };
        let is_dirty = force_all
            || match state.last_checkpointed_balls.get(&bid) {
                None => true,
                Some(prev) => {
                    prev.space_entry_epoch != next_state.space_entry_epoch
                        || (prev.shield_hp - next_state.shield_hp).abs() > 1e-6
                        || (prev.armor_hp - next_state.armor_hp).abs() > 1e-6
                        || (prev.structure_hp - next_state.structure_hp).abs() > 1e-6
                        || (prev.capacitor - next_state.capacitor).abs() > 1e-6
                        || (prev.position[0] - next_state.position[0]).abs() > 1e-3
                        || (prev.position[1] - next_state.position[1]).abs() > 1e-3
                        || (prev.position[2] - next_state.position[2]).abs() > 1e-3
                }
            };
        if is_dirty {
            state.last_checkpointed_balls.insert(bid, next_state);
            dirty.push(crate::system_actor::types::ConditionCheckpointEntry {
                ball_id: bid,
                space_entry_epoch: epoch,
                combat_state: cs_opt.cloned(),
                position: pos,
            });
        }
    }
    dirty
}

/// Collects condition checkpoint entries for any non-NPC ball whose `armor_hp` or `structure_hp`
/// changed since the last checkpoint (`RFC-1 E-4` `T-4`), while leaving passive shield/capacitor/position
/// changes for the staggered 10-tick checkpoint (`T-5`).
pub(crate) fn collect_immediate_armor_hull_checkpoint_entries(
    state: &mut crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
) -> Option<Vec<crate::system_actor::types::ConditionCheckpointEntry>> {
    let mut dirty = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if ballpark.is_static_ball(bid)
            || ballpark
                .ball_slim_meta
                .get(&bid)
                .is_some_and(|m| m.category_id == 11)
            || !b.position.is_finite()
        {
            continue;
        }
        let Some(cs) = ballpark.combat_states.get(&bid) else {
            continue;
        };
        let Some(prev) = state.last_checkpointed_balls.get(&bid) else {
            continue;
        };
        let armor_or_hull_dirty = (prev.armor_hp - cs.armor_hp).abs() > 1e-6
            || (prev.structure_hp - cs.structure_hp).abs() > 1e-6;
        if !armor_or_hull_dirty {
            continue;
        }
        let in_tunnel_warp = matches!(
            b.mode,
            eve_destiny::MovementMode::Warp { phase, .. }
                if phase != eve_destiny::WarpPhase::Aligning
        );
        let pos = if in_tunnel_warp {
            prev.position
        } else {
            b.position.to_array()
        };
        let epoch = prev.space_entry_epoch;
        state.last_checkpointed_balls.insert(
            bid,
            crate::system_actor::types::LastCheckpointedBallState {
                space_entry_epoch: epoch,
                shield_hp: cs.shield_hp,
                armor_hp: cs.armor_hp,
                structure_hp: cs.structure_hp,
                capacitor: cs.capacitor,
                position: pos,
            },
        );
        dirty.push(crate::system_actor::types::ConditionCheckpointEntry {
            ball_id: bid,
            space_entry_epoch: epoch,
            combat_state: Some(cs.clone()),
            position: pos,
        });
    }
    if dirty.is_empty() {
        None
    } else {
        Some(dirty)
    }
}

/// Updates the actor's atomic dynamic-ball count and hibernation state (`RFC-1 E-4`, `#919`, `#866`).
pub(crate) fn update_system_hibernation_flag(
    state: &crate::system_actor::types::SystemActorState,
    ballpark: &Ballpark,
    active_subscribers: usize,
) {
    use std::sync::atomic::Ordering;
    let dyn_balls = ballpark
        .balls
        .keys()
        .filter(|&&bid| !ballpark.is_static_ball(bid))
        .count();
    state.dynamic_ball_count.store(dyn_balls, Ordering::Relaxed);
    let has_active_work = dyn_balls > 0
        || active_subscribers > 0
        || !state.active_module_cycles.is_empty()
        || !state.pending_anchoring_disruptors.is_empty()
        || !state.active_nanite_repairs.is_empty()
        || !state.active_drone_miners.is_empty()
        || state.fighters.active_fighters.values().any(|f| !f.is_abandoned)
        || !state.reserved_drone_launches.is_empty()
        || state
            .active_drones_in_space
            .values()
            .any(|d| !d.is_abandoned || d.target_id.is_some())
        || (state.db_opt.is_some()
            && (state.active_drones_checkpoint_dirty
                || state.active_drones_needs_sqlite_hydration))
        || state.weapon_effects.has_active_work()
        || crate::system_actor::weapon_effects::has_unflushed_weapon_effects_in_state(state)
        || state.pending_destroyed_balls.is_non_empty()
        || state
            .asteroids
            .values()
            .any(|a| a.depleted || a.respawn_at_tick.is_some());
    if has_active_work {
        state.hibernated.store(false, Ordering::Relaxed);
        state.hibernated_since_unix.store(0, Ordering::Relaxed);
        state.idle_ticks.store(0, Ordering::Relaxed);
    } else if !state.hibernated.swap(true, Ordering::Relaxed) {
        let now_unix = state
            .db_opt
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(1);
        state
            .hibernated_since_unix
            .store(now_unix.max(1), Ordering::Relaxed);
    }
}

/// Passively recharges stationary or unstepped ships during `fast_forward_clock` and collects updated HP/capacitor for post-unlock DB flush.
fn recharge_ship_combat_state_on_fast_forward_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    unstepped_secs: u64,
    post_unlock: &mut post_unlock::PostUnlockOps,
) {
    if unstepped_secs == 0 {
        return;
    }
    let dt_ms = (unstepped_secs as f64) * 1000.0;
    let recharge_field = |cur: &mut f64, max_val: f64, tau_ms: f64| -> bool {
        if max_val <= 0.0 || tau_ms <= 0.0 || *cur >= max_val {
            return false;
        }
        let next = eve_destiny::combat::compute_passive_recharge(*cur, max_val, tau_ms, dt_ms);
        if (next - *cur).abs() > 1e-9 {
            *cur = next;
            true
        } else {
            false
        }
    };
    let updated = if let Some(cs) = ballpark.combat_states.get_mut(&ship_id) {
        let cap_changed =
            recharge_field(&mut cs.capacitor, cs.max_capacitor, cs.capacitor_recharge_time_ms);
        let shield_changed =
            recharge_field(&mut cs.shield_hp, cs.max_shield_hp, cs.shield_recharge_time_ms);
        (cap_changed || shield_changed).then(|| cs.clone())
    } else {
        None
    };
    if let Some(cs) = updated {
        post_unlock.push_ship_combat_hp(post_unlock::CombatHpDbUpdate {
            ball_id: ship_id,
            combat_state: cs,
            expected_system: Some(solar_system_id),
            require_persisted_entity: true,
        });
    }
}

/// Fast-forwards physics ticks inside the system actor for `timeControl.AdvanceClock`
/// without per-tick DB snapshot cloning or flooding intermediate `DoDestinyUpdate` broadcasts.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_positions_actor: &Arc<BallPositionSyncCache>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
) {
    let empty_epochs = HashMap::new();
    let mut empty_warp_state = crate::system_actor::types::ActorWarpState::default();
    fast_forward_clock_in_actor_with_epochs(
        solar_system_id,
        ballpark,
        ship_id,
        secs,
        sim_elapsed_secs,
        db_opt,
        &empty_epochs,
        ball_positions_actor,
        scrambled_balls_actor,
        event_tx_actor,
        &mut empty_warp_state,
    );
}

#[cfg(test)]
#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor_with_epochs(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    ball_positions_actor: &Arc<BallPositionSyncCache>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
    warp_state: &mut crate::system_actor::types::ActorWarpState,
) {
    fast_forward_clock_in_actor_with_epochs_and_seq(
        solar_system_id,
        ballpark,
        ship_id,
        secs,
        sim_elapsed_secs,
        db_opt,
        ball_space_entry_epochs,
        ball_positions_actor,
        scrambled_balls_actor,
        event_tx_actor,
        None,
        warp_state,
    );
}

#[allow(dead_code, clippy::too_many_arguments)]
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor_with_epochs_and_seq(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    ball_positions_actor: &Arc<BallPositionSyncCache>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
    broadcast_seq_gate: Option<&std::sync::Mutex<u64>>,
    warp_state: &mut crate::system_actor::types::ActorWarpState,
) {
    let mut ops = post_unlock::PostUnlockOps::default();
    fast_forward_clock_in_actor_collecting(
        solar_system_id,
        ballpark,
        ship_id,
        secs,
        sim_elapsed_secs,
        db_opt,
        ball_space_entry_epochs,
        ball_positions_actor,
        scrambled_balls_actor,
        event_tx_actor,
        broadcast_seq_gate,
        warp_state,
        false,
        &mut ops,
    );
    if !ops.is_empty() {
        let _ = post_unlock::execute_post_unlock_ops(db_opt.as_ref(), None, ops, None, None);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    ball_positions_actor: &Arc<BallPositionSyncCache>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
    broadcast_seq_gate: Option<&std::sync::Mutex<u64>>,
    warp_state: &mut crate::system_actor::types::ActorWarpState,
    has_in_flight: bool,
    post_unlock: &mut post_unlock::PostUnlockOps,
) {
    let Some(ball0) = ballpark.balls.get(&ship_id) else {
        return;
    };
    let mut saw_warp = matches!(ball0.mode, eve_destiny::MovementMode::Warp { .. });
    let is_moving = !matches!(ball0.mode, eve_destiny::MovementMode::Stop);
    let was_invuln = ballpark
        .combat_states
        .get(&ship_id)
        .is_some_and(|cs| cs.is_invulnerable());
    if !is_moving && !was_invuln {
        recharge_ship_combat_state_on_fast_forward_collecting(
            solar_system_id,
            ballpark,
            ship_id,
            secs,
            post_unlock,
        );
        return;
    }
    let max_sim = if is_moving {
        secs.min(300)
    } else {
        secs.min(65)
    };
    let initial_tick = ballpark.current_tick;
    let mut all_events = Vec::new();
    let mut sim_steps: u64 = 0;
    for _ in 0..max_sim {
        *sim_elapsed_secs += 1.0;
        sim_steps += 1;
        let evs = ballpark.step(1.0);
        collect_landed_warps_for_post_unlock_with_in_flight(
            ballpark,
            &evs,
            db_opt,
            ball_space_entry_epochs,
            has_in_flight || !post_unlock.is_empty(),
            post_unlock,
        );
        clear_landed_warp_tracking_from_events(ballpark, &evs, warp_state);
        let warp_exited = evs.iter().any(|e| {
            matches!(
                e,
                eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id } if *ball_id == ship_id
            )
        });
        all_events.extend(evs);
        let mode_now = ballpark.balls.get(&ship_id).map(|b| &b.mode);
        if matches!(mode_now, Some(eve_destiny::MovementMode::Warp { .. })) {
            saw_warp = true;
        }
        if warp_exited
            || (saw_warp && !matches!(mode_now, Some(eve_destiny::MovementMode::Warp { .. })))
            || (is_moving && matches!(mode_now, Some(eve_destiny::MovementMode::Stop) | None))
        {
            break;
        }
        if !is_moving
            && ballpark
                .combat_states
                .get(&ship_id)
                .is_none_or(|cs| !cs.is_invulnerable())
        {
            break;
        }
    }
    let unstepped_secs = secs.saturating_sub(sim_steps);
    if unstepped_secs > 0 {
        recharge_ship_combat_state_on_fast_forward_collecting(
            solar_system_id,
            ballpark,
            ship_id,
            unstepped_secs,
            post_unlock,
        );
    } else if sim_steps > 0 {
        if let Some(cs) = ballpark.combat_states.get(&ship_id) {
            post_unlock.push_ship_combat_hp(post_unlock::CombatHpDbUpdate {
                ball_id: ship_id,
                combat_state: cs.clone(),
                expected_system: Some(solar_system_id),
                require_persisted_entity: true,
            });
        }
    }
    // Preserve `initial_tick` instead of bumping `ballpark.current_tick` ahead of live clients:
    // `AdvanceClock` advances server simulation state without rebasing client `blue.os`, and V24
    // `destiny/net/client/_ticker.pyj` buffers (+1..+2) or forces `_parent_Evolve` / recoverable
    // desync (>= +3) when server stamps drift ahead of client `currentTime` (`#1159`, `#1165`),
    // while `outbound_sequencer` and `_util.merge_state_into_history` accept equal stamps.
    ballpark.current_tick = initial_tick;
    if is_moving {
        if let Some((pos, vel)) = ballpark
            .balls
            .get(&ship_id)
            .map(|b| (b.position.to_array(), b.velocity.to_array()))
        {
            let pos_ev = eve_destiny::DestinyEvent::SetBallPosition {
                ball_id: ship_id,
                position: pos,
            };
            let vel_ev = eve_destiny::DestinyEvent::SetBallVelocity {
                ball_id: ship_id,
                velocity: vel,
            };
            let obs = ballpark.observer_events.entry(ship_id).or_default();
            let had_set_state = obs
                .iter()
                .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }));
            obs.insert(0, pos_ev.clone());
            obs.insert(1, vel_ev.clone());
            if had_set_state {
                ballpark.queue_set_state(ship_id);
            }
            all_events.insert(0, pos_ev);
            all_events.insert(1, vel_ev);
        }
        collect_ball_positions_for_post_unlock(
            ballpark,
            &[ship_id],
            db_opt,
            ball_space_entry_epochs,
            post_unlock,
        );
    }
    crate::system_actor::sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
    let broadcast = crate::system_actor::build_tick_broadcast(solar_system_id, ballpark, all_events);
    if let Some(gate) = broadcast_seq_gate {
        crate::system_actor::send_tick_broadcast_sequenced_owned(event_tx_actor, gate, broadcast);
    } else {
        let _ = event_tx_actor.send(broadcast);
    }
}

/// Records wallclock warp-stop timestamps (`tokio::time::Instant`) whenever warp ends via
/// `SetBallMassive { is_massive: true }`, and clears entries when a ball enters warp or leaves space.
pub(crate) fn record_warp_stops_from_events_in_map(
    events: &[eve_destiny::DestinyEvent],
    map: &RankedRwLock<HashMap<BallId, tokio::time::Instant>, RANK_LEAF>,
) {
    let has_relevant = events.iter().any(|ev| {
        matches!(
            ev,
            eve_destiny::DestinyEvent::SetBallMassive { is_massive: true, .. }
                | eve_destiny::DestinyEvent::WarpTo { .. }
                | eve_destiny::DestinyEvent::RemoveBall { .. }
                | eve_destiny::DestinyEvent::RemoveBalls { .. }
        )
    });
    if !has_relevant {
        return;
    }
    let now = tokio::time::Instant::now();
    let mut guard = map.write().unwrap_or_else(|e| e.into_inner());
    for ev in events {
        match ev {
            eve_destiny::DestinyEvent::SetBallMassive {
                ball_id,
                is_massive: true,
            } => {
                guard.insert(*ball_id, now);
            }
            eve_destiny::DestinyEvent::WarpTo { ball_id, .. }
            | eve_destiny::DestinyEvent::RemoveBall { ball_id } => {
                guard.remove(ball_id);
            }
            eve_destiny::DestinyEvent::RemoveBalls { ball_ids } => {
                for id in ball_ids {
                    guard.remove(id);
                }
            }
            _ => {}
        }
    }
}

impl SystemHandle {
    /// Records an immediate warp-stop timestamp for `ball_id`.
    pub(crate) fn record_warp_stop(&self, ball_id: BallId) {
        let now = tokio::time::Instant::now();
        self.warp_stopped_at
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ball_id, now);
    }

    /// Clears any recorded warp-stop settle timestamp for `ball_id`.
    pub(crate) fn clear_warp_stopped_at(&self, ball_id: BallId) {
        self.warp_stopped_at
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
    }

    /// Updates `warp_stopped_at` from a batch of destiny events.
    pub(crate) fn record_warp_stops_from_events(&self, events: &[eve_destiny::DestinyEvent]) {
        record_warp_stops_from_events_in_map(events, &self.warp_stopped_at);
    }

    /// Returns the settle deadline if `ball_id` exited warp within `window` and the client
    /// `Warp.WarpLoop` tasklet (`blue.synchro.SleepSim(500)`) may still need time to finish
    /// before `OnReleaseBallpark`.
    pub(crate) fn warp_stop_settle_deadline(
        &self,
        ball_id: BallId,
        window: std::time::Duration,
    ) -> Option<tokio::time::Instant> {
        let stopped_at = self
            .warp_stopped_at
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ball_id)
            .copied()?;
        let deadline = stopped_at + window;
        (tokio::time::Instant::now() < deadline).then_some(deadline)
    }

    /// Stops an arriving warp before docking, optionally snapping `ball.position` to the
    /// verified `landing_pos` inside the docking perimeter so subsequent post-settle checks
    /// stay within range.
    ///
    /// Requires no ranked locks held by the caller (panics under held locks).
    pub(crate) fn finish_warp_for_dock(
        &self,
        ball_id: BallId,
        landing_pos: Option<DVec3>,
    ) -> Result<u64, String> {
        let tick = crate::system_actor::ball_ops::dispatch_actor_sync_with_deadline(
            self,
            "FinishWarpForDock",
            crate::system_actor::ball_ops::ACTOR_CMD_TIMEOUT,
            (ball_id, landing_pos),
            |(ball_id, landing_pos), reply| crate::system_actor::SystemCommand::FinishWarpForDock {
                ball_id,
                landing_pos,
                reply,
            },
            |ctx, (ball_id, landing_pos)| {
                crate::system_actor::handle_cmds::finish_warp_for_dock_in_ctx(ctx, ball_id, landing_pos)
            },
        )?;
        self.record_warp_stop(ball_id);
        Ok(tick)
    }

    /// Server-authoritative warp termination (reconnect recovery / admin reset) that stops even
    /// mid-tunnel warps, zeroes velocity, and restores `SetBallMassive { is_massive: true }`.
    pub fn force_stop_warp(&self, ball_id: BallId) -> Result<u64, String> {
        self.finish_warp_for_dock(ball_id, None)
    }

    /// Returns `(asteroid_id, remaining_units)` (`0` if depleted) for each tracked asteroid ID (`#1342`).
    pub fn live_asteroid_remaining_units(&self, asteroid_ids: &[BallId]) -> Vec<(BallId, i64)> {
        let view = self.view();
        asteroid_ids
            .iter()
            .filter_map(|&aid| {
                view.asteroids.get(&aid).map(|st| {
                    (
                        aid,
                        if st.depleted {
                            0_i64
                        } else {
                            st.remaining_units as i64
                        },
                    )
                })
            })
            .collect()
    }

    /// Returns the last synced velocity `[vx, vy, vz]` for `ball_id`, reading [`SystemView`]
    /// first or falling back to [`BallPositionSyncCache`].
    pub fn get_ball_velocity_sync(&self, ball_id: BallId) -> Option<[f64; 3]> {
        if let Some(vel) = self.view().ball_velocity(ball_id) {
            return Some([vel.x, vel.y, vel.z]);
        }
        self.ball_positions.get_velocity(ball_id)
    }

    /// Returns live `(position, velocity)` for `ball_id` only if the ball is currently present in
    /// this system (`GH #1085`).
    pub fn get_live_ball_kinematics_sync(&self, ball_id: BallId) -> Option<(DVec3, DVec3)> {
        if let Some(ball) = self.view().ball(ball_id) {
            return Some((ball.position, ball.velocity));
        }
        let pos = self.get_ball_position_sync(ball_id)?;
        let vel = self
            .get_ball_velocity_sync(ball_id)
            .map(DVec3::from_array)
            .unwrap_or(DVec3::ZERO);
        Some((pos, vel))
    }

    /// Synchronously collects dirty condition checkpoint entries and routes persistence through this
    /// system's ordered `post_unlock_worker` (`W1`, `W2`, `W3`, `W6`, `#1170`, `#919`).
    ///
    /// Requires no ranked locks held by the caller (panics under held locks). When invoked from
    /// the post-unlock worker (`wait_for_post_unlock == false`), SQLite persistence runs
    /// asynchronously on the worker and this returns `0` without blocking (invocations on
    /// `actor_flush_thread`, where the worker is closed, refuse enqueueing and return `0`).
    pub fn flush_dirty_condition_checkpoint_sync(&self) -> usize {
        let wait_for_post_unlock = post_unlock::caller_can_wait_for_handle(self);
        let written_direct = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written_direct_clone = Arc::clone(&written_direct);
        let actor_written = crate::system_actor::ball_ops::dispatch_actor_sync_reply_with_deadline(
            self,
            "FlushDirtyConditionCheckpointSync",
            crate::system_actor::ball_ops::ACTOR_CMD_TIMEOUT,
            (),
            move |(), reply| crate::system_actor::SystemCommand::FlushDirtyConditionCheckpointSync {
                wait_for_post_unlock,
                reply,
            },
            move |ctx, ()| {
                let entries = collect_dirty_condition_checkpoint_entries(ctx.actor_st, ctx.ballpark);
                if !entries.is_empty() && ctx.actor_st.db_opt.is_some() {
                    ctx.post_unlock_ops.push(post_unlock::PostUnlockIntent::ConditionCheckpoint {
                        solar_system_id: ctx.solar_system_id,
                        entries,
                        written_out: Some(written_direct_clone),
                    });
                }
                0
            },
        )
        .unwrap_or(0);
        actor_written.max(written_direct.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(crate) async fn wait_for_post_unlock_barrier(&self) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let item = post_unlock::PostUnlockWorkItem {
            db_opt: None,
            sde_opt: None,
            ops: post_unlock::PostUnlockOps::default(),
            replies: vec![Box::new(move || { let _ = tx.send(()); })],
            in_flight_guard: None,
            work_seq: 0,
        };
        if self.post_unlock_tx.send(post_unlock::PostUnlockMsg::Work(item)).is_err() {
            return false;
        }
        rx.await.is_ok()
    }
}

#[cfg(test)]
#[path = "celestials_helpers_cov_unit_tests.rs"]
mod celestials_helpers_cov_unit_tests;
#[cfg(test)]
#[path = "celestials_helpers_edges_tests.rs"]
mod celestials_helpers_edges_tests;
