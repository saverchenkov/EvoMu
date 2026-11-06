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
            crate::system_actor::sync_ball_positions(&bp, &pos_arc, &scram_arc);
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
