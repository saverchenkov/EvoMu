//! DB-backed ballpark lookup, ball removal, and warp landing persistence helpers for `celestials`.

#![forbid(unsafe_code)]

use super::SystemHandle;
use eve_destiny::{BallId, Ballpark};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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

pub(crate) fn sync_ball_positions(
    ballpark: &Ballpark,
    cache: &RwLock<HashMap<BallId, DVec3>>,
    scrambled: &RwLock<std::collections::HashSet<BallId>>,
) {
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    let mut scram_guard = scrambled.write().unwrap_or_else(|e| e.into_inner());
    guard.clear();
    scram_guard.clear();
    for (&id, ball) in &ballpark.balls {
        guard.insert(id, ball.position);
        if ballpark.is_ball_scrambled(id) {
            scram_guard.insert(id);
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
            let mut bp = bp_arc.write().unwrap_or_else(|e| e.into_inner());
            let _ = bp.remove_ball(ball_id);
            sync_ball_positions(&bp, &pos_arc, &scram_arc);
        }
    }
}

/// Persists dynamic ship/item landing coordinates into `snap.item_positions` when a warp slide exits warp.
pub(crate) fn persist_landed_warps_in_db(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let Some(db) = db_opt.as_ref() else {
        return;
    };
    if !events
        .iter()
        .any(|e| matches!(e, eve_destiny::DestinyEvent::OnDeactivatingWarp { .. }))
    {
        return;
    }
    db.with_mut_snapshot(|snap| {
        for ev in events {
            if let eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id } = ev {
                if !ballpark.is_static_ball(*ball_id) && snap.items.contains_key(ball_id) {
                    if let Some(b) = ballpark.balls.get(ball_id) {
                        snap.item_positions.insert(*ball_id, b.position.to_array());
                    }
                }
            }
        }
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
    ball_positions_actor: &Arc<RwLock<HashMap<BallId, DVec3>>>,
    scrambled_balls_actor: &Arc<RwLock<std::collections::HashSet<BallId>>>,
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
            obs.insert(0, pos_ev.clone());
            obs.insert(1, vel_ev.clone());
            all_events.insert(0, pos_ev);
            all_events.insert(1, vel_ev);
        }
    }
    crate::system_actor::sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
    let broadcast = crate::system_actor::build_tick_broadcast(solar_system_id, ballpark, all_events);
    let _ = event_tx_actor.send(broadcast);
}

#[cfg(test)]
#[path = "celestials_helpers_cov_unit_tests.rs"]
mod celestials_helpers_cov_unit_tests;
