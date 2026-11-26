//! Test-only façade over runtime active space drones owned by [`SystemActor`] (`#1541` Track D2b).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) seed, mutate, and inspect active space drones without reaching
//! directly into actor internals.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::system_actor::DroneSpaceState;
use std::collections::HashMap;

/// Inserts or replaces an active space [`DroneSpaceState`] keyed by `drone.ball_id`.
pub fn seed_active_drone(ctx: &ServerContext, drone: DroneSpaceState) {
    let sys_handle = ctx.systems.wake_for_drone_seed(drone.solar_system_id);
    sys_handle
        .upsert_active_drone_sync(drone)
        .expect("seed_active_drone");
}

/// Inserts or replaces a batch of [`DroneSpaceState`] entries grouped by each entry's `solar_system_id`.
pub fn seed_active_drones(
    ctx: &ServerContext,
    drones: impl IntoIterator<Item = DroneSpaceState>,
) {
    let mut by_sys: HashMap<u32, Vec<DroneSpaceState>> = HashMap::new();
    for drone in drones {
        by_sys.entry(drone.solar_system_id).or_default().push(drone);
    }
    for (sys_id, batch) in by_sys {
        let sys_handle = ctx.systems.wake_for_drone_seed(sys_id);
        sys_handle
            .upsert_active_drones_sync(batch, false)
            .expect("seed_active_drones");
    }
}

/// Returns a cloned [`DroneSpaceState`] for `ball_id`, if present.
pub fn active_drone(ctx: &ServerContext, ball_id: i64) -> Option<DroneSpaceState> {
    ctx.systems.find_active_drone(ball_id).map(|(_, d)| d)
}

/// Returns `true` if `ball_id` is currently registered as an active drone in space.
pub fn has_active_drone(ctx: &ServerContext, ball_id: i64) -> bool {
    ctx.systems.find_active_drone(ball_id).is_some()
}

/// Mutates the [`DroneSpaceState`] for `ball_id` in place if present, returning `f`'s output.
pub fn update_active_drone<R>(
    ctx: &ServerContext,
    ball_id: i64,
    f: impl FnOnce(&mut DroneSpaceState) -> R,
) -> Option<R> {
    let (handle, mut drone) = ctx.systems.find_active_drone(ball_id)?;
    let out = f(&mut drone);
    handle
        .upsert_active_drone_sync(drone)
        .expect("update_active_drone");
    Some(out)
}

/// Mutates every active [`DroneSpaceState`] currently in space.
pub fn update_active_drones(ctx: &ServerContext, mut f: impl FnMut(&mut DroneSpaceState)) {
    let mut by_sys: HashMap<u32, (crate::system_actor::SystemHandle, Vec<DroneSpaceState>)> =
        HashMap::new();
    for (handle, mut drone) in ctx.systems.all_active_drones() {
        f(&mut drone);
        by_sys
            .entry(handle.solar_system_id)
            .or_insert_with(|| (handle, Vec::new()))
            .1
            .push(drone);
    }
    for (_, (handle, batch)) in by_sys {
        handle
            .upsert_active_drones_sync(batch, false)
            .expect("update_active_drones");
    }
}

/// Removes and returns the [`DroneSpaceState`] for `ball_id`, if present.
pub fn remove_active_drone(ctx: &ServerContext, ball_id: i64) -> Option<DroneSpaceState> {
    ctx.systems.remove_active_drone_across_systems_sync(ball_id)
}

/// Clears all tracked active drones across all systems.
pub fn clear_active_drones(ctx: &ServerContext) {
    ctx.systems.clear_all_active_drones_sync();
}

/// Returns the total number of tracked active drones across all systems.
pub fn active_drone_count(ctx: &ServerContext) -> usize {
    ctx.systems.active_drones_count()
}

/// Returns the ball IDs of all tracked active drones across all systems.
pub fn active_drone_ids(ctx: &ServerContext) -> Vec<i64> {
    ctx.systems
        .all_active_drones()
        .into_iter()
        .map(|(_, d)| d.ball_id)
        .collect()
}

/// Finds an active space drone whose backing inventory `drone_item_id` matches.
pub fn find_active_drone_by_item(
    ctx: &ServerContext,
    drone_item_id: i64,
) -> Option<DroneSpaceState> {
    ctx.systems
        .find_active_drone_by_item(drone_item_id)
        .map(|(_, d)| d)
}

/// Returns cloned [`DroneSpaceState`] records owned by `owner_char_id`.
pub fn active_drones_for_owner(
    ctx: &ServerContext,
    owner_char_id: i64,
) -> Vec<DroneSpaceState> {
    ctx.systems
        .all_active_drones()
        .into_iter()
        .filter_map(|(_, d)| (d.owner_char_id == owner_char_id).then_some(d))
        .collect()
}

/// Returns cloned [`DroneSpaceState`] records attached to `parent_ship_id`.
pub fn active_drones_for_ship(
    ctx: &ServerContext,
    parent_ship_id: i64,
) -> Vec<DroneSpaceState> {
    ctx.systems
        .all_active_drones()
        .into_iter()
        .filter_map(|(_, d)| (d.parent_ship_id == parent_ship_id).then_some(d))
        .collect()
}

/// Returns cloned [`DroneSpaceState`] records matching `pred`.
pub fn active_drones_matching(
    ctx: &ServerContext,
    mut pred: impl FnMut(&DroneSpaceState) -> bool,
) -> Vec<DroneSpaceState> {
    ctx.systems
        .all_active_drones()
        .into_iter()
        .filter_map(|(_, d)| pred(&d).then_some(d))
        .collect()
}
