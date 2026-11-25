//! Test-only façade over runtime active space drones (`#1541` Track D2a).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) seed, mutate, and inspect active space drones without reaching
//! directly into `CombatRuntimeState::active_drones_in_space`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_ephemeral, with_mut_combat_ephemeral, DroneSpaceState,
};
#[cfg(test)]
use crate::services::combat_crimewatch::CombatRuntimeState;

/// Inserts or replaces an active space [`DroneSpaceState`] keyed by `drone.ball_id`.
pub fn seed_active_drone(ctx: &ServerContext, drone: DroneSpaceState) {
    with_mut_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space.insert(drone.ball_id, drone);
    });
}

/// Inserts or replaces a batch of [`DroneSpaceState`] entries keyed by each entry's `ball_id`.
pub fn seed_active_drones(
    ctx: &ServerContext,
    drones: impl IntoIterator<Item = DroneSpaceState>,
) {
    with_mut_combat_ephemeral(ctx, |rt| {
        for drone in drones {
            rt.active_drones_in_space.insert(drone.ball_id, drone);
        }
    });
}

/// Returns a cloned [`DroneSpaceState`] for `ball_id`, if present.
pub fn active_drone(ctx: &ServerContext, ball_id: i64) -> Option<DroneSpaceState> {
    with_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.get(&ball_id).cloned())
}

/// Returns `true` if `ball_id` is currently registered as an active drone in space.
pub fn has_active_drone(ctx: &ServerContext, ball_id: i64) -> bool {
    with_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.contains_key(&ball_id))
}

/// Mutates the [`DroneSpaceState`] for `ball_id` in place if present, returning `f`'s output.
pub fn update_active_drone<R>(
    ctx: &ServerContext,
    ball_id: i64,
    f: impl FnOnce(&mut DroneSpaceState) -> R,
) -> Option<R> {
    with_mut_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.get_mut(&ball_id).map(f))
}

/// Mutates every active [`DroneSpaceState`] currently in space.
pub fn update_active_drones(ctx: &ServerContext, mut f: impl FnMut(&mut DroneSpaceState)) {
    with_mut_combat_ephemeral(ctx, |rt| {
        for drone in rt.active_drones_in_space.values_mut() {
            f(drone);
        }
    });
}

/// Removes and returns the [`DroneSpaceState`] for `ball_id`, if present.
pub fn remove_active_drone(ctx: &ServerContext, ball_id: i64) -> Option<DroneSpaceState> {
    with_mut_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.remove(&ball_id))
}

/// Clears all tracked active drones across all systems.
pub fn clear_active_drones(ctx: &ServerContext) {
    with_mut_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space.clear();
    });
}

/// Returns the total number of tracked active drones across all systems.
pub fn active_drone_count(ctx: &ServerContext) -> usize {
    with_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.len())
}

/// Returns the ball IDs of all tracked active drones across all systems.
pub fn active_drone_ids(ctx: &ServerContext) -> Vec<i64> {
    with_combat_ephemeral(ctx, |rt| rt.active_drones_in_space.keys().copied().collect())
}

/// Finds an active space drone whose backing inventory `drone_item_id` matches.
pub fn find_active_drone_by_item(
    ctx: &ServerContext,
    drone_item_id: i64,
) -> Option<DroneSpaceState> {
    with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.drone_item_id == drone_item_id)
            .cloned()
    })
}

/// Returns cloned [`DroneSpaceState`] records owned by `owner_char_id`.
pub fn active_drones_for_owner(
    ctx: &ServerContext,
    owner_char_id: i64,
) -> Vec<DroneSpaceState> {
    with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| d.owner_char_id == owner_char_id)
            .cloned()
            .collect()
    })
}

/// Returns cloned [`DroneSpaceState`] records attached to `parent_ship_id`.
pub fn active_drones_for_ship(
    ctx: &ServerContext,
    parent_ship_id: i64,
) -> Vec<DroneSpaceState> {
    with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| d.parent_ship_id == parent_ship_id)
            .cloned()
            .collect()
    })
}

/// Returns cloned [`DroneSpaceState`] records matching `pred`.
pub fn active_drones_matching(
    ctx: &ServerContext,
    mut pred: impl FnMut(&DroneSpaceState) -> bool,
) -> Vec<DroneSpaceState> {
    with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| pred(d))
            .cloned()
            .collect()
    })
}

/// Unit-test-only helper for serialization coverage of `CombatRuntimeState::to_persisted`.
#[cfg(test)]
pub(crate) fn seed_active_drone_in_runtime(rt: &mut CombatRuntimeState, drone: DroneSpaceState) {
    rt.active_drones_in_space.insert(drone.ball_id, drone);
}

/// Unit-test-only helper for clearing `active_drones_in_space` on a standalone `CombatRuntimeState`.
#[cfg(test)]
pub(crate) fn clear_active_drones_in_runtime(rt: &mut CombatRuntimeState) {
    rt.active_drones_in_space.clear();
}
