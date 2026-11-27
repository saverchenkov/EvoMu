//! Canonical test-only façade over ephemeral combat, container-lifetime, self-destruct,
//! and GM soak/deadwarp runtime state (`#1541` Track E2a).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) access per-entity ephemeral state via value-based helpers
//! without touching `CombatRuntimeState` directly, allowing Track E2b to re-point this single
//! module to `SystemActor` / `SystemRegistry`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, AssistLogEntry, ContainerLifetimeEntry,
    SelfDestructCountdown,
};
use super::{GmDeadwarpState, GmLogiSoakState};

/// Replaces the recorded aggression assist log entries for `ship_id`.
pub fn seed_assist_log(ctx: &ServerContext, ship_id: i64, entries: Vec<AssistLogEntry>) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.aggression_assist_log.insert(ship_id, entries);
    });
}

/// Appends a single aggression assist log entry for `ship_id`.
pub fn push_assist_log(ctx: &ServerContext, ship_id: i64, entry: AssistLogEntry) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.aggression_assist_log
            .entry(ship_id)
            .or_default()
            .push(entry);
    });
}

/// Returns a cloned snapshot of aggression assist log entries recorded for `ship_id`.
pub fn assist_log_for_ship(ctx: &ServerContext, ship_id: i64) -> Vec<AssistLogEntry> {
    with_combat_runtime(ctx, |rt| {
        rt.aggression_assist_log
            .get(&ship_id)
            .cloned()
            .unwrap_or_default()
    })
}

/// Inserts or replaces the container lifetime tracking record for `item_id`.
pub fn seed_container_lifetime(
    ctx: &ServerContext,
    item_id: i64,
    entry: ContainerLifetimeEntry,
) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.insert(item_id, entry);
    });
}

/// Returns a cloned snapshot of the container lifetime record for `item_id`, if present.
pub fn get_container_lifetime(
    ctx: &ServerContext,
    item_id: i64,
) -> Option<ContainerLifetimeEntry> {
    with_combat_runtime(ctx, |rt| rt.container_lifetimes.get(&item_id).cloned())
}

/// Returns `true` if a container lifetime record is currently tracked for `item_id`.
pub fn contains_container_lifetime(ctx: &ServerContext, item_id: i64) -> bool {
    with_combat_runtime(ctx, |rt| rt.container_lifetimes.contains_key(&item_id))
}

/// Removes and returns the container lifetime record for `item_id`, if present.
pub fn remove_container_lifetime(
    ctx: &ServerContext,
    item_id: i64,
) -> Option<ContainerLifetimeEntry> {
    with_mut_combat_runtime(ctx, |rt| rt.container_lifetimes.remove(&item_id))
}

/// Inserts or replaces the active self-destruct countdown for `ship_id`.
pub fn seed_self_destruct(ctx: &ServerContext, ship_id: i64, cd: SelfDestructCountdown) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns.insert(ship_id, cd);
    });
}

/// Returns a cloned snapshot of the active self-destruct countdown for `ship_id`, if present.
pub fn get_self_destruct(ctx: &ServerContext, ship_id: i64) -> Option<SelfDestructCountdown> {
    with_combat_runtime(ctx, |rt| rt.self_destruct_countdowns.get(&ship_id).cloned())
}

/// Mutates the active self-destruct countdown for `ship_id` in place if present,
/// returning `true` when an entry existed and `f` ran.
pub fn mutate_self_destruct(
    ctx: &ServerContext,
    ship_id: i64,
    f: impl FnOnce(&mut SelfDestructCountdown),
) -> bool {
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(cd) = rt.self_destruct_countdowns.get_mut(&ship_id) {
            f(cd);
            true
        } else {
            false
        }
    })
}

/// Returns `true` if `ship_id` currently has an active self-destruct countdown.
pub fn contains_self_destruct(ctx: &ServerContext, ship_id: i64) -> bool {
    with_combat_runtime(ctx, |rt| rt.self_destruct_countdowns.contains_key(&ship_id))
}

/// Returns the total number of active self-destruct countdowns currently tracked.
pub fn self_destruct_count(ctx: &ServerContext) -> usize {
    with_combat_runtime(ctx, |rt| rt.self_destruct_countdowns.len())
}

/// Inserts or replaces the `/soak logi` state for `sys_id`.
pub fn seed_gm_logi_soak(ctx: &ServerContext, sys_id: u32, state: GmLogiSoakState) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.gm_logi_soaks.insert(sys_id, state);
    });
}

/// Returns a cloned snapshot of the `/soak logi` state for `sys_id`, if present.
pub fn get_gm_logi_soak(ctx: &ServerContext, sys_id: u32) -> Option<GmLogiSoakState> {
    with_combat_runtime(ctx, |rt| rt.gm_logi_soaks.get(&sys_id).cloned())
}

/// Inserts or replaces the `/deadwarp` state for `ship_id`.
pub fn seed_gm_deadwarp(ctx: &ServerContext, ship_id: i64, state: GmDeadwarpState) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.gm_deadwarps.insert(ship_id, state);
    });
}

/// Returns a cloned snapshot of the `/deadwarp` state for `ship_id`, if present.
pub fn get_gm_deadwarp(ctx: &ServerContext, ship_id: i64) -> Option<GmDeadwarpState> {
    with_combat_runtime(ctx, |rt| rt.gm_deadwarps.get(&ship_id).cloned())
}
