//! Ordered FIFO post-unlock DB persistence and hook execution for `system_actor` (`#1170`).
//!
//! Producers collect typed [`PostUnlockIntent`] entries while holding `RANK_BALLPARK` (`20`) /
//! `RANK_ACTOR_STATE` (`21`) locks and send each item onto the actor's FIFO `post_unlock_tx`
//! queue **before releasing the actor locks** (W1), guaranteeing DB write order matches actor
//! memory order without per-table sequence watermarks.

#![forbid(unsafe_code)]

use eve_db::lock_rank::{held_lock_ranks, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK, RANK_LEAF, RANK_WORLD_INNER};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord, TxError, WorldRepository};
use eve_destiny::BallId;
use eve_sde::{attr_ids, SdeRegistry, SolarSystemId, TypeId};
use std::{collections::HashMap, sync::{atomic::{AtomicU64, AtomicUsize, Ordering}, Arc, OnceLock}};

use super::types::{
    ActiveModuleCycle, CharacterId, ConditionCheckpointEntry, ItemId, ModuleDeactivationHook,
    PendingCycleAction, PostUnlockHookAction, SystemActorState, TargetLockCompletedHook,
};

#[derive(Debug, Clone)]
pub(crate) struct MineAsteroidDbUpdate { pub asteroid_id: BallId, pub belt_id: BallId, pub remaining_units: u64, pub belt_due_unix: Option<u64> }

#[derive(Debug, Clone)]
pub(crate) struct CombatHpDbUpdate { pub ball_id: BallId, pub combat_state: eve_destiny::CombatState, pub expected_system: Option<SolarSystemId>, pub require_persisted_entity: bool }

#[derive(Debug, Clone)]
pub(crate) struct ModuleCycleCleanupDbOp {
    pub ship_id: BallId, pub module_id: ItemId, pub linked_turrets: Vec<ItemId>,
    pub clear_adc_ehe_attrs: bool, pub clear_mutadaptive_spool: bool, pub clear_rah_attrs: bool,
}

impl ModuleCycleCleanupDbOp {
    pub(crate) fn from_cycle(removed: &ActiveModuleCycle) -> Self {
        Self {
            ship_id: removed.ship_id, module_id: removed.module_id, linked_turrets: removed.linked_turrets.clone(),
            clear_adc_ehe_attrs: removed.is_adc || removed.ehe_hull_resists.is_some(),
            clear_mutadaptive_spool: removed.mutadaptive_spool_step > 0.0, clear_rah_attrs: removed.is_rah,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NaniteRepairTickDbOp {
    pub char_id: CharacterId, pub ship_id: BallId, pub module_id: ItemId, pub paste_delta: i32,
    pub initial_damage_hp: f64, pub paste_total_needed: i32, pub rem_dmg: f64, pub done_now: bool, pub halted_for_paste: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NaniteRepairCancelDbOp { pub char_id: CharacterId, pub ship_id: BallId, pub module_id: ItemId, pub rem_dmg: f64, pub refund_paste: bool, pub refund: i32 }

#[derive(Debug, Clone)]
pub(crate) struct ModuleReloadDbOp { pub finished: Vec<ItemId>, pub desired_updates: Vec<(ItemId, u64, bool)> }

#[derive(Debug, Clone)]
pub(crate) struct LauncherConsumeDbOp {
    pub char_id: Option<CharacterId>,
    pub ship_id: BallId,
    pub launchers: Vec<ItemId>,
    pub ammo_type_id: Option<TypeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct CycleReloadDbSpec {
    pub ammo_type_id: TypeId,
    pub linked_turrets: Vec<ItemId>,
    pub master_module_id: ItemId,
    pub actual_per_mod: i32,
    pub reload_time_ms: f64,
    pub reload_until: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CycleAmmoStepDbOp {
    pub char_id: Option<CharacterId>,
    pub ship_id: BallId,
    pub linked_turrets: Vec<ItemId>,
    pub ammo_type_id: Option<TypeId>,
    pub missile_charge_type_id: Option<TypeId>,
    pub damage_vector_total: f64,
    pub crystal_volatility_chance: f64,
    pub crystal_volatility_damage: f64,
    pub pre_consumed_mode: bool,
    pub consume_pre_round_for_next_cycle: bool,
    pub reload_spec: Option<CycleReloadDbSpec>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShipChargeConsumeKind {
    Asb,
    AarPaste { paste_rate: i32 },
    CapBooster,
}

#[derive(Debug, Clone)]
pub(crate) struct ShipChargeConsumeDbOp {
    pub char_id: Option<CharacterId>,
    pub ship_id: BallId,
    pub module_id: ItemId,
    pub kind: ShipChargeConsumeKind,
}

#[derive(Debug, Clone)]
pub(crate) struct DeferredDeactivationHookOp {
    pub solar_system_id: SolarSystemId,
    pub char_id: Option<CharacterId>,
    pub fallback_owner_id: Option<CharacterId>,
    pub cycle: ActiveModuleCycle,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeferredTargetLockHookOp {
    pub char_id: CharacterId,
    pub target_id: BallId,
}

/// Ordered persistence or post-commit hook intent executed strictly in issue order (`W5`, `#1170`).
#[derive(Debug, Clone)]
pub(crate) enum PostUnlockIntent {
    BeltAsteroidInsertIfAbsent(AsteroidRecord),
    BeltAsteroidUpsert(AsteroidRecord),
    BeltRespawnDueUpsert { belt_id: BallId, due_unix: u64 },
    BeltRespawnDueClear(BallId),
    MineAsteroid(MineAsteroidDbUpdate),
    ItemPosition {
        ball_id: BallId,
        pos: [f64; 3],
        solar_system_id: SolarSystemId,
        space_entry_epoch: Option<u64>,
    },
    ShipCombatHp(CombatHpDbUpdate),
    RepairTetherModuleHeal { ship_ids: Vec<BallId>, heal_hp: f64 },
    NaniteRepairTick(NaniteRepairTickDbOp),
    NaniteRepairCancel(NaniteRepairCancelDbOp),
    MissileLauncherConsume(LauncherConsumeDbOp),
    TransientAttrSets { item_id: ItemId, attrs: Vec<(u16, f64)> },
    RahResonances { ship_id: BallId, module_id: ItemId, resonances: [f64; 4] },
    ModuleBurnout { ship_id: BallId, module_id: ItemId },
    CycleAmmoStep(CycleAmmoStepDbOp),
    ShipChargeConsume(ShipChargeConsumeDbOp),
    ModuleReload(ModuleReloadDbOp),
    ModuleCycleCleanup(ModuleCycleCleanupDbOp),
    ShipCycleClearAll(BallId),
    RemovedShipBall(BallId),
    ConditionCheckpoint {
        solar_system_id: SolarSystemId,
        entries: Vec<ConditionCheckpointEntry>,
        written_out: Option<Arc<AtomicUsize>>,
    },
    DeactivationHook(DeferredDeactivationHookOp),
    TargetLockHook(DeferredTargetLockHookOp),
}

impl PostUnlockIntent {
    fn writes_db(&self) -> bool {
        !matches!(self, Self::DeactivationHook(_) | Self::TargetLockHook(_))
    }
}

/// Ordered queue item collected under actor locks (`RANK_BALLPARK` / `RANK_ACTOR_STATE`) (`#1170`).
#[derive(Debug, Clone, Default)]
pub(crate) struct PostUnlockOps {
    pub intents: Vec<PostUnlockIntent>,
    pub item_qty_deltas: Vec<(ItemId, i32)>,
    pub crystal_dmg_deltas: Vec<(ItemId, f64)>,
}

impl PostUnlockOps {
    pub(crate) fn is_empty(&self) -> bool { self.intents.is_empty() }
    pub(crate) fn push(&mut self, intent: PostUnlockIntent) { self.intents.push(intent); }
    pub(crate) fn push_item_positions<I>(&mut self, updates: I)
    where
        I: IntoIterator<Item = (BallId, [f64; 3], SolarSystemId, Option<u64>)>,
    {
        for (ball_id, pos, solar_system_id, space_entry_epoch) in updates {
            self.intents.push(PostUnlockIntent::ItemPosition {
                ball_id, pos, solar_system_id, space_entry_epoch,
            });
        }
    }
    pub(crate) fn push_ship_combat_hp(&mut self, update: CombatHpDbUpdate) {
        self.intents.push(PostUnlockIntent::ShipCombatHp(update));
    }
}

fn assert_no_actor_locks_before_db_tx() {
    eve_db::lock_rank::assert_locks_disallowed(&[RANK_BALLPARK, RANK_ACTOR_STATE], RANK_WORLD_INNER, "system_actor post-unlock DB flush invoked while holding RANK_BALLPARK or RANK_ACTOR_STATE");
}

fn set_module_damage_in_snap(snap: &mut eve_db::WorldStateSnapshot, module_id: ItemId, dmg: f64) {
    if let Some(m_item) = snap.get_any_item_mut(module_id) {
        if dmg <= 1e-6 { m_item.dynamic_attributes.remove(&attr_ids::DAMAGE); } else { m_item.dynamic_attributes.insert(attr_ids::DAMAGE, dmg); }
    }
}

fn push_charge_diff_action(out_actions: &mut Vec<PendingCycleAction>, char_hint: Option<CharacterId>, ship_id: BallId, diffs: Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)>) {
    if diffs.is_empty() { return; }
    let eff_char_id = char_hint.filter(|&c| c > 0).or_else(|| diffs.iter().find_map(|(pre, post)| pre.as_ref().or(post.as_ref()).map(|r| r.owner_id)).filter(|&c| c > 0));
    if let Some(cid) = eff_char_id { out_actions.push(PendingCycleAction::CycleChargeItemMutations { char_id: cid, ship_id, diffs }); }
}

fn apply_module_cycle_cleanup_in_snap(snap: &mut eve_db::WorldStateSnapshot, op: &ModuleCycleCleanupDbOp) {
    if op.clear_adc_ehe_attrs { if let Some(tr) = snap.transient_item_attributes.get_mut(&op.ship_id) { for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112, 113] { tr.remove(&aid); } } }
    if op.clear_mutadaptive_spool { if let Some(tr) = snap.transient_item_attributes.get_mut(&op.module_id) { tr.remove(&84); } }
    if op.clear_rah_attrs { for id in [op.module_id, op.ship_id] { if let Some(tr) = snap.transient_item_attributes.get_mut(&id) { for aid in 267..=270 { tr.remove(&aid); } } } }
    snap.clear_module_active_state(op.module_id);
    for &tid in &op.linked_turrets { snap.module_active_charges.remove(&tid); }
}

fn apply_nanite_repair_tick_in_snap(snap: &mut eve_db::WorldStateSnapshot, tick: &NaniteRepairTickDbOp) {
    let key = (tick.ship_id, tick.module_id);
    let prior_shortfall = snap.sp6_lifecycle.nanite_paste_shortfall.get(&key).copied().unwrap_or(0).max(0);
    let total_due = prior_shortfall.saturating_add(tick.paste_delta.max(0));
    let deducted = if total_due > 0 {
        crate::services::tactical_module_mechanics::deduct_paste_from_cargo_in_snap(
            snap, tick.char_id, tick.ship_id, total_due,
        )
    } else {
        0
    };
    let rem_shortfall = (total_due - deducted).max(0);
    if rem_shortfall == 0 {
        snap.sp6_lifecycle.nanite_paste_shortfall.remove(&key);
        set_module_damage_in_snap(snap, tick.module_id, tick.rem_dmg);
    } else {
        if tick.done_now || tick.halted_for_paste {
            snap.sp6_lifecycle.nanite_paste_shortfall.remove(&key);
        } else {
            snap.sp6_lifecycle.nanite_paste_shortfall.insert(key, rem_shortfall);
        }
        if deducted > 0 {
            if let Some(cur_dmg) = snap.items.get(&tick.module_id).and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied()) {
                if cur_dmg > tick.rem_dmg {
                    let hp_per_paste = tick.initial_damage_hp.max(0.0) / (tick.paste_total_needed.max(1) as f64);
                    let scaled_rem = (cur_dmg - (deducted as f64) * hp_per_paste).max(tick.rem_dmg);
                    set_module_damage_in_snap(snap, tick.module_id, scaled_rem);
                }
            }
        }
    }
}

fn apply_nanite_repair_cancel_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&Arc<SdeRegistry>>,
    cancel: &NaniteRepairCancelDbOp,
) {
    let shortfall = snap.sp6_lifecycle.nanite_paste_shortfall.remove(&(cancel.ship_id, cancel.module_id)).unwrap_or(0);
    let safe_refund = (cancel.refund - shortfall).max(0);
    let next_dmg = if shortfall > 0 {
        snap.items
            .get(&cancel.module_id)
            .and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
            .map_or(cancel.rem_dmg, |cur| cur.max(cancel.rem_dmg))
    } else {
        cancel.rem_dmg
    };
    set_module_damage_in_snap(snap, cancel.module_id, next_dmg);
    if cancel.refund_paste && safe_refund > 0 {
        if let Some(sde) = sde_opt {
            crate::services::tactical_module_mechanics::refund_paste_to_cargo_capped_in_snap(
                snap, sde, cancel.char_id, cancel.ship_id, safe_refund,
            );
        }
    }
}

fn collect_item_diffs(
    snap: &eve_db::WorldStateSnapshot,
    mut pre_items: HashMap<i64, Option<InventoryItemRecord>>,
    sort_created_last: bool,
) -> Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> {
    let mut keys: Vec<i64> = pre_items.keys().copied().collect();
    if sort_created_last {
        keys.sort_by_key(|id| (pre_items.get(id).and_then(|o| o.as_ref()).is_none(), *id));
    }
    let mut diffs = Vec::new();
    for id in keys {
        let pre = pre_items.remove(&id).flatten();
        let post = snap.items.get(&id).cloned();
        if pre != post {
            diffs.push((pre, post));
        }
    }
    diffs
}

fn apply_launcher_consume_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    op: &LauncherConsumeDbOp,
) -> Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> {
    let mut chosen = Vec::with_capacity(op.launchers.len());
    for &lid in &op.launchers {
        let Some(l_flag) = snap.items.get(&lid).map(|i| i.flag) else { return Vec::new(); };
        let Some(cid) = snap.items.values()
            .find(|i| i.location_id == op.ship_id && i.flag == l_flag && i.item_id != lid
                && (op.ammo_type_id.is_none() || Some(i.type_id) == op.ammo_type_id) && i.quantity > 0)
            .map(|i| i.item_id)
        else { return Vec::new(); };
        chosen.push(cid);
    }
    let mut pre_items: HashMap<i64, Option<InventoryItemRecord>> = HashMap::new();
    for cid in chosen {
        pre_items.entry(cid).or_insert_with(|| snap.items.get(&cid).cloned());
        let _ = snap.decrement_or_remove_charge_item(cid, 1);
    }
    collect_item_diffs(snap, pre_items, false)
}

fn apply_cycle_ammo_step_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    op: &CycleAmmoStepDbOp,
) -> Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> {
    let mut pre_items: HashMap<i64, Option<InventoryItemRecord>> = HashMap::new();
    let mut crystal_shattered_empty = false;

    for &turret_id in &op.linked_turrets {
        let Some(t_flag) = snap.items.get(&turret_id).map(|i| i.flag) else { continue; };
        let charge_id_opt = snap.items.values()
            .find(|i| i.location_id == op.ship_id && i.flag == t_flag && i.item_id != turret_id
                && (op.ammo_type_id.is_none() || Some(i.type_id) == op.ammo_type_id) && i.quantity > 0)
            .map(|i| i.item_id);
        if let Some(cid) = charge_id_opt {
            pre_items.entry(cid).or_insert_with(|| snap.items.get(&cid).cloned());
            if op.crystal_volatility_chance > 0.0 && op.crystal_volatility_damage > 0.0 {
                if snap.step_volatile_crystal_charge(cid, op.crystal_volatility_damage, attr_ids::DAMAGE, attr_ids::HP_STRUCT) {
                    snap.module_active_charges.remove(&turret_id);
                    crystal_shattered_empty = true;
                }
            } else if op.crystal_volatility_damage >= 0.0 {
                if op.pre_consumed_mode {
                    if op.consume_pre_round_for_next_cycle {
                        let _ = snap.decrement_or_remove_charge_item(cid, 1);
                    }
                } else if snap.decrement_or_remove_charge_item(cid, 1) {
                    snap.module_active_charges.remove(&turret_id);
                    crystal_shattered_empty = true;
                }
            }
        } else if op.ammo_type_id.or(op.missile_charge_type_id).is_some() && op.damage_vector_total > 0.0 {
            snap.items.retain(|_, i| !(i.location_id == op.ship_id && i.flag == t_flag && i.item_id != turret_id && i.quantity <= 0));
            snap.module_active_charges.remove(&turret_id);
            crystal_shattered_empty = true;
        }
    }

    if crystal_shattered_empty && op.crystal_volatility_damage == 0.0 {
        if let Some(spec) = &op.reload_spec {
            let num_mods = spec.linked_turrets.len().max(1) as i32;
            let cargo_stack_opt = snap.items.values()
                .filter(|i| i.location_id == op.ship_id
                    && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
                    && i.type_id == spec.ammo_type_id && i.quantity > 0)
                .max_by_key(|i| i.quantity)
                .map(|i| (i.item_id, i.owner_id, i.quantity));
            if let Some((stack_id, owner_id, avail_qty)) = cargo_stack_opt {
                if avail_qty >= num_mods {
                    pre_items.entry(stack_id).or_insert_with(|| snap.items.get(&stack_id).cloned());
                    let actual_per_mod = spec.actual_per_mod.min(avail_qty / num_mods).max(1);
                    snap.deduct_cargo_ammo_for_reload(stack_id, actual_per_mod * num_mods);
                    for &tid in &spec.linked_turrets {
                        if let Some(tf) = snap.items.get(&tid).map(|i| i.flag) {
                            let mut new_cid = snap.alloc_entity_id();
                            while snap.items.contains_key(&new_cid) { new_cid = snap.alloc_entity_id(); }
                            pre_items.insert(new_cid, None);
                            snap.insert_loaded_charge_item(InventoryItemRecord {
                                item_id: new_cid, type_id: spec.ammo_type_id, owner_id,
                                location_id: op.ship_id, flag: tf, quantity: actual_per_mod,
                                singleton: false, version: 1, ..Default::default()
                            });
                            snap.module_active_charges.insert(tid, spec.ammo_type_id);
                        }
                        snap.cluster_and_runtime.module_reload_until_unix.insert(tid, spec.reload_until);
                    }
                    snap.cluster_and_runtime.auto_reloaded_event.insert(
                        spec.master_module_id,
                        (spec.linked_turrets.clone(), spec.ammo_type_id, spec.reload_time_ms),
                    );
                }
            }
        }
    }
    collect_item_diffs(snap, pre_items, true)
}

fn apply_ship_charge_consume_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    op: &ShipChargeConsumeDbOp,
) -> Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> {
    let Some(m_flag) = snap.items.get(&op.module_id).map(|i| i.flag) else { return Vec::new(); };
    let (dec_qty, only_paste) = match op.kind {
        ShipChargeConsumeKind::Asb | ShipChargeConsumeKind::CapBooster => (1, false),
        ShipChargeConsumeKind::AarPaste { paste_rate } => (paste_rate.max(1), true),
    };
    let Some(cid) = snap.items.values()
        .find(|i| i.location_id == op.ship_id && i.flag == m_flag && i.item_id != op.module_id
            && (!only_paste || i.type_id == 28668) && i.quantity >= dec_qty)
        .map(|i| i.item_id)
    else { return Vec::new(); };
    let pre = snap.items.get(&cid).cloned();
    let _ = snap.decrement_or_remove_charge_item(cid, dec_qty);
    let post = snap.items.get(&cid).cloned();
    if pre != post { vec![(pre, post)] } else { Vec::new() }
}

fn apply_module_reload_in_snap(snap: &mut eve_db::WorldStateSnapshot, reload_op: &ModuleReloadDbOp) {
    for &mod_id in &reload_op.finished {
        snap.cluster_and_runtime.module_reload_until_unix.remove(&mod_id);
    }
    for &(mod_id, new_until, is_dilated) in &reload_op.desired_updates {
        let cur = snap.cluster_and_runtime.module_reload_until_unix.get(&mod_id).copied();
        if cur.is_some_and(|c| is_dilated || c > new_until) || (is_dilated && cur.is_none()) {
            snap.cluster_and_runtime.module_reload_until_unix.insert(mod_id, new_until);
        }
    }
}

pub(crate) enum NaniteTickCollect {
    Applied { halted_for_paste: bool, effective_progress: f64 },
    Defer,
}

pub(crate) fn collect_nanite_repair_tick(
    db: &WorldRepository,
    nr: &mut super::types::ActiveNaniteRepair,
    next_elapsed: f64,
    next_progress: f64,
    target_consumed: i32,
    virtual_paste_used: &mut HashMap<BallId, i32>,
    post_unlock: &mut PostUnlockOps,
) -> NaniteTickCollect {
    let total_needed = nr.paste_total_needed.max(1);
    let mut halted_for_paste = false;
    let mut effective_progress = next_progress;
    let mut delta_taken = 0;
    if target_consumed > nr.paste_consumed_so_far {
        let delta = target_consumed - nr.paste_consumed_so_far;
        let used_before = virtual_paste_used.get(&nr.ship_id).copied().unwrap_or(0);
        let Some(avail_total) = db.try_with_snapshot(|snap| {
            snap.items.values()
                .filter(|i| i.owner_id == nr.char_id && i.location_id == nr.ship_id
                    && i.flag == InventoryFlag::Cargo && i.type_id == 28668 && i.quantity > 0
                    && !snap.is_item_locked_in_contract_or_job(i.item_id))
                .map(|i| i.quantity).sum::<i32>()
        }) else {
            return NaniteTickCollect::Defer;
        };
        let taken = delta.min((avail_total - used_before).max(0));
        delta_taken = taken;
        if taken > 0 {
            *virtual_paste_used.entry(nr.ship_id).or_insert(0) += taken;
        }
        nr.paste_consumed_so_far += taken;
        if taken < delta {
            halted_for_paste = true;
            effective_progress = ((nr.paste_consumed_so_far as f64) / (total_needed as f64)).clamp(0.0, 1.0);
        }
    }
    let done_now = !halted_for_paste && (next_elapsed + 1e-6 >= nr.duration_s);
    let rem_dmg = if done_now { 0.0 } else { nr.initial_damage_hp * (1.0 - effective_progress).clamp(0.0, 1.0) };
    post_unlock.push(PostUnlockIntent::NaniteRepairTick(NaniteRepairTickDbOp {
        char_id: nr.char_id,
        ship_id: nr.ship_id,
        module_id: nr.module_id,
        paste_delta: delta_taken,
        initial_damage_hp: nr.initial_damage_hp,
        paste_total_needed: total_needed,
        rem_dmg,
        done_now,
        halted_for_paste,
    }));
    NaniteTickCollect::Applied { halted_for_paste, effective_progress }
}

pub(crate) fn collect_launcher_consume(
    db: &WorldRepository,
    eff_char_id: Option<CharacterId>,
    ship_id: BallId,
    launchers: &[ItemId],
    ammo_type_id: Option<TypeId>,
    virtual_item_deltas: &mut HashMap<ItemId, i32>,
    post_unlock: &mut PostUnlockOps,
) -> bool {
    let Some(chosen) = db.with_snapshot(|snap| {
        let mut local_used: HashMap<ItemId, i32> = HashMap::new();
        let mut chosen = Vec::with_capacity(launchers.len());
        for &lid in launchers {
            let l_flag = snap.items.get(&lid).map(|i| i.flag)?;
            let cid = snap.items.values()
                .filter_map(|i| {
                    let q = i.quantity
                        + virtual_item_deltas.get(&i.item_id).copied().unwrap_or(0)
                        - local_used.get(&i.item_id).copied().unwrap_or(0);
                    (i.location_id == ship_id && i.flag == l_flag && i.item_id != lid
                        && (ammo_type_id.is_none() || Some(i.type_id) == ammo_type_id) && q > 0)
                        .then_some(i.item_id)
                })
                .next()?;
            *local_used.entry(cid).or_insert(0) += 1;
            chosen.push(cid);
        }
        Some(chosen)
    }) else {
        return false;
    };
    for cid in chosen {
        *virtual_item_deltas.entry(cid).or_insert(0) -= 1;
        post_unlock.item_qty_deltas.push((cid, -1));
    }
    post_unlock.push(PostUnlockIntent::MissileLauncherConsume(LauncherConsumeDbOp {
        char_id: eff_char_id, ship_id, launchers: launchers.to_vec(), ammo_type_id,
    }));
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_cycle_ammo_step(
    db: &WorldRepository,
    cycle: &ActiveModuleCycle,
    eff_char_id: Option<CharacterId>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    sim_elapsed_secs: f64,
    tidi_factor: f64,
    burned_out: bool,
    target_destroyed_in_cycle: bool,
    guided_next_out_of_range: bool,
    reload_sim_remaining: &mut Option<&mut HashMap<i64, super::types::ModuleReloadSimEntry>>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    virtual_item_deltas: &mut HashMap<ItemId, i32>,
    virtual_crystal_dmg_deltas: &mut HashMap<ItemId, f64>,
    post_unlock: &mut PostUnlockOps,
) -> (bool, bool, f64) {
    let ammo_is_non_volatile_crystal = cycle.ammo_type_id
        .and_then(|tid| sde_opt.as_ref().and_then(|sde| sde.types.get(&tid)))
        .is_some_and(|t| matches!(t.group_id, 86 | 374 | 375 | 482 | 663))
        && cycle.crystal_volatility_damage <= 0.0;
    if cycle.linked_turrets.is_empty() || cycle.crystal_volatility_damage < 0.0
        || ammo_is_non_volatile_crystal || guided_next_out_of_range
    {
        return (false, false, 0.0);
    }

    let pre_consumed_mode = cycle.crystal_volatility_chance < 0.0;
    let consume_pre_round_for_next_cycle = pre_consumed_mode && cycle.repeat > 0 && !burned_out && !target_destroyed_in_cycle;
    let now_unix = db.clock().now_unix_secs();

    enum TurretSim { Delta { cid: ItemId, dq: i32, dd: f64 }, Empty, NoOp }

    let (crystal_shattered_empty, turret_sims, cargo_opt) = db.with_snapshot(|snap| {
        let mut shattered = false;
        let mut sims = Vec::with_capacity(cycle.linked_turrets.len());
        let mut local_qty: HashMap<ItemId, i32> = HashMap::new();
        let mut local_dmg: HashMap<ItemId, f64> = HashMap::new();
        let cur_q = |id: ItemId, orig: i32, lq: &HashMap<ItemId, i32>| -> i32 {
            orig + virtual_item_deltas.get(&id).copied().unwrap_or(0) + lq.get(&id).copied().unwrap_or(0)
        };
        let cur_d = |id: ItemId, orig: f64, ld: &HashMap<ItemId, f64>| -> f64 {
            (orig + virtual_crystal_dmg_deltas.get(&id).copied().unwrap_or(0.0) + ld.get(&id).copied().unwrap_or(0.0)).max(0.0)
        };

        for &turret_id in &cycle.linked_turrets {
            let Some(t_flag) = snap.items.get(&turret_id).map(|i| i.flag) else {
                sims.push(TurretSim::NoOp);
                continue;
            };
            let charge_opt = snap.items.values()
                .find(|i| i.location_id == cycle.ship_id && i.flag == t_flag && i.item_id != turret_id
                    && (cycle.ammo_type_id.is_none() || Some(i.type_id) == cycle.ammo_type_id)
                    && cur_q(i.item_id, i.quantity, &local_qty) > 0)
                .map(|i| (i.item_id, cur_q(i.item_id, i.quantity, &local_qty),
                    cur_d(i.item_id, i.dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0), &local_dmg)));
            if let Some((cid, qty, dmg)) = charge_opt {
                if cycle.crystal_volatility_chance > 0.0 && cycle.crystal_volatility_damage > 0.0 {
                    let next_dmg = dmg + cycle.crystal_volatility_damage;
                    if next_dmg >= 1.0 - 1e-6 {
                        *local_qty.entry(cid).or_insert(0) -= 1;
                        if qty > 1 {
                            *local_dmg.entry(cid).or_insert(0.0) -= dmg;
                            sims.push(TurretSim::Delta { cid, dq: -1, dd: -dmg });
                        } else {
                            shattered = true;
                            sims.push(TurretSim::Delta { cid, dq: -1, dd: 0.0 });
                        }
                    } else {
                        *local_dmg.entry(cid).or_insert(0.0) += cycle.crystal_volatility_damage;
                        sims.push(TurretSim::Delta { cid, dq: 0, dd: cycle.crystal_volatility_damage });
                    }
                } else if cycle.crystal_volatility_damage >= 0.0 {
                    if !pre_consumed_mode || consume_pre_round_for_next_cycle {
                        *local_qty.entry(cid).or_insert(0) -= 1;
                        if !pre_consumed_mode && qty <= 1 { shattered = true; }
                        sims.push(TurretSim::Delta { cid, dq: -1, dd: 0.0 });
                    } else {
                        sims.push(TurretSim::NoOp);
                    }
                } else {
                    sims.push(TurretSim::NoOp);
                }
            } else if cycle.ammo_type_id.or(cycle.missile_charge_type_id).is_some() && cycle.damage_vector.total() > 0.0 {
                shattered = true;
                sims.push(TurretSim::Empty);
            } else {
                sims.push(TurretSim::NoOp);
            }
        }

        let cargo_stack = if shattered && cycle.crystal_volatility_damage == 0.0 {
            cycle.ammo_type_id.or(cycle.missile_charge_type_id).and_then(|ammo_tid| {
                snap.items.values()
                    .filter_map(|i| {
                        let q = cur_q(i.item_id, i.quantity, &local_qty);
                        (i.location_id == cycle.ship_id
                            && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
                            && i.type_id == ammo_tid && q > 0)
                            .then_some((i.item_id, q))
                    })
                    .max_by_key(|&(_, q)| q)
                    .map(|(sid, avail)| (ammo_tid, sid, avail))
            })
        } else {
            None
        };
        (shattered, sims, cargo_stack)
    });

    let mut any_turret_change = false;
    for sim in turret_sims {
        match sim {
            TurretSim::Delta { cid, dq, dd } => {
                any_turret_change = true;
                if dq != 0 {
                    *virtual_item_deltas.entry(cid).or_insert(0) += dq;
                    post_unlock.item_qty_deltas.push((cid, dq));
                }
                if dd != 0.0 {
                    *virtual_crystal_dmg_deltas.entry(cid).or_insert(0.0) += dd;
                    post_unlock.crystal_dmg_deltas.push((cid, dd));
                }
            }
            TurretSim::Empty => any_turret_change = true,
            TurretSim::NoOp => {}
        }
    }

    let mut guided_reload_started = false;
    let mut guided_reload_ms = 0.0_f64;
    let mut reload_spec = None;
    let notify_no_charges = |actions: &mut Vec<PendingCycleAction>| {
        if cycle.weapon_is_guided_missile && cycle.repeat > 0 && !burned_out && !target_destroyed_in_cycle {
            if let Some(cid) = cycle.owner_char_id.filter(|&c| c > 0) {
                actions.push(PendingCycleAction::NoChargesDeactivated {
                    char_id: cid, module_type_id: cycle.module_type_id, notified: false,
                });
            }
        }
    };

    if crystal_shattered_empty && cycle.crystal_volatility_damage == 0.0 {
        if let Some((ammo_tid, stack_id, avail_qty)) = cargo_opt {
            let num_mods = cycle.linked_turrets.len().max(1) as i32;
            if avail_qty >= num_mods {
                let charge_vol = sde_opt.as_ref().and_then(|s| s.types.get(&ammo_tid)).map(|t| t.volume.max(0.0001)).unwrap_or(0.0025);
                let (rounds_per_mod, reload_time_ms) = sde_opt.as_ref().and_then(|s| s.types.get(&cycle.module_type_id)).map(|mt| {
                    let r = if cycle.missile_charge_capacity > 0 { cycle.missile_charge_capacity as i32 }
                        else if mt.capacity > 0.0 { ((mt.capacity / charge_vol) + 1e-9).floor() as i32 } else { 1 };
                    let rt = if cycle.weapon_is_guided_missile && cycle.missile_reload_ms > 0.0 { cycle.missile_reload_ms }
                        else if matches!(mt.group_id, 511 | 1245) { mt.default_attributes.get(&1795).copied().filter(|&v| v > 0.0).unwrap_or(35_000.0) }
                        else { mt.default_attributes.get(&1795).copied().unwrap_or(10_000.0) };
                    (r.max(1), rt)
                }).unwrap_or_else(|| (
                    (cycle.missile_charge_capacity as i32).max(1),
                    if cycle.missile_reload_ms > 0.0 { cycle.missile_reload_ms } else { 10_000.0 },
                ));
                let actual_per_mod = rounds_per_mod.min(avail_qty / num_mods).max(1);
                let pulled = actual_per_mod * num_mods;
                *virtual_item_deltas.entry(stack_id).or_insert(0) -= pulled;
                post_unlock.item_qty_deltas.push((stack_id, -pulled));
                let reload_sim_secs = reload_time_ms / 1000.0;
                let scaled_wall_secs = if tidi_factor < 0.995 { (reload_sim_secs / tidi_factor).ceil().max(1.0) as u64 } else { reload_sim_secs.ceil() as u64 };
                let reload_until = now_unix + scaled_wall_secs;
                for &tid in &cycle.linked_turrets {
                    if let Some(ref mut sim_map) = reload_sim_remaining {
                        sim_map.insert(tid, super::types::ModuleReloadSimEntry {
                            remaining_sim_s: reload_sim_secs,
                            min_wall_until_unix: now_unix.saturating_add(reload_sim_secs.ceil().max(1.0) as u64),
                            tidi_stretched: tidi_factor < 0.995,
                        });
                    }
                }
                if cycle.weapon_is_guided_missile && cycle.repeat > 0 && !burned_out && !target_destroyed_in_cycle {
                    guided_reload_started = true;
                    guided_reload_ms = reload_time_ms;
                    if let Some(cid) = cycle.owner_char_id.filter(|&c| c > 0) {
                        pending_cycle_actions.push(PendingCycleAction::AutoReloadStarted {
                            char_id: cid, module_ids: cycle.linked_turrets.clone(), charge_type_id: ammo_tid,
                            time_ms: reload_time_ms, reload_until_unix: reload_until,
                            sim_deadline_s: sim_elapsed_secs + reload_sim_secs, notified: false,
                        });
                    }
                }
                reload_spec = Some(CycleReloadDbSpec {
                    ammo_type_id: ammo_tid, linked_turrets: cycle.linked_turrets.clone(),
                    master_module_id: cycle.module_id, actual_per_mod, reload_time_ms, reload_until,
                });
            } else {
                notify_no_charges(pending_cycle_actions);
            }
        } else {
            notify_no_charges(pending_cycle_actions);
        }
    }

    if any_turret_change || reload_spec.is_some() {
        post_unlock.push(PostUnlockIntent::CycleAmmoStep(CycleAmmoStepDbOp {
            char_id: eff_char_id, ship_id: cycle.ship_id, linked_turrets: cycle.linked_turrets.clone(),
            ammo_type_id: cycle.ammo_type_id, missile_charge_type_id: cycle.missile_charge_type_id,
            damage_vector_total: cycle.damage_vector.total(), crystal_volatility_chance: cycle.crystal_volatility_chance,
            crystal_volatility_damage: cycle.crystal_volatility_damage, pre_consumed_mode,
            consume_pre_round_for_next_cycle, reload_spec,
        }));
    }
    (crystal_shattered_empty, guided_reload_started, guided_reload_ms)
}

pub(crate) fn collect_ship_charge_consume(
    db: &WorldRepository,
    eff_char_id: Option<CharacterId>,
    ship_id: BallId,
    module_id: ItemId,
    kind: ShipChargeConsumeKind,
    virtual_item_deltas: &mut HashMap<ItemId, i32>,
    post_unlock: &mut PostUnlockOps,
) -> bool {
    let found = db.with_snapshot(|snap| {
        let m_flag = snap.items.get(&module_id).map(|i| i.flag)?;
        let (min_needed, only_paste) = match kind {
            ShipChargeConsumeKind::Asb | ShipChargeConsumeKind::CapBooster => (1, false),
            ShipChargeConsumeKind::AarPaste { paste_rate } => (paste_rate.max(1), true),
        };
        snap.items.values().filter_map(|i| {
            let q = i.quantity + virtual_item_deltas.get(&i.item_id).copied().unwrap_or(0);
            (i.location_id == ship_id && i.flag == m_flag && i.item_id != module_id
                && (!only_paste || i.type_id == 28668) && q >= min_needed)
                .then_some((i.item_id, min_needed))
        }).next()
    });
    let Some((cid, dec)) = found else { return false; };
    *virtual_item_deltas.entry(cid).or_insert(0) -= dec;
    post_unlock.item_qty_deltas.push((cid, -dec));
    post_unlock.push(PostUnlockIntent::ShipChargeConsume(ShipChargeConsumeDbOp {
        char_id: eff_char_id, ship_id, module_id, kind,
    }));
    true
}

pub(crate) fn step_module_reloads_sim_clock_collecting(
    reload_sim_remaining: &mut HashMap<i64, super::types::ModuleReloadSimEntry>,
    reload_sim_deadlines_opt: Option<
        &eve_db::lock_rank::RankedRwLock<HashMap<i64, (SolarSystemId, f64, f64)>, { eve_db::lock_rank::RANK_LEAF }>,
    >,
    db_opt: &Option<Arc<WorldRepository>>,
    dt_sim: f64,
    tidi_factor: f64,
    post_unlock: &mut PostUnlockOps,
) {
    if reload_sim_remaining.is_empty() && db_opt.is_none() {
        return;
    }
    let tidi = if tidi_factor.is_finite() { tidi_factor.clamp(0.10, 1.0) } else { 1.0 };
    let mut finished = Vec::new();
    let mut active_updates = Vec::new();
    for (&mod_id, entry) in reload_sim_remaining.iter_mut() {
        entry.remaining_sim_s -= dt_sim.max(0.0);
        if entry.remaining_sim_s <= 1e-6 {
            finished.push(mod_id);
        } else {
            let was_stretched = entry.tidi_stretched;
            if tidi < 0.995 { entry.tidi_stretched = true; }
            active_updates.push((mod_id, entry.remaining_sim_s, entry.min_wall_until_unix, was_stretched));
        }
    }
    for mod_id in &finished {
        reload_sim_remaining.remove(mod_id);
    }
    if !finished.is_empty() || !active_updates.is_empty() {
        if let Some(db) = db_opt.as_ref() {
            let now_unix = db.clock().now_unix_secs();
            let desired_updates: Vec<(i64, u64, bool)> = active_updates.iter()
                .filter_map(|&(mod_id, rem, min_wall_until, was_stretched)| {
                    if tidi < 0.995 {
                        Some((mod_id, now_unix.saturating_add((rem / tidi).ceil().max(1.0) as u64), true))
                    } else if was_stretched {
                        Some((mod_id, now_unix.saturating_add(rem.ceil().max(1.0) as u64).max(min_wall_until), false))
                    } else {
                        None
                    }
                })
                .collect();
            if tidi >= 0.995 {
                for &(mod_id, _, _) in &desired_updates {
                    if let Some(entry) = reload_sim_remaining.get_mut(&mod_id) {
                        entry.tidi_stretched = false;
                    }
                }
            }
            if !finished.is_empty() || !desired_updates.is_empty() {
                post_unlock.push(PostUnlockIntent::ModuleReload(ModuleReloadDbOp {
                    finished: finished.clone(), desired_updates,
                }));
            }
        }
        if !finished.is_empty() {
            if let Some(deadlines) = reload_sim_deadlines_opt {
                let mut dead_guard = deadlines.write().unwrap_or_else(|e| e.into_inner());
                for mod_id in &finished { dead_guard.remove(mod_id); }
            }
        }
    }
}

type CheckpointBatch = Vec<(SolarSystemId, Vec<ConditionCheckpointEntry>, usize, Option<Arc<AtomicUsize>>)>;

fn should_open_db_tx(db: &Arc<WorldRepository>, intents: &[PostUnlockIntent]) -> (bool, CheckpointBatch) {
    let mut has_unconditional_write = false;
    let mut check_asteroid_inserts = Vec::new();
    let mut check_tether_heals = Vec::new();
    let mut check_positions = Vec::new();
    let mut check_combat_hps = Vec::new();
    let mut check_reloads = Vec::new();
    let mut check_ship_clears = Vec::new();
    let mut check_checkpoints = Vec::new();

    for intent in intents {
        match intent {
            PostUnlockIntent::BeltAsteroidInsertIfAbsent(r) => check_asteroid_inserts.push(r.asteroid_id),
            PostUnlockIntent::RepairTetherModuleHeal { ship_ids, heal_hp } if *heal_hp > 0.0 => {
                check_tether_heals.push(ship_ids.as_slice());
            }
            PostUnlockIntent::RepairTetherModuleHeal { .. } => {}
            PostUnlockIntent::ItemPosition { ball_id, pos, solar_system_id, space_entry_epoch } => {
                check_positions.push((*ball_id, *pos, *solar_system_id, *space_entry_epoch));
            }
            PostUnlockIntent::ShipCombatHp(u) => check_combat_hps.push(u),
            PostUnlockIntent::ModuleReload(op) => check_reloads.push(op),
            PostUnlockIntent::ShipCycleClearAll(sid) => check_ship_clears.push(*sid),
            PostUnlockIntent::ConditionCheckpoint { solar_system_id, entries, written_out } => {
                check_checkpoints.push((*solar_system_id, entries.as_slice(), written_out));
            }
            other if other.writes_db() => { has_unconditional_write = true; break; }
            _ => {}
        }
    }

    if has_unconditional_write { return (true, Vec::new()); }
    if check_asteroid_inserts.is_empty() && check_tether_heals.is_empty() && check_positions.is_empty()
        && check_combat_hps.is_empty() && check_reloads.is_empty()
        && check_ship_clears.is_empty() && check_checkpoints.is_empty()
    {
        return (false, Vec::new());
    }

    let (any_change, skipped_checkpoints) = db.with_snapshot(|snap| {
        let mut changed = check_asteroid_inserts.iter().any(|aid| !snap.asteroids.contains_key(aid))
            || check_tether_heals.iter().any(|ship_ids| {
                snap.items.values().any(|it| {
                    ship_ids.contains(&it.location_id)
                        && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
                        && it.dynamic_attributes.contains_key(&attr_ids::DAMAGE)
                })
            })
            || check_positions.iter().any(|&(bid, pos, sys_id, ep)| {
            snap.items.contains_key(&bid)
                && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(snap, bid, Some(sys_id), ep)
                && snap.item_positions.get(&bid).is_none_or(|&prev| (glam::DVec3::from_array(prev) - glam::DVec3::from_array(pos)).length_squared() > 1e-6)
        }) || check_combat_hps.iter().any(|u| {
            crate::sp6_lifecycle::has_ship_combat_hp_change_in_snap(
                snap, u.ball_id, &u.combat_state, u.expected_system, u.require_persisted_entity,
            )
        }) || check_reloads.iter().any(|op| {
            op.finished.iter().any(|id| snap.cluster_and_runtime.module_reload_until_unix.contains_key(id))
                || op.desired_updates.iter().any(|&(mid, nu, dil)| {
                    match snap.cluster_and_runtime.module_reload_until_unix.get(&mid).copied() {
                        Some(cur) => cur != nu && (dil || cur > nu),
                        None => dil,
                    }
                })
        }) || check_ship_clears.iter().any(|&sid| snap.ship_has_any_active_module_or_charge(sid));
        let mut valid_only = Vec::new();
        for &(sys_id, entries, out_opt) in &check_checkpoints {
            let (valid, updates) = crate::sp6_lifecycle::prepare_condition_checkpoint_updates_in_snap(snap, sys_id, entries);
            if !updates.is_empty() { changed = true; break; }
            if !valid.is_empty() { valid_only.push((sys_id, valid, 0, out_opt.clone())); }
        }
        (changed, valid_only)
    });
    if any_change { (true, Vec::new()) } else { (false, skipped_checkpoints) }
}

fn collect_non_db_hooks(
    intents: Vec<PostUnlockIntent>,
    deferred_deact_hooks: &mut Vec<DeferredDeactivationHookOp>,
    deferred_lock_hooks: &mut Vec<DeferredTargetLockHookOp>,
) {
    for intent in intents {
        match intent {
            PostUnlockIntent::DeactivationHook(h) => deferred_deact_hooks.push(h),
            PostUnlockIntent::TargetLockHook(h) => deferred_lock_hooks.push(h),
            _ => {}
        }
    }
}

fn apply_intent_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&Arc<SdeRegistry>>,
    intent: PostUnlockIntent,
    local_actions: &mut Vec<PendingCycleAction>,
    sqlite_checkpoints: &mut CheckpointBatch,
    deferred_deact_hooks: &mut Vec<DeferredDeactivationHookOp>,
    deferred_lock_hooks: &mut Vec<DeferredTargetLockHookOp>,
) {
    match intent {
        PostUnlockIntent::BeltAsteroidInsertIfAbsent(record) => {
            snap.asteroids.entry(record.asteroid_id).or_insert(record);
        }
        PostUnlockIntent::BeltAsteroidUpsert(record) => {
            snap.asteroids.insert(record.asteroid_id, record);
        }
        PostUnlockIntent::BeltRespawnDueUpsert { belt_id, due_unix } => {
            snap.mining_and_ess_state.belt_respawn_due_unix.insert(belt_id, due_unix);
        }
        PostUnlockIntent::BeltRespawnDueClear(belt_id) => {
            snap.mining_and_ess_state.belt_respawn_due_unix.remove(&belt_id);
        }
        PostUnlockIntent::MineAsteroid(upd) => {
            if let Some(a) = snap.asteroids.get_mut(&upd.asteroid_id) {
                a.quantity = i64::try_from(upd.remaining_units).unwrap_or(0);
            }
            if let Some(due_unix) = upd.belt_due_unix {
                snap.mining_and_ess_state.belt_respawn_due_unix.insert(upd.belt_id, due_unix);
            }
        }
        PostUnlockIntent::ItemPosition { ball_id, pos, solar_system_id, space_entry_epoch } => {
            if snap.items.contains_key(&ball_id)
                && !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                    snap, ball_id, Some(solar_system_id), space_entry_epoch,
                )
                && snap.item_positions.get(&ball_id).is_none_or(|&prev| {
                    (glam::DVec3::from_array(prev) - glam::DVec3::from_array(pos)).length_squared() > 1e-6
                })
            {
                snap.item_positions.insert(ball_id, pos);
            }
        }
        PostUnlockIntent::ShipCombatHp(upd) => {
            crate::sp6_lifecycle::apply_ship_combat_hp_update_in_snap(
                snap, upd.ball_id, &upd.combat_state, upd.expected_system, upd.require_persisted_entity,
            );
        }
        PostUnlockIntent::RepairTetherModuleHeal { ship_ids, heal_hp } => {
            for it in snap.items.values_mut() {
                if ship_ids.contains(&it.location_id)
                    && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
                {
                    if let Some(cur_dmg) = it.dynamic_attributes.get(&attr_ids::DAMAGE).copied() {
                        let new_dmg = (cur_dmg - heal_hp).max(0.0);
                        if new_dmg <= 1e-6 { it.dynamic_attributes.remove(&attr_ids::DAMAGE); }
                        else { it.dynamic_attributes.insert(attr_ids::DAMAGE, new_dmg); }
                    }
                }
            }
        }
        PostUnlockIntent::NaniteRepairTick(tick) => apply_nanite_repair_tick_in_snap(snap, &tick),
        PostUnlockIntent::NaniteRepairCancel(cancel) => apply_nanite_repair_cancel_in_snap(snap, sde_opt, &cancel),
        PostUnlockIntent::MissileLauncherConsume(consume) => {
            let diffs = apply_launcher_consume_in_snap(snap, &consume);
            push_charge_diff_action(local_actions, consume.char_id, consume.ship_id, diffs);
        }
        PostUnlockIntent::TransientAttrSets { item_id, attrs } => {
            let entry = snap.transient_item_attributes.entry(item_id).or_default();
            for (aid, val) in attrs { entry.insert(aid, val); }
        }
        PostUnlockIntent::RahResonances { ship_id, module_id, resonances } => {
            for id in [module_id, ship_id] {
                let entry = snap.transient_item_attributes.entry(id).or_default();
                for (idx, aid) in (267_u16..=270).enumerate() { entry.insert(aid, resonances[idx]); }
            }
        }
        PostUnlockIntent::ModuleBurnout { ship_id, module_id } => {
            if let Some(m_item) = snap.get_any_item_mut(module_id) {
                let max_hp = m_item.dynamic_attributes.get(&attr_ids::HP_STRUCT).copied().unwrap_or(40.0);
                m_item.dynamic_attributes.insert(attr_ids::DAMAGE, max_hp);
            }
            snap.mark_module_burned_out(module_id);
            crate::router::ServerContext::clear_persisted_module_overload(snap, ship_id, module_id);
        }
        PostUnlockIntent::CycleAmmoStep(step_op) => {
            let diffs = apply_cycle_ammo_step_in_snap(snap, &step_op);
            push_charge_diff_action(local_actions, step_op.char_id, step_op.ship_id, diffs);
        }
        PostUnlockIntent::ShipChargeConsume(charge_op) => {
            let diffs = apply_ship_charge_consume_in_snap(snap, &charge_op);
            push_charge_diff_action(local_actions, charge_op.char_id, charge_op.ship_id, diffs);
        }
        PostUnlockIntent::ModuleReload(reload_op) => apply_module_reload_in_snap(snap, &reload_op),
        PostUnlockIntent::ModuleCycleCleanup(cleanup) => apply_module_cycle_cleanup_in_snap(snap, &cleanup),
        PostUnlockIntent::ShipCycleClearAll(ship_id) | PostUnlockIntent::RemovedShipBall(ship_id) => {
            snap.clear_ship_active_module_states(ship_id);
        }
        PostUnlockIntent::ConditionCheckpoint { solar_system_id, entries, written_out } => {
            let (valid, updates) = crate::sp6_lifecycle::prepare_condition_checkpoint_updates_in_snap(snap, solar_system_id, &entries);
            let snap_written = if updates.is_empty() { 0 } else {
                crate::sp6_lifecycle::apply_condition_checkpoint_updates_in_snap(snap, solar_system_id, updates)
            };
            if !valid.is_empty() || written_out.is_some() {
                sqlite_checkpoints.push((solar_system_id, valid, snap_written, written_out));
            }
        }
        PostUnlockIntent::DeactivationHook(hook) => deferred_deact_hooks.push(hook),
        PostUnlockIntent::TargetLockHook(hook) => deferred_lock_hooks.push(hook),
    }
}

/// Executes collected [`PostUnlockOps`] against `WorldRepository` and runtime hooks
/// strictly after `ballpark` and `actor_state` locks have been released (`#1170`).
pub(crate) fn execute_post_unlock_ops(
    db_opt: Option<&Arc<WorldRepository>>,
    sde_opt: Option<&Arc<SdeRegistry>>,
    ops: PostUnlockOps,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    lock_completed_hook: Option<&TargetLockCompletedHook>,
) -> Vec<PendingCycleAction> {
    if ops.is_empty() {
        return Vec::new();
    }
    assert_no_actor_locks_before_db_tx();

    let can_write_db = !held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER);
    let mut emitted_actions = Vec::new();
    let mut deferred_deact_hooks = Vec::new();
    let mut deferred_lock_hooks = Vec::new();
    let mut sqlite_checkpoints: CheckpointBatch = Vec::new();

    if let Some(db) = db_opt.filter(|_| can_write_db) {
        let (open_tx, prechecked_checkpoints) = should_open_db_tx(db, &ops.intents);
        if open_tx {
            let mut tx_hooks = Vec::new();
            let mut tx_lock_hooks = Vec::new();
            let mut tx_actions = Vec::new();
            let mut tx_checkpoints = Vec::new();
            let mut intents_cell = Some(ops.intents);
            match db.tx("system_actor_post_unlock", |wtx| {
                if let Some(intents) = intents_cell.take() {
                    for intent in intents {
                        apply_intent_in_snap(
                            &mut wtx.snap, sde_opt, intent,
                            &mut tx_actions, &mut tx_checkpoints, &mut tx_hooks, &mut tx_lock_hooks,
                        );
                    }
                }
                Ok::<_, TxError>(())
            }) {
                Ok(()) => {
                    emitted_actions = tx_actions;
                    sqlite_checkpoints = tx_checkpoints;
                    deferred_deact_hooks = tx_hooks;
                    deferred_lock_hooks = tx_lock_hooks;
                }
                Err(err) => {
                    tracing::error!(error = %err, "system_actor_post_unlock DB transaction failed; skipping post-commit hooks");
                }
            }
        } else {
            sqlite_checkpoints = prechecked_checkpoints;
            collect_non_db_hooks(ops.intents, &mut deferred_deact_hooks, &mut deferred_lock_hooks);
        }

        for (sys_id, valid_entries, snap_written, written_out) in sqlite_checkpoints {
            let sqlite_written = if !valid_entries.is_empty() {
                crate::sp6_lifecycle::persist_system_condition_checkpoints_sqlite(db, sys_id, &valid_entries)
            } else {
                0
            };
            if let Some(slot) = written_out {
                slot.store(snap_written.max(sqlite_written), Ordering::Release);
            }
        }
    } else {
        collect_non_db_hooks(ops.intents, &mut deferred_deact_hooks, &mut deferred_lock_hooks);
    }

    if can_write_db {
        if let Some(deact_hook) = deactivation_hook {
            for hook_op in deferred_deact_hooks {
                let cid_opt = hook_op.char_id.or_else(|| {
                    db_opt.and_then(|db| db.with_snapshot(|s| {
                        s.characters.values().find(|c| c.active_ship_id == hook_op.cycle.ship_id)
                            .map(|c| c.character_id)
                            .or_else(|| s.items.get(&hook_op.cycle.ship_id).map(|it| it.owner_id))
                    }))
                }).or(hook_op.fallback_owner_id);
                if let Some(cid) = cid_opt {
                    deact_hook(hook_op.solar_system_id, cid, &hook_op.cycle);
                }
            }
        }
        if let Some(lock_hook) = lock_completed_hook {
            for hook_op in deferred_lock_hooks {
                lock_hook(hook_op.char_id, hook_op.target_id);
            }
        }
    } else if !deferred_deact_hooks.is_empty() || !deferred_lock_hooks.is_empty() {
        tracing::warn!(
            deact = deferred_deact_hooks.len(),
            lock = deferred_lock_hooks.len(),
            "skipping post-unlock hooks under held >= RANK_WORLD_INNER lock"
        );
    }
    emitted_actions
}

/// Returns `true` only when the calling thread holds zero ranked locks (`W3`, `#1170`).
pub(crate) fn can_run_post_unlock_sync_now() -> bool {
    held_lock_ranks().is_empty()
}

/// Returns `true` when an external helper should block waiting for its own `post_unlock_worker`
/// item to commit (`W3`: unlocked caller not on this system's worker thread).
///
/// MUST be evaluated BEFORE acquiring `ballpark` / `actor_state` locks.
pub(in crate::system_actor) fn caller_can_wait_for_handle(handle: &super::SystemHandle) -> bool {
    can_run_post_unlock_sync_now()
        && handle.post_unlock_worker_thread_id.get() != Some(&std::thread::current().id())
}

pub(in crate::system_actor) fn build_removed_ball_post_unlock_ops(
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    removed_cycles: Vec<(ActiveModuleCycle, Option<CharacterId>)>,
    fallback_slim_owner: Option<CharacterId>,
    include_deactivation_hooks: bool,
) -> PostUnlockOps {
    let mut ops = PostUnlockOps::default();
    ops.push(PostUnlockIntent::RemovedShipBall(ball_id));
    for (removed, _) in &removed_cycles {
        ops.push(PostUnlockIntent::ModuleCycleCleanup(ModuleCycleCleanupDbOp::from_cycle(removed)));
    }
    if include_deactivation_hooks {
        for (removed, char_id_opt) in removed_cycles {
            if removed.module_id < 0 || super::cycles_tackle::parse_minion_cycle_parent(&removed.effect_name).is_some() {
                continue;
            }
            ops.push(PostUnlockIntent::DeactivationHook(DeferredDeactivationHookOp {
                solar_system_id, char_id: char_id_opt, fallback_owner_id: fallback_slim_owner, cycle: removed,
            }));
        }
    }
    ops
}

pub(in crate::system_actor) struct PostUnlockInFlightGuard(pub(in crate::system_actor) Arc<AtomicUsize>);

impl Drop for PostUnlockInFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(in crate::system_actor) fn send_post_unlock_item(
    tx: &tokio::sync::mpsc::UnboundedSender<PostUnlockMsg>,
    in_flight: &Arc<AtomicUsize>,
    mut item: PostUnlockWorkItem,
) -> Result<(), PostUnlockWorkItem> {
    in_flight.fetch_add(1, Ordering::SeqCst);
    item.in_flight_guard = Some(PostUnlockInFlightGuard(Arc::clone(in_flight)));
    match tx.send(PostUnlockMsg::Work(item)) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::SendError(PostUnlockMsg::Work(mut unsent))) => {
            unsent.in_flight_guard.take();
            Err(unsent)
        }
        Err(tokio::sync::mpsc::error::SendError(PostUnlockMsg::Shutdown)) => {
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Err(PostUnlockWorkItem { db_opt: None, sde_opt: None, ops: PostUnlockOps::default(), replies: Vec::new(), in_flight_guard: None, work_seq: 0 })
        }
    }
}

pub(in crate::system_actor) fn seed_pending_charge_reservations(
    actor_st: &mut SystemActorState,
    completed_seq: &AtomicU64,
) -> (HashMap<ItemId, i32>, HashMap<ItemId, f64>) {
    let done = completed_seq.load(Ordering::Acquire);
    while actor_st.pending_item_qty_deltas.front().is_some_and(|&(seq, _, _)| seq <= done) {
        actor_st.pending_item_qty_deltas.pop_front();
    }
    while actor_st.pending_crystal_dmg_deltas.front().is_some_and(|&(seq, _, _)| seq <= done) {
        actor_st.pending_crystal_dmg_deltas.pop_front();
    }
    let mut qty = HashMap::new();
    for &(_, id, dq) in &actor_st.pending_item_qty_deltas { *qty.entry(id).or_insert(0) += dq; }
    let mut dmg = HashMap::new();
    for &(_, id, dd) in &actor_st.pending_crystal_dmg_deltas { *dmg.entry(id).or_insert(0.0) += dd; }
    (qty, dmg)
}

pub(in crate::system_actor) fn enqueue_actor_cmd_post_unlock_ops(
    actor_st: &mut SystemActorState,
    tx_opt: Option<&tokio::sync::mpsc::UnboundedSender<PostUnlockMsg>>,
    in_flight: &Arc<AtomicUsize>,
    mut ops: PostUnlockOps,
) {
    if ops.is_empty() { return; }
    let Some(tx) = tx_opt else { return; };
    actor_st.next_post_unlock_seq = actor_st.next_post_unlock_seq.saturating_add(1);
    let work_seq = actor_st.next_post_unlock_seq;
    let qty_deltas = std::mem::take(&mut ops.item_qty_deltas);
    let dmg_deltas = std::mem::take(&mut ops.crystal_dmg_deltas);
    let item = PostUnlockWorkItem {
        db_opt: actor_st.db_opt.clone(), sde_opt: actor_st.sde_opt.clone(),
        ops, replies: Vec::new(), in_flight_guard: None, work_seq,
    };
    if send_post_unlock_item(tx, in_flight, item).is_ok() {
        for (id, dq) in qty_deltas { actor_st.pending_item_qty_deltas.push_back((work_seq, id, dq)); }
        for (id, dd) in dmg_deltas { actor_st.pending_crystal_dmg_deltas.push_back((work_seq, id, dd)); }
    }
}

pub(in crate::system_actor) enum PostUnlockWaitTicket {
    None,
    WaitWorker(std::sync::mpsc::Receiver<()>),
    RunDirectAfterUnlock(Box<PostUnlockWorkItem>),
}

impl PostUnlockWaitTicket {
    pub(in crate::system_actor) fn is_wait_worker(&self) -> bool {
        matches!(self, Self::WaitWorker(_))
    }
}

fn enqueue_work_ticket_with_seq(
    handle: &super::SystemHandle,
    db_opt: Option<&Arc<WorldRepository>>,
    sde_opt: Option<&Arc<SdeRegistry>>,
    ops: PostUnlockOps,
    caller_can_wait: bool,
    work_seq: u64,
) -> (bool, PostUnlockWaitTicket) {
    let (done_tx, done_rx) = if caller_can_wait {
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let mut replies: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    if let Some(tx) = done_tx {
        replies.push(Box::new(move || { let _ = tx.send(()); }));
    }
    let item = PostUnlockWorkItem {
        db_opt: db_opt.cloned(), sde_opt: sde_opt.cloned(), ops, replies, in_flight_guard: None, work_seq,
    };
    match send_post_unlock_item(&handle.post_unlock_tx, &handle.post_unlock_in_flight, item) {
        Ok(()) => (true, done_rx.map_or(PostUnlockWaitTicket::None, PostUnlockWaitTicket::WaitWorker)),
        Err(unsent) if unsent.ops.is_empty() => (false, PostUnlockWaitTicket::None),
        Err(unsent) if caller_can_wait => (false, PostUnlockWaitTicket::RunDirectAfterUnlock(Box::new(unsent))),
        Err(unsent) => {
            tracing::warn!(
                solar_system_id = handle.solar_system_id,
                intents = unsent.ops.intents.len(),
                "post_unlock_tx closed while caller holds ranked locks or is on worker thread; skipping direct fallback"
            );
            for send_reply in unsent.replies {
                send_reply();
            }
            (false, PostUnlockWaitTicket::None)
        }
    }
}

/// Enqueues `ops` onto `handle.post_unlock_tx` under actor locks, or an empty-ops FIFO barrier when work is in flight (`W1`, `W4`).
pub(in crate::system_actor) fn enqueue_post_unlock_or_barrier_under_locks(handle: &super::SystemHandle, db_opt: Option<&Arc<WorldRepository>>, sde_opt: Option<&Arc<SdeRegistry>>, ops: PostUnlockOps, caller_can_wait: bool) -> PostUnlockWaitTicket {
    if !ops.is_empty() { enqueue_work_ticket_with_seq(handle, db_opt, sde_opt, ops, caller_can_wait, 0).1 }
    else if caller_can_wait && handle.has_in_flight_post_unlock() { enqueue_barrier_under_locks(handle) }
    else { PostUnlockWaitTicket::None }
}

pub(in crate::system_actor) fn enqueue_tick_post_unlock_or_barrier_under_locks(
    handle: &super::SystemHandle,
    actor_st: &mut SystemActorState,
    mut ops: PostUnlockOps,
    caller_can_wait: bool,
) -> PostUnlockWaitTicket {
    if ops.is_empty() {
        return enqueue_post_unlock_or_barrier_under_locks(handle, actor_st.db_opt.as_ref(), actor_st.sde_opt.as_ref(), ops, caller_can_wait);
    }
    actor_st.next_post_unlock_seq = actor_st.next_post_unlock_seq.saturating_add(1);
    let work_seq = actor_st.next_post_unlock_seq;
    let qty_deltas = std::mem::take(&mut ops.item_qty_deltas);
    let dmg_deltas = std::mem::take(&mut ops.crystal_dmg_deltas);
    let (queued, ticket) = enqueue_work_ticket_with_seq(
        handle, actor_st.db_opt.as_ref(), actor_st.sde_opt.as_ref(), ops, caller_can_wait, work_seq,
    );
    if queued {
        for (id, dq) in qty_deltas { actor_st.pending_item_qty_deltas.push_back((work_seq, id, dq)); }
        for (id, dd) in dmg_deltas { actor_st.pending_crystal_dmg_deltas.push_back((work_seq, id, dd)); }
    }
    ticket
}

/// Enqueues an empty-ops FIFO barrier onto `handle.post_unlock_tx` while actor locks are held (`W4`, `BF-3`).
pub(in crate::system_actor) fn enqueue_barrier_under_locks(handle: &super::SystemHandle) -> PostUnlockWaitTicket {
    enqueue_work_ticket_with_seq(handle, None, None, PostUnlockOps::default(), true, 0).1
}

/// Finishes waiting or fallback execution strictly **after** all actor locks are dropped (`W2`, `W3`).
pub(in crate::system_actor) fn finish_post_unlock_after_unlock(handle: &super::SystemHandle, deact_guard: Option<&ModuleDeactivationHook>, lock_guard: Option<&TargetLockCompletedHook>, ticket: PostUnlockWaitTicket) {
    let safe_sync = can_run_post_unlock_sync_now() && handle.post_unlock_worker_thread_id.get() != Some(&std::thread::current().id());
    match ticket {
        PostUnlockWaitTicket::None => if safe_sync { super::flush_post_unlock_actions(&handle.post_unlock_actions); },
        PostUnlockWaitTicket::WaitWorker(rx) => { let _ = rx.recv(); }
        PostUnlockWaitTicket::RunDirectAfterUnlock(unsent) => {
            let unsent = *unsent;
            if safe_sync {
                let extra_actions = execute_post_unlock_ops(unsent.db_opt.as_ref(), unsent.sde_opt.as_ref(), unsent.ops, deact_guard, lock_guard);
                if unsent.work_seq > 0 {
                    handle.completed_post_unlock_seq.store(unsent.work_seq, Ordering::Release);
                }
                if !extra_actions.is_empty() { handle.actor_state.write().unwrap_or_else(|e| e.into_inner()).pending_cycle_actions.extend(extra_actions); }
                super::flush_post_unlock_actions(&handle.post_unlock_actions);
            } else {
                tracing::warn!(solar_system_id = handle.solar_system_id, intents = unsent.ops.intents.len(), "skipping RunDirectAfterUnlock after worker shutdown under held ranked locks or worker reentrancy");
            }
            for send_reply in unsent.replies { send_reply(); }
        }
    }
}

pub(in crate::system_actor) struct PostUnlockWorkItem {
    pub(in crate::system_actor) db_opt: Option<Arc<WorldRepository>>,
    pub(in crate::system_actor) sde_opt: Option<Arc<SdeRegistry>>,
    pub(in crate::system_actor) ops: PostUnlockOps,
    pub(in crate::system_actor) replies: Vec<Box<dyn FnOnce() + Send>>,
    pub(in crate::system_actor) in_flight_guard: Option<PostUnlockInFlightGuard>,
    pub(in crate::system_actor) work_seq: u64,
}

pub(in crate::system_actor) enum PostUnlockMsg {
    Work(PostUnlockWorkItem),
    Shutdown,
}

/// Sends `PostUnlockMsg::Shutdown` when the owning `SystemActor` exits OR is aborted by `Runtime::drop()`.
pub(in crate::system_actor) struct PostUnlockShutdownGuard(
    pub(in crate::system_actor) Option<tokio::sync::mpsc::UnboundedSender<PostUnlockMsg>>,
);

impl PostUnlockShutdownGuard {
    pub(in crate::system_actor) fn send_shutdown(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(PostUnlockMsg::Shutdown);
        }
    }
}

impl Drop for PostUnlockShutdownGuard { fn drop(&mut self) { self.send_shutdown(); } }

/// Spawns the ordered FIFO post-unlock persistence and hook worker for one `SystemActor` (`W2`, `#1170`).
///
/// Uses a dedicated OS thread instead of Tokio `spawn_blocking` so `tokio::runtime::Runtime::drop()`
/// cannot deadlock waiting on `blocking_recv()` while an external `SystemHandle` stays alive.
pub(in crate::system_actor) fn spawn_post_unlock_worker(
    solar_system_id: SolarSystemId,
    worker_thread_id: Arc<OnceLock<std::thread::ThreadId>>,
    completed_seq_worker: Arc<AtomicU64>,
    actor_state_worker: Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>,
    deact_hook_worker: Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    lock_hook_worker: Arc<RankedRwLock<Option<TargetLockCompletedHook>, RANK_LEAF>>,
    post_unlock_actions_worker: Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
) -> (tokio::sync::mpsc::UnboundedSender<PostUnlockMsg>, tokio::sync::oneshot::Receiver<()>) {
    let (post_unlock_tx, mut post_unlock_rx) = tokio::sync::mpsc::unbounded_channel::<PostUnlockMsg>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::Builder::new().name(format!("post-unlock-{solar_system_id}")).spawn(move || {
        struct WorkerDoneGuard(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for WorkerDoneGuard { fn drop(&mut self) { if let Some(tx) = self.0.take() { let _ = tx.send(()); } } }
        let _done_guard = WorkerDoneGuard(Some(done_tx));
        let _ = worker_thread_id.set(std::thread::current().id());
        let process_item = |item: PostUnlockWorkItem| {
            let deact_hook = deact_hook_worker.read().unwrap_or_else(|e| e.into_inner()).clone();
            let lock_hook = lock_hook_worker.read().unwrap_or_else(|e| e.into_inner()).clone();
            let PostUnlockWorkItem { db_opt, sde_opt, ops, replies, in_flight_guard, work_seq } = item;
            let extra_actions = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_post_unlock_ops(db_opt.as_ref(), sde_opt.as_ref(), ops, deact_hook.as_ref(), lock_hook.as_ref())
            })).unwrap_or_else(|_| { tracing::error!(solar_system_id, "SystemActor post_unlock worker thread panicked; continuing FIFO worker"); Vec::new() });
            if work_seq > 0 {
                completed_seq_worker.store(work_seq, Ordering::Release);
            }
            if !extra_actions.is_empty() {
                actor_state_worker.write().unwrap_or_else(|e| e.into_inner()).pending_cycle_actions.extend(extra_actions);
            }
            super::flush_post_unlock_actions(&post_unlock_actions_worker);
            drop(in_flight_guard);
            for send_reply in replies { send_reply(); }
        };

        while let Some(msg) = post_unlock_rx.blocking_recv() {
            match msg {
                PostUnlockMsg::Work(item) => process_item(item),
                PostUnlockMsg::Shutdown => {
                    post_unlock_rx.close();
                    while let Ok(rem) = post_unlock_rx.try_recv() { if let PostUnlockMsg::Work(item) = rem { process_item(item); } }
                    break;
                }
            }
        }
    }).expect("spawn post_unlock worker thread");
    (post_unlock_tx, done_rx)
}

#[cfg(test)]
#[path = "post_unlock_ruling_tests.rs"]
mod post_unlock_ruling_tests;
