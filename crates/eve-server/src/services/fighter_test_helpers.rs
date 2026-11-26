//! Canonical test-only façade over fighter runtime state (`#1541` Track F1/F1b/F2).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) access per-system fighter state without touching `ServerContext`
//! or `CombatRuntimeState`, routing seamlessly across live `SystemActor`s and cold SQLite
//! `KIND_FIGHTERS` checkpoints without calling `get_or_spawn`.

#![forbid(unsafe_code)]

use crate::router::context::{ActiveFighterSquadron, ServerContext};
use crate::system_actor::{
    apply_checkpoint_entry_if_absent, collect_fighter_checkpoint_rows, FighterCheckpointEntry,
    FighterTestHelperOp, SystemFighterState,
};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, load_all_checkpoints_for_kind, load_system_checkpoints,
    replace_system_checkpoints_for_kind, KIND_FIGHTERS,
};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet};

fn read_cold_sqlite_states(ctx: &ServerContext, exclude_systems: &HashSet<SolarSystemId>) -> HashMap<SolarSystemId, SystemFighterState> {
    let mut out: HashMap<SolarSystemId, SystemFighterState> = HashMap::new();
    let rows = ctx.db.try_with_sqlite_conn(|c| load_all_checkpoints_for_kind::<FighterCheckpointEntry>(c, KIND_FIGHTERS)).and_then(Result::ok).unwrap_or_default();
    for (sys_id, _, entry) in rows {
        if !exclude_systems.contains(&sys_id) {
            apply_checkpoint_entry_if_absent(out.entry(sys_id).or_default(), entry, &HashMap::new());
        }
    }
    out
}

fn mutate_sqlite_system_state<R>(ctx: &ServerContext, sys_id: SolarSystemId, f: impl FnOnce(&mut SystemFighterState) -> R) -> Option<R> {
    ctx.db.with_sqlite_conn(|conn| {
        let rows = load_system_checkpoints::<FighterCheckpointEntry>(conn, sys_id, KIND_FIGHTERS).unwrap_or_default();
        let mut state = SystemFighterState::default();
        for (_, entry) in rows { apply_checkpoint_entry_if_absent(&mut state, entry, &HashMap::new()); }
        let out = f(&mut state);
        let encoded = collect_fighter_checkpoint_rows(&state, &HashSet::new());
        replace_system_checkpoints_for_kind(conn, sys_id, KIND_FIGHTERS, &encoded, current_unix_secs())?;
        Ok::<R, String>(out)
    }).ok()
}

fn query_all_systems<R>(ctx: &ServerContext, mut f: impl FnMut(SolarSystemId, &SystemFighterState) -> Option<R>) -> Option<R> {
    let handles = ctx.systems.active_system_handles();
    let mut live = HashSet::with_capacity(handles.len());
    for (sys_id, h) in handles {
        let _ = ctx.systems.ensure_live_fighters_hydrated_sync(sys_id, &h);
        live.insert(sys_id);
        if let Some(res) = f(sys_id, &h.view().fighters) { return Some(res); }
    }
    read_cold_sqlite_states(ctx, &live).into_iter().find_map(|(sid, st)| f(sid, &st))
}

fn for_each_system_state(ctx: &ServerContext, mut f: impl FnMut(&SystemFighterState)) {
    let handles = ctx.systems.active_system_handles();
    let mut live = HashSet::with_capacity(handles.len());
    for (sys_id, h) in handles {
        let _ = ctx.systems.ensure_live_fighters_hydrated_sync(sys_id, &h);
        live.insert(sys_id);
        f(&h.view().fighters);
    }
    for (_, st) in read_cold_sqlite_states(ctx, &live) { f(&st); }
}

fn mutate_first_matching_system_with<P, R>(
    ctx: &ServerContext, payload: P, mut pred: impl FnMut(&SystemFighterState) -> bool,
    live_op: impl FnOnce(P, &crate::system_actor::SystemHandle, &SystemFighterState) -> Option<R>,
    cold_fn: impl FnOnce(P, &mut SystemFighterState) -> Option<R>,
) -> Option<R> {
    let handles = ctx.systems.active_system_handles();
    let mut live = HashSet::with_capacity(handles.len());
    for (sys_id, h) in handles {
        let _ = ctx.systems.ensure_live_fighters_hydrated_sync(sys_id, &h);
        live.insert(sys_id);
        let matched = { let v = h.view(); pred(&v.fighters).then(|| v.fighters.clone()) };
        if let Some(snap) = matched { return live_op(payload, &h, &snap); }
    }
    let cold_id = read_cold_sqlite_states(ctx, &live).into_iter().find_map(|(sid, st)| pred(&st).then_some(sid))?;
    mutate_sqlite_system_state(ctx, cold_id, |st| cold_fn(payload, st)).flatten()
}

fn mutate_first_matching_system<R>(
    ctx: &ServerContext, pred: impl FnMut(&SystemFighterState) -> bool,
    live_op: impl FnOnce(&crate::system_actor::SystemHandle, &SystemFighterState) -> Option<R>,
    cold_fn: impl FnOnce(&mut SystemFighterState) -> Option<R>,
) -> Option<R> {
    mutate_first_matching_system_with(ctx, (live_op, cold_fn), pred, |(lo, _), h, s| lo(h, s), |(_, cf), st| cf(st))
}

fn set_in_system(ctx: &ServerContext, sys_id: SolarSystemId, op: FighterTestHelperOp, cold_fn: impl FnOnce(&mut SystemFighterState)) {
    if let Some(h) = ctx.systems.get_handle(sys_id) { let _ = h.test_helper_fighter_op_sync(op); }
    else { let _ = mutate_sqlite_system_state(ctx, sys_id, cold_fn); }
}

fn collect_all_systems<T>(ctx: &ServerContext, mut ext: impl FnMut(&SystemFighterState, &mut Vec<T>)) -> Vec<T> {
    let mut out = Vec::new();
    for_each_system_state(ctx, |fg| ext(fg, &mut out));
    out
}

fn all_systems_satisfy(ctx: &ServerContext, pred: impl Fn(&SystemFighterState) -> bool) -> bool {
    let mut ok = true;
    for_each_system_state(ctx, |fg| if ok && !pred(fg) { ok = false; });
    ok
}

fn clear_all_systems_part(ctx: &ServerContext, make_op: fn() -> FighterTestHelperOp, clear_fn: fn(&mut SystemFighterState)) {
    let handles = ctx.systems.active_system_handles();
    let mut live = HashSet::with_capacity(handles.len());
    for (sys_id, h) in handles {
        live.insert(sys_id);
        let _ = h.test_helper_fighter_op_sync(make_op());
        let _ = ctx.systems.flush_system_fighter_checkpoint_sync(sys_id);
    }
    for sys_id in read_cold_sqlite_states(ctx, &live).into_keys() {
        let _ = mutate_sqlite_system_state(ctx, sys_id, clear_fn);
    }
}

/// Inserts or replaces an active fighter squadron in an explicit solar system.
pub fn insert_active_fighter_in_system(ctx: &ServerContext, sys_id: SolarSystemId, fighter_id: i64, squadron: ActiveFighterSquadron) {
    let sq = squadron.clone();
    set_in_system(ctx, sys_id, FighterTestHelperOp::InsertActive(fighter_id, squadron), |st| { st.active_fighters.insert(fighter_id, sq); });
}

/// Inserts or replaces an active fighter squadron entry by key.
pub fn insert_active_fighter(ctx: &ServerContext, fighter_id: i64, squadron: ActiveFighterSquadron) {
    let sys_id = ctx.systems.resolve_fighter_entity_system_id(&[squadron.item_id, squadron.ball_id, fighter_id, squadron.parent_ship_id]);
    insert_active_fighter_in_system(ctx, sys_id, fighter_id, squadron);
}

/// Returns a cloned snapshot of the active fighter squadron for `fighter_id`.
pub fn active_fighter(ctx: &ServerContext, fighter_id: i64) -> Option<ActiveFighterSquadron> {
    query_all_systems(ctx, |_, fg| fg.active_fighters.get(&fighter_id).cloned())
}

/// Returns `true` if `fighter_id` is currently registered as an active fighter squadron.
pub fn has_active_fighter(ctx: &ServerContext, fighter_id: i64) -> bool { active_fighter(ctx, fighter_id).is_some() }

/// Mutates an existing active fighter squadron entry in place if present.
pub fn update_active_fighter<R>(ctx: &ServerContext, fighter_id: i64, f: impl FnOnce(&mut ActiveFighterSquadron) -> R) -> Option<R> {
    mutate_first_matching_system_with(
        ctx, f, |fg| fg.active_fighters.contains_key(&fighter_id),
        |f, h, snap| {
            let mut sq = snap.active_fighters.get(&fighter_id)?.clone();
            let res = f(&mut sq);
            let _ = h.test_helper_fighter_op_sync(FighterTestHelperOp::InsertActive(fighter_id, sq));
            Some(res)
        },
        |f, st| st.active_fighters.get_mut(&fighter_id).map(f),
    )
}

/// Removes and returns an active fighter squadron entry by key.
pub fn remove_active_fighter(ctx: &ServerContext, fighter_id: i64) -> Option<ActiveFighterSquadron> {
    mutate_first_matching_system(ctx, |fg| fg.active_fighters.contains_key(&fighter_id), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveActive(fighter_id)).removed_squadron, |st| st.active_fighters.remove(&fighter_id))
}

/// Finds an active fighter squadron whose backing inventory `item_id` matches.
pub fn find_active_fighter_by_item(ctx: &ServerContext, item_id: i64) -> Option<ActiveFighterSquadron> {
    query_all_systems(ctx, |_, fg| fg.active_fighters.values().find(|f| f.item_id == item_id).cloned())
}

/// Finds an active fighter squadron launched from `parent_ship_id` and `tube_index`.
pub fn find_active_fighter_by_tube(ctx: &ServerContext, parent_ship_id: i64, tube_index: usize) -> Option<ActiveFighterSquadron> {
    query_all_systems(ctx, |_, fg| fg.active_fighters.values().find(|f| f.parent_ship_id == parent_ship_id && f.tube_index == tube_index).cloned())
}

/// Returns all currently active fighter squadron keys.
pub fn active_fighter_ids(ctx: &ServerContext) -> Vec<i64> { collect_all_systems(ctx, |fg, out| out.extend(fg.active_fighters.keys().copied())) }

/// Returns `true` if every registered active fighter squadron satisfies `pred`.
pub fn all_active_fighter_squadrons(ctx: &ServerContext, mut pred: impl FnMut(&ActiveFighterSquadron) -> bool) -> bool {
    let mut ok = true;
    for_each_system_state(ctx, |fg| if ok && !fg.active_fighters.values().all(&mut pred) { ok = false; });
    ok
}

/// Returns `true` if any registered active fighter squadron satisfies `pred`.
pub fn any_active_fighter_squadron(ctx: &ServerContext, mut pred: impl FnMut(&ActiveFighterSquadron) -> bool) -> bool {
    query_all_systems(ctx, |_, fg| fg.active_fighters.values().any(&mut pred).then_some(())).is_some()
}

/// Returns the number of currently active fighter squadrons.
pub fn active_fighter_count(ctx: &ServerContext) -> usize {
    let mut count = 0usize;
    for_each_system_state(ctx, |fg| count += fg.active_fighters.len());
    count
}

/// Clears all active fighter squadrons.
pub fn clear_active_fighters(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllActive, |st| st.active_fighters.clear()); }

/// Clears all active fighter squadrons (alias for integration test hygiene).
pub fn clear_all_fighter_squadrons(ctx: &ServerContext) { clear_active_fighters(ctx); }

/// Returns the fighter item ID currently loaded in `(ship_id, tube_index)`, if any.
pub fn loaded_fighter_in_tube(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<i64> {
    query_all_systems(ctx, |_, fg| fg.tube_loads.get(&(ship_id, tube_index)).map(|&(fid, _)| fid))
}

/// Returns `true` if `(ship_id, tube_index)` currently has a loaded fighter squadron.
pub fn has_fighter_tube_load(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> bool { loaded_fighter_in_tube(ctx, ship_id, tube_index).is_some() }

/// Sets `(ship_id, tube_index)` load in an explicit solar system.
pub fn set_fighter_tube_load_in_system(ctx: &ServerContext, sys_id: SolarSystemId, ship_id: i64, tube_index: usize, fighter_item_id: i64) {
    let ftype = ctx.db.with_snapshot(|snap| snap.items.get(&fighter_item_id).map(|it| it.type_id)).unwrap_or(0);
    set_in_system(ctx, sys_id, FighterTestHelperOp::SetTubeLoad(ship_id, tube_index, fighter_item_id, ftype), |st| { st.tube_loads.insert((ship_id, tube_index), (fighter_item_id, ftype)); });
}

/// Records `fighter_item_id` as loaded in `(ship_id, tube_index)`.
pub fn set_fighter_tube_load(ctx: &ServerContext, ship_id: i64, tube_index: usize, fighter_item_id: i64) {
    set_fighter_tube_load_in_system(ctx, ctx.systems.resolve_fighter_entity_system_id(&[ship_id, fighter_item_id]), ship_id, tube_index, fighter_item_id);
}

/// Mutates an existing loaded fighter item ID entry for `(ship_id, tube_index)` if present.
pub fn update_fighter_tube_load<R>(ctx: &ServerContext, ship_id: i64, tube_index: usize, f: impl FnOnce(&mut i64) -> R) -> Option<R> {
    mutate_first_matching_system_with(
        ctx, f, |fg| fg.tube_loads.contains_key(&(ship_id, tube_index)),
        |f, h, snap| {
            let (mut fid, ftype) = *snap.tube_loads.get(&(ship_id, tube_index))?;
            let res = f(&mut fid);
            let _ = h.test_helper_fighter_op_sync(FighterTestHelperOp::SetTubeLoad(ship_id, tube_index, fid, ftype));
            Some(res)
        },
        |f, st| st.tube_loads.get_mut(&(ship_id, tube_index)).map(|(fid, _)| f(fid)),
    )
}

/// Removes any loaded fighter item entry for `(ship_id, tube_index)`.
pub fn remove_fighter_tube_load(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<i64> {
    mutate_first_matching_system(ctx, |fg| fg.tube_loads.contains_key(&(ship_id, tube_index)), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveTubeLoad(ship_id, tube_index)).removed_i64, |st| st.tube_loads.remove(&(ship_id, tube_index)).map(|(fid, _)| fid))
}

/// Returns all `(ship_id, tube_index)` keys currently present in tube loads.
pub fn fighter_tube_load_keys(ctx: &ServerContext) -> Vec<(i64, usize)> { collect_all_systems(ctx, |fg, out| out.extend(fg.tube_loads.keys().copied())) }

/// Returns all `((ship_id, tube_index), (fighter_item_id, fighter_type_id))` entries currently present in tube loads.
pub fn fighter_tube_load_entries(ctx: &ServerContext) -> Vec<((i64, usize), (i64, u32))> { collect_all_systems(ctx, |fg, out| out.extend(fg.tube_loads.iter().map(|(&k, &v)| (k, v)))) }

/// Clears all fighter tube load entries.
pub fn clear_all_fighter_tubes(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllTubes, |st| st.tube_loads.clear()); }

/// Returns the Unix timestamp until which `(fighter_id, slot_id)` is on cooldown.
pub fn fighter_ability_cooldown(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u64> { query_all_systems(ctx, |_, fg| fg.cooldown_state.get(&(fighter_id, slot_id)).copied()) }

/// Sets the Unix cooldown expiration timestamp for `(fighter_id, slot_id)` in an explicit system.
pub fn set_fighter_ability_cooldown_in_system(ctx: &ServerContext, sys_id: SolarSystemId, fighter_id: i64, slot_id: u32, ready_at_unix: u64) {
    set_in_system(ctx, sys_id, FighterTestHelperOp::SetCooldown(fighter_id, slot_id, ready_at_unix), |st| { st.cooldown_state.insert((fighter_id, slot_id), ready_at_unix); });
}

/// Sets the Unix cooldown expiration timestamp for `(fighter_id, slot_id)`.
pub fn set_fighter_ability_cooldown(ctx: &ServerContext, fighter_id: i64, slot_id: u32, ready_at_unix: u64) {
    set_fighter_ability_cooldown_in_system(ctx, ctx.systems.resolve_fighter_entity_system_id(&[fighter_id]), fighter_id, slot_id, ready_at_unix);
}

/// Clears the cooldown entry for `(fighter_id, slot_id)`.
pub fn clear_fighter_ability_cooldown(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u64> {
    mutate_first_matching_system(ctx, |fg| fg.cooldown_state.contains_key(&(fighter_id, slot_id)), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveCooldown(fighter_id, slot_id)).removed_u64, |st| st.cooldown_state.remove(&(fighter_id, slot_id)))
}

/// Returns `true` if no fighter ability cooldown entries are active.
pub fn fighter_cooldowns_is_empty(ctx: &ServerContext) -> bool { all_systems_satisfy(ctx, |fg| fg.cooldown_state.is_empty()) }

/// Returns all `((fighter_id, slot_id), ready_at_unix)` cooldown entries.
pub fn fighter_cooldown_entries(ctx: &ServerContext) -> Vec<((i64, u32), u64)> { collect_all_systems(ctx, |fg, out| out.extend(fg.cooldown_state.iter().map(|(&k, &v)| (k, v)))) }

/// Clears all fighter ability cooldown entries.
pub fn clear_all_fighter_cooldowns(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllCooldowns, |st| st.cooldown_state.clear()); }

/// Returns the number of consumed ability charges for `(fighter_id, slot_id)`.
pub fn fighter_ability_charges_used(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u32> { query_all_systems(ctx, |_, fg| fg.charge_state.get(&(fighter_id, slot_id)).copied()) }

/// Sets consumed ability charges for `(fighter_id, slot_id)` in an explicit solar system.
pub fn set_fighter_ability_charges_used_in_system(ctx: &ServerContext, sys_id: SolarSystemId, fighter_id: i64, slot_id: u32, used: u32) {
    set_in_system(ctx, sys_id, FighterTestHelperOp::SetCharge(fighter_id, slot_id, used), |st| { st.charge_state.insert((fighter_id, slot_id), used); });
}

/// Sets the number of consumed ability charges for `(fighter_id, slot_id)`.
pub fn set_fighter_ability_charges_used(ctx: &ServerContext, fighter_id: i64, slot_id: u32, used: u32) {
    set_fighter_ability_charges_used_in_system(ctx, ctx.systems.resolve_fighter_entity_system_id(&[fighter_id]), fighter_id, slot_id, used);
}

/// Clears the consumed ability charge entry for `(fighter_id, slot_id)`.
pub fn clear_fighter_ability_charges(ctx: &ServerContext, fighter_id: i64, slot_id: u32) -> Option<u32> {
    mutate_first_matching_system(ctx, |fg| fg.charge_state.contains_key(&(fighter_id, slot_id)), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveCharge(fighter_id, slot_id)).removed_u32, |st| st.charge_state.remove(&(fighter_id, slot_id)))
}

/// Returns `true` if no fighter ability charge usage entries are recorded.
pub fn fighter_charges_is_empty(ctx: &ServerContext) -> bool { all_systems_satisfy(ctx, |fg| fg.charge_state.is_empty()) }

/// Returns all `((fighter_id, slot_id), used)` charge usage entries.
pub fn fighter_charge_entries(ctx: &ServerContext) -> Vec<((i64, u32), u32)> { collect_all_systems(ctx, |fg, out| out.extend(fg.charge_state.iter().map(|(&k, &v)| (k, v)))) }

/// Clears all fighter ability charge usage entries.
pub fn clear_all_fighter_charges(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllCharges, |st| st.charge_state.clear()); }

/// Returns the accumulated unit damage for `fighter_ball_id`, if any.
pub fn fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> Option<f64> { query_all_systems(ctx, |_, fg| fg.unit_damage_accum.get(&fighter_ball_id).copied()) }

/// Returns `true` if `fighter_ball_id` has an entry in the unit damage accumulator.
pub fn has_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> bool { fighter_damage_accum(ctx, fighter_ball_id).is_some() }

/// Sets accumulated unit damage for `fighter_ball_id` in an explicit solar system.
pub fn set_fighter_unit_damage_accum_in_system(ctx: &ServerContext, sys_id: SolarSystemId, fighter_ball_id: i64, damage: f64) {
    set_in_system(ctx, sys_id, FighterTestHelperOp::SetDamageAccum(fighter_ball_id, damage), |st| { st.unit_damage_accum.insert(fighter_ball_id, damage); });
}

/// Sets the accumulated unit damage for `fighter_ball_id`.
pub fn set_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64, damage: f64) {
    set_fighter_unit_damage_accum_in_system(ctx, ctx.systems.resolve_fighter_entity_system_id(&[fighter_ball_id]), fighter_ball_id, damage);
}

/// Removes and returns the accumulated unit damage entry for `fighter_ball_id`.
pub fn remove_fighter_damage_accum(ctx: &ServerContext, fighter_ball_id: i64) -> Option<f64> {
    mutate_first_matching_system(ctx, |fg| fg.unit_damage_accum.contains_key(&fighter_ball_id), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveDamageAccum(fighter_ball_id)).removed_f64, |st| st.unit_damage_accum.remove(&fighter_ball_id))
}

/// Clears all fighter unit damage accumulator entries.
pub fn clear_all_fighter_damage_accum(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllDamageAccum, |st| st.unit_damage_accum.clear()); }

/// Returns the Unix timestamp until which `(ship_id, tube_index)` is rearming/refueling, if any.
pub fn fighter_tube_rearm_unix(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<u64> { query_all_systems(ctx, |_, fg| fg.tube_rearm_until.get(&(ship_id, tube_index)).copied()) }

/// Sets rearm/refuel expiration timestamp for `(ship_id, tube_index)` in an explicit solar system.
pub fn set_fighter_tube_rearm_until_in_system(ctx: &ServerContext, sys_id: SolarSystemId, ship_id: i64, tube_index: usize, ready_at_unix: u64) {
    set_in_system(ctx, sys_id, FighterTestHelperOp::SetTubeRearm(ship_id, tube_index, ready_at_unix), |st| { st.tube_rearm_until.insert((ship_id, tube_index), ready_at_unix); });
}

/// Sets the Unix rearm/refuel expiration timestamp for `(ship_id, tube_index)`.
pub fn set_fighter_tube_rearm_unix(ctx: &ServerContext, ship_id: i64, tube_index: usize, ready_at_unix: u64) {
    set_fighter_tube_rearm_until_in_system(ctx, ctx.systems.resolve_fighter_entity_system_id(&[ship_id]), ship_id, tube_index, ready_at_unix);
}

/// Removes and returns the rearm/refuel expiration timestamp for `(ship_id, tube_index)`.
pub fn clear_fighter_tube_rearm_unix(ctx: &ServerContext, ship_id: i64, tube_index: usize) -> Option<u64> {
    mutate_first_matching_system(ctx, |fg| fg.tube_rearm_until.contains_key(&(ship_id, tube_index)), |h, _| h.test_helper_fighter_op_sync(FighterTestHelperOp::RemoveTubeRearm(ship_id, tube_index)).removed_u64, |st| st.tube_rearm_until.remove(&(ship_id, tube_index)))
}

/// Clears all fighter tube rearm/refuel timers.
pub fn clear_all_fighter_tube_rearm_timers(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllTubeRearm, |st| st.tube_rearm_until.clear()); }

/// Clears all per-system fighter state across live actors and persisted SQLite `KIND_FIGHTERS`.
pub fn clear_all_fighter_state(ctx: &ServerContext) { clear_all_systems_part(ctx, || FighterTestHelperOp::ClearAllState, |st| st.clear_all()); }
