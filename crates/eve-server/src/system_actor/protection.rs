//! Unified actor-owned cloak/invulnerability protection registry and ball lifecycle handlers (`#1486`).

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{BallId, Ballpark, CombatProtectionSnapshot};
use std::sync::Arc;

/// Returns `true` when `cycle` represents an active cloaking device module cycle.
#[inline]
pub(crate) fn is_cloak_cycle(cycle: &ActiveModuleCycle) -> bool {
    cycle.effect_name.to_ascii_lowercase().starts_with("cloaking")
}

#[inline]
fn is_covert_cloak_cycle(
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    cycle: &ActiveModuleCycle,
) -> bool {
    cycle.effect_name.eq_ignore_ascii_case("cloakingWarpSafe")
        || is_sde_covert_cloak_type(sde_opt, cycle.module_type_id)
            .is_some_and(|(is_cloak, is_covert)| is_cloak && is_covert)
}

pub(in crate::system_actor) fn sync_ball_protection_under_bp(
    handle: &SystemHandle,
    bp: &Ballpark,
    ball_id: BallId,
) {
    let mut st = handle.actor_state.write().unwrap_or_else(|e| e.into_inner());
    sync_ball_protection_in_actor(bp, &mut st, ball_id);
}

/// Synchronizes `actor_st.protection_registry[ball_id]` from `ballpark.combat_states`,
/// `ballpark.ball_slim_meta`, and `ledger.by_char` on the actor sim clock.
fn sync_ball_protection_with_ledger(
    ballpark: &Ballpark,
    actor_st: &mut SystemActorState,
    ledger: &LocationInvulnLedger,
    ball_id: BallId,
) {
    let Some(cs) = ballpark.combat_states.get(&ball_id) else {
        actor_st.protection_registry.remove(&ball_id);
        return;
    };
    let owner_char_id = ballpark
        .ball_slim_meta
        .get(&ball_id)
        .and_then(|meta| meta.char_id)
        .filter(|&cid| cid > 0);
    let sys_id = if actor_st.solar_system_id != 0 {
        actor_st.solar_system_id
    } else {
        ballpark.solar_system_id
    };
    let location_change_invuln_until_unix = owner_char_id
        .and_then(|cid| ledger.by_char.get(&cid).copied())
        .filter(|entry| entry.matches_system(sys_id))
        .map(|entry| entry.until_unix);
    let has_active_cloak_cycle = actor_st
        .active_module_cycles
        .iter()
        .any(|c| c.ship_id == ball_id && is_cloak_cycle(c));
    actor_st.protection_registry.insert(
        ball_id,
        ActorShipProtection {
            owner_char_id,
            snapshot: cs.protection_snapshot(),
            has_active_cloak_cycle,
            location_change_invuln_until_unix,
            updated_sim_elapsed_secs: actor_st.sim_elapsed_secs,
            updated_sim_unix_secs: ballpark.sim_unix_secs,
        },
    );
}

/// Synchronizes `actor_st.protection_registry[ball_id]` from `ballpark.combat_states`,
/// `ballpark.ball_slim_meta`, and `actor_st.location_invuln_ledger` on the actor sim clock.
pub(crate) fn sync_ball_protection_in_actor(
    ballpark: &Ballpark,
    actor_st: &mut SystemActorState,
    ball_id: BallId,
) {
    let ledger_arc = Arc::clone(&actor_st.location_invuln_ledger);
    let ledger = ledger_arc.lock().unwrap_or_else(|e| e.into_inner());
    sync_ball_protection_with_ledger(ballpark, actor_st, &ledger, ball_id);
}

/// Refreshes all active `protection_registry` entries against `ballpark.combat_states` and the
/// actor's current monotonic `sim_elapsed_secs` / `sim_unix_secs`.
pub(crate) fn sync_all_protection_registry_in_actor(
    ballpark: &Ballpark,
    actor_st: &mut SystemActorState,
) {
    actor_st
        .protection_registry
        .retain(|ball_id, _| ballpark.combat_states.contains_key(ball_id));
    let ball_ids: Vec<BallId> = ballpark.combat_states.keys().copied().collect();
    let ledger_arc = Arc::clone(&actor_st.location_invuln_ledger);
    let ledger = ledger_arc.lock().unwrap_or_else(|e| e.into_inner());
    for ball_id in ball_ids {
        sync_ball_protection_with_ledger(ballpark, actor_st, &ledger, ball_id);
    }
}

/// Canonical actor check for whether `ship_id` has an active non-gate module cloak (`#1486`).
pub(crate) fn is_ship_actively_cloaked_in_actor(
    ballpark: &Ballpark,
    actor_st: &SystemActorState,
    ship_id: BallId,
) -> bool {
    let has_cloak_cycle = actor_st
        .active_module_cycles
        .iter()
        .any(|c| c.ship_id == ship_id && is_cloak_cycle(c));
    if let Some(cs) = ballpark.combat_states.get(&ship_id) {
        return cs.protection_snapshot().has_active_non_gate_cloak_or_vel() || has_cloak_cycle;
    }
    has_cloak_cycle
        || actor_st
            .protection_registry
            .get(&ship_id)
            .is_some_and(ActorShipProtection::has_active_non_gate_cloak)
}

fn resolve_owner_char_id(
    ballpark: &Ballpark,
    ball_id: BallId,
    explicit_char_id: Option<CharacterId>,
) -> Option<CharacterId> {
    explicit_char_id.filter(|&cid| cid > 0).or_else(|| {
        ballpark
            .ball_slim_meta
            .get(&ball_id)
            .and_then(|meta| meta.char_id)
            .filter(|&cid| cid > 0)
    })
}

fn clear_protection_registry_location_invuln(
    actor_st: &mut SystemActorState,
    cid: CharacterId,
) {
    for entry in actor_st.protection_registry.values_mut() {
        if entry.owner_char_id == Some(cid) {
            entry.location_change_invuln_until_unix = None;
        }
    }
}

/// Atomically consumes an active `location_change_invuln` grant for `owner_char_id` inside the
/// shared ledger owned by `SystemRegistry` / `SystemActor` (`#1486`).
///
/// Returns `true` if a non-expired grant (`until > real_now_unix`) was cancelled on this call.
fn consume_location_change_invuln(
    actor_st: &mut SystemActorState,
    exec_sys_id: SolarSystemId,
    owner_char_id: Option<CharacterId>,
    max_invuln_seq: Option<u64>,
    real_now_unix: u64,
    clear_if_active: bool,
) -> bool {
    if !clear_if_active {
        return false;
    }
    let Some(cid) = owner_char_id else {
        return false;
    };
    let expected_sys = (exec_sys_id > 0).then_some(exec_sys_id);
    let (removed, cancelled_active) = {
        let mut ledger = actor_st
            .location_invuln_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ledger.pop_if_matches(cid, expected_sys, max_invuln_seq, real_now_unix)
    };
    if removed {
        clear_protection_registry_location_invuln(actor_st, cid);
    }
    cancelled_active
}

fn is_sde_covert_cloak_type(
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    type_id: u32,
) -> Option<(bool, bool)> {
    if let Some(sde) = sde_opt.as_ref() {
        let t = sde.types.get(&type_id)?;
        if t.group_id != 330 {
            return Some((false, false));
        }
        let is_covert = type_id == 11578
            || t.effect_ids.contains(&980)
            || t.effect_ids.iter().any(|eid| {
                sde.effects
                    .get(eid)
                    .is_some_and(|e| e.is_warp_safe || e.name.eq_ignore_ascii_case("cloakingWarpSafe"))
            })
            || t.name.contains("Covert Ops");
        Some((true, is_covert))
    } else {
        match type_id {
            11578 => Some((true, true)),
            11370 | 11577 => Some((true, false)),
            _ => None,
        }
    }
}

fn strip_cloak_cycles_for_ship(
    exec_ctx: &mut ActorExecCtx<'_>,
    ship_id: BallId,
    include_covert: bool,
) -> Vec<RemovedCloakCycleInfo> {
    let deact_guard = read_deact_hook(exec_ctx);
    let mut removed_cycles = Vec::new();
    let mut removed_ids = std::collections::HashSet::new();
    let mut idx = 0;
    while idx < exec_ctx.actor_st.active_module_cycles.len() {
        let is_target = {
            let cyc = &exec_ctx.actor_st.active_module_cycles[idx];
            cyc.ship_id == ship_id
                && is_cloak_cycle(cyc)
                && (include_covert || !is_covert_cloak_cycle(&exec_ctx.actor_st.sde_opt, cyc))
        };
        if is_target {
            let removed = exec_ctx.actor_st.active_module_cycles.remove(idx);
            let is_covert = is_covert_cloak_cycle(&exec_ctx.actor_st.sde_opt, &removed);
            exec_ctx
                .actor_st
                .weapon_cycle_start_modules
                .remove(&(ship_id, removed.module_id));
            cleanup_removed_module_cycle(
                exec_ctx.ballpark,
                &removed,
                &exec_ctx.actor_st.db_opt,
            );
            super::cycles::emit_cycle_deactivation(
                &removed,
                exec_ctx.ballpark,
                &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(),
            );
            removed_ids.insert(removed.module_id);
            removed_cycles.push(RemovedCloakCycleInfo {
                module_id: removed.module_id,
                module_type_id: removed.module_type_id,
                is_covert,
                effect_name: removed.effect_name,
            });
        } else {
            idx += 1;
        }
    }
    if let Some(db_ref) = exec_ctx.actor_st.db_opt.as_ref() {
        let orphaned_db_cloaks: Vec<(ItemId, u32, bool)> = db_ref.with_snapshot(|snap| {
            if snap.module_active_states.is_empty() {
                return Vec::new();
            }
            snap.items
                .values()
                .filter(|it| {
                    it.location_id == ship_id
                        && snap.module_active_states.contains(&it.item_id)
                        && !removed_ids.contains(&it.item_id)
                })
                .filter_map(|it| {
                    let (is_cloak, is_covert) =
                        is_sde_covert_cloak_type(&exec_ctx.actor_st.sde_opt, it.type_id)?;
                    (is_cloak && (include_covert || !is_covert)).then_some((
                        it.item_id,
                        it.type_id,
                        is_covert,
                    ))
                })
                .collect()
        });
        if !orphaned_db_cloaks.is_empty() {
            db_ref.with_mut_snapshot(|snap| {
                for &(mod_id, _, _) in &orphaned_db_cloaks {
                    snap.module_active_states.remove(&mod_id);
                    snap.module_active_charges.remove(&mod_id);
                }
            });
            let owner_char_id = exec_ctx
                .ballpark
                .ball_slim_meta
                .get(&ship_id)
                .and_then(|meta| meta.char_id);
            for (mod_id, mod_type_id, is_covert) in orphaned_db_cloaks {
                let effect_name = if is_covert {
                    "cloakingWarpSafe".to_string()
                } else {
                    "cloaking".to_string()
                };
                let synth_cycle = ActiveModuleCycle {
                    ship_id,
                    module_id: mod_id,
                    module_type_id: mod_type_id,
                    owner_char_id,
                    effect_name: effect_name.clone(),
                    ..ActiveModuleCycle::default()
                };
                super::cycles::emit_cycle_deactivation(
                    &synth_cycle,
                    exec_ctx.ballpark,
                    &exec_ctx.actor_st.db_opt,
                    deact_guard.as_ref(),
                );
                removed_cycles.push(RemovedCloakCycleInfo {
                    module_id: mod_id,
                    module_type_id: mod_type_id,
                    is_covert,
                    effect_name,
                });
            }
        }
    }
    if !removed_cycles.is_empty() {
        reconcile_tackle_and_bubbles(
            exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles,
            &exec_ctx.actor_st.base_scramble_strengths, &exec_ctx.actor_st.base_core_strengths,
            &exec_ctx.actor_st.db_opt, deact_guard.as_ref(),
        );
        sync_active_ships_cache(exec_ctx);
    }
    removed_cycles
}

fn clear_slim_cloak_meta(ballpark: &mut Ballpark, ball_id: BallId, deactivated_by_ballpark: bool) -> bool {
    if deactivated_by_ballpark { return true; }
    if let Some(mut meta) = ballpark.ball_slim_meta.get(&ball_id).cloned() {
        if meta.is_cloaked { meta.is_cloaked = false; return ballpark.change_slim_item(ball_id, meta); }
    }
    false
}

/// Atomically breaks cloak and/or invulnerability protection on `ball_id` according to `reason`,
/// returning unique structured transition events (`OnInvulnCancelled` at most once) (`#1486`).
pub(in crate::system_actor) fn break_protection_in_actor(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    char_id: Option<CharacterId>,
    reason: ProtectionBreakReason,
    real_now_unix: u64,
    pre_existing_snapshot: Option<CombatProtectionSnapshot>,
    max_invuln_seq: Option<u64>,
) -> ProtectionBreakEvents {
    break_protection_in_actor_impl(
        exec_ctx,
        ball_id,
        char_id,
        reason,
        real_now_unix,
        pre_existing_snapshot,
        false,
        max_invuln_seq,
    )
}

fn break_protection_in_actor_impl(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    char_id: Option<CharacterId>,
    reason: ProtectionBreakReason,
    real_now_unix: u64,
    pre_existing_snapshot: Option<CombatProtectionSnapshot>,
    force_all_cloaks: bool,
    max_invuln_seq: Option<u64>,
) -> ProtectionBreakEvents {
    if !exec_ctx.ballpark.balls.contains_key(&ball_id) {
        return ProtectionBreakEvents {
            sim_elapsed_secs: exec_ctx.actor_st.sim_elapsed_secs,
            ..Default::default()
        };
    }
    if let Some(explicit_cid) = char_id.filter(|&cid| cid > 0) {
        if let Some(ball_cid) = exec_ctx
            .ballpark
            .ball_slim_meta
            .get(&ball_id)
            .and_then(|meta| meta.char_id)
            .filter(|&cid| cid > 0)
        {
            if ball_cid != explicit_cid {
                return ProtectionBreakEvents {
                    sim_elapsed_secs: exec_ctx.actor_st.sim_elapsed_secs,
                    ..Default::default()
                };
            }
        }
    }
    let owner_char_id = resolve_owner_char_id(exec_ctx.ballpark, ball_id, char_id);
    let exec_sys_id = if exec_ctx.solar_system_id != 0 {
        exec_ctx.solar_system_id
    } else {
        exec_ctx.ballpark.solar_system_id
    };
    let location_invuln_cancelled = consume_location_change_invuln(
        exec_ctx.actor_st,
        exec_sys_id,
        owner_char_id,
        max_invuln_seq,
        real_now_unix,
        reason.clears_invuln(),
    );

    let mut events = ProtectionBreakEvents {
        sim_elapsed_secs: exec_ctx.actor_st.sim_elapsed_secs,
        ..Default::default()
    };

    // Inspect raw `CombatState` fields BEFORE calling `sync_action_breaks()` so that an
    // intermediate step (e.g. `add_module_cycle` zeroing `undock_invulnerable_remaining_ticks`)
    // never hides active `gate_cloak_remaining_ticks` from `ballpark.deactivate_cloak` (`#1486`).
    let (
        raw_gate_cloak,
        raw_non_gate_cloak,
        raw_covert_cloak,
        raw_non_covert_cloak,
        raw_undock_invuln,
        raw_warp_exit_invuln,
    ) = if let Some(cs) = exec_ctx.ballpark.combat_states.get(&ball_id) {
        let raw_gate = cs.gate_cloak_remaining_ticks > 0;
        let raw_non_gate = !raw_gate && (cs.is_cloaked || cs.cloak_velocity_multiplier.is_some());
        (
            raw_gate,
            raw_non_gate,
            raw_non_gate && cs.is_covert_cloak,
            raw_non_gate && !cs.is_covert_cloak,
            cs.undock_invulnerable_remaining_ticks > 0,
            cs.warp_exit_invulnerable_remaining_ticks > 0,
        )
    } else {
        (false, false, false, false, false, false)
    };
    let pre_snap = pre_existing_snapshot.unwrap_or_default();
    let had_gate_cloak = raw_gate_cloak || pre_snap.has_gate_cloak();
    let had_non_gate_cloak = raw_non_gate_cloak || pre_snap.has_active_non_gate_cloak();
    let had_covert_cloak = raw_covert_cloak || pre_snap.has_active_covert_cloak();
    let had_non_covert_cloak = raw_non_covert_cloak || pre_snap.has_active_non_covert_cloak();
    let had_undock_invuln = raw_undock_invuln || pre_snap.undock_invulnerable_remaining_ticks > 0;
    let had_warp_exit_invuln = raw_warp_exit_invuln || pre_snap.has_warp_exit_invuln();

    let strip_all_cloaks = force_all_cloaks || reason.breaks_covert_cloak();
    let should_deactivate_gate_cloak = had_gate_cloak;
    let should_deactivate_non_covert_cloak =
        had_non_covert_cloak && (strip_all_cloaks || reason.breaks_non_covert_module_cloak());
    let should_deactivate_covert_cloak = had_covert_cloak && strip_all_cloaks;

    if should_deactivate_gate_cloak
        || should_deactivate_non_covert_cloak
        || should_deactivate_covert_cloak
    {
        let deactivated_by_ballpark = exec_ctx.ballpark.deactivate_cloak(ball_id);
        if should_deactivate_gate_cloak {
            events.gate_cloak_broken = true;
        }
        if should_deactivate_non_covert_cloak || should_deactivate_covert_cloak {
            events.non_gate_cloak_broken = had_non_gate_cloak;
            events.covert_cloak_broken = should_deactivate_covert_cloak;
        }
        if clear_slim_cloak_meta(exec_ctx.ballpark, ball_id, deactivated_by_ballpark) {
            events.slim_cloak_cleared = true;
        }
    } else if strip_all_cloaks && clear_slim_cloak_meta(exec_ctx.ballpark, ball_id, false) {
        events.slim_cloak_cleared = true;
    }

    if strip_all_cloaks || reason.breaks_non_covert_module_cloak() || should_deactivate_non_covert_cloak
    {
        let removed_cycles = strip_cloak_cycles_for_ship(exec_ctx, ball_id, strip_all_cloaks);
        if !removed_cycles.is_empty() {
            if removed_cycles.iter().any(|c| c.is_covert) {
                events.covert_cloak_broken = true;
            }
            if !had_gate_cloak {
                events.non_gate_cloak_broken = true;
            }
        }
        events.removed_cloak_cycles = removed_cycles;
        if strip_all_cloaks {
            if let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&ball_id) {
                let was_gate = cs.gate_cloak_remaining_ticks > 0;
                let was_non_gate = !was_gate && (cs.is_cloaked || cs.cloak_velocity_multiplier.is_some());
                let had_residual = cs.is_cloaked
                    || cs.is_covert_cloak
                    || was_gate
                    || cs.cloak_stabilization_remaining_s > 0.0
                    || cs.cloak_velocity_multiplier.is_some();
                if had_residual {
                    cs.is_cloaked = false;
                    cs.was_cloaked_last_tick = false;
                    cs.is_covert_cloak = false;
                    cs.gate_cloak_remaining_ticks = 0;
                    cs.cloak_velocity_multiplier = None;
                    cs.cloak_stabilization_remaining_s = 0.0;
                    if was_gate {
                        events.gate_cloak_broken = true;
                    }
                    if was_non_gate {
                        events.non_gate_cloak_broken = true;
                        if had_covert_cloak {
                            events.covert_cloak_broken = true;
                        }
                    }
                }
            }
        } else if !had_covert_cloak {
            let mut cleared_non_covert_residual = false;
            if let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&ball_id) {
                if !cs.is_covert_cloak
                    && cs.gate_cloak_remaining_ticks == 0
                    && (cs.is_cloaked
                        || cs.cloak_velocity_multiplier.is_some()
                        || cs.cloak_stabilization_remaining_s > 0.0)
                {
                    cs.is_cloaked = false;
                    cs.was_cloaked_last_tick = false;
                    cs.cloak_velocity_multiplier = None;
                    cs.cloak_stabilization_remaining_s = 0.0;
                    cleared_non_covert_residual = true;
                    events.non_gate_cloak_broken = true;
                }
            }
            if (cleared_non_covert_residual || !events.removed_cloak_cycles.is_empty())
                && clear_slim_cloak_meta(exec_ctx.ballpark, ball_id, false)
            {
                events.slim_cloak_cleared = true;
            }
        }
    }

    if reason.clears_invuln() {
        if let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&ball_id) {
            let (cs_gate_broken, cs_undock_broken) = cs.clear_action_invuln();
            if cs_gate_broken {
                events.gate_cloak_broken = true;
            }
            if cs_undock_broken || had_undock_invuln {
                events.invuln_cancelled = true;
                events.undock_invuln_cancelled = true;
            }
            if matches!(reason, ProtectionBreakReason::Manual | ProtectionBreakReason::TargetLock)
                && cs.is_tethered
            {
                cs.is_tethered = false;
                events.tether_broken = true;
            }
        } else if had_undock_invuln {
            events.invuln_cancelled = true;
            events.undock_invuln_cancelled = true;
        }
        if had_warp_exit_invuln {
            events.warp_exit_invuln_cancelled = true;
        }
        if location_invuln_cancelled {
            events.invuln_cancelled = true;
        }
    }

    if let Some(cs) = exec_ctx.ballpark.combat_states.get(&ball_id) {
        events.decloak_targeting_delay_s = cs.decloak_targeting_delay_remaining_s;
    }

    sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
    if events.any_cloak_broken() || events.tether_broken || !events.removed_cloak_cycles.is_empty() {
        exec_ctx.ballpark.recompute_ball_attributes(ball_id);
        sync_positions_in_actor(exec_ctx);
    }
    events
}

/// Atomically forces decloak on `ball_id` (clearing gate cloak, non-covert/covert cloak,
/// cloaking `ActiveModuleCycle`s, `slim_meta.is_cloaked`, and uncloaking speed) while leaving
/// unrelated `CombatState` HP and capacitor values intact (`#1486`).
pub(in crate::system_actor) fn force_decloak_in_actor(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    char_id: Option<CharacterId>,
    reason: ProtectionBreakReason,
    real_now_unix: u64,
    max_invuln_seq: Option<u64>,
) -> ProtectionBreakEvents {
    break_protection_in_actor_impl(
        exec_ctx,
        ball_id,
        char_id,
        reason,
        real_now_unix,
        None,
        true,
        max_invuln_seq,
    )
}

/// Clears gate cloak and action invulnerability on `ship_id` during disconnected warp sync,
/// syncing `protection_registry` under the actor state write lock (`#1486`).
pub(crate) fn finish_disconnected_warp_protection_sync(
    ballpark: &mut Ballpark,
    actor_st: &mut SystemActorState,
    ship_id: BallId,
) {
    let had_gate_cloak = ballpark
        .combat_states
        .get(&ship_id)
        .is_some_and(|cs| cs.gate_cloak_remaining_ticks > 0);
    let deactivated = had_gate_cloak && ballpark.deactivate_cloak(ship_id);
    let _ = clear_slim_cloak_meta(ballpark, ship_id, deactivated);
    if let Some(cs) = ballpark.combat_states.get_mut(&ship_id) {
        let _ = cs.clear_action_invuln();
    }
    sync_ball_protection_in_actor(ballpark, actor_st, ship_id);
}

pub(crate) fn sync_active_module_ships_cache(
    active_module_cycles: &[ActiveModuleCycle],
    active_module_ships_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
) {
    let mut mod_guard = active_module_ships_actor
        .write()
        .unwrap_or_else(|e| e.into_inner());
    mod_guard.clear();
    for c in active_module_cycles {
        mod_guard.insert(c.ship_id);
    }
}

pub(in crate::system_actor) fn read_deact_hook(
    exec_ctx: &ActorExecCtx<'_>,
) -> Option<ModuleDeactivationHook> {
    exec_ctx.module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(in crate::system_actor) fn sync_positions_in_actor(exec_ctx: &mut ActorExecCtx<'_>) {
    sync_ball_positions(exec_ctx.ballpark, exec_ctx.ball_positions_actor, exec_ctx.scrambled_balls_actor);
}

pub(in crate::system_actor) fn sync_active_ships_cache(exec_ctx: &mut ActorExecCtx<'_>) {
    sync_active_module_ships_cache(&exec_ctx.actor_st.active_module_cycles, exec_ctx.active_module_ships_actor);
}

pub(in crate::system_actor) fn sync_positions_and_active_ships(exec_ctx: &mut ActorExecCtx<'_>) {
    sync_positions_in_actor(exec_ctx);
    sync_active_ships_cache(exec_ctx);
}

pub(in crate::system_actor) fn snapshot_tackle_totals_in_actor(exec_ctx: &mut ActorExecCtx<'_>) {
    cycles_tackle::snapshot_active_tackle_totals(
        exec_ctx.ballpark,
        &exec_ctx.actor_st.active_module_cycles,
        exec_ctx.last_active_totals,
    );
}

pub(in crate::system_actor) async fn call_actor_reply<T>(
    handle: &SystemHandle,
    label: &str,
    make_cmd: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> SystemCommand,
) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .cmd_sender()
        .send(make_cmd(tx))
        .await
        .map_err(|_| format!("system {} actor closed", handle.solar_system_id))?;
    rx.await
        .map_err(|_| format!("system {} actor dropped {label} reply", handle.solar_system_id))
}

pub(in crate::system_actor) async fn call_actor_result<T>(
    handle: &SystemHandle,
    label: &str,
    make_cmd: impl FnOnce(tokio::sync::oneshot::Sender<Result<T, String>>) -> SystemCommand,
) -> Result<T, String> {
    call_actor_reply(handle, label, make_cmd).await?
}

pub(in crate::system_actor) fn handle_protection_or_lifecycle_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        cmd @ (SystemCommand::SpawnBallWithCombat { .. }
        | SystemCommand::SpawnBallWithMeta { .. }
        | SystemCommand::SpawnBallWithMetaTagged { .. }) => {
            let (ball, combat_state, slim_meta, spawn_token, check_closed, reply) = match cmd {
                SystemCommand::SpawnBallWithCombat { ball, combat_state, reply } => {
                    (ball, combat_state, None, None, false, reply)
                }
                SystemCommand::SpawnBallWithMeta { ball, combat_state, slim_meta, reply } => {
                    (ball, combat_state, Some(slim_meta), None, true, reply)
                }
                SystemCommand::SpawnBallWithMetaTagged {
                    ball,
                    combat_state,
                    slim_meta,
                    spawn_token,
                    reply,
                } => (ball, combat_state, Some(slim_meta), Some(spawn_token), true, reply),
                _ => unreachable!(),
            };
            if check_closed && reply.is_closed() {
                return None;
            }
            let ball_id = ball.id;
            match spawn_token {
                Some(tok) => {
                    exec_ctx.ball_spawn_tokens.insert(ball_id, tok);
                }
                None => {
                    exec_ctx.ball_spawn_tokens.remove(&ball_id);
                }
            }
            let was_warping = matches!(
                exec_ctx.ballpark.balls.get(&ball_id).map(|b| &b.mode),
                Some(eve_destiny::MovementMode::Warp { .. })
            );
            let is_warp = matches!(ball.mode, eve_destiny::MovementMode::Warp { .. });
            let deact_guard = read_deact_hook(exec_ctx);
            let tick = cycles_tackle::spawn_ball_with_combat_in_actor(
                exec_ctx.ballpark,
                &mut exec_ctx.actor_st.active_module_cycles,
                &mut exec_ctx.actor_st.base_scramble_strengths,
                &mut exec_ctx.actor_st.base_core_strengths,
                &exec_ctx.actor_st.sde_opt,
                &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(),
                ball,
                combat_state,
                slim_meta,
            );
            if was_warping && !is_warp {
                exec_ctx.actor_st.warp_state.clear_ball(ball_id, true);
            }
            *exec_ctx.spawned_checkpoint_ball = Some(ball_id);
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            snapshot_tackle_totals_in_actor(exec_ctx);
            sync_positions_in_actor(exec_ctx);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(tick));
            None
        }
        SystemCommand::BreakGateCloak { ball_id, reply } => {
            let broken = exec_ctx.ballpark.deactivate_cloak(ball_id);
            if let Some(mut meta) = exec_ctx.ballpark.ball_slim_meta.get(&ball_id).cloned() {
                meta.is_cloaked = false;
                exec_ctx.ballpark.change_slim_item(ball_id, meta);
            }
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, broken);
            None
        }
        SystemCommand::BreakProtection {
            ball_id,
            char_id,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        } => {
            let events = break_protection_in_actor(
                exec_ctx,
                ball_id,
                char_id,
                reason,
                real_now_unix,
                None,
                max_invuln_seq,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(events));
            None
        }
        SystemCommand::ForceDecloak {
            ball_id,
            char_id,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        } => {
            let events = force_decloak_in_actor(
                exec_ctx,
                ball_id,
                char_id,
                reason,
                real_now_unix,
                max_invuln_seq,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(events));
            None
        }
        SystemCommand::GetProtectionSnapshot { ball_id, reply } => {
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            let snap = exec_ctx.actor_st.protection_registry.get(&ball_id).copied();
            defer_reply(exec_ctx.deferred_replies, reply, snap);
            None
        }
        SystemCommand::TakeBall {
            ball_id,
            accept_docking_target,
            preserve_db_active_states,
            reply,
        } => {
            exec_ctx.ball_spawn_tokens.remove(&ball_id);
            exec_ctx
                .explicit_update_state_guards_actor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, g| !(g.sys_id == exec_ctx.solar_system_id && g.ship_id == ball_id));
            if !exec_ctx.ballpark.balls.contains_key(&ball_id) {
                let _ = exec_ctx.ballpark.remove_ball(ball_id);
                exec_ctx.actor_st.protection_registry.remove(&ball_id);
                exec_ctx.actor_st.warp_state.clear_ball(ball_id, true);
                defer_reply(exec_ctx.deferred_replies, reply, None);
                return None;
            }
            let tick = exec_ctx.ballpark.current_tick;
            let before_cycles = exec_ctx.actor_st.active_module_cycles.len();
            crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                exec_ctx.ballpark,
                &exec_ctx.actor_st.db_opt,
                exec_ctx.actor_st.tactical_state.as_ref(),
                ball_id,
            );
            let deact_guard = read_deact_hook(exec_ctx);
            if accept_docking_target.is_some() {
                emit_propulsion_stop_fx_before_detach(
                    exec_ctx.ballpark,
                    &exec_ctx.actor_st.active_module_cycles,
                    &exec_ctx.actor_st.sde_opt,
                    ball_id,
                );
            }
            let pre_cleanup_combat_flags = if preserve_db_active_states {
                exec_ctx.ballpark.combat_states.get(&ball_id).map(|cs| {
                    types::TakenBallPreCleanupCombatFlags {
                        rah_armor_resonance: cs.rah_armor_resonance,
                        is_covert_cloak: cs.is_covert_cloak,
                        cloak_stabilization_remaining_s: cs.cloak_stabilization_remaining_s,
                        decloak_targeting_delay_remaining_s: cs
                            .decloak_targeting_delay_remaining_s,
                        cloak_velocity_multiplier: cs.cloak_velocity_multiplier,
                    }
                })
            } else {
                None
            };
            let removed_cycles = remove_all_ship_module_cycles_preserving_own_db(
                exec_ctx.ballpark,
                &mut exec_ctx.actor_st.active_module_cycles,
                &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths,
                &exec_ctx.actor_st.db_opt,
                ball_id,
                deact_guard.as_ref(),
                preserve_db_active_states,
            );
            let mut removed_own_cycles: Vec<ActiveModuleCycle> = if preserve_db_active_states {
                removed_cycles
                    .into_iter()
                    .filter_map(|(c, _)| (c.ship_id == ball_id).then_some(c))
                    .collect()
            } else {
                Vec::new()
            };
            removed_own_cycles.reverse();
            let cycles_cleared =
                before_cycles.saturating_sub(exec_ctx.actor_st.active_module_cycles.len());
            let lock_count = exec_ctx.ballpark.clear_target_locks(ball_id);
            if lock_count > 0 {
                on_target_unlocked_in_actor(
                    exec_ctx.ballpark,
                    &mut exec_ctx.actor_st.active_module_cycles,
                    &exec_ctx.actor_st.base_scramble_strengths,
                    &exec_ctx.actor_st.base_core_strengths,
                    &exec_ctx.actor_st.db_opt,
                    ball_id,
                    None,
                    deact_guard.as_ref(),
                );
            }
            exec_ctx.actor_st.base_scramble_strengths.remove(&ball_id);
            exec_ctx.actor_st.base_core_strengths.remove(&ball_id);
            exec_ctx.actor_st.asteroids.remove(&ball_id);
            exec_ctx.actor_st.fit_projections.remove(&ball_id);
            exec_ctx.actor_st.protection_registry.remove(&ball_id);
            *exec_ctx.removed_checkpoint_ball = Some(ball_id);
            let combat_state = exec_ctx.ballpark.combat_states.get(&ball_id).cloned();
            let slim_meta = exec_ctx.ballpark.ball_slim_meta.get(&ball_id).cloned();
            let removed = if let Some((station_id, station_pos)) = accept_docking_target {
                if exec_ctx.ballpark.balls.contains_key(&station_id) {
                    exec_ctx.ballpark.accept_docking(ball_id, station_id)
                } else {
                    exec_ctx
                        .ballpark
                        .accept_docking_at(ball_id, station_id, station_pos)
                }
            } else {
                exec_ctx.ballpark.remove_ball(ball_id)
            };
            sync_positions_and_active_ships(exec_ctx);
            defer_reply(
                exec_ctx.deferred_replies,
                reply,
                removed.map(|ball| TakenBall {
                    tick,
                    ball,
                    combat_state,
                    slim_meta,
                    cycles_cleared,
                    removed_own_cycles,
                    pre_cleanup_combat_flags,
                }),
            );
            None
        }
        cmd @ (SystemCommand::RestoreBall { .. }
        | SystemCommand::RestoreBallIfAbsent { .. }) => {
            let (mut taken, only_if_absent, reply) = match cmd {
                SystemCommand::RestoreBall { taken, reply } => (taken, false, reply),
                SystemCommand::RestoreBallIfAbsent { taken, reply } => (taken, true, reply),
                _ => unreachable!(),
            };
            let ball_id = taken.ball.id;
            if only_if_absent && exec_ctx.ballpark.balls.contains_key(&ball_id) {
                defer_reply(
                    exec_ctx.deferred_replies,
                    reply,
                    Ok(exec_ctx.ballpark.current_tick),
                );
                return None;
            }
            let removed_own_cycles = std::mem::take(&mut taken.removed_own_cycles);
            let pre_cleanup_combat_flags = taken.pre_cleanup_combat_flags.take();
            exec_ctx.ball_spawn_tokens.remove(&ball_id);
            let deact_guard = read_deact_hook(exec_ctx);
            let tick = cycles_tackle::spawn_ball_with_combat_in_actor(
                exec_ctx.ballpark,
                &mut exec_ctx.actor_st.active_module_cycles,
                &mut exec_ctx.actor_st.base_scramble_strengths,
                &mut exec_ctx.actor_st.base_core_strengths,
                &exec_ctx.actor_st.sde_opt,
                &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(),
                taken.ball,
                taken.combat_state.unwrap_or_default(),
                taken.slim_meta,
            );
            if let Some(flags) = pre_cleanup_combat_flags {
                if let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&ball_id) {
                    cs.rah_armor_resonance = flags.rah_armor_resonance;
                    cs.is_covert_cloak = flags.is_covert_cloak;
                    cs.cloak_stabilization_remaining_s = flags.cloak_stabilization_remaining_s;
                    cs.decloak_targeting_delay_remaining_s =
                        flags.decloak_targeting_delay_remaining_s;
                    cs.cloak_velocity_multiplier = flags.cloak_velocity_multiplier;
                }
            }
            let force_recompute = pre_cleanup_combat_flags.is_some();
            restore_taken_own_cycles_in_actor(
                exec_ctx.ballpark,
                &mut exec_ctx.actor_st.active_module_cycles,
                &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths,
                &exec_ctx.actor_st.db_opt,
                ball_id,
                deact_guard.as_ref(),
                removed_own_cycles,
                force_recompute,
            );
            *exec_ctx.spawned_checkpoint_ball = Some(ball_id);
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            sync_positions_and_active_ships(exec_ctx);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(tick));
            None
        }
        SystemCommand::GetTargetLocks { ball_id, reply } => {
            let locks = exec_ctx.ballpark.get_target_locks(ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, locks);
            None
        }
        SystemCommand::AddTargetLock {
            source_id,
            lock,
            reply,
        } => {
            if exec_ctx
                .ballpark
                .combat_states
                .get(&lock.target_id)
                .is_some_and(|tcs| tcs.is_tethered)
            {
                defer_reply(
                    exec_ctx.deferred_replies,
                    reply,
                    Err("Target is tethered and invulnerable to target locks".into()),
                );
                return None;
            }
            let mut tether_cleared = false;
            if let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&source_id) {
                cs.undock_invulnerable_remaining_ticks = 0;
                if cs.is_tethered {
                    cs.is_tethered = false;
                    tether_cleared = true;
                }
            }
            let res = if exec_ctx.ballpark.add_target_lock(source_id, lock) {
                Ok(())
            } else {
                Err("Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay, or target is untargetable".to_string())
            };
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, source_id);
            if tether_cleared {
                sync_positions_in_actor(exec_ctx);
            }
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        cmd @ (SystemCommand::RemoveTargetLock { .. }
        | SystemCommand::RemoveTargetLockQuiet { .. }) => {
            let (source_id, target_id, reply, emit_lost) = match cmd {
                SystemCommand::RemoveTargetLock {
                    source_id,
                    target_id,
                    reply,
                } => (source_id, target_id, reply, true),
                SystemCommand::RemoveTargetLockQuiet {
                    source_id,
                    target_id,
                    reply,
                } => (source_id, target_id, reply, false),
                _ => unreachable!(),
            };
            let removed = if emit_lost {
                exec_ctx.ballpark.remove_target_lock(source_id, target_id)
            } else {
                exec_ctx
                    .ballpark
                    .remove_target_lock_quiet(source_id, target_id)
            };
            if removed {
                let deact_guard = read_deact_hook(exec_ctx);
                on_target_unlocked_in_actor(
                    exec_ctx.ballpark,
                    &mut exec_ctx.actor_st.active_module_cycles,
                    &exec_ctx.actor_st.base_scramble_strengths,
                    &exec_ctx.actor_st.base_core_strengths,
                    &exec_ctx.actor_st.db_opt,
                    source_id,
                    Some(target_id),
                    deact_guard.as_ref(),
                );
            }
            defer_reply(exec_ctx.deferred_replies, reply, removed);
            None
        }
        cmd @ (SystemCommand::ClearTargetLocks { .. }
        | SystemCommand::ClearTargetLocksQuiet { .. }) => {
            let (source_id, reply, emit_lost) = match cmd {
                SystemCommand::ClearTargetLocks { source_id, reply } => (source_id, reply, true),
                SystemCommand::ClearTargetLocksQuiet { source_id, reply } => {
                    (source_id, reply, false)
                }
                _ => unreachable!(),
            };
            let count = if emit_lost {
                exec_ctx.ballpark.clear_target_locks(source_id)
            } else {
                exec_ctx.ballpark.clear_target_locks_quiet(source_id)
            };
            if count > 0 {
                let deact_guard = read_deact_hook(exec_ctx);
                on_target_unlocked_in_actor(
                    exec_ctx.ballpark,
                    &mut exec_ctx.actor_st.active_module_cycles,
                    &exec_ctx.actor_st.base_scramble_strengths,
                    &exec_ctx.actor_st.base_core_strengths,
                    &exec_ctx.actor_st.db_opt,
                    source_id,
                    None,
                    deact_guard.as_ref(),
                );
            }
            defer_reply(exec_ctx.deferred_replies, reply, count);
            None
        }
        SystemCommand::FastForwardClock {
            ship_id,
            secs,
            reply,
        } => {
            let was_warping = matches!(
                exec_ctx.ballpark.balls.get(&ship_id).map(|b| &b.mode),
                Some(eve_destiny::MovementMode::Warp { .. })
            );
            super::fast_forward_clock_in_actor_with_epochs_and_seq(
                exec_ctx.solar_system_id,
                exec_ctx.ballpark,
                ship_id,
                secs,
                &mut exec_ctx.actor_st.sim_elapsed_secs,
                &exec_ctx.actor_st.db_opt,
                &mut exec_ctx.actor_st.ball_space_entry_epochs,
                exec_ctx.ball_positions_actor,
                exec_ctx.scrambled_balls_actor,
                exec_ctx.event_tx_actor,
                Some(exec_ctx.broadcast_seq_gate_actor),
                &mut exec_ctx.actor_st.warp_state,
            );
            sync_all_protection_registry_in_actor(exec_ctx.ballpark, exec_ctx.actor_st);
            exec_ctx.sim_elapsed_secs_bits_actor.store(
                exec_ctx.actor_st.sim_elapsed_secs.to_bits(),
                std::sync::atomic::Ordering::Release,
            );
            let now_warping = matches!(
                exec_ctx.ballpark.balls.get(&ship_id).map(|b| &b.mode),
                Some(eve_destiny::MovementMode::Warp { .. })
            );
            if was_warping && !now_warping {
                exec_ctx
                    .warp_stopped_at_actor
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(ship_id, tokio::time::Instant::now());
            }
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        other => Some(other),
    }
}

impl SystemHandle {
    /// Captures the shared ledger's current monotonic grant sequence when no rank `>= 55` lock is held.
    pub(crate) fn current_invuln_seq_if_safe(&self) -> Option<u64> {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= super::types::RANK_INVULN_LEDGER)
        {
            return None;
        }
        Some(
            self.location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .next_seq,
        )
    }

    /// Atomically evaluates and breaks cloak/invulnerability protection in the actor (`#1486`).
    pub fn break_protection(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
    ) -> impl std::future::Future<Output = Result<ProtectionBreakEvents, String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.break_protection_at_seq(ball_id, char_id, reason, real_now_unix, max_invuln_seq)
    }

    /// Variant of [`Self::break_protection`] bounded to `max_invuln_seq` (`#1486`).
    pub async fn break_protection_at_seq(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        max_invuln_seq: Option<u64>,
    ) -> Result<ProtectionBreakEvents, String> {
        call_actor_result(self, "BreakProtection", |reply| SystemCommand::BreakProtection {
            ball_id,
            char_id,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        })
        .await
    }

    /// Atomically forces decloak on `ball_id` (stripping gate/module cloak, cloak cycles, and
    /// `slim_meta.is_cloaked`) without overwriting concurrent HP or capacitor state (`#1486`).
    pub fn force_decloak(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
    ) -> impl std::future::Future<Output = Result<ProtectionBreakEvents, String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.force_decloak_at_seq(ball_id, char_id, reason, real_now_unix, max_invuln_seq)
    }

    /// Variant of [`Self::force_decloak`] bounded to `max_invuln_seq` (`#1486`).
    pub async fn force_decloak_at_seq(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
        max_invuln_seq: Option<u64>,
    ) -> Result<ProtectionBreakEvents, String> {
        call_actor_result(self, "ForceDecloak", |reply| SystemCommand::ForceDecloak {
            ball_id,
            char_id,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        })
        .await
    }

    /// Returns the actor-owned unified protection record for `ball_id` (`#1486`).
    pub async fn get_protection_snapshot(
        &self,
        ball_id: BallId,
    ) -> Result<Option<ActorShipProtection>, String> {
        call_actor_reply(self, "GetProtectionSnapshot", |reply| {
            SystemCommand::GetProtectionSnapshot { ball_id, reply }
        }).await
    }

    /// Queries the canonical actor state for whether `ship_id` has an active non-gate module cloak (`#1486`).
    pub async fn is_ship_actively_cloaked(&self, ship_id: BallId) -> bool {
        self.get_protection_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|p| p.has_active_non_gate_cloak())
    }

    /// Synchronous read-only check of actor protection registry / combat state for non-gate cloak (`#1486`).
    pub fn has_active_non_gate_cloak_sync(&self, ship_id: BallId) -> bool {
        let state = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        if state
            .protection_registry
            .get(&ship_id)
            .is_some_and(ActorShipProtection::has_active_non_gate_cloak)
        {
            return true;
        }
        state
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && is_cloak_cycle(c))
    }

    /// Synchronously applies `event` for `ball_id` or falls back to non-blocking actor dispatch
    /// if `ballpark` is currently write-locked (for synchronous hooks inside `step_system_tick`).
    pub fn try_emit_destiny_event(&self, ball_id: BallId, event: DestinyEvent) -> bool {
        if !eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
        {
            if let Ok(mut bp) = self.ballpark.try_write() {
                if let Ok(mut st) = self.actor_state.try_write() {
                    let known = super::cycles_tackle::apply_destiny_event_in_actor(
                        &mut bp, ball_id, event,
                    );
                    if known {
                        sync_ball_protection_in_actor(&bp, &mut st, ball_id);
                    }
                    return known;
                }
            }
        }
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .try_send(SystemCommand::EmitDestinyEvent {
                ball_id,
                event,
                reply: tx,
            })
            .is_ok()
    }
}

impl SystemRegistry {
    fn refresh_loaded_protection_registries(&self, target_char_id: Option<CharacterId>) {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_SYSTEM_REGISTRY)
        {
            return;
        }
        let handles = self.active_system_handles();
        if handles.is_empty() {
            return;
        }
        for (sys_id, handle) in handles {
            if let Ok(mut st) = handle.actor_state.try_write() {
                let ledger = self
                    .location_invuln_ledger
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(cid) = target_char_id {
                    let until_opt = ledger
                        .by_char
                        .get(&cid)
                        .copied()
                        .filter(|entry| entry.matches_system(sys_id))
                        .map(|entry| entry.until_unix);
                    for entry in st.protection_registry.values_mut() {
                        if entry.owner_char_id == Some(cid) {
                            entry.location_change_invuln_until_unix = until_opt;
                        }
                    }
                } else {
                    for entry in st.protection_registry.values_mut() {
                        entry.location_change_invuln_until_unix = entry
                            .owner_char_id
                            .and_then(|cid| ledger.by_char.get(&cid).copied())
                            .filter(|inv| inv.matches_system(sys_id))
                            .map(|inv| inv.until_unix);
                    }
                }
            }
        }
    }

    /// Returns the loaded solar system ID currently holding a ball owned by `char_id`, if any (`#1486`).
    pub fn resolve_loaded_char_system(&self, char_id: CharacterId) -> Option<SolarSystemId> {
        if char_id <= 0 {
            return None;
        }
        for (sys_id, handle) in self.active_system_handles() {
            let view = handle.view();
            if view
                .ball_slim_meta
                .values()
                .any(|meta| meta.char_id == Some(char_id))
            {
                return (sys_id > 0).then_some(sys_id);
            }
        }
        None
    }

    /// Returns the shared location-change invulnerability ledger's current monotonic sequence (`#1486`).
    pub fn current_location_invuln_seq(&self) -> u64 {
        self.location_invuln_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_seq
    }

    /// Grants or refreshes location-change invulnerability for `char_id` up to `until_unix`,
    /// scoped to `solar_system_id` when positive, returning the monotonic ledger sequence (`#1486`).
    pub fn grant_location_change_invuln_in_system(
        &self,
        char_id: CharacterId,
        solar_system_id: Option<SolarSystemId>,
        until_unix: u64,
    ) -> u64 {
        let scoped_sys = solar_system_id.filter(|&sys_id| sys_id > 0);
        let seq = {
            let mut ledger = self
                .location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ledger.grant(char_id, scoped_sys, until_unix).seq
        };
        self.refresh_loaded_protection_registries(Some(char_id));
        seq
    }

    /// Grants or refreshes location-change invulnerability for `char_id` up to `until_unix` (`#1486`).
    pub fn grant_location_change_invuln(&self, char_id: CharacterId, until_unix: u64) {
        let scoped_sys = if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_SYSTEM_REGISTRY)
        {
            None
        } else {
            self.resolve_loaded_char_system(char_id)
        };
        self.grant_location_change_invuln_in_system(char_id, scoped_sys, until_unix);
    }

    /// Atomically removes and returns an active location-change invulnerability deadline when
    /// it matches optional `expected_sys` and `max_seq` bounds (`#1486`).
    pub fn pop_active_location_change_invuln_scoped(
        &self,
        char_id: CharacterId,
        expected_sys: Option<SolarSystemId>,
        max_seq: Option<u64>,
        now_unix: u64,
    ) -> Option<u64> {
        let scope = expected_sys.filter(|&sys_id| sys_id > 0);
        let (removed, cancelled_active, prev_until) = {
            let mut ledger = self
                .location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev = ledger.by_char.get(&char_id).map(|e| e.until_unix);
            let (removed, active) = ledger.pop_if_matches(char_id, scope, max_seq, now_unix);
            (removed, active, prev)
        };
        if removed {
            self.refresh_loaded_protection_registries(Some(char_id));
        }
        cancelled_active.then_some(prev_until).flatten()
    }

    /// Atomically removes and returns any active location-change invulnerability deadline (`#1486`).
    pub fn pop_active_location_change_invuln(
        &self,
        char_id: CharacterId,
        now_unix: u64,
    ) -> Option<u64> {
        let (removed, popped) = {
            let mut ledger = self
                .location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let had_entry = ledger.by_char.contains_key(&char_id);
            let active = ledger.pop_active(char_id, now_unix);
            (had_entry, active)
        };
        if removed {
            self.refresh_loaded_protection_registries(Some(char_id));
        }
        popped
    }

    /// Clears any recorded location-change invulnerability deadline for `char_id` without checks (`#1486`).
    pub fn clear_location_change_invuln(&self, char_id: CharacterId) {
        let removed = {
            let mut ledger = self
                .location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ledger.by_char.remove(&char_id).is_some()
        };
        if removed {
            self.refresh_loaded_protection_registries(Some(char_id));
        }
    }

    /// Returns the active location-change invulnerability deadline for `char_id`, if any (`#1486`).
    pub fn location_change_invuln_until(
        &self,
        char_id: CharacterId,
        now_unix: u64,
    ) -> Option<u64> {
        self.location_invuln_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_until(char_id, now_unix)
    }

    /// Returns `true` when `char_id` currently holds an active location-change invulnerability grant (`#1486`).
    pub fn is_char_location_invulnerable(&self, char_id: CharacterId, now_unix: u64) -> bool {
        self.location_change_invuln_until(char_id, now_unix)
            .is_some()
    }

    /// Canonical protection query for whether a ship/target (identified by ball ID, optional system ID,
    /// and optional owner character ID) has an active location-change invulnerability grant (`#1486`).
    pub fn is_target_location_invulnerable(
        &self,
        solar_system_id: Option<SolarSystemId>,
        target_id: i64,
        owner_char_id: Option<CharacterId>,
        now_unix: u64,
    ) -> bool {
        if owner_char_id
            .is_some_and(|cid| cid > 0 && self.is_char_location_invulnerable(cid, now_unix))
        {
            return true;
        }
        if target_id > 0 && self.is_char_location_invulnerable(target_id, now_unix) {
            return true;
        }
        let Some(sys_id) = solar_system_id else {
            return false;
        };
        let Some(handle) = self.get_handle(sys_id) else {
            return false;
        };
        let resolved_cid = handle
            .view()
            .ball_slim_meta
            .get(&target_id)
            .and_then(|meta| meta.char_id)
            .filter(|&cid| cid > 0);
        resolved_cid.is_some_and(|cid| self.is_char_location_invulnerable(cid, now_unix))
    }

    /// Snapshots all tracked location-change invulnerability deadlines for runtime persistence (`#1486`).
    pub fn snapshot_location_change_invuln(&self) -> HashMap<CharacterId, u64> {
        self.location_invuln_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot_map()
    }

    /// Restores tracked location-change invulnerability deadlines from runtime persistence (`#1486`).
    pub fn restore_location_change_invuln(&self, restored: HashMap<CharacterId, u64>) {
        {
            let mut ledger = self
                .location_invuln_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ledger.restore_map(restored);
        }
        self.refresh_loaded_protection_registries(None);
    }
}
