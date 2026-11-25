//! Canonical test-only façade over fighter runtime state (`#1541` Track F1/F1b).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) access fighter runtime state without reaching directly into
//! `ServerContext` locks or `CombatRuntimeState`.

#![forbid(unsafe_code)]

use crate::router::context::{ActiveFighterSquadron, ServerContext};
use crate::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};

/// Inserts or replaces an active fighter squadron entry by key.
pub fn insert_active_fighter(
    ctx: &ServerContext,
    fighter_id: i64,
    squadron: ActiveFighterSquadron,
) {
    ctx.active_fighters.write().unwrap().insert(fighter_id, squadron);
}

/// Returns a cloned snapshot of the active fighter squadron for `fighter_id`.
pub fn active_fighter(ctx: &ServerContext, fighter_id: i64) -> Option<ActiveFighterSquadron> {
    ctx.active_fighters.read().unwrap().get(&fighter_id).cloned()
}

/// Returns `true` if `fighter_id` is currently registered as an active fighter squadron.
pub fn has_active_fighter(ctx: &ServerContext, fighter_id: i64) -> bool {
    ctx.active_fighters.read().unwrap().contains_key(&fighter_id)
}

/// Mutates an existing active fighter squadron entry in place if present.
pub fn update_active_fighter<R>(
    ctx: &ServerContext,
    fighter_id: i64,
    f: impl FnOnce(&mut ActiveFighterSquadron) -> R,
) -> Option<R> {
    ctx.active_fighters.write().unwrap().get_mut(&fighter_id).map(f)
}

/// Removes and returns an active fighter squadron entry by key.
pub fn remove_active_fighter(ctx: &ServerContext, fighter_id: i64) -> Option<ActiveFighterSquadron> {
    ctx.active_fighters.write().unwrap().remove(&fighter_id)
}

/// Finds an active fighter squadron whose backing inventory `item_id` matches.
pub fn find_active_fighter_by_item(
    ctx: &ServerContext,
    item_id: i64,
) -> Option<ActiveFighterSquadron> {
    ctx.active_fighters
        .read()
        .unwrap()
        .values()
        .find(|f| f.item_id == item_id)
        .cloned()
}

/// Finds an active fighter squadron launched from `parent_ship_id` and `tube_index`.
pub fn find_active_fighter_by_tube(
    ctx: &ServerContext,
    parent_ship_id: i64,
    tube_index: usize,
) -> Option<ActiveFighterSquadron> {
    ctx.active_fighters
        .read()
        .unwrap()
        .values()
        .find(|f| f.parent_ship_id == parent_ship_id && f.tube_index == tube_index)
        .cloned()
}

/// Returns all currently active fighter squadron keys.
pub fn active_fighter_ids(ctx: &ServerContext) -> Vec<i64> {
    ctx.active_fighters.read().unwrap().keys().copied().collect()
}

/// Returns `true` if every registered active fighter squadron satisfies `pred`.
pub fn all_active_fighter_squadrons(
    ctx: &ServerContext,
    mut pred: impl FnMut(&ActiveFighterSquadron) -> bool,
) -> bool {
    ctx.active_fighters.read().unwrap().values().all(|f| pred(f))
}

/// Returns `true` if any registered active fighter squadron satisfies `pred`.
pub fn any_active_fighter_squadron(
    ctx: &ServerContext,
    mut pred: impl FnMut(&ActiveFighterSquadron) -> bool,
) -> bool {
    ctx.active_fighters.read().unwrap().values().any(|f| pred(f))
}

/// Returns the number of currently active fighter squadrons.
pub fn active_fighter_count(ctx: &ServerContext) -> usize {
    ctx.active_fighters.read().unwrap().len()
}

/// Clears all active fighter squadrons.
pub fn clear_active_fighters(ctx: &ServerContext) {
    ctx.active_fighters.write().unwrap().clear();
}

/// Clears all active fighter squadrons (alias for integration test hygiene).
pub fn clear_all_fighter_squadrons(ctx: &ServerContext) {
    ctx.active_fighters.write().unwrap().clear();
}

/// Returns the fighter item ID currently loaded in `(ship_id, tube_index)`, if any.
pub fn loaded_fighter_in_tube(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<i64> {
    ctx.fighter_tube_loads.read().unwrap().get(&(ship_id, tube_index)).copied()
}

/// Returns `true` if `(ship_id, tube_index)` currently has a loaded fighter squadron.
pub fn has_fighter_tube_load(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> bool {
    ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, tube_index))
}

/// Records `fighter_item_id` as loaded in `(ship_id, tube_index)`.
pub fn set_fighter_tube_load(
    ctx: &ServerContext,
    ship_id: i64,
    tube_index: usize,
    fighter_item_id: i64,
) {
    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((ship_id, tube_index), fighter_item_id);
}

/// Mutates an existing loaded fighter item ID entry for `(ship_id, tube_index)` if present.
pub fn update_fighter_tube_load<R>(
    ctx: &ServerContext,
    ship_id: i64,
    tube_index: usize,
    f: impl FnOnce(&mut i64) -> R,
) -> Option<R> {
    ctx.fighter_tube_loads.write().unwrap().get_mut(&(ship_id, tube_index)).map(f)
}

/// Removes any loaded fighter item entry for `(ship_id, tube_index)`.
pub fn remove_fighter_tube_load(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<i64> {
    ctx.fighter_tube_loads.write().unwrap().remove(&(ship_id, tube_index))
}

/// Returns all `(ship_id, tube_index)` keys currently present in tube loads.
pub fn fighter_tube_load_keys(ctx: &ServerContext) -> Vec<(i64, usize)> {
    ctx.fighter_tube_loads.read().unwrap().keys().copied().collect()
}

/// Returns all `((ship_id, tube_index), fighter_item_id)` entries currently present in tube loads.
pub fn fighter_tube_load_entries(ctx: &ServerContext) -> Vec<((i64, usize), i64)> {
    ctx.fighter_tube_loads
        .read()
        .unwrap()
        .iter()
        .map(|(&k, &v)| (k, v))
        .collect()
}

/// Clears all fighter tube load entries.
pub fn clear_all_fighter_tubes(ctx: &ServerContext) {
    ctx.fighter_tube_loads.write().unwrap().clear();
}

/// Returns the Unix timestamp until which `(fighter_id, slot_id)` is on cooldown.
pub fn fighter_ability_cooldown(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u64> {
    ctx.fighter_cooldown_state.read().unwrap().get(&(fighter_id, slot_id)).copied()
}

/// Sets the Unix cooldown expiration timestamp for `(fighter_id, slot_id)`.
pub fn set_fighter_ability_cooldown(
    ctx: &ServerContext,
    fighter_id: i64,
    slot_id: u32,
    ready_at_unix: u64,
) {
    ctx.fighter_cooldown_state
        .write()
        .unwrap()
        .insert((fighter_id, slot_id), ready_at_unix);
}

/// Clears the cooldown entry for `(fighter_id, slot_id)`.
pub fn clear_fighter_ability_cooldown(
    ctx: &ServerContext,
    fighter_id: i64,
    slot_id: u32,
) -> Option<u64> {
    ctx.fighter_cooldown_state.write().unwrap().remove(&(fighter_id, slot_id))
}

/// Returns `true` if no fighter ability cooldown entries are active.
pub fn fighter_cooldowns_is_empty(ctx: &ServerContext) -> bool {
    ctx.fighter_cooldown_state.read().unwrap().is_empty()
}

/// Returns all `((fighter_id, slot_id), ready_at_unix)` cooldown entries.
pub fn fighter_cooldown_entries(ctx: &ServerContext) -> Vec<((i64, u32), u64)> {
    ctx.fighter_cooldown_state
        .read()
        .unwrap()
        .iter()
        .map(|(&k, &v)| (k, v))
        .collect()
}

/// Clears all fighter ability cooldown entries.
pub fn clear_all_fighter_cooldowns(ctx: &ServerContext) {
    ctx.fighter_cooldown_state.write().unwrap().clear();
}

/// Returns the number of consumed ability charges for `(fighter_id, slot_id)`.
pub fn fighter_ability_charges_used(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u32> {
    ctx.fighter_charge_state.read().unwrap().get(&(fighter_id, slot_id)).copied()
}

/// Sets the number of consumed ability charges for `(fighter_id, slot_id)`.
pub fn set_fighter_ability_charges_used(
    ctx: &ServerContext,
    fighter_id: i64,
    slot_id: u32,
    used: u32,
) {
    ctx.fighter_charge_state.write().unwrap().insert((fighter_id, slot_id), used);
}

/// Clears the consumed ability charge entry for `(fighter_id, slot_id)`.
pub fn clear_fighter_ability_charges(
    ctx: &ServerContext,
    fighter_id: i64,
    slot_id: u32,
) -> Option<u32> {
    ctx.fighter_charge_state.write().unwrap().remove(&(fighter_id, slot_id))
}

/// Returns `true` if no fighter ability charge usage entries are recorded.
pub fn fighter_charges_is_empty(ctx: &ServerContext) -> bool {
    ctx.fighter_charge_state.read().unwrap().is_empty()
}

/// Returns all `((fighter_id, slot_id), used)` charge usage entries.
pub fn fighter_charge_entries(ctx: &ServerContext) -> Vec<((i64, u32), u32)> {
    ctx.fighter_charge_state
        .read()
        .unwrap()
        .iter()
        .map(|(&k, &v)| (k, v))
        .collect()
}

/// Clears all fighter ability charge usage entries.
pub fn clear_all_fighter_charges(ctx: &ServerContext) {
    ctx.fighter_charge_state.write().unwrap().clear();
}

/// Returns the accumulated unit damage for `fighter_ball_id`, if any.
pub fn fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> Option<f64> {
    with_combat_ephemeral(ctx, |rt| rt.fighter_unit_damage_accum.get(&fighter_ball_id).copied())
}

/// Returns `true` if `fighter_ball_id` has an entry in the unit damage accumulator.
pub fn has_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> bool {
    with_combat_ephemeral(ctx, |rt| rt.fighter_unit_damage_accum.contains_key(&fighter_ball_id))
}

/// Sets the accumulated unit damage for `fighter_ball_id`.
pub fn set_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64, damage: f64) {
    with_mut_combat_ephemeral(ctx, |rt| {
        rt.fighter_unit_damage_accum.insert(fighter_ball_id, damage);
    });
}

/// Removes and returns the accumulated unit damage entry for `fighter_ball_id`.
pub fn remove_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> Option<f64> {
    with_mut_combat_ephemeral(ctx, |rt| rt.fighter_unit_damage_accum.remove(&fighter_ball_id))
}

/// Clears all fighter unit damage accumulator entries.
pub fn clear_all_fighter_damage_accum(ctx: &ServerContext) {
    with_mut_combat_ephemeral(ctx, |rt| rt.fighter_unit_damage_accum.clear());
}

/// Returns the Unix timestamp until which `(ship_id, tube_index)` is rearming/refueling, if any.
pub fn fighter_tube_rearm_unix(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<u64> {
    with_combat_ephemeral(ctx, |rt| {
        rt.fighter_tube_rearm_until.get(&(ship_id, tube_index)).copied()
    })
}

/// Sets the Unix rearm/refuel expiration timestamp for `(ship_id, tube_index)`.
pub fn set_fighter_tube_rearm_unix(
    ctx: &ServerContext,
    ship_id: i64,
    tube_index: usize,
    ready_at_unix: u64,
) {
    with_mut_combat_ephemeral(ctx, |rt| {
        rt.fighter_tube_rearm_until.insert((ship_id, tube_index), ready_at_unix);
    });
}

/// Removes and returns the rearm/refuel expiration timestamp for `(ship_id, tube_index)`.
pub fn clear_fighter_tube_rearm_unix(
    ctx: &ServerContext,
    ship_id: i64,
    tube_index: usize,
) -> Option<u64> {
    with_mut_combat_ephemeral(ctx, |rt| rt.fighter_tube_rearm_until.remove(&(ship_id, tube_index)))
}

/// Clears all fighter tube rearm/refuel timers.
pub fn clear_all_fighter_tube_rearm_timers(ctx: &ServerContext) {
    with_mut_combat_ephemeral(ctx, |rt| rt.fighter_tube_rearm_until.clear());
}
