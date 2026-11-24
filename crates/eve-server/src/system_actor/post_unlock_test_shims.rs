//! Legacy `#[cfg(test)]` wrapper shims around `_collecting` `post_unlock` functions so existing
//! unit tests outside `system_actor/` keep compiling without direct DB writes in production paths (`W2`).

#![allow(clippy::too_many_arguments)]

use super::cycles::{
    apply_weapon_cycle_damage_collecting, cleanup_removed_module_cycle_collecting,
    emit_cycle_deactivation_collecting, remove_all_ship_module_cycles_collecting,
    remove_all_ship_module_cycles_preserving_own_db_collecting,
    remove_non_warp_ship_module_cycles_collecting, restore_taken_own_cycles_in_actor_collecting,
    step_cycles_and_capacitor_with_missiles_and_tidi_collecting,
    teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting,
};
use super::cycles_tackle::{
    handle_add_module_cycle_collecting, handle_update_module_overload_collecting,
    on_target_unlocked_in_actor_collecting, reconcile_module_cycle_before_activate_in_actor_collecting,
    reconcile_tackle_and_bubbles_collecting, set_combat_state_in_actor_collecting,
    spawn_ball_with_combat_in_actor_collecting,
};
use super::post_unlock::{execute_post_unlock_ops, PostUnlockOps};
use super::*;
use eve_destiny::{Ball, BallId, Ballpark, CombatState, SlimItemMeta};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn drain_test_ops(
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ops: PostUnlockOps,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    pending_cycle_actions: Option<&mut Vec<PendingCycleAction>>,
) {
    if ops.is_empty() {
        return;
    }
    let actions = execute_post_unlock_ops(db_opt.as_ref(), None, ops, deactivation_hook, None);
    if let Some(out) = pending_cycle_actions {
        out.extend(actions);
    }
}

pub(crate) fn spawn_ball_with_combat_in_actor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &mut HashMap<BallId, i32>, core: &mut HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    hook: Option<&ModuleDeactivationHook>, ball: Ball, cs: CombatState, meta: Option<SlimItemMeta>,
) -> u64 {
    let mut ops = PostUnlockOps::default();
    let r = spawn_ball_with_combat_in_actor_collecting(
        sys_id, bp, cycles, scram, core, sde_opt, db_opt, hook, ball, cs, meta, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
    r
}

pub(crate) fn set_combat_state_in_actor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &mut HashMap<BallId, i32>, core: &mut HashMap<BallId, i32>,
    last_tot: &mut HashMap<BallId, (Option<i32>, Option<i32>)>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, hook: Option<&ModuleDeactivationHook>,
    ball_id: BallId, cs: CombatState,
) {
    let mut ops = PostUnlockOps::default();
    set_combat_state_in_actor_collecting(
        sys_id, bp, cycles, scram, core, last_tot, db_opt, hook, ball_id, cs, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
}

pub(crate) fn reconcile_tackle_and_bubbles(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, hook: Option<&ModuleDeactivationHook>,
) {
    let mut ops = PostUnlockOps::default();
    reconcile_tackle_and_bubbles_collecting(sys_id, bp, cycles, scram, core, db_opt, hook, &mut ops);
    drain_test_ops(db_opt, ops, hook, None);
}

pub(crate) fn on_target_unlocked_in_actor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, src: BallId, tgt: Option<BallId>,
    hook: Option<&ModuleDeactivationHook>,
) {
    let mut ops = PostUnlockOps::default();
    on_target_unlocked_in_actor_collecting(sys_id, bp, cycles, scram, core, db_opt, src, tgt, hook, &mut ops);
    drain_test_ops(db_opt, ops, hook, None);
}

pub(crate) fn handle_add_module_cycle(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    hook: Option<&ModuleDeactivationHook>, cycle: ActiveModuleCycle,
) -> Result<(), String> {
    let mut ops = PostUnlockOps::default();
    let res = handle_add_module_cycle_collecting(sys_id, bp, cycles, scram, core, sde_opt, db_opt, hook, cycle, &mut ops);
    if res.is_ok() { drain_test_ops(db_opt, ops, hook, None); }
    res
}

#[allow(dead_code)]
pub(crate) fn reconcile_module_cycle_before_activate_in_actor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    hook: Option<&ModuleDeactivationHook>, ship_id: BallId, module_id: i64, now_unix: u64,
) -> (Option<(i32, Option<BallId>)>, bool) {
    let mut ops = PostUnlockOps::default();
    let r = reconcile_module_cycle_before_activate_in_actor_collecting(
        sys_id, bp, cycles, scram, core, sde_opt, db_opt, hook, ship_id, module_id, now_unix, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
    r
}

pub(crate) fn handle_update_module_overload(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    hook: Option<&ModuleDeactivationHook>, ship_id: BallId, module_id: i64, is_overloaded: bool,
) -> bool {
    let mut ops = PostUnlockOps::default();
    let r = handle_update_module_overload_collecting(
        sys_id, bp, cycles, scram, core, sde_opt, db_opt, hook, ship_id, module_id, is_overloaded, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
    r
}

pub(crate) fn apply_weapon_cycle_damage(
    cycle: &ActiveModuleCycle, bp: &mut Ballpark, rah: &mut HashMap<BallId, [f64; 4]>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    pending_actions: &mut Vec<PendingCycleAction>, destroyed: &mut Vec<(BallId, Option<CharacterId>)>,
    outbox: Option<&PendingDestroyedOutbox>,
) {
    let mut ops = PostUnlockOps::default();
    apply_weapon_cycle_damage_collecting(
        bp.solar_system_id, cycle, bp, rah, sde_opt, db_opt, pending_actions, destroyed, outbox, &mut ops,
    );
    drain_test_ops(db_opt, ops, None, Some(pending_actions));
}

pub(crate) fn step_cycles_and_capacitor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>,
    heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    tac: &dyn AnyRwLock<crate::services::tactical_modules::TacticalModulesState>,
    actions: &mut Vec<PendingCycleAction>, dt: f64, hook: Option<&ModuleDeactivationHook>,
) -> Vec<(BallId, Option<CharacterId>)> {
    let (empty_ep, empty_proj, mut empty_ws) = (HashMap::new(), HashMap::new(), HashSet::new());
    step_cycles_and_capacitor_with_epochs(
        sys_id, bp, cycles, disruptors, nanite, heat, rah, scram, core, sde_opt, db_opt,
        &empty_ep, &empty_proj, &mut empty_ws, rng_opt, tac, actions, dt, hook,
    )
}

pub(crate) fn step_cycles_and_capacitor_with_epochs(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>,
    heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    epochs: &HashMap<BallId, u64>, proj: &HashMap<BallId, eve_dogma::FitProjection>,
    ws: &mut HashSet<(BallId, i64)>, rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    tac: &dyn AnyRwLock<crate::services::tactical_modules::TacticalModulesState>,
    actions: &mut Vec<PendingCycleAction>, dt: f64, hook: Option<&ModuleDeactivationHook>,
) -> Vec<(BallId, Option<CharacterId>)> {
    let mut unused_launches = Vec::new();
    step_cycles_and_capacitor_with_missiles_and_tidi(
        sys_id, bp, cycles, disruptors, nanite, heat, rah, scram, core, sde_opt, db_opt,
        epochs, proj, ws, &mut unused_launches, rng_opt, tac, actions, 0.0, dt, 1.0, None, hook, None,
    )
}

pub(crate) fn step_cycles_and_capacitor_with_missiles_and_tidi(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>,
    heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    epochs: &HashMap<BallId, u64>, proj: &HashMap<BallId, eve_dogma::FitProjection>,
    ws: &mut HashSet<(BallId, i64)>, launches: &mut Vec<super::PendingMissileLaunch>,
    rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    tac: &dyn AnyRwLock<crate::services::tactical_modules::TacticalModulesState>,
    actions: &mut Vec<PendingCycleAction>, sim_s: f64, dt: f64, tidi: f64,
    reload_sim: Option<&mut HashMap<i64, ModuleReloadSimEntry>>,
    hook: Option<&ModuleDeactivationHook>, outbox: Option<&PendingDestroyedOutbox>,
) -> Vec<(BallId, Option<CharacterId>)> {
    let mut ops = PostUnlockOps::default();
    let dead = step_cycles_and_capacitor_with_missiles_and_tidi_collecting(
        sys_id, bp, cycles, disruptors, nanite, heat, rah, scram, core, sde_opt, db_opt,
        epochs, proj, ws, launches, rng_opt, tac, actions, sim_s, dt, tidi, reload_sim, hook, outbox, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, Some(actions));
    dead
}

pub(crate) fn cleanup_removed_module_cycle(
    bp: &mut Ballpark, removed: &ActiveModuleCycle, db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let mut ops = PostUnlockOps::default();
    cleanup_removed_module_cycle_collecting(bp, removed, db_opt, &mut ops);
    drain_test_ops(db_opt, ops, None, None);
}

#[allow(dead_code)]
pub(crate) fn emit_cycle_deactivation(
    sys_id: SolarSystemId, cycle: &ActiveModuleCycle, bp: &Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, hook: Option<&ModuleDeactivationHook>,
) {
    let mut ops = PostUnlockOps::default();
    emit_cycle_deactivation_collecting(sys_id, cycle, bp, hook.is_some(), &mut ops);
    drain_test_ops(db_opt, ops, hook, None);
}

pub(crate) fn remove_non_warp_ship_module_cycles(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, ship_id: BallId, hook: Option<&ModuleDeactivationHook>,
) {
    let mut ops = PostUnlockOps::default();
    remove_non_warp_ship_module_cycles_collecting(sys_id, bp, cycles, scram, core, db_opt, ship_id, hook, &mut ops);
    drain_test_ops(db_opt, ops, hook, None);
}

pub(crate) fn remove_all_ship_module_cycles_preserving_own_db(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, ship_id: i64, hook: Option<&ModuleDeactivationHook>,
    preserve_own_db_active_states: bool,
) -> Vec<(ActiveModuleCycle, Option<i64>)> {
    let mut ops = PostUnlockOps::default();
    let res = remove_all_ship_module_cycles_preserving_own_db_collecting(
        sys_id, bp, cycles, scram, core, db_opt, ship_id, hook, preserve_own_db_active_states, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
    res
}

pub(crate) fn remove_all_ship_module_cycles(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, ship_id: i64, hook: Option<&ModuleDeactivationHook>,
) -> Vec<(ActiveModuleCycle, Option<i64>)> {
    let mut ops = PostUnlockOps::default();
    let res = remove_all_ship_module_cycles_collecting(sys_id, bp, cycles, scram, core, db_opt, ship_id, hook, &mut ops);
    drain_test_ops(db_opt, ops, hook, None);
    res
}

#[allow(dead_code)]
pub(crate) fn restore_taken_own_cycles_in_actor(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>, ball_id: i64, hook: Option<&ModuleDeactivationHook>,
    removed_own_cycles: Vec<ActiveModuleCycle>, force_recompute: bool,
) {
    let mut ops = PostUnlockOps::default();
    restore_taken_own_cycles_in_actor_collecting(
        sys_id, bp, cycles, scram, core, db_opt, ball_id, hook, removed_own_cycles, force_recompute, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
}

#[allow(dead_code)]
pub(crate) fn teardown_attacker_locks_and_cycles_for_destroyed_balls(
    sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>,
    scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, db_opt: &Option<Arc<eve_db::WorldRepository>>,
    pending_opt: Option<&dyn AnyRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>,
    notify_tx_opt: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    hook: Option<&ModuleDeactivationHook>, destroyed_ids: &[BallId],
) {
    let mut ops = PostUnlockOps::default();
    teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting(
        sys_id, bp, cycles, scram, core, db_opt, pending_opt, notify_tx_opt, hook, destroyed_ids, &mut ops,
    );
    drain_test_ops(db_opt, ops, hook, None);
}
