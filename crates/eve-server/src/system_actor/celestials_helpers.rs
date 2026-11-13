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

pub(crate) fn sync_ball_positions(
    ballpark: &Ballpark,
    cache: &RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>,
    scrambled: &RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>,
) {
    let mut positions = HashMap::with_capacity(ballpark.balls.len());
    let mut scram = std::collections::HashSet::new();
    let mut warps = HashMap::new();
    for (&id, ball) in &ballpark.balls {
        positions.insert(id, ball.position);
        if ballpark.is_ball_scrambled(id) {
            scram.insert(id);
        }
        if let eve_destiny::MovementMode::Warp {
            destination, phase, ..
        } = ball.mode
        {
            if phase != eve_destiny::WarpPhase::Aligning {
                warps.insert(id, destination);
            }
        }
    }
    let leaf_held = eve_db::lock_rank::held_lock_ranks()
        .iter()
        .any(|&r| r >= RANK_LEAF);
    if leaf_held {
        if let Ok(mut g) = cache.try_write_unranked() {
            *g = positions;
        }
        if let Ok(mut g) = scrambled.try_write_unranked() {
            *g = scram;
        }
    } else {
        *cache.write().unwrap_or_else(|e| e.into_inner()) = positions;
        *scrambled.write().unwrap_or_else(|e| e.into_inner()) = scram;
    }
    let warp_arc_opt = committed_warp_cache_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(cache as *const RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF> as usize))
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
            if snap.items.contains_key(&bid)
                && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap, bid, None, None,
                )
            {
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
                || crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap, bid, None, None,
                )
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
            if snap.items.contains_key(&bid)
                && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap, bid, None, None,
                )
            {
                snap.item_positions.insert(bid, pos_arr);
            }
        }
        Ok(())
    });
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
        let is_dirty = match state.last_checkpointed_balls.get(&bid) {
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
        || state
            .asteroids
            .values()
            .any(|a| a.depleted_at_sim_s.is_some());
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
