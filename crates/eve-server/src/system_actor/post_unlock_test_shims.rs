//! Legacy `#[cfg(test)]` wrapper shims around `_collecting` `post_unlock` functions so unit tests
//! outside `system_actor/` keep compiling without direct DB writes in production paths (`W2`).
#![allow(clippy::too_many_arguments)]

use super::{post_unlock::{execute_post_unlock_ops, PostUnlockOps}, *};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, SlimItemMeta};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type DbOpt = Option<Arc<eve_db::WorldRepository>>;
type SdeOpt = Option<Arc<eve_sde::SdeRegistry>>;
type TacLock<'a> = &'a dyn AnyRwLock<crate::services::tactical_modules::TacticalModulesState>;

fn drain_test_ops(db_opt: &DbOpt, ops: PostUnlockOps, hook: Option<&ModuleDeactivationHook>, actions: Option<&mut Vec<PendingCycleAction>>) {
    if !ops.is_empty() { let extra = execute_post_unlock_ops(db_opt.as_ref(), None, ops, hook, None); if let Some(out) = actions { out.extend(extra); } }
}

macro_rules! ops_shim {
    ($(#[$m:meta])* fn $n:ident($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)? => $c:ident($db:expr, $hk:expr, $ac:expr $(,$extra:expr)*)) => {
        $(#[$m])* pub(crate) fn $n($($a: $t),*) $(-> $r)? {
            let mut ops = PostUnlockOps::default();
            let res = $c($($extra,)* $($a),*, &mut ops);
            drain_test_ops($db, ops, $hk, $ac);
            res
        }
    };
}

ops_shim!(fn spawn_ball_with_combat_in_actor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &mut HashMap<BallId, i32>, core: &mut HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, hook: Option<&ModuleDeactivationHook>, ball: Ball, cs: CombatState, meta: Option<SlimItemMeta>) -> u64 => spawn_ball_with_combat_in_actor_collecting(db, hook, None));
ops_shim!(fn set_combat_state_in_actor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &mut HashMap<BallId, i32>, core: &mut HashMap<BallId, i32>, last_tot: &mut HashMap<BallId, (Option<i32>, Option<i32>)>, db: &DbOpt, hook: Option<&ModuleDeactivationHook>, ball_id: BallId, cs: CombatState) => set_combat_state_in_actor_collecting(db, hook, None));
ops_shim!(fn reconcile_tackle_and_bubbles(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, db: &DbOpt, hook: Option<&ModuleDeactivationHook>) => reconcile_tackle_and_bubbles_collecting(db, hook, None));
ops_shim!(fn on_target_unlocked_in_actor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, db: &DbOpt, src: BallId, tgt: Option<BallId>, hook: Option<&ModuleDeactivationHook>) => on_target_unlocked_in_actor_collecting(db, hook, None));
ops_shim!(#[allow(dead_code)] fn reconcile_module_cycle_before_activate_in_actor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, hook: Option<&ModuleDeactivationHook>, ship_id: BallId, module_id: i64, now_unix: u64) -> (Option<(i32, Option<BallId>)>, bool) => reconcile_module_cycle_before_activate_in_actor_collecting(db, hook, None));
ops_shim!(fn handle_update_module_overload(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, hook: Option<&ModuleDeactivationHook>, ship_id: BallId, module_id: i64, is_overloaded: bool) -> bool => handle_update_module_overload_collecting(db, hook, None));
ops_shim!(fn apply_weapon_cycle_damage(cycle: &ActiveModuleCycle, bp: &mut Ballpark, rah: &mut HashMap<BallId, [f64; 4]>, sde: &SdeOpt, db: &DbOpt, actions: &mut Vec<PendingCycleAction>, destroyed: &mut Vec<(BallId, Option<CharacterId>)>, outbox: Option<&PendingDestroyedOutbox>) => apply_weapon_cycle_damage_collecting(db, None, Some(actions), bp.solar_system_id));
ops_shim!(fn step_cycles_and_capacitor_with_missiles_and_tidi(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>, heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, epochs: &HashMap<BallId, u64>, proj: &HashMap<BallId, eve_dogma::FitProjection>, ws: &mut HashSet<(BallId, i64)>, launches: &mut Vec<super::PendingMissileLaunch>, rng: &Option<Arc<crate::rng::MechanicRng>>, tac: TacLock<'_>, actions: &mut Vec<PendingCycleAction>, sim_s: f64, dt: f64, tidi: f64, reload_sim: Option<&mut HashMap<i64, ModuleReloadSimEntry>>, hook: Option<&ModuleDeactivationHook>, outbox: Option<&PendingDestroyedOutbox>) -> Vec<(BallId, Option<CharacterId>)> => step_cycles_and_capacitor_with_missiles_and_tidi_collecting(db, hook, Some(actions)));
ops_shim!(fn cleanup_removed_module_cycle(bp: &mut Ballpark, removed: &ActiveModuleCycle, db: &DbOpt) => cleanup_removed_module_cycle_collecting(db, None, None));
ops_shim!(#[allow(dead_code)] fn emit_cycle_deactivation(sys_id: SolarSystemId, cycle: &ActiveModuleCycle, bp: &Ballpark, db: &DbOpt, hook: Option<&ModuleDeactivationHook>) => test_emit_cycle_deactivation_collecting(db, hook, None));
ops_shim!(fn remove_non_warp_ship_module_cycles(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, db: &DbOpt, ship_id: BallId, hook: Option<&ModuleDeactivationHook>) => remove_non_warp_ship_module_cycles_collecting(db, hook, None));
ops_shim!(fn remove_all_ship_module_cycles_preserving_own_db(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>, db: &DbOpt, ship_id: i64, hook: Option<&ModuleDeactivationHook>, preserve_own_db_active_states: bool) -> Vec<(ActiveModuleCycle, Option<i64>)> => remove_all_ship_module_cycles_preserving_own_db_collecting(db, hook, None));
ops_shim!(fn remove_all_ship_module_cycles(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>, db: &DbOpt, ship_id: i64, hook: Option<&ModuleDeactivationHook>) -> Vec<(ActiveModuleCycle, Option<i64>)> => remove_all_ship_module_cycles_collecting(db, hook, None));
ops_shim!(#[allow(dead_code)] fn restore_taken_own_cycles_in_actor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<i64, i32>, core: &HashMap<i64, i32>, db: &DbOpt, ball_id: i64, hook: Option<&ModuleDeactivationHook>, removed_own_cycles: Vec<ActiveModuleCycle>, force_recompute: bool) => restore_taken_own_cycles_in_actor_collecting(db, hook, None));
ops_shim!(#[allow(dead_code)] fn teardown_attacker_locks_and_cycles_for_destroyed_balls(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, db: &DbOpt, pending_opt: Option<&dyn AnyRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>, notify_tx_opt: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>, hook: Option<&ModuleDeactivationHook>, destroyed_ids: &[BallId]) => teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting(db, hook, None));

pub(crate) fn handle_add_module_cycle(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, hook: Option<&ModuleDeactivationHook>, cycle: ActiveModuleCycle) -> Result<(), String> {
    let mut ops = PostUnlockOps::default();
    let res = handle_add_module_cycle_collecting(sys_id, bp, cycles, scram, core, sde, db, hook, cycle, &mut ops);
    if res.is_ok() { drain_test_ops(db, ops, hook, None); }
    res
}
pub(crate) fn step_cycles_and_capacitor(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>, heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, rng: &Option<Arc<crate::rng::MechanicRng>>, tac: TacLock<'_>, actions: &mut Vec<PendingCycleAction>, dt: f64, hook: Option<&ModuleDeactivationHook>) -> Vec<(BallId, Option<CharacterId>)> {
    step_cycles_and_capacitor_with_epochs(sys_id, bp, cycles, disruptors, nanite, heat, rah, scram, core, sde, db, &HashMap::new(), &HashMap::new(), &mut HashSet::new(), rng, tac, actions, dt, hook)
}
pub(crate) fn step_cycles_and_capacitor_with_epochs(sys_id: SolarSystemId, bp: &mut Ballpark, cycles: &mut Vec<ActiveModuleCycle>, disruptors: &mut Vec<PendingAnchoringDisruptor>, nanite: &mut Vec<ActiveNaniteRepair>, heat: &mut HashMap<BallId, [f64; 3]>, rah: &mut HashMap<BallId, [f64; 4]>, scram: &HashMap<BallId, i32>, core: &HashMap<BallId, i32>, sde: &SdeOpt, db: &DbOpt, epochs: &HashMap<BallId, u64>, proj: &HashMap<BallId, eve_dogma::FitProjection>, ws: &mut HashSet<(BallId, i64)>, rng: &Option<Arc<crate::rng::MechanicRng>>, tac: TacLock<'_>, actions: &mut Vec<PendingCycleAction>, dt: f64, hook: Option<&ModuleDeactivationHook>) -> Vec<(BallId, Option<CharacterId>)> {
    step_cycles_and_capacitor_with_missiles_and_tidi(sys_id, bp, cycles, disruptors, nanite, heat, rah, scram, core, sde, db, epochs, proj, ws, &mut Vec::new(), rng, tac, actions, 0.0, dt, 1.0, None, hook, None)
}
fn test_emit_cycle_deactivation_collecting(sys_id: SolarSystemId, cycle: &ActiveModuleCycle, bp: &Ballpark, _db: &DbOpt, hook: Option<&ModuleDeactivationHook>, ops: &mut PostUnlockOps) {
    emit_cycle_deactivation_collecting(sys_id, cycle, bp, hook.is_some(), ops);
}