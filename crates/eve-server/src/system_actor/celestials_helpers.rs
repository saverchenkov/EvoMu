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

fn leaf_write_value<T>(lock: &RankedRwLock<T, RANK_LEAF>, leaf_held: bool, value: T) {
    if leaf_held {
        if let Ok(mut g) = lock.try_write_unranked() {
            *g = value;
        }
    } else {
        *lock.write().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

fn register_weak_leaf_cache<T>(
    map: &std::sync::Mutex<HashMap<usize, std::sync::Weak<RankedRwLock<T, RANK_LEAF>>>>,
    cache: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    target: &Arc<RankedRwLock<T, RANK_LEAF>>,
) {
    let key = Arc::as_ptr(cache) as usize;
    let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() > 256 {
        g.retain(|_, w| w.strong_count() > 0);
    }
    g.insert(key, Arc::downgrade(target));
}

fn update_weak_leaf_cache<T>(
    map: &std::sync::Mutex<HashMap<usize, std::sync::Weak<RankedRwLock<T, RANK_LEAF>>>>,
    cache_key: usize,
    leaf_held: bool,
    value: T,
) {
    let arc_opt = map
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .and_then(|w| w.upgrade());
    if let Some(arc) = arc_opt {
        leaf_write_value(&arc, leaf_held, value);
    }
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
    register_weak_leaf_cache(committed_warp_cache_map(), cache, committed_warps);
}

fn tethered_balls_cache_map(
) -> &'static std::sync::Mutex<
    HashMap<usize, std::sync::Weak<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>>,
> {
    static MAP: std::sync::OnceLock<
        std::sync::Mutex<
            HashMap<
                usize,
                std::sync::Weak<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
            >,
        >,
    > = std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn register_tethered_balls_cache(
    cache: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    tethered_balls: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
) {
    register_weak_leaf_cache(tethered_balls_cache_map(), cache, tethered_balls);
}

fn static_position_sync_state(
) -> &'static std::sync::Mutex<HashMap<usize, (u64, usize)>> {
    static MAP: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, (u64, usize)>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[allow(clippy::type_complexity)]
fn ball_velocity_cache_map(
) -> &'static std::sync::Mutex<HashMap<usize, HashMap<BallId, [f64; 3]>>> {
    static MAP: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, HashMap<BallId, [f64; 3]>>>> =
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

        let mut dyn_vel = HashMap::new();
        if ballpark.balls.len() > cur_static_count {
            for (&id, ball) in &ballpark.balls {
                if !ballpark.is_static_ball(id) {
                    dyn_vel.insert(id, [ball.velocity.x, ball.velocity.y, ball.velocity.z]);
                }
            }
        }
        let mut vel_map = ball_velocity_cache_map()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if vel_map.len() > 512 {
            vel_map.clear();
        }
        vel_map.insert(cache_key, dyn_vel);
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
    update_weak_leaf_cache(committed_warp_cache_map(), cache_key, leaf_held, warps);
    update_weak_leaf_cache(tethered_balls_cache_map(), cache_key, leaf_held, teth);
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
    ball_space_entry_epochs: &HashMap<BallId, u64>,
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
    let solar_system_id = ballpark.solar_system_id;
    let updates: Vec<(BallId, [f64; 3])> = db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for &bid in ball_ids {
            if ballpark.is_static_ball(bid)
                || ballpark
                    .ball_slim_meta
                    .get(&bid)
                    .is_some_and(|m| m.category_id == 11)
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
            if !changed
                || !snap.items.contains_key(&bid)
                || crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap,
                    bid,
                    Some(solar_system_id),
                    ball_space_entry_epochs.get(&bid).copied(),
                )
            {
                continue;
            }
            out.push((bid, b.position.to_array()));
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
                    snap,
                    bid,
                    Some(solar_system_id),
                    ball_space_entry_epochs.get(&bid).copied(),
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
    ball_space_entry_epochs: &HashMap<BallId, u64>,
) {
    let Some(db) = db_opt
        .as_ref()
        .filter(|db| !db.is_write_locked_by_current_thread())
    else {
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
    let updates: Vec<(BallId, [f64; 3])> = db.with_snapshot(|snap| {
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
            if should_write
                && snap.items.contains_key(&bid)
                && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap,
                    bid,
                    Some(solar_system_id),
                    ball_space_entry_epochs.get(&bid).copied(),
                )
            {
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
                    snap,
                    bid,
                    Some(solar_system_id),
                    ball_space_entry_epochs.get(&bid).copied(),
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
    state.warp_provenance.remove(&ball_id);
    clear_shared_warp_tracking(&state.warp_tracking, ball_id, true);
}

/// Clears shared `ServerContext` active-warp tracking tables for `ball_id` without holding
/// multiple `RANK_LEAF` locks simultaneously (`#1175`).
pub(crate) fn clear_shared_warp_tracking(
    tracking: &crate::system_actor::types::SharedWarpTracking,
    ball_id: BallId,
    clear_pending_finished: bool,
) {
    if let Some(ref active_warps) = tracking.active_warps {
        active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
    }
    if let Some(ref return_warps) = tracking.return_warps {
        return_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
    }
    if clear_pending_finished {
        if let Some(ref pending) = tracking.pending_warp_finished {
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ball_id);
        }
    }
}

/// Prunes landed or deactivated warps from actor provenance and shared `active_warps` /
/// `return_warps`, deferring `OnWarpFinished` delivery to `pending_warp_finished` when a
/// player or fleet warp completes (`#1175`).
pub(crate) fn clear_landed_warp_tracking_from_events(
    ballpark: &Ballpark,
    events: &[eve_destiny::DestinyEvent],
    warp_provenance: &mut HashMap<BallId, crate::system_actor::types::WarpProvenance>,
    warp_tracking: &crate::system_actor::types::SharedWarpTracking,
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
            _ => {}
        }
    }
    if deactivated_ids.is_empty() && removed_ids.is_empty() {
        return;
    }
    let mut finished_player_or_fleet: Vec<BallId> = Vec::new();
    for &ball_id in &deactivated_ids {
        let prev_prov = warp_provenance.remove(&ball_id);
        let mut removed_active = false;
        if let Some(ref active_warps) = warp_tracking.active_warps {
            removed_active = active_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ball_id)
                .is_some();
        }
        if let Some(ref return_warps) = warp_tracking.return_warps {
            return_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ball_id);
        }
        if stopped_ids.contains(&ball_id)
            && (removed_active
                || prev_prov
                    == Some(crate::system_actor::types::WarpProvenance::PlayerOrFleetWarp))
        {
            finished_player_or_fleet.push(ball_id);
        }
    }
    if !finished_player_or_fleet.is_empty() {
        if let Some(ref pending) = warp_tracking.pending_warp_finished {
            let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
            for &ball_id in &finished_player_or_fleet {
                guard.insert(ball_id);
            }
        }
    }
    for &ball_id in &removed_ids {
        warp_provenance.remove(&ball_id);
        clear_shared_warp_tracking(warp_tracking, ball_id, true);
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

/// Passively recharges stationary or unstepped ships during `fast_forward_clock` and persists updated HP/capacitor to DB.
fn recharge_and_persist_ship_combat_state_on_fast_forward(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    unstepped_secs: u64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
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
    if let (Some(cs), Some(db)) = (updated, db_opt) {
        crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
            db,
            solar_system_id,
            std::iter::once((ship_id, &cs)),
        );
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
    ball_positions_actor: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
) {
    let empty_epochs = HashMap::new();
    let mut empty_provenance = HashMap::new();
    let empty_tracking = crate::system_actor::types::SharedWarpTracking::default();
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
        &mut empty_provenance,
        &empty_tracking,
    );
}

#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor_with_epochs(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    ball_positions_actor: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
    warp_provenance: &mut HashMap<BallId, crate::system_actor::types::WarpProvenance>,
    warp_tracking: &crate::system_actor::types::SharedWarpTracking,
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
        warp_provenance,
        warp_tracking,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fast_forward_clock_in_actor_with_epochs_and_seq(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ship_id: BallId,
    secs: u64,
    sim_elapsed_secs: &mut f64,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    ball_positions_actor: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    event_tx_actor: &tokio::sync::broadcast::Sender<crate::system_actor::TickBroadcast>,
    broadcast_seq_gate: Option<&std::sync::Mutex<u64>>,
    warp_provenance: &mut HashMap<BallId, crate::system_actor::types::WarpProvenance>,
    warp_tracking: &crate::system_actor::types::SharedWarpTracking,
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
        recharge_and_persist_ship_combat_state_on_fast_forward(
            solar_system_id,
            ballpark,
            ship_id,
            secs,
            db_opt,
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
        persist_landed_warps_in_db(ballpark, &evs, db_opt, ball_space_entry_epochs);
        clear_landed_warp_tracking_from_events(ballpark, &evs, warp_provenance, warp_tracking);
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
        recharge_and_persist_ship_combat_state_on_fast_forward(
            solar_system_id,
            ballpark,
            ship_id,
            unstepped_secs,
            db_opt,
        );
    } else if sim_steps > 0 {
        if let (Some(cs), Some(db)) = (ballpark.combat_states.get(&ship_id), db_opt) {
            crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
                db,
                solar_system_id,
                std::iter::once((ship_id, cs)),
            );
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
        persist_ball_positions_in_db(ballpark, &[ship_id], db_opt, ball_space_entry_epochs);
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
    pub(crate) fn finish_warp_for_dock(
        &self,
        ball_id: BallId,
        landing_pos: Option<DVec3>,
    ) -> Result<u64, String> {
        let tick = {
            let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = landing_pos.filter(|p| p.is_finite()) {
                if let Some(ball) = bp.balls.get_mut(&ball_id) {
                    if matches!(ball.mode, eve_destiny::MovementMode::Warp { .. }) {
                        ball.position = pos;
                    }
                }
            }
            bp.force_stop_warp(ball_id)?;
            crate::system_actor::sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
            bp.current_tick
        };
        self.record_warp_stop(ball_id);
        let tracking = {
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            st.warp_provenance.remove(&ball_id);
            st.warp_tracking.clone()
        };
        clear_shared_warp_tracking(&tracking, ball_id, true);
        Ok(tick)
    }

    /// Server-authoritative warp termination (reconnect recovery / admin reset) that stops even
    /// mid-tunnel warps, zeroes velocity, and restores `SetBallMassive { is_massive: true }`.
    pub fn force_stop_warp(&self, ball_id: BallId) -> Result<u64, String> {
        self.finish_warp_for_dock(ball_id, None)
    }

    /// Returns the last synced velocity `[vx, vy, vz]` for `ball_id`, reading `ballpark` when lock
    /// rank permits or falling back to [`ball_velocity_cache_map`].
    pub fn get_ball_velocity_sync(&self, ball_id: BallId) -> Option<[f64; 3]> {
        if !eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
        {
            if let Ok(bp) = self.ballpark.try_read() {
                return bp
                    .balls
                    .get(&ball_id)
                    .map(|b| [b.velocity.x, b.velocity.y, b.velocity.z]);
            }
        }
        let cache_key =
            Arc::as_ptr(&self.ball_positions) as usize;
        ball_velocity_cache_map()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cache_key)
            .and_then(|m| m.get(&ball_id).copied())
    }

    /// Returns live `(position, velocity)` for `ball_id` only if the ball is currently present in
    /// this system's ballpark (`GH #1085`).
    pub fn get_live_ball_kinematics_sync(&self, ball_id: BallId) -> Option<(DVec3, DVec3)> {
        if !eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
        {
            let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
            return bp.balls.get(&ball_id).map(|b| (b.position, b.velocity));
        }
        let pos = self.get_ball_position_sync(ball_id)?;
        let vel = self
            .get_ball_velocity_sync(ball_id)
            .map(DVec3::from_array)
            .unwrap_or(DVec3::ZERO);
        Some((pos, vel))
    }
}

#[cfg(test)]
#[path = "celestials_helpers_cov_unit_tests.rs"]
mod celestials_helpers_cov_unit_tests;
#[cfg(test)]
#[path = "celestials_helpers_edges_tests.rs"]
mod celestials_helpers_edges_tests;
