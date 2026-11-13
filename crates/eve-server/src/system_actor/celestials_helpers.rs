//! DB-backed ballpark lookup, ball removal, and warp landing persistence helpers for `celestials`.

#![forbid(unsafe_code)]

use super::SystemHandle;
use eve_db::lock_rank::{RankedRwLock, RANK_BALLPARK, RANK_LEAF};
use eve_destiny::{BallId, Ballpark};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(clippy::type_complexity)]
fn db_ballpark_map() -> &'static std::sync::Mutex<
    HashMap<
        (u64, SolarSystemId),
        (
            std::sync::Weak<RankedRwLock<Ballpark, RANK_BALLPARK>>,
            std::sync::Weak<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
            std::sync::Weak<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
        ),
    >,
> {
    static MAP: std::sync::OnceLock<
        std::sync::Mutex<
            HashMap<
                (u64, SolarSystemId),
                (
                    std::sync::Weak<RankedRwLock<Ballpark, RANK_BALLPARK>>,
                    std::sync::Weak<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
                    std::sync::Weak<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
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

fn committed_warp_cache_map(
) -> &'static std::sync::Mutex<
    HashMap<usize, std::sync::Weak<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>>,
> {
    static MAP: std::sync::OnceLock<
        std::sync::Mutex<
            HashMap<usize, std::sync::Weak<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>>,
        >,
    > = std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn register_committed_warp_cache(
    cache: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    committed_warps: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
) {
    let key = Arc::as_ptr(cache) as usize;
    let mut g = committed_warp_cache_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if g.len() > 256 {
        g.retain(|_, w| w.strong_count() > 0);
    }
    g.insert(key, Arc::downgrade(committed_warps));
}

fn static_position_sync_state(
) -> &'static std::sync::Mutex<HashMap<usize, (u64, usize)>> {
    static MAP: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, (u64, usize)>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn sync_ball_positions(
    ballpark: &Ballpark,
    cache: &RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>,
    scrambled: &RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>,
) {
    let cache_key = cache as *const RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF> as usize;
    let cur_static_epoch = ballpark.static_epoch();
    let cur_static_count = ballpark.static_ball_ids.len();
    let prev_static = static_position_sync_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .copied();
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
        if let Ok(mut g) = cache.try_write_unranked() {
            update_cache(&mut g);
            cache_updated = true;
        }
    } else {
        let mut g = cache.write().unwrap_or_else(|e| e.into_inner());
        update_cache(&mut g);
        cache_updated = true;
    }
    if cache_updated {
        let mut meta = static_position_sync_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if meta.len() > 512 {
            meta.clear();
        }
        meta.insert(cache_key, (cur_static_epoch, cur_static_count));
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

    if leaf_held {
        if let Ok(mut g) = scrambled.try_write_unranked() {
            *g = scram;
        }
    } else {
        *scrambled.write().unwrap_or_else(|e| e.into_inner()) = scram;
    }
    let warp_arc_opt = committed_warp_cache_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .and_then(|w| w.upgrade());
    if let Some(warp_arc) = warp_arc_opt {
        if leaf_held {
            if let Ok(mut g) = warp_arc.try_write_unranked() {
                *g = warps;
            }
        } else {
            *warp_arc.write().unwrap_or_else(|e| e.into_inner()) = warps;
        }
    }
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
            if eve_db::lock_rank::held_lock_ranks().contains(&eve_db::lock_rank::RANK_BALLPARK) {
                eve_db::lock_rank::assert_locks_below(eve_db::lock_rank::RANK_BALLPARK + 1);
                if let Ok(mut bp) = bp_arc.try_write_unranked() {
                    let _ = bp.remove_ball(ball_id);
                    sync_ball_positions(&bp, &pos_arc, &scram_arc);
                }
            } else {
                let mut bp = bp_arc.write().unwrap_or_else(|e| e.into_inner());
                let _ = bp.remove_ball(ball_id);
                sync_ball_positions(&bp, &pos_arc, &scram_arc);
            }
        }
    }
}

/// Minimum sub-warp displacement squared (`100 m`) triggering an immediate `snap.item_positions` flush
/// between periodic 5-tick flushes.
const SUBWARP_FLUSH_DISPLACEMENT_SQ_M2: f64 = 100.0 * 100.0;
/// Periodic sub-warp position flush cadence in 1 Hz ticks when moving slowly (`< 100 m/s`).
const SUBWARP_FLUSH_INTERVAL_TICKS: u64 = 5;

/// Persists explicit `ball_ids` coordinates from `ballpark` into `snap.item_positions`
/// when backed by `snap.items` and changed (`#741`).
pub(crate) fn persist_ball_positions_in_db(
    ballpark: &Ballpark,
    ball_ids: &[BallId],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let Some(db) = db_opt
        .as_ref()
        .filter(|db| !db.is_write_locked_by_current_thread())
    else {
        return;
    };
    if ball_ids.is_empty() {
        return;
    }
    let updates: Vec<(BallId, [f64; 3])> = db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for &bid in ball_ids {
            if ballpark.is_static_ball(bid)
                || ballpark
                    .ball_slim_meta
                    .get(&bid)
                    .is_some_and(|m| m.category_id == 11)
                || !snap.items.contains_key(&bid)
            {
                continue;
            }
            let Some(b) = ballpark.balls.get(&bid) else {
                continue;
            };
            if !b.position.is_finite() {
                continue;
            }
            let changed = match snap.item_positions.get(&bid) {
                None => true,
                Some(&prev) => (DVec3::from_array(prev) - b.position).length_squared() > 1e-6,
            };
            if changed {
                out.push((bid, b.position.to_array()));
            }
        }
        out
    });
    if updates.is_empty() {
        return;
    }
    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        for (bid, pos_arr) in updates {
            if snap.items.contains_key(&bid) {
                snap.item_positions.insert(bid, pos_arr);
            }
        }
        Ok(())
    });
}

/// Persists dynamic ship/item coordinates into `snap.item_positions` on warp exit (`OnDeactivatingWarp`),
/// explicit `SetBallPosition` / `Stop` events, and periodic / resting sub-warp movement flushes,
/// while performing zero snapshot writes when persisted items are stationary (`#741`).
pub(crate) fn persist_landed_warps_in_db(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let Some(db) = db_opt
        .as_ref()
        .filter(|db| !db.is_write_locked_by_current_thread())
    else {
        return;
    };
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
    let updates: Vec<(BallId, [f64; 3])> = db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for (&bid, b) in &ballpark.balls {
            if ballpark.is_static_ball(bid)
                || ballpark
                    .ball_slim_meta
                    .get(&bid)
                    .is_some_and(|m| m.category_id == 11)
                || !snap.items.contains_key(&bid)
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
            if should_write {
                out.push((bid, b.position.to_array()));
            }
        }
        out
    });
    if updates.is_empty() {
        return;
    }
    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        for (bid, pos_arr) in updates {
            if snap.items.contains_key(&bid) {
                snap.item_positions.insert(bid, pos_arr);
            }
        }
        Ok(())
    });
}

/// Fast-forwards physics ticks inside the system actor for `timeControl.AdvanceClock`
/// without per-tick DB snapshot cloning or flooding intermediate `DoDestinyUpdate` broadcasts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_positions_actor: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
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
        return;
    }
    let max_sim = if is_moving {
        secs.min(300)
    } else {
        secs.min(65)
    };
    let initial_tick = ballpark.current_tick;
    let mut all_events = Vec::new();
    for _ in 0..max_sim {
        *sim_elapsed_secs += 1.0;
        let evs = ballpark.step(1.0);
        persist_landed_warps_in_db(ballpark, &evs, db_opt);
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
    ballpark.current_tick = (initial_tick + 1).min(ballpark.current_tick);
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
        persist_ball_positions_in_db(ballpark, &[ship_id], db_opt);
    }
    crate::system_actor::sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
    let broadcast = crate::system_actor::build_tick_broadcast(solar_system_id, ballpark, all_events);
    let _ = event_tx_actor.send(broadcast);
}

impl SystemHandle {
    /// Server-authoritative warp termination (reconnect recovery / admin reset) that stops even
    /// mid-tunnel warps, zeroes velocity, and restores `SetBallMassive { is_massive: true }`.
    pub fn force_stop_warp(&self, ball_id: BallId) -> Result<u64, String> {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.force_stop_warp(ball_id)?;
        crate::system_actor::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        Ok(bp.current_tick)
    }
}

#[cfg(test)]
#[path = "celestials_helpers_cov_unit_tests.rs"]
mod celestials_helpers_cov_unit_tests;
#[cfg(test)]
#[path = "celestials_helpers_edges_tests.rs"]
mod celestials_helpers_edges_tests;
