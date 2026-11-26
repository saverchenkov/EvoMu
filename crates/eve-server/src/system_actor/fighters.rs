//! Per-system authoritative carrier fighter state, actor commands, and SQLite
//! `KIND_FIGHTERS` persistence (`#1541` Track F2, `#1170`).
//!
//! All live fighter squadrons, tube loads, ability cooldowns/charges, unit damage
//! accumulators, and tube rearm timers live inside [`SystemActorState::fighters`] and
//! mirror lock-free onto [`SystemView::fighters`]. Persistent checkpoints are written
//! exclusively under the owning `solar_system_id` outside ballpark/actor/registry locks.

use super::{
    ball_ops, celestials, cycles, cycles_tackle, defer_reply, fighters_persistence, protection,
    sync_ball_positions, ActorExecCtx, FighterCheckpointSnapshot, SystemActorState, SystemCommand,
    SystemHandle,
};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_proto::{BridgeRpcError, ClientError};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveFighterSquadron {
    pub ball_id: i64,
    pub item_id: i64,
    pub type_id: u32,
    pub owner_char_id: i64,
    pub parent_ship_id: i64,
    pub tube_index: usize,
    pub squadron_size: u32,
    pub target_id: Option<i64>,
    pub is_abandoned: bool,
    pub mwd_active: bool,
}

/// Consolidated carrier fighter runtime state owned by a single solar system actor (`#1541`).
#[derive(Debug, Clone, Default)]
pub struct SystemFighterState {
    pub cooldown_state: HashMap<(i64, u32), u64>,
    pub charge_state: HashMap<(i64, u32), u32>,
    pub active_fighters: HashMap<i64, ActiveFighterSquadron>,
    pub tube_loads: HashMap<(i64, usize), (i64, u32)>,
    pub unit_damage_accum: HashMap<i64, f64>,
    pub tube_rearm_until: HashMap<(i64, usize), u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingClaimedSquadron {
    pub squadron: ActiveFighterSquadron,
    pub saved_charges: Vec<((i64, u32), u32)>,
    pub saved_cooldowns: Vec<((i64, u32), u64)>,
    pub accum_dmg: f64,
}

impl SystemActorState {
    #[inline]
    pub(crate) fn is_fighters_dirty(&self) -> bool {
        self.fighters_dirty_stamp.load(std::sync::atomic::Ordering::Acquire) != 0
    }
    #[inline]
    pub(crate) fn mark_fighters_dirty(&mut self) {
        self.fighters_rev = self.fighters_rev.saturating_add(1);
        if (self.fighters_rev & 0xFFFF_FFFF) == 0 {
            self.fighters_rev = self.fighters_rev.saturating_add(1);
        }
        let stamp = ((self.fighters_lifecycle_gen & 0xFFFF_FFFF) << 32)
            | (self.fighters_rev & 0xFFFF_FFFF);
        self.fighters_dirty_stamp.store(stamp, std::sync::atomic::Ordering::Release);
    }
    #[inline]
    pub(crate) fn clear_fighters_clean(&mut self) {
        self.fighters_rev = self.fighters_rev.saturating_add(1);
        self.fighters_dirty_stamp.store(0, std::sync::atomic::Ordering::Release);
        self.fighters_removed_pre_hydration.clear();
    }

    pub(crate) fn clear_fighter_ball_and_track(&mut self, ball_id: i64) -> Option<ActiveFighterSquadron> {
        let removed = self.fighters.clear_fighter_ball(ball_id);
        if self.fighters_needs_sqlite_hydration {
            if let Some(ref f) = removed {
                self.fighters_removed_pre_hydration.extend([ball_id, f.ball_id, f.item_id]);
            } else {
                self.fighters_removed_pre_hydration.insert(ball_id);
            }
            self.mark_fighters_dirty();
        } else if removed.is_some() {
            self.mark_fighters_dirty();
        }
        removed
    }

    /// Clones durable fighter state (overlaying pre-commit claim reservations) together with its
    /// `(lifecycle_gen, rev)` stamp while keeping `fighters_dirty = true` until SQLite commits.
    /// Returns `None` when clean or awaiting SQLite hydration (`F2-R5-01`).
    pub(crate) fn take_fighter_checkpoint_snapshot(&self) -> Option<FighterCheckpointSnapshot> {
        let dirty_stamp = self.fighters_dirty_stamp.load(std::sync::atomic::Ordering::Acquire);
        if dirty_stamp == 0 || self.fighters_needs_sqlite_hydration {
            return None;
        }
        let mut snap = self.fighters.clone();
        for (&rid, &(tube_key, tube_val)) in &self.pending_launch_reservations {
            snap.active_fighters.remove(&rid);
            snap.tube_loads.entry(tube_key).or_insert(tube_val);
        }
        for (&bid, claimed) in &self.pending_recall_or_scoop_claims {
            snap.active_fighters.entry(bid).or_insert_with(|| claimed.squadron.clone());
            for &(k, v) in &claimed.saved_charges {
                snap.charge_state.entry(k).or_insert(v);
            }
            for &(k, v) in &claimed.saved_cooldowns {
                snap.cooldown_state.entry(k).or_insert(v);
            }
            if claimed.accum_dmg > 0.0 {
                snap.unit_damage_accum.entry(bid).or_insert(claimed.accum_dmg);
            }
        }
        Some(FighterCheckpointSnapshot {
            state: snap,
            lifecycle_gen: self.fighters_lifecycle_gen,
            rev: self.fighters_rev,
            dirty_stamp,
        })
    }
}

pub(crate) fn merge_missing_fighter_entries(
    actor_st: &mut super::SystemActorState, state: SystemFighterState, mark_dirty: bool, clear_hydration: bool,
) -> (u64, u64) {
    if clear_hydration && !actor_st.fighters_needs_sqlite_hydration {
        return (actor_st.fighters_lifecycle_gen, actor_st.fighters_rev);
    }
    let removed = if clear_hydration {
        std::mem::take(&mut actor_st.fighters_removed_pre_hydration)
    } else {
        actor_st.fighters_removed_pre_hydration.clone()
    };
    let (mut dropped_pre_hydration, mut removed_tubes) = (false, HashSet::new());
    let reserved_tubes: HashSet<(i64, usize)> = actor_st.pending_launch_reservations.values().map(|&(tk, _)| tk).collect();
    for (k, sq) in state.active_fighters {
        if removed.contains(&k) || removed.contains(&sq.ball_id) || removed.contains(&sq.item_id) {
            removed_tubes.insert((sq.parent_ship_id, sq.tube_index));
            actor_st.fighters.tube_loads.remove(&(sq.parent_ship_id, sq.tube_index));
            actor_st.fighters.tube_rearm_until.remove(&(sq.parent_ship_id, sq.tube_index));
            dropped_pre_hydration = true;
            continue;
        }
        if !actor_st.pending_recall_or_scoop_claims.contains_key(&k) && !actor_st.pending_launch_reservations.contains_key(&k) {
            actor_st.fighters.active_fighters.entry(k).or_insert(sq);
        }
    }
    for (k, v) in state.tube_loads {
        if removed.contains(&v.0) || removed_tubes.contains(&k) { dropped_pre_hydration = true; continue; }
        if !reserved_tubes.contains(&k) { actor_st.fighters.tube_loads.entry(k).or_insert(v); }
    }
    for (k, v) in state.cooldown_state {
        if removed.contains(&k.0) { dropped_pre_hydration = true; continue; }
        actor_st.fighters.cooldown_state.entry(k).or_insert(v);
    }
    for (k, v) in state.charge_state {
        if removed.contains(&k.0) { dropped_pre_hydration = true; continue; }
        actor_st.fighters.charge_state.entry(k).or_insert(v);
    }
    for (k, v) in state.unit_damage_accum {
        if removed.contains(&k) { dropped_pre_hydration = true; continue; }
        actor_st.fighters.unit_damage_accum.entry(k).or_insert(v);
    }
    for (k, v) in state.tube_rearm_until {
        if removed_tubes.contains(&k) { dropped_pre_hydration = true; continue; }
        actor_st.fighters.tube_rearm_until.entry(k).or_insert(v);
    }
    if clear_hydration { actor_st.fighters_needs_sqlite_hydration = false; }
    if mark_dirty || dropped_pre_hydration {
        actor_st.mark_fighters_dirty();
    } else {
        actor_st.fighters_rev = actor_st.fighters_rev.saturating_add(1);
    }
    (actor_st.fighters_lifecycle_gen, actor_st.fighters_rev)
}

impl SystemFighterState {
    pub fn is_empty(&self) -> bool {
        self.cooldown_state.is_empty()
            && self.charge_state.is_empty()
            && self.active_fighters.is_empty()
            && self.tube_loads.is_empty()
            && self.unit_damage_accum.is_empty()
            && self.tube_rearm_until.is_empty()
    }

    pub fn clear_all(&mut self) {
        self.cooldown_state.clear();
        self.charge_state.clear();
        self.active_fighters.clear();
        self.tube_loads.clear();
        self.unit_damage_accum.clear();
        self.tube_rearm_until.clear();
    }

    pub fn clear_fighter_ball(&mut self, ball_id: i64) -> Option<ActiveFighterSquadron> {
        let removed = self.active_fighters.remove(&ball_id);
        self.charge_state.retain(|&(bid, _), _| bid != ball_id);
        self.cooldown_state.retain(|&(bid, _), _| bid != ball_id);
        self.unit_damage_accum.remove(&ball_id);
        removed
    }

    pub fn extract_ship_tube_transfer(&mut self, ship_id: i64) -> ShipFighterTubeTransfer {
        let mut transfer = ShipFighterTubeTransfer::default();
        let tube_keys: Vec<(i64, usize)> = self
            .tube_loads
            .keys()
            .copied()
            .filter(|&(sid, _)| sid == ship_id)
            .collect();
        for key in tube_keys {
            if let Some(entry) = self.tube_loads.remove(&key) {
                transfer.tube_loads.push((key.1, entry));
            }
        }
        let rearm_keys: Vec<(i64, usize)> = self
            .tube_rearm_until
            .keys()
            .copied()
            .filter(|&(sid, _)| sid == ship_id)
            .collect();
        for key in rearm_keys {
            if let Some(until) = self.tube_rearm_until.remove(&key) {
                transfer.tube_rearm_until.push((key.1, until));
            }
        }
        let loaded_fids: HashSet<i64> = transfer.tube_loads.iter().map(|&(_, (fid, _))| fid).collect();
        if !loaded_fids.is_empty() {
            let cd_keys: Vec<(i64, u32)> = self
                .cooldown_state
                .keys()
                .copied()
                .filter(|&(id, _)| loaded_fids.contains(&id) && !self.active_fighters.contains_key(&id))
                .collect();
            for key in cd_keys {
                if let Some(until) = self.cooldown_state.remove(&key) {
                    transfer.cooldowns.push((key, until));
                }
            }
            let ch_keys: Vec<(i64, u32)> = self
                .charge_state
                .keys()
                .copied()
                .filter(|&(id, _)| loaded_fids.contains(&id) && !self.active_fighters.contains_key(&id))
                .collect();
            for key in ch_keys {
                if let Some(used) = self.charge_state.remove(&key) {
                    transfer.charges.push((key, used));
                }
            }
        }
        transfer
    }

    pub fn insert_ship_tube_transfer(&mut self, ship_id: i64, transfer: ShipFighterTubeTransfer) {
        for (tube_idx, entry) in transfer.tube_loads {
            self.tube_loads.insert((ship_id, tube_idx), entry);
        }
        for (tube_idx, until) in transfer.tube_rearm_until {
            self.tube_rearm_until.insert((ship_id, tube_idx), until);
        }
        for (k, v) in transfer.cooldowns {
            self.cooldown_state.entry(k).or_insert(v);
        }
        for (k, v) in transfer.charges {
            self.charge_state.entry(k).or_insert(v);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShipFighterTubeTransfer {
    pub tube_loads: Vec<(usize, (i64, u32))>,
    pub tube_rearm_until: Vec<(usize, u64)>,
    pub cooldowns: Vec<((i64, u32), u64)>,
    pub charges: Vec<((i64, u32), u32)>,
}

impl ShipFighterTubeTransfer {
    pub fn is_empty(&self) -> bool {
        self.tube_loads.is_empty()
            && self.tube_rearm_until.is_empty()
            && self.cooldowns.is_empty()
            && self.charges.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct FighterActorError {
    pub code: &'static str,
    pub message: String,
    pub with_client_error: bool,
}

impl FighterActorError {
    pub(crate) fn plain(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            with_client_error: false,
        }
    }

    pub(crate) fn with_client(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            with_client_error: true,
        }
    }

    pub(crate) fn into_bridge_err(self) -> BridgeRpcError {
        if self.with_client_error {
            BridgeRpcError::with_client_error(self.code, self.message.clone(), ClientError::CustomInfo(self.message))
        } else {
            BridgeRpcError { code: self.code.into(), message: self.message, client_error: None }
        }
    }
}

/// Maps fighter type attributes / groups to `(fighter_class_attr, ship_slot_attr)`.
pub(crate) fn fighter_class_and_slot_attr(sde: &SdeRegistry, fighter_type_id: u32) -> Option<(u16, u16)> {
    let ty = sde.types.get(&fighter_type_id)?;
    for (fa, sa) in [(2212u16, 2217u16), (2213, 2218), (2214, 2219), (2740, 2737), (2741, 2738), (2742, 2739)] {
        if ty.default_attributes.get(&fa).copied().unwrap_or(0.0) > 0.0 { return Some((fa, sa)); }
    }
    match ty.group_id {
        1652 => Some((2212, 2217)), 1537 => Some((2213, 2218)), 1653 => Some((2214, 2219)),
        4777 => Some((2740, 2737)), 4778 => Some((2741, 2738)), 4779 => Some((2742, 2739)),
        _ => None,
    }
}

pub(crate) fn check_fighter_class_slot_limit_in_state(
    fighters: &SystemFighterState, sde: &SdeRegistry, ship_id: i64, carrier_type_id: u32,
    fighter_type_id: u32, count_loaded_tubes: bool, extra_squadrons: usize, replace_tube_idx: Option<usize>,
) -> Result<(), FighterActorError> {
    let Some((f_attr, s_attr)) = fighter_class_and_slot_attr(sde, fighter_type_id) else { return Ok(()); };
    let Some(raw_max) = sde.types.get(&carrier_type_id).and_then(|t| t.default_attributes.get(&s_attr).copied()) else {
        return Ok(());
    };
    let max_slots = raw_max.max(0.0) as usize;
    if max_slots == 0 {
        let code = if count_loaded_tubes { "CannotLoadFighterTubeNoTubesExistForClass" } else { "CannotLaunchFighterNotEnoughTubesAvailableForClass" };
        return Err(FighterActorError::plain(code, "UserError: No fighter launch tubes exist on this hull for that fighter class"));
    }
    let is_same_class = |tid: u32| fighter_class_and_slot_attr(sde, tid).is_some_and(|(fa, _)| fa == f_attr);
    let active_in_class = fighters.active_fighters.values()
        .filter(|f| f.parent_ship_id == ship_id && !f.is_abandoned && is_same_class(f.type_id)).count();
    let loaded_in_class = if count_loaded_tubes {
        fighters.tube_loads.iter()
            .filter(|&(&(sid, tidx), &(_, tid))| sid == ship_id && Some(tidx) != replace_tube_idx && is_same_class(tid))
            .count()
    } else { 0 };
    if active_in_class + loaded_in_class + extra_squadrons > max_slots {
        let code = if count_loaded_tubes { "CannotLoadFighterTubeNoTubesAvailableForClass" } else { "CannotLaunchFighterNotEnoughTubesAvailableForClass" };
        return Err(FighterActorError::plain(code, format!("UserError: Fighter class limit ({max_slots}) exceeded for hull {carrier_type_id}")));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum FighterTubeOp {
    PreCheckLoad { ship_id: i64, tube_index: usize, fighter_item_id: i64 },
    CommitLoad { ship_id: i64, tube_index: usize, fighter_item_id: i64, carrier_type_id: u32, fighter_type_id: u32, sde: Arc<SdeRegistry> },
    Unload { ship_id: i64, tube_index: usize },
    RestoreLoad { ship_id: i64, tube_index: usize, fighter_item_id: i64, fighter_type_id: u32 },
    RollbackLoad { ship_id: i64, tube_index: usize, expected_fighter_item_id: i64, restore_prev: Option<(i64, u32)> },
}

#[derive(Debug, Clone)]
pub struct FighterLaunchSpawnInput {
    pub squadron: ActiveFighterSquadron, pub ball: Ball, pub combat_state: CombatState, pub slim_meta: Option<SlimItemMeta>,
}

#[derive(Debug, Clone)]
pub enum FighterLaunchActorOp {
    ClaimAndReserve {
        ship_id: i64, char_id: i64, carrier_type_id: u32, tube_count: usize,
        requested_tubes: Option<Vec<usize>>, now_unix: u64, next_res_ids: Vec<i64>, sde: Arc<SdeRegistry>,
    },
    CommitSpawned { res_ids: Vec<i64>, spawns: Vec<FighterLaunchSpawnInput> },
    Rollback { ship_id: i64, res_ids: Vec<i64>, claimed_tubes: Vec<(usize, (i64, u32))> },
}

#[derive(Debug, Clone, Default)]
pub struct FighterLaunchActorOut {
    pub claimed_tubes: Vec<(usize, (i64, u32))>, pub res_ids: Vec<(usize, i64)>, pub committed_ball_ids: Vec<i64>,
}

#[derive(Debug, Clone)]
pub enum FighterRecallActorOp {
    ClaimActive { ball_id: i64, ship_id: i64, char_id: i64, sde: Arc<SdeRegistry> },
    CommitToTube { ship_id: i64, tube_index: usize, item_id: i64, type_id: u32, ball_id: i64, rearm_until_unix: u64 },
    DiscardClaimed { ball_id: i64 },
    RestoreClaimed {
        squadron: ActiveFighterSquadron, saved_charges: Vec<((i64, u32), u32)>,
        saved_cooldowns: Vec<((i64, u32), u64)>, accum_dmg: f64, abandoned_fallback: bool,
    },
}

#[derive(Debug, Clone)]
pub struct FighterRecallClaim {
    pub squadron: ActiveFighterSquadron, pub charge_reload_s: f64, pub loaded_tube_ids: HashSet<i64>,
    pub saved_charges: Vec<((i64, u32), u32)>, pub saved_cooldowns: Vec<((i64, u32), u64)>,
    pub accum_dmg: f64, pub fighter_pos: Option<DVec3>, pub ship_pos: Option<DVec3>, pub missing_cs_hp: f64,
}

#[derive(Debug, Clone)]
pub struct FighterDamageOutcome {
    pub lost: u32, pub after_size: u32, pub wiped_out: bool,
    pub item_id: i64, pub owner_char_id: i64, pub parent_ship_id: i64, pub tube_flag: i64,
}

#[derive(Debug, Clone, Default)]
pub struct FighterAbandonActorResult {
    pub abandoned: Vec<(i64, i64, usize)>, pub abandoned_positions: Vec<(i64, Option<DVec3>)>, pub foreign_active_match: bool,
}

#[derive(Debug, Clone)]
pub enum FighterScoopActorOp {
    Claim { target_fighter_id: i64 },
    Restore { key: i64, squadron: ActiveFighterSquadron },
    Commit { key: i64 },
}

#[derive(Debug, Clone)]
pub struct FighterMwdSpec {
    pub fighter_ball_id: i64, pub cooldown_secs: u64, pub slot_id: u32, pub max_charges: Option<u32>, pub speed_bonus_pct: f64,
}

#[derive(Debug, Clone)]
pub enum FighterAbilityActorOp {
    ReserveSlot { char_id: i64, fighter_ball_id: i64, parent_ship_id: i64, slot_id: u32, ability_id: u32, now_unix: u64, effective_gate_sec: f64, charge_count: i64 },
    RollbackSlot { fighter_ball_id: i64, slot_id: u32, ability_id: u32, prev_cooldown: Option<u64>, prev_mwd_active: bool, spent_charge: bool },
    DeactivateSlot { fighter_ball_id: i64, slot_id: u32, ability_id: u32, speed_bonus_pct: f64 },
    ActivateMwdBatch { char_id: i64, specs: Vec<FighterMwdSpec>, now_unix: u64 },
    ReserveMjdBatch { char_id: i64, targets: Vec<(i64, u32, f64, u64)>, now_unix: u64 },
    RestoreMovementCooldown { fighter_ball_id: i64, slot_idx: u32, prev_cooldown: Option<u64> },
}

#[derive(Debug, Clone, Default)]
pub struct FighterAbilityActorOut {
    pub squadron_size: u32,
    pub remaining_charges: i64,
    pub prev_cooldown: Option<u64>,
    pub prev_mwd_active: bool,
    pub spent_charge: bool,
    pub mjd_committed: Vec<(i64, u32, f64, Option<u64>)>,
    pub was_mwd: bool,
}

#[cfg(any(test, feature = "testkit"))]
#[derive(Debug, Clone)]
pub enum FighterTestHelperOp {
    InsertActive(i64, ActiveFighterSquadron), RemoveActive(i64), ClearAllActive,
    SetTubeLoad(i64, usize, i64, u32), RemoveTubeLoad(i64, usize), ClearAllTubes,
    SetCooldown(i64, u32, u64), RemoveCooldown(i64, u32), ClearAllCooldowns,
    SetCharge(i64, u32, u32), RemoveCharge(i64, u32), ClearAllCharges,
    SetDamageAccum(i64, f64), RemoveDamageAccum(i64), ClearAllDamageAccum,
    SetTubeRearm(i64, usize, u64), RemoveTubeRearm(i64, usize), ClearAllTubeRearm,
    ClearAllState,
}

#[cfg(any(test, feature = "testkit"))]
#[derive(Debug, Clone, Default)]
pub struct FighterTestHelperOut {
    pub removed_squadron: Option<ActiveFighterSquadron>,
    pub removed_i64: Option<i64>, pub removed_u64: Option<u64>, pub removed_u32: Option<u32>, pub removed_f64: Option<f64>,
}

#[cfg(any(test, feature = "testkit"))]
pub(crate) fn apply_test_helper_op_to_state(
    state: &mut SystemFighterState,
    op: FighterTestHelperOp,
) -> FighterTestHelperOut {
    let mut out = FighterTestHelperOut::default();
    match op {
        FighterTestHelperOp::InsertActive(id, sq) => { state.active_fighters.insert(id, sq); }
        FighterTestHelperOp::RemoveActive(id) => { out.removed_squadron = state.active_fighters.remove(&id); }
        FighterTestHelperOp::ClearAllActive => state.active_fighters.clear(),
        FighterTestHelperOp::SetTubeLoad(sid, tidx, fid, ftype) => { state.tube_loads.insert((sid, tidx), (fid, ftype)); }
        FighterTestHelperOp::RemoveTubeLoad(sid, tidx) => { out.removed_i64 = state.tube_loads.remove(&(sid, tidx)).map(|(fid, _)| fid); }
        FighterTestHelperOp::ClearAllTubes => state.tube_loads.clear(),
        FighterTestHelperOp::SetCooldown(fid, slot, until) => { state.cooldown_state.insert((fid, slot), until); }
        FighterTestHelperOp::RemoveCooldown(fid, slot) => { out.removed_u64 = state.cooldown_state.remove(&(fid, slot)); }
        FighterTestHelperOp::ClearAllCooldowns => state.cooldown_state.clear(),
        FighterTestHelperOp::SetCharge(fid, slot, used) => { state.charge_state.insert((fid, slot), used); }
        FighterTestHelperOp::RemoveCharge(fid, slot) => { out.removed_u32 = state.charge_state.remove(&(fid, slot)); }
        FighterTestHelperOp::ClearAllCharges => state.charge_state.clear(),
        FighterTestHelperOp::SetDamageAccum(fid, dmg) => { state.unit_damage_accum.insert(fid, dmg); }
        FighterTestHelperOp::RemoveDamageAccum(fid) => { out.removed_f64 = state.unit_damage_accum.remove(&fid); }
        FighterTestHelperOp::ClearAllDamageAccum => state.unit_damage_accum.clear(),
        FighterTestHelperOp::SetTubeRearm(sid, tidx, until) => { state.tube_rearm_until.insert((sid, tidx), until); }
        FighterTestHelperOp::RemoveTubeRearm(sid, tidx) => { out.removed_u64 = state.tube_rearm_until.remove(&(sid, tidx)); }
        FighterTestHelperOp::ClearAllTubeRearm => state.tube_rearm_until.clear(),
        FighterTestHelperOp::ClearAllState => state.clear_all(),
    }
    out
}

fn remove_fighter_synth_cycles_inner(
    solar_system_id: SolarSystemId, ballpark: &mut Ballpark, actor_st: &mut super::SystemActorState,
    ball_positions: &Arc<eve_db::lock_rank::RankedRwLock<HashMap<BallId, DVec3>, { eve_db::lock_rank::RANK_LEAF }>>,
    scrambled_balls: &Arc<eve_db::lock_rank::RankedRwLock<HashSet<BallId>, { eve_db::lock_rank::RANK_LEAF }>>,
    active_module_ships: Option<&Arc<eve_db::lock_rank::RankedRwLock<HashSet<BallId>, { eve_db::lock_rank::RANK_LEAF }>>>,
    module_deactivation_hook: &Arc<eve_db::lock_rank::RankedRwLock<Option<super::ModuleDeactivationHook>, { eve_db::lock_rank::RANK_LEAF }>>,
    post_unlock: &mut super::post_unlock::PostUnlockOps, last_active_totals: Option<&mut HashMap<BallId, (Option<i32>, Option<i32>)>>,
    fighter_ball_id: i64, only_slot: Option<u32>,
) {
    let slots = match only_slot { Some(s) => vec![s], None => vec![0, 1, 2] };
    let mut removed_any = false;
    let mut scratch_totals = HashMap::new();
    let totals_ref = last_active_totals.unwrap_or(&mut scratch_totals);
    for slot_id in slots {
        let module_id = -(fighter_ball_id * 10 + i64::from(slot_id));
        actor_st.weapon_cycle_start_modules.remove(&(fighter_ball_id, module_id));
        if let Some(pos) = actor_st.active_module_cycles.iter().position(|c| c.ship_id == fighter_ball_id && c.module_id == module_id) {
            if !removed_any {
                cycles_tackle::snapshot_active_tackle_totals(ballpark, &actor_st.active_module_cycles, totals_ref);
                removed_any = true;
            }
            let removed = actor_st.active_module_cycles.remove(pos);
            cycles::cleanup_removed_module_cycle_collecting(ballpark, &removed, &actor_st.db_opt, post_unlock);
        }
    }
    if removed_any {
        let deact_guard = module_deactivation_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            solar_system_id, ballpark, &mut actor_st.active_module_cycles,
            &actor_st.base_scramble_strengths, &actor_st.base_core_strengths, &actor_st.db_opt, deact_guard.as_ref(), post_unlock,
        );
        protection::sync_ball_protection_in_actor(ballpark, actor_st, fighter_ball_id);
        sync_ball_positions(ballpark, ball_positions, scrambled_balls);
        if let Some(ships_cache) = active_module_ships {
            protection::sync_active_module_ships_cache(&actor_st.active_module_cycles, &actor_st.external_active_modules, ships_cache);
        }
    }
}

fn remove_fighter_synth_cycles_in_actor(exec_ctx: &mut ActorExecCtx<'_>, fighter_ball_id: i64, only_slot: Option<u32>) {
    remove_fighter_synth_cycles_inner(
        exec_ctx.solar_system_id, exec_ctx.ballpark, exec_ctx.actor_st, exec_ctx.ball_positions_actor,
        exec_ctx.scrambled_balls_actor, Some(exec_ctx.active_module_ships_actor), exec_ctx.module_deactivation_hook_actor,
        exec_ctx.post_unlock_ops, Some(exec_ctx.last_active_totals), fighter_ball_id, only_slot,
    );
}

fn check_tube_load_conflicts(
    state: &SystemFighterState, ship_id: i64, tube_index: usize, fighter_item_id: i64, loaded_code: &'static str,
) -> Result<(), FighterActorError> {
    if state.tube_loads.iter().any(|(&(sid, tidx), &(fid, _))| sid == ship_id && fid == fighter_item_id && tidx != tube_index) {
        return Err(FighterActorError::plain(loaded_code, format!("Fighter item {fighter_item_id} is already loaded in another fighter tube")));
    }
    if state.active_fighters.values().any(|f| (f.item_id == fighter_item_id || (f.parent_ship_id == ship_id && f.tube_index == tube_index)) && !f.is_abandoned) {
        return Err(FighterActorError::plain("FighterAlreadyLaunched", format!("Tube {tube_index} or fighter {fighter_item_id} currently deployed in space")));
    }
    Ok(())
}

pub(crate) fn exec_tube_op_in_state(state: &mut SystemFighterState, op: FighterTubeOp) -> Result<Option<i64>, FighterActorError> {
    match op {
        FighterTubeOp::PreCheckLoad { ship_id, tube_index, fighter_item_id } => {
            check_tube_load_conflicts(state, ship_id, tube_index, fighter_item_id, "FighterAlreadyLoaded")?;
            Ok(None)
        }
        FighterTubeOp::CommitLoad { ship_id, tube_index, fighter_item_id, carrier_type_id, fighter_type_id, sde } => {
            check_tube_load_conflicts(state, ship_id, tube_index, fighter_item_id, "FighterAlreadyLoadedInTube")?;
            check_fighter_class_slot_limit_in_state(state, &sde, ship_id, carrier_type_id, fighter_type_id, true, 1, Some(tube_index))?;
            Ok(state.tube_loads.insert((ship_id, tube_index), (fighter_item_id, fighter_type_id)).map(|(fid, _)| fid))
        }
        FighterTubeOp::Unload { ship_id, tube_index } => {
            if state.active_fighters.values().any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_index && !f.is_abandoned) {
                return Err(FighterActorError::plain("FighterAlreadyLaunched", format!("Cannot unload tube {tube_index} while squadron is deployed in space")));
            }
            Ok(state.tube_loads.remove(&(ship_id, tube_index)).map(|(fid, _)| fid))
        }
        FighterTubeOp::RestoreLoad { ship_id, tube_index, fighter_item_id, fighter_type_id } => {
            state.tube_loads.insert((ship_id, tube_index), (fighter_item_id, fighter_type_id));
            Ok(Some(fighter_item_id))
        }
        FighterTubeOp::RollbackLoad { ship_id, tube_index, expected_fighter_item_id, restore_prev } => {
            if state.tube_loads.get(&(ship_id, tube_index)).is_some_and(|&(fid, _)| fid == expected_fighter_item_id) {
                state.tube_loads.remove(&(ship_id, tube_index));
                if let Some(prev) = restore_prev {
                    state.tube_loads.insert((ship_id, tube_index), prev);
                    return Ok(Some(prev.0));
                }
            }
            Ok(None)
        }
    }
}

fn handle_launch_op(exec_ctx: &mut ActorExecCtx<'_>, op: FighterLaunchActorOp) -> Result<FighterLaunchActorOut, FighterActorError> {
    match op {
        FighterLaunchActorOp::ClaimAndReserve { ship_id, char_id, carrier_type_id, tube_count, requested_tubes, now_unix, next_res_ids, sde } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            let active_tube = |f: &ActiveFighterSquadron, tidx: usize| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned;
            let tubes_to_launch = if let Some(req) = requested_tubes {
                for &tidx in &req {
                    if tidx >= tube_count {
                        return Err(FighterActorError::plain("InvalidFighterTube", format!("Fighter tube {tidx} out of range (max {tube_count})")));
                    }
                    if fg.active_fighters.values().any(|f| active_tube(f, tidx)) {
                        return Err(FighterActorError::plain("FighterAlreadyLaunched", format!("Fighter squadron in tube {tidx} is already launched in space")));
                    }
                    if !fg.tube_loads.contains_key(&(ship_id, tidx)) {
                        return Err(FighterActorError::plain("EmptyFighterTube", format!("Fighter tube {tidx} is empty")));
                    }
                    let rearm_until = fg.tube_rearm_until.get(&(ship_id, tidx)).copied().unwrap_or(0);
                    if rearm_until > now_unix {
                        return Err(FighterActorError::plain("TubeRearming", format!("Fighter tube {tidx} is currently rearming/repairing ({}s remaining)", rearm_until - now_unix)));
                    }
                }
                req
            } else {
                let available: Vec<usize> = (0..tube_count)
                    .filter(|&tidx| fg.tube_loads.contains_key(&(ship_id, tidx)) && fg.tube_rearm_until.get(&(ship_id, tidx)).copied().unwrap_or(0) <= now_unix)
                    .collect();
                if available.is_empty() {
                    if (0..tube_count).any(|tidx| fg.active_fighters.values().any(|f| active_tube(f, tidx))) {
                        return Err(FighterActorError::plain("FighterAlreadyLaunched", "All loaded fighter squadrons are already launched in space"));
                    }
                    if let Some((tidx, rem)) = (0..tube_count).find_map(|tidx| {
                        fg.tube_loads.contains_key(&(ship_id, tidx)).then(|| fg.tube_rearm_until.get(&(ship_id, tidx)).copied().filter(|&u| u > now_unix).map(|u| (tidx, u - now_unix)))?
                    }) {
                        return Err(FighterActorError::plain("TubeRearming", format!("Fighter tube {tidx} is currently rearming/repairing ({rem}s remaining)")));
                    }
                    return Err(FighterActorError::plain("EmptyFighterTube", "No fighters loaded in tubes"));
                }
                available
            };

            let mut claimed_tubes = Vec::with_capacity(tubes_to_launch.len());
            for &tidx in &tubes_to_launch {
                if let Some(entry) = fg.tube_loads.remove(&(ship_id, tidx)) {
                    claimed_tubes.push((tidx, entry));
                }
            }
            if claimed_tubes.is_empty() {
                return Err(FighterActorError::plain(
                    "EmptyFighterTube",
                    "Requested fighter tubes are empty",
                ));
            }
            let db_opt = exec_ctx.actor_st.db_opt.clone();
            let mut res_ids = Vec::with_capacity(claimed_tubes.len());
            for (idx, &(tube_idx, (fid, raw_ftype))) in claimed_tubes.iter().enumerate() {
                let ftype = if raw_ftype != 0 {
                    raw_ftype
                } else {
                    db_opt.as_ref().and_then(|db| db.with_snapshot(|s| s.items.get(&fid).map(|i| i.type_id))).unwrap_or(0)
                };
                if ftype == 0 {
                    for &(_, rid) in &res_ids {
                        fg.active_fighters.remove(&rid);
                        exec_ctx.actor_st.pending_launch_reservations.remove(&rid);
                    }
                    for &(ctidx, centry) in &claimed_tubes { fg.tube_loads.insert((ship_id, ctidx), centry); }
                    return Err(FighterActorError::plain("ItemNotFound", format!("Fighter item {fid} in tube {tube_idx} no longer exists")));
                }
                if let Err(err) = check_fighter_class_slot_limit_in_state(fg, &sde, ship_id, carrier_type_id, ftype, false, 1, None) {
                    for &(_, rid) in &res_ids {
                        fg.active_fighters.remove(&rid);
                        exec_ctx.actor_st.pending_launch_reservations.remove(&rid);
                    }
                    for &(ctidx, centry) in &claimed_tubes { fg.tube_loads.insert((ship_id, ctidx), centry); }
                    return Err(err);
                }
                let rid = next_res_ids[idx];
                fg.active_fighters.insert(rid, ActiveFighterSquadron {
                    ball_id: rid, item_id: fid, type_id: ftype, owner_char_id: char_id, parent_ship_id: ship_id,
                    tube_index: tube_idx, squadron_size: 1, target_id: None, is_abandoned: false, mwd_active: false,
                });
                exec_ctx.actor_st.pending_launch_reservations.insert(rid, ((ship_id, tube_idx), (fid, ftype)));
                res_ids.push((tube_idx, rid));
            }
            Ok(FighterLaunchActorOut { claimed_tubes, res_ids, committed_ball_ids: Vec::new() })
        }
        FighterLaunchActorOp::CommitSpawned { res_ids, spawns } => {
            for rid in &res_ids {
                exec_ctx.actor_st.fighters.active_fighters.remove(rid);
                exec_ctx.actor_st.pending_launch_reservations.remove(rid);
            }
            let deact_guard = protection::read_deact_hook(exec_ctx);
            let mut committed_ball_ids = Vec::with_capacity(spawns.len());
            for s in spawns {
                let bid = s.squadron.ball_id;
                exec_ctx.actor_st.fighters.active_fighters.insert(bid, s.squadron);
                let token = cycles_tackle::spawn_ball_with_combat_in_actor_collecting(
                    exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles,
                    &mut exec_ctx.actor_st.base_scramble_strengths, &mut exec_ctx.actor_st.base_core_strengths,
                    &exec_ctx.actor_st.sde_opt, &exec_ctx.actor_st.db_opt, deact_guard.as_ref(),
                    s.ball, s.combat_state, s.slim_meta, exec_ctx.post_unlock_ops,
                );
                exec_ctx.ball_spawn_tokens.insert(bid, token);
                celestials::record_spawned_ball_checkpoint_baseline(exec_ctx.actor_st, exec_ctx.ballpark, bid);
                committed_ball_ids.push(bid);
            }
            protection::sync_positions_in_actor(exec_ctx);
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterLaunchActorOut { claimed_tubes: Vec::new(), res_ids: Vec::new(), committed_ball_ids })
        }
        FighterLaunchActorOp::Rollback { ship_id, res_ids, claimed_tubes } => {
            for rid in res_ids {
                exec_ctx.actor_st.fighters.active_fighters.remove(&rid);
                exec_ctx.actor_st.pending_launch_reservations.remove(&rid);
            }
            for (tidx, entry) in claimed_tubes { exec_ctx.actor_st.fighters.tube_loads.insert((ship_id, tidx), entry); }
            Ok(FighterLaunchActorOut::default())
        }
    }
}

fn handle_recall_op(
    exec_ctx: &mut ActorExecCtx<'_>,
    op: FighterRecallActorOp,
) -> Option<FighterRecallClaim> {
    match op {
        FighterRecallActorOp::ClaimActive {
            ball_id,
            ship_id,
            char_id,
            sde,
        } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            let valid = fg.active_fighters.get(&ball_id).is_some_and(|cur| {
                cur.parent_ship_id == ship_id
                    && cur.owner_char_id == char_id
                    && !cur.is_abandoned
                    && cur.squadron_size > 0
            });
            if !valid {
                return None;
            }
            let f = fg.active_fighters.remove(&ball_id)?;
            let charge_reload_s: f64 = sde
                .fighter_abilities_by_type
                .get(&f.type_id)
                .map(|slots| {
                    slots
                        .iter()
                        .filter_map(|s| {
                            let ch = s.charges.as_ref()?;
                            let used = fg
                                .charge_state
                                .get(&(ball_id, s.slot_index as u32))
                                .copied()
                                .unwrap_or(0);
                            let max_c = ch.charge_count.max(1) as f64;
                            (used > 0)
                                .then_some(((used as f64) / max_c) * (ch.rearm_time_seconds.max(1.0) * 0.45))
                        })
                        .sum()
                })
                .unwrap_or(0.0);
            let saved_charges: Vec<_> = fg
                .charge_state
                .iter()
                .filter(|(&(id, _), _)| id == ball_id)
                .map(|(&k, &v)| (k, v))
                .collect();
            let saved_cooldowns: Vec<_> = fg
                .cooldown_state
                .iter()
                .filter(|(&(id, _), _)| id == ball_id)
                .map(|(&k, &v)| (k, v))
                .collect();
            fg.charge_state.retain(|&(id, _), _| id != ball_id);
            fg.cooldown_state.retain(|&(id, _), _| id != ball_id);
            let accum_dmg = fg.unit_damage_accum.remove(&ball_id).unwrap_or(0.0);
            let loaded_tube_ids: HashSet<i64> = fg.tube_loads.values().map(|&(fid, _)| fid).collect();
            let fighter_pos = exec_ctx.ballpark.balls.get(&ball_id).map(|b| b.position);
            let ship_pos = exec_ctx.ballpark.balls.get(&ship_id).map(|b| b.position);
            let missing_cs_hp = exec_ctx
                .ballpark
                .combat_states
                .get(&ball_id)
                .map(|cs| {
                    (cs.max_shield_hp - cs.shield_hp).max(0.0)
                        + (cs.max_armor_hp - cs.armor_hp).max(0.0)
                        + (cs.max_structure_hp - cs.structure_hp).max(0.0)
                })
                .unwrap_or(0.0);
            exec_ctx.actor_st.pending_recall_or_scoop_claims.insert(
                ball_id,
                PendingClaimedSquadron {
                    squadron: f.clone(),
                    saved_charges: saved_charges.clone(),
                    saved_cooldowns: saved_cooldowns.clone(),
                    accum_dmg,
                },
            );
            Some(FighterRecallClaim {
                squadron: f,
                charge_reload_s,
                loaded_tube_ids,
                saved_charges,
                saved_cooldowns,
                accum_dmg,
                fighter_pos,
                ship_pos,
                missing_cs_hp,
            })
        }
        FighterRecallActorOp::CommitToTube {
            ship_id,
            tube_index,
            item_id,
            type_id,
            ball_id,
            rearm_until_unix,
        } => {
            exec_ctx.actor_st.pending_recall_or_scoop_claims.remove(&ball_id);
            exec_ctx
                .actor_st
                .fighters
                .tube_loads
                .entry((ship_id, tube_index))
                .or_insert((item_id, type_id));
            exec_ctx
                .actor_st
                .fighters
                .tube_rearm_until
                .insert((ship_id, tube_index), rearm_until_unix);
            exec_ctx.actor_st.mark_fighters_dirty();
            let mut bctx = ball_ops::BallOpsCtx::from_exec_ctx(exec_ctx);
            let _ = ball_ops::remove_ball_in_actor(&mut bctx, ball_id, false);
            None
        }
        FighterRecallActorOp::DiscardClaimed { ball_id } => {
            exec_ctx.actor_st.pending_recall_or_scoop_claims.remove(&ball_id);
            let _ = exec_ctx.actor_st.clear_fighter_ball_and_track(ball_id);
            exec_ctx.actor_st.mark_fighters_dirty();
            let mut bctx = ball_ops::BallOpsCtx::from_exec_ctx(exec_ctx);
            let _ = ball_ops::remove_ball_in_actor(&mut bctx, ball_id, false);
            None
        }
        FighterRecallActorOp::RestoreClaimed {
            mut squadron,
            saved_charges,
            saved_cooldowns,
            accum_dmg,
            abandoned_fallback,
        } => {
            let bid = squadron.ball_id;
            exec_ctx.actor_st.pending_recall_or_scoop_claims.remove(&bid);
            if abandoned_fallback {
                squadron.is_abandoned = true;
                squadron.parent_ship_id = 0;
                squadron.target_id = None;
            }
            let fg = &mut exec_ctx.actor_st.fighters;
            fg.active_fighters.entry(bid).or_insert(squadron);
            for (k, v) in saved_charges {
                fg.charge_state.insert(k, v);
            }
            for (k, v) in saved_cooldowns {
                fg.cooldown_state.insert(k, v);
            }
            if accum_dmg > 0.0 {
                fg.unit_damage_accum.insert(bid, accum_dmg);
            }
            if abandoned_fallback {
                let _ = exec_ctx.ballpark.force_stop_warp(bid);
                let _ = exec_ctx.ballpark.set_mode(bid, MovementMode::Stop);
                exec_ctx.actor_st.warp_state.clear_ball(bid, true);
                sync_ball_positions(
                    exec_ctx.ballpark,
                    exec_ctx.ball_positions_actor,
                    exec_ctx.scrambled_balls_actor,
                );
                exec_ctx.actor_st.mark_fighters_dirty();
            }
            None
        }
    }
}

fn handle_apply_damage(
    exec_ctx: &mut ActorExecCtx<'_>,
    fighter_ball_id: i64,
    raw_damage: f64,
    sde: &SdeRegistry,
) -> Option<FighterDamageOutcome> {
    let fg = &mut exec_ctx.actor_st.fighters;
    let Some(type_id) = fg.active_fighters.get(&fighter_ball_id).map(|s| s.type_id) else {
        fg.unit_damage_accum.remove(&fighter_ball_id);
        return None;
    };
    let unit_hp = sde.types.get(&type_id)
        .and_then(|t| t.default_attributes.get(&263).copied().or_else(|| t.default_attributes.get(&9).copied()))
        .unwrap_or(1_000.0).max(250.0);
    let accum = fg.unit_damage_accum.entry(fighter_ball_id).or_insert(0.0);
    *accum += raw_damage.max(0.0);
    let units_killed = (*accum / unit_hp).floor() as u32;
    if units_killed > 0 { *accum = (*accum - (units_killed as f64) * unit_hp).max(0.0); }

    let live = fg.active_fighters.get_mut(&fighter_ball_id)?;
    let before_size = live.squadron_size;
    let lost = units_killed.min(before_size);
    let after_size = before_size.saturating_sub(lost);
    let wiped_out = after_size == 0;
    let (item_id, owner_char_id, parent_ship_id, tube_flag) = (live.item_id, live.owner_char_id, live.parent_ship_id, (159 + live.tube_index) as i64);
    if wiped_out {
        let _ = exec_ctx.actor_st.clear_fighter_ball_and_track(fighter_ball_id);
    } else if lost > 0 {
        live.squadron_size = after_size;
    }
    if wiped_out || lost > 0 || raw_damage > 0.0 { exec_ctx.actor_st.mark_fighters_dirty(); }
    Some(FighterDamageOutcome { lost, after_size, wiped_out, item_id, owner_char_id, parent_ship_id, tube_flag })
}

#[allow(clippy::too_many_arguments)]
fn abandon_owned_fighters_core(
    solar_system_id: SolarSystemId, ballpark: &mut Ballpark, actor_st: &mut super::SystemActorState,
    ball_positions: &Arc<eve_db::lock_rank::RankedRwLock<HashMap<BallId, DVec3>, { eve_db::lock_rank::RANK_LEAF }>>,
    scrambled_balls: &Arc<eve_db::lock_rank::RankedRwLock<HashSet<BallId>, { eve_db::lock_rank::RANK_LEAF }>>,
    active_module_ships: Option<&Arc<eve_db::lock_rank::RankedRwLock<HashSet<BallId>, { eve_db::lock_rank::RANK_LEAF }>>>,
    module_deactivation_hook: &Arc<eve_db::lock_rank::RankedRwLock<Option<super::ModuleDeactivationHook>, { eve_db::lock_rank::RANK_LEAF }>>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
    mut last_active_totals: Option<&mut HashMap<BallId, (Option<i32>, Option<i32>)>>,
    char_id: i64, ship_id: i64, filter_ids: Option<&[i64]>, db_in_sys_items: &HashSet<i64>,
    clear_tubes: bool, only_matching_sys_items: bool,
) -> FighterAbandonActorResult {
    let mut result = FighterAbandonActorResult::default();
    let fg = &mut actor_st.fighters;
    let candidates: Vec<(i64, i64, i64, usize, bool, i64, i64)> = fg.active_fighters.iter()
        .map(|(&key, f)| (key, f.ball_id, f.item_id, f.tube_index, f.is_abandoned, f.owner_char_id, f.parent_ship_id)).collect();
    for (key, bid, iid, tidx, is_ab, owner, psid) in candidates {
        if filter_ids.is_some_and(|ids| !ids.contains(&bid) && !ids.contains(&iid) && !ids.contains(&key)) { continue; }
        let matches_owner = if only_matching_sys_items {
            (ballpark.balls.contains_key(&bid) || db_in_sys_items.contains(&iid)) && ((char_id > 0 && owner == char_id) || psid == ship_id) && !is_ab
        } else { psid == ship_id && owner == char_id && !is_ab };
        if matches_owner {
            if let Some(f) = fg.active_fighters.get_mut(&key) {
                (f.is_abandoned, f.parent_ship_id, f.target_id) = (true, 0, None);
                result.abandoned.push((bid, iid, tidx));
            }
        } else if filter_ids.is_some() && !is_ab && owner != char_id { result.foreign_active_match = true; }
    }
    if !result.abandoned.is_empty() {
        if clear_tubes { for &(_, _, tidx) in &result.abandoned { fg.tube_loads.remove(&(ship_id, tidx)); } }
        for &(bid, _, _) in &result.abandoned {
            remove_fighter_synth_cycles_inner(
                solar_system_id, ballpark, actor_st, ball_positions, scrambled_balls,
                active_module_ships, module_deactivation_hook, post_unlock, last_active_totals.as_deref_mut(), bid, None,
            );
            let _ = ballpark.force_stop_warp(bid);
            let _ = ballpark.set_mode(bid, MovementMode::Stop);
            actor_st.warp_state.clear_ball(bid, true);
            result.abandoned_positions.push((bid, ballpark.balls.get(&bid).map(|b| b.position)));
        }
        sync_ball_positions(ballpark, ball_positions, scrambled_balls);
        actor_st.mark_fighters_dirty();
    }
    result
}

fn handle_abandon_owned(
    exec_ctx: &mut ActorExecCtx<'_>, char_id: i64, ship_id: i64, filter_ids: Option<Vec<i64>>,
    db_in_sys_items: HashSet<i64>, clear_tubes: bool, only_matching_sys_items: bool,
) -> FighterAbandonActorResult {
    abandon_owned_fighters_core(
        exec_ctx.solar_system_id, exec_ctx.ballpark, exec_ctx.actor_st, exec_ctx.ball_positions_actor,
        exec_ctx.scrambled_balls_actor, Some(exec_ctx.active_module_ships_actor), exec_ctx.module_deactivation_hook_actor,
        exec_ctx.post_unlock_ops, Some(exec_ctx.last_active_totals), char_id, ship_id,
        filter_ids.as_deref(), &db_in_sys_items, clear_tubes, only_matching_sys_items,
    )
}

fn handle_ability_op(
    exec_ctx: &mut ActorExecCtx<'_>,
    op: FighterAbilityActorOp,
) -> Result<FighterAbilityActorOut, FighterActorError> {
    match op {
        FighterAbilityActorOp::ReserveSlot {
            char_id, fighter_ball_id, parent_ship_id, slot_id, ability_id, now_unix, effective_gate_sec, charge_count,
        } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            let (squadron_size, prev_mwd_active) = {
                let Some(live) = fg.active_fighters.get(&fighter_ball_id) else {
                    return Err(FighterActorError::with_client("FighterNotFound", format!("Fighter squadron {fighter_ball_id} not found or not launched")));
                };
                if live.owner_char_id != char_id || live.is_abandoned || live.parent_ship_id != parent_ship_id {
                    return Err(FighterActorError::with_client("FighterNotOwned", format!("Fighter squadron {fighter_ball_id} is not controlled by character {char_id}")));
                }
                if live.squadron_size == 0 {
                    return Err(FighterActorError::with_client("FighterNotFound", format!("Fighter squadron {fighter_ball_id} has no remaining fighters")));
                }
                (live.squadron_size, live.mwd_active)
            };
            if let Some(&ready_at) = fg.cooldown_state.get(&(fighter_ball_id, slot_id)) {
                if now_unix < ready_at {
                    return Err(FighterActorError::with_client("AbilityOnCooldown", format!("Fighter ability slot {slot_id} on {fighter_ball_id} is on cooldown until {ready_at}")));
                }
            }
            let (remaining_charges, spent_charge) = if charge_count > 0 {
                let entry = fg.charge_state.entry((fighter_ball_id, slot_id)).or_insert(0);
                if *entry >= charge_count as u32 {
                    return Err(FighterActorError::with_client("OutOfCharges", format!("Fighter ability slot {slot_id} on {fighter_ball_id} has exhausted all {charge_count} charges")));
                }
                *entry = entry.saturating_add(1);
                ((charge_count as i64).saturating_sub(*entry as i64), true)
            } else { (0, false) };
            let prev_cooldown = fg.cooldown_state.insert((fighter_ball_id, slot_id), now_unix.saturating_add(effective_gate_sec.ceil() as u64));
            let is_mwd = matches!(ability_id, 4 | 9 | 13);
            if is_mwd {
                if let Some(live) = fg.active_fighters.get_mut(&fighter_ball_id) { live.mwd_active = true; }
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut {
                squadron_size, remaining_charges, prev_cooldown,
                prev_mwd_active: if is_mwd { prev_mwd_active } else { false }, spent_charge, ..Default::default()
            })
        }
        FighterAbilityActorOp::RollbackSlot { fighter_ball_id, slot_id, ability_id, prev_cooldown, prev_mwd_active, spent_charge } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            if fg.active_fighters.contains_key(&fighter_ball_id) {
                if let Some(old_cd) = prev_cooldown { fg.cooldown_state.insert((fighter_ball_id, slot_id), old_cd); } else { fg.cooldown_state.remove(&(fighter_ball_id, slot_id)); }
                if spent_charge {
                    if let Some(used) = fg.charge_state.get_mut(&(fighter_ball_id, slot_id)) {
                        *used = used.saturating_sub(1);
                        if *used == 0 { fg.charge_state.remove(&(fighter_ball_id, slot_id)); }
                    }
                }
                if matches!(ability_id, 4 | 9 | 13) {
                    if let Some(live) = fg.active_fighters.get_mut(&fighter_ball_id) { live.mwd_active = prev_mwd_active; }
                }
            } else {
                fg.cooldown_state.remove(&(fighter_ball_id, slot_id));
                fg.charge_state.remove(&(fighter_ball_id, slot_id));
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut::default())
        }
        FighterAbilityActorOp::DeactivateSlot { fighter_ball_id, slot_id, ability_id, speed_bonus_pct } => {
            let mut was_mwd = false;
            if matches!(ability_id, 4 | 9 | 13) || slot_id == 1 {
                if let Some(f) = exec_ctx.actor_st.fighters.active_fighters.get_mut(&fighter_ball_id) {
                    was_mwd = f.mwd_active;
                    f.mwd_active = false;
                }
                if was_mwd {
                    if let Some(cur_max) = exec_ctx.ballpark.balls.get(&fighter_ball_id).map(|b| b.max_velocity) {
                        exec_ctx.ballpark.set_ball_max_velocity(fighter_ball_id, (cur_max / (1.0 + speed_bonus_pct / 100.0)).max(1.0));
                    }
                }
            }
            remove_fighter_synth_cycles_in_actor(exec_ctx, fighter_ball_id, Some(slot_id));
            if slot_id == 0 {
                if let Some(f) = exec_ctx.actor_st.fighters.active_fighters.get_mut(&fighter_ball_id) { f.target_id = None; }
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut { was_mwd, ..Default::default() })
        }
        FighterAbilityActorOp::ActivateMwdBatch { char_id, specs, now_unix } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            let (mut pending_charges, mut pending_cds) = (HashMap::new(), HashMap::new());
            let mut plan: Vec<(i64, f64, bool)> = Vec::with_capacity(specs.len());
            for spec in &specs {
                let fid = spec.fighter_ball_id;
                let Some(f) = fg.active_fighters.get(&fid) else {
                    return Err(FighterActorError::plain("FighterNotFound", format!("Fighter squadron {fid} not found or not launched")));
                };
                if f.owner_char_id != char_id || f.is_abandoned {
                    return Err(FighterActorError::plain("FighterNotOwned", format!("Fighter squadron {fid} is not controlled by pilot")));
                }
                let was_mwd = f.mwd_active || plan.iter().any(|&(id, _, _)| id == fid);
                if let Some(ready_at) = pending_cds.get(&(fid, spec.slot_id)).copied().or_else(|| fg.cooldown_state.get(&(fid, spec.slot_id)).copied()) {
                    if now_unix < ready_at {
                        return Err(FighterActorError::plain("AbilityOnCooldown", format!("Fighter MWD slot {} on {fid} is on cooldown until {ready_at}", spec.slot_id)));
                    }
                }
                if let Some(max_c) = spec.max_charges {
                    let used = pending_charges.get(&(fid, spec.slot_id)).copied()
                        .unwrap_or_else(|| fg.charge_state.get(&(fid, spec.slot_id)).copied().unwrap_or(0));
                    if used >= max_c {
                        return Err(FighterActorError::plain("OutOfCharges", format!("Fighter MWD slot {} on {fid} has exhausted all {max_c} charges", spec.slot_id)));
                    }
                    pending_charges.insert((fid, spec.slot_id), used.saturating_add(1));
                }
                pending_cds.insert((fid, spec.slot_id), now_unix + spec.cooldown_secs);
                plan.push((fid, spec.speed_bonus_pct, was_mwd));
            }
            for (k, v) in pending_charges { fg.charge_state.insert(k, v); }
            for (k, v) in pending_cds { fg.cooldown_state.insert(k, v); }
            for &(fid, speed_bonus_pct, was_mwd) in &plan {
                if let Some(f) = fg.active_fighters.get_mut(&fid) { f.mwd_active = true; }
                if !was_mwd {
                    if let Some(cur_max) = exec_ctx.ballpark.balls.get(&fid).map(|b| b.max_velocity) {
                        exec_ctx.ballpark.set_ball_max_velocity(fid, cur_max * (1.0 + speed_bonus_pct / 100.0));
                    }
                }
                exec_ctx.ballpark.set_speed_fraction(fid, 1.0);
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut::default())
        }
        FighterAbilityActorOp::ReserveMjdBatch { char_id, targets, now_unix } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            for &(fid, slot_idx, _, _) in &targets {
                let Some(sq) = fg.active_fighters.get(&fid) else {
                    return Err(FighterActorError::with_client("FighterNotFound", format!("Fighter squadron {fid} not found or not launched")));
                };
                if sq.owner_char_id != char_id || sq.is_abandoned {
                    return Err(FighterActorError::with_client("FighterNotOwned", format!("Fighter squadron {fid} is not controlled by character {char_id}")));
                }
                if let Some(&ready_at) = fg.cooldown_state.get(&(fid, slot_idx)) {
                    if now_unix < ready_at {
                        return Err(FighterActorError::with_client("AbilityOnCooldown", format!("Fighter micro jump drive on {fid} is on cooldown until {ready_at}")));
                    }
                }
            }
            let mut committed = Vec::with_capacity(targets.len());
            for (fid, slot_idx, mjd_dist, cd_secs) in targets {
                let prev_cd = fg.cooldown_state.insert((fid, slot_idx), now_unix + cd_secs);
                committed.push((fid, slot_idx, mjd_dist, prev_cd));
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut { mjd_committed: committed, ..Default::default() })
        }
        FighterAbilityActorOp::RestoreMovementCooldown { fighter_ball_id, slot_idx, prev_cooldown } => {
            let fg = &mut exec_ctx.actor_st.fighters;
            if fg.active_fighters.contains_key(&fighter_ball_id) {
                if let Some(old) = prev_cooldown {
                    fg.cooldown_state.insert((fighter_ball_id, slot_idx), old);
                } else {
                    fg.cooldown_state.remove(&(fighter_ball_id, slot_idx));
                }
            } else {
                fg.cooldown_state.remove(&(fighter_ball_id, slot_idx));
            }
            exec_ctx.actor_st.mark_fighters_dirty();
            Ok(FighterAbilityActorOut::default())
        }
    }
}

pub(in crate::system_actor) fn handle_fighter_runtime_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    let is_view_mutating = cmd.may_mutate_system_view();
    match cmd {
        SystemCommand::FlushFighterCheckpoint { reply } => {
            let snap = exec_ctx.actor_st.take_fighter_checkpoint_snapshot();
            defer_reply(exec_ctx.deferred_replies, reply, snap);
        }
        SystemCommand::LoadOrUnloadFighterTube { mut op, reply } => {
            if let FighterTubeOp::CommitLoad { ship_id, .. } = &op {
                if exec_ctx.actor_st.fighters.tube_loads.iter().any(|(&(sid, _), &(_, tid))| sid == *ship_id && tid == 0) {
                    if let Some(ref db) = exec_ctx.actor_st.db_opt {
                        db.with_snapshot(|snap| {
                            for (&(sid, _), (fid, tid)) in &mut exec_ctx.actor_st.fighters.tube_loads {
                                if sid == *ship_id && *tid == 0 { if let Some(it) = snap.items.get(fid) { *tid = it.type_id; } }
                            }
                        });
                    }
                }
            }
            if let FighterTubeOp::RollbackLoad { restore_prev: Some((pfid, ref mut ptid)), .. } = &mut op {
                if *ptid == 0 {
                    if let Some(ref db) = exec_ctx.actor_st.db_opt {
                        *ptid = db.with_snapshot(|snap| snap.items.get(pfid).map(|it| it.type_id)).unwrap_or(0);
                    }
                }
            }
            let mark_dirty = !matches!(op, FighterTubeOp::PreCheckLoad { .. });
            let res = exec_tube_op_in_state(&mut exec_ctx.actor_st.fighters, op);
            if mark_dirty && res.is_ok() { exec_ctx.actor_st.mark_fighters_dirty(); }
            defer_reply(exec_ctx.deferred_replies, reply, res);
        }
        SystemCommand::FighterLaunchOp { op, reply } => { let res = handle_launch_op(exec_ctx, op); defer_reply(exec_ctx.deferred_replies, reply, res); }
        SystemCommand::FighterRecallOp { op, reply } => { let res = handle_recall_op(exec_ctx, op); defer_reply(exec_ctx.deferred_replies, reply, res); }
        SystemCommand::ApplyDamageToFighterSquadron { fighter_ball_id, raw_damage, sde, reply } => {
            let res = handle_apply_damage(exec_ctx, fighter_ball_id, raw_damage, &sde);
            defer_reply(exec_ctx.deferred_replies, reply, res);
        }
        SystemCommand::AbandonOwnedFighters { char_id, ship_id, filter_ids, db_in_sys_items, clear_tubes, only_matching_sys_items, reply } => {
            let res = handle_abandon_owned(exec_ctx, char_id, ship_id, filter_ids, db_in_sys_items, clear_tubes, only_matching_sys_items);
            defer_reply(exec_ctx.deferred_replies, reply, res);
        }
        SystemCommand::ClaimOrRestoreAbandonedFighterForScoop { op, reply } => {
            let res = match op {
                FighterScoopActorOp::Claim { target_fighter_id } => {
                    let fg = &mut exec_ctx.actor_st.fighters;
                    let resolved = if fg.active_fighters.contains_key(&target_fighter_id) { Some(target_fighter_id) }
                        else { fg.active_fighters.iter().find(|(_, f)| f.item_id == target_fighter_id).map(|(&k, _)| k) };
                    match resolved {
                        Some(resolved_key) if !fg.active_fighters[&resolved_key].is_abandoned => Err(FighterActorError::plain(
                            "FighterNotAbandoned", format!("Fighter squadron {target_fighter_id} is active and cannot be scooped"),
                        )),
                        Some(resolved_key) => {
                            let sq = fg.active_fighters.remove(&resolved_key);
                            if let Some(ref s) = sq {
                                exec_ctx.actor_st.pending_recall_or_scoop_claims.insert(resolved_key, PendingClaimedSquadron {
                                    squadron: s.clone(), saved_charges: Vec::new(), saved_cooldowns: Vec::new(), accum_dmg: 0.0,
                                });
                            }
                            Ok(sq.map(|s| (resolved_key, s)))
                        }
                        None => Err(FighterActorError::plain("FighterNotFound", format!("Fighter squadron {target_fighter_id} not found in space"))),
                    }
                }
                FighterScoopActorOp::Restore { key, squadron } => {
                    exec_ctx.actor_st.pending_recall_or_scoop_claims.remove(&key);
                    exec_ctx.actor_st.fighters.active_fighters.entry(key).or_insert(squadron);
                    Ok(None)
                }
                FighterScoopActorOp::Commit { key } => {
                    exec_ctx.actor_st.pending_recall_or_scoop_claims.remove(&key);
                    let _ = exec_ctx.actor_st.clear_fighter_ball_and_track(key);
                    exec_ctx.actor_st.mark_fighters_dirty();
                    Ok(None)
                }
            };
            defer_reply(exec_ctx.deferred_replies, reply, res);
        }
        SystemCommand::MutateFighterAbilityState { op, reply } => { let res = handle_ability_op(exec_ctx, op); defer_reply(exec_ctx.deferred_replies, reply, res); }
        SystemCommand::CleanupDestroyedFighter { ball_id, reply } => { defer_reply(exec_ctx.deferred_replies, reply, cleanup_destroyed_fighter_in_actor(exec_ctx.actor_st, ball_id)); }
        SystemCommand::ExtractShipFighterTubes { ship_id, reply } => {
            let transfer = exec_ctx.actor_st.fighters.extract_ship_tube_transfer(ship_id);
            if !transfer.is_empty() { exec_ctx.actor_st.mark_fighters_dirty(); }
            defer_reply(exec_ctx.deferred_replies, reply, transfer);
        }
        SystemCommand::InsertShipFighterTubes { ship_id, transfer, reply } => {
            if !transfer.is_empty() { exec_ctx.actor_st.fighters.insert_ship_tube_transfer(ship_id, transfer); exec_ctx.actor_st.mark_fighters_dirty(); }
            defer_reply(exec_ctx.deferred_replies, reply, ());
        }
        SystemCommand::SeedAbandonedFighters { squadrons, reply } => {
            for sq in squadrons { exec_ctx.actor_st.fighters.active_fighters.entry(sq.ball_id).or_insert(sq); }
            exec_ctx.actor_st.mark_fighters_dirty();
            defer_reply(exec_ctx.deferred_replies, reply, ());
        }
        SystemCommand::ReloadFighterStateClean { state, reply } => {
            exec_ctx.actor_st.fighters = state;
            exec_ctx.actor_st.pending_launch_reservations.clear();
            exec_ctx.actor_st.pending_recall_or_scoop_claims.clear();
            exec_ctx.actor_st.fighters_needs_sqlite_hydration = false;
            exec_ctx.actor_st.clear_fighters_clean();
            let stamp = (exec_ctx.actor_st.fighters_lifecycle_gen, exec_ctx.actor_st.fighters_rev);
            defer_reply(exec_ctx.deferred_replies, reply, stamp);
        }
        SystemCommand::MergeFighterState { state, mark_dirty, clear_hydration, reply } => {
            let stamp = merge_missing_fighter_entries(exec_ctx.actor_st, state, mark_dirty, clear_hydration);
            defer_reply(exec_ctx.deferred_replies, reply, stamp);
        }
        #[cfg(any(test, feature = "testkit"))]
        SystemCommand::TestHelperFighterOp { op, reply } => {
            let out = apply_test_helper_op_to_state(&mut exec_ctx.actor_st.fighters, op);
            exec_ctx.actor_st.mark_fighters_dirty();
            defer_reply(exec_ctx.deferred_replies, reply, out);
        }
        other => return Some(other),
    }
    if is_view_mutating {
        if let Some(bid) = exec_ctx.spawned_checkpoint_ball.take() {
            celestials::record_spawned_ball_checkpoint_baseline(exec_ctx.actor_st, exec_ctx.ballpark, bid);
        }
        if let Some(bid) = *exec_ctx.removed_checkpoint_ball {
            celestials::remove_ball_checkpoint_tracking(exec_ctx.actor_st, bid);
        }
        celestials::update_system_hibernation_flag(exec_ctx.actor_st, exec_ctx.ballpark, exec_ctx.event_tx_actor.receiver_count());
    }
    None
}

pub(crate) fn cleanup_destroyed_fighter_in_actor(
    actor_st: &mut super::SystemActorState, ball_id: i64,
) -> Option<ActiveFighterSquadron> {
    let key = actor_st.fighters.active_fighters.iter().find(|(_, f)| f.ball_id == ball_id || f.item_id == ball_id).map(|(&k, _)| k);
    let removed = match key {
        Some(k) => actor_st.clear_fighter_ball_and_track(k),
        None => { let _ = actor_st.clear_fighter_ball_and_track(ball_id); None }
    };
    let mut dirty = false;
    let res_keys: Vec<i64> = actor_st.pending_launch_reservations.iter()
        .filter(|&(&rid, &(_, (fid, _)))| rid == ball_id || fid == ball_id || removed.as_ref().is_some_and(|f| rid == f.ball_id))
        .map(|(&rid, _)| rid).collect();
    for rid in res_keys {
        if let Some((_, (fid, _))) = actor_st.pending_launch_reservations.remove(&rid) {
            if actor_st.fighters_needs_sqlite_hydration { actor_st.fighters_removed_pre_hydration.extend([rid, fid]); }
        }
        dirty = true;
    }
    let claim_keys: Vec<i64> = actor_st.pending_recall_or_scoop_claims.iter()
        .filter(|(&k, c)| k == ball_id || c.squadron.ball_id == ball_id || c.squadron.item_id == ball_id)
        .map(|(&k, _)| k).collect();
    let mut claimed_sq = None;
    for k in claim_keys {
        if let Some(c) = actor_st.pending_recall_or_scoop_claims.remove(&k) {
            if actor_st.fighters_needs_sqlite_hydration {
                actor_st.fighters_removed_pre_hydration.extend([k, c.squadron.ball_id, c.squadron.item_id]);
            }
            let _ = actor_st.clear_fighter_ball_and_track(k);
            actor_st.fighters.tube_loads.remove(&(c.squadron.parent_ship_id, c.squadron.tube_index));
            actor_st.fighters.tube_rearm_until.remove(&(c.squadron.parent_ship_id, c.squadron.tube_index));
            if claimed_sq.is_none() { claimed_sq = Some(c.squadron); }
            dirty = true;
        }
    }
    if let Some(ref f) = removed {
        actor_st.fighters.tube_loads.remove(&(f.parent_ship_id, f.tube_index));
        actor_st.fighters.tube_rearm_until.remove(&(f.parent_ship_id, f.tube_index));
        dirty = true;
    } else if actor_st.fighters.unit_damage_accum.remove(&ball_id).is_some() {
        dirty = true;
    }
    if dirty { actor_st.mark_fighters_dirty(); }
    removed.or(claimed_sq)
}

impl SystemHandle {
    pub(super) fn persist_snapshot_sync(&self, snap: &super::types::FighterCheckpointSnapshot) -> Result<usize, String> {
        match self.fighter_ctx.db_opt {
            Some(ref db) => fighters_persistence::persist_live_fighter_snapshot_sync(
                db, self.solar_system_id, &self.fighter_ctx.persist, snap,
                &self.fighter_ctx.dirty_stamp, self.fighter_ctx.sde_opt.as_deref(), false,
            ),
            None => Ok(0),
        }
    }

    pub(super) fn flush_step_fighter_checkpoint(&self, snap: &super::types::FighterCheckpointSnapshot) {
        if let Err(err) = self.persist_snapshot_sync(snap) {
            tracing::warn!(solar_system_id = self.solar_system_id, error = %err, "Failed to flush step fighter checkpoint; keeping dirty flag");
        }
    }

    pub async fn flush_fighter_checkpoint(&self) -> Result<usize, String> {
        let Some(snap) = ball_ops::call_actor_reply_with_deadline(
            self, "FlushFighterCheckpoint", ball_ops::ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::FlushFighterCheckpoint { reply },
        ).await? else { return Ok(0); };
        self.persist_snapshot_sync(&snap)
    }

    pub fn flush_fighter_checkpoint_sync(&self) -> Result<usize, String> {
        let Some(snap) = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "FlushFighterCheckpoint", ball_ops::ACTOR_CMD_TIMEOUT, (),
            |_, reply| SystemCommand::FlushFighterCheckpoint { reply },
            |ctx, _| ctx.actor_st.take_fighter_checkpoint_snapshot(),
        )? else { return Ok(0); };
        self.persist_snapshot_sync(&snap)
    }

    async fn call_fighter_cmd<R>(
        &self,
        op_name: &'static str,
        build: impl FnOnce(tokio::sync::oneshot::Sender<Result<R, FighterActorError>>) -> SystemCommand,
    ) -> Result<R, FighterActorError> {
        ball_ops::call_actor_reply_with_deadline(self, op_name, ball_ops::ACTOR_CMD_TIMEOUT, build)
            .await
            .unwrap_or_else(|e| Err(FighterActorError::plain("SolarSystemUnavailable", e)))
    }

    pub async fn load_or_unload_fighter_tube(&self, op: FighterTubeOp) -> Result<Option<i64>, FighterActorError> { self.call_fighter_cmd("LoadOrUnloadFighterTube", |reply| SystemCommand::LoadOrUnloadFighterTube { op, reply }).await }
    pub async fn fighter_launch_op(&self, op: FighterLaunchActorOp) -> Result<FighterLaunchActorOut, FighterActorError> { self.call_fighter_cmd("FighterLaunchOp", |reply| SystemCommand::FighterLaunchOp { op, reply }).await }
    pub async fn fighter_recall_op(&self, op: FighterRecallActorOp) -> Result<Option<FighterRecallClaim>, String> { ball_ops::call_actor_reply_with_deadline(self, "FighterRecallOp", ball_ops::ACTOR_CMD_TIMEOUT, |reply| SystemCommand::FighterRecallOp { op, reply }).await }
    pub async fn apply_damage_to_fighter_squadron_in_actor(&self, fighter_ball_id: i64, raw_damage: f64, sde: Arc<SdeRegistry>) -> Option<FighterDamageOutcome> { ball_ops::call_actor_reply_with_deadline(self, "ApplyDamageToFighterSquadron", ball_ops::ACTOR_CMD_TIMEOUT, |reply| SystemCommand::ApplyDamageToFighterSquadron { fighter_ball_id, raw_damage, sde, reply }).await.ok().flatten() }
    pub async fn abandon_owned_fighters(&self, char_id: i64, ship_id: i64, filter_ids: Option<Vec<i64>>) -> Result<FighterAbandonActorResult, String> {
        ball_ops::call_actor_reply_with_deadline(self, "AbandonOwnedFighters", ball_ops::ACTOR_CMD_TIMEOUT, |reply| SystemCommand::AbandonOwnedFighters { char_id, ship_id, filter_ids, db_in_sys_items: HashSet::new(), clear_tubes: false, only_matching_sys_items: false, reply }).await
    }
    pub fn abandon_ship_fighters_sync(&self, char_id: i64, ship_id: i64, db_in_sys_items: HashSet<i64>) -> Result<FighterAbandonActorResult, String> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "AbandonOwnedFighters", ball_ops::ACTOR_CMD_TIMEOUT, (char_id, ship_id, db_in_sys_items),
            |(cid, sid, items), reply| SystemCommand::AbandonOwnedFighters { char_id: cid, ship_id: sid, filter_ids: None, db_in_sys_items: items, clear_tubes: true, only_matching_sys_items: true, reply },
            |ctx, (cid, sid, items)| abandon_owned_fighters_core(ctx.solar_system_id, ctx.ballpark, ctx.actor_st, ctx.ball_positions, ctx.scrambled_balls, Some(&self.active_module_ships), ctx.module_deactivation_hook, ctx.post_unlock_ops, None, cid, sid, None, &items, true, true),
        )
    }
    pub async fn scoop_abandoned_fighter_op(&self, op: FighterScoopActorOp) -> Result<Option<(i64, ActiveFighterSquadron)>, FighterActorError> { self.call_fighter_cmd("ClaimOrRestoreAbandonedFighterForScoop", |reply| SystemCommand::ClaimOrRestoreAbandonedFighterForScoop { op, reply }).await }
    pub async fn mutate_fighter_ability_state(&self, op: FighterAbilityActorOp) -> Result<FighterAbilityActorOut, FighterActorError> { self.call_fighter_cmd("MutateFighterAbilityState", |reply| SystemCommand::MutateFighterAbilityState { op, reply }).await }
    pub fn cleanup_destroyed_fighter_sync(&self, ball_id: i64) -> Option<ActiveFighterSquadron> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "CleanupDestroyedFighter", ball_ops::ACTOR_CMD_TIMEOUT, ball_id,
            |bid, reply| SystemCommand::CleanupDestroyedFighter { ball_id: bid, reply },
            |ctx, bid| cleanup_destroyed_fighter_in_actor(ctx.actor_st, bid),
        ).ok().flatten()
    }
    pub fn extract_ship_fighter_tubes_sync(&self, ship_id: i64) -> Result<ShipFighterTubeTransfer, String> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "ExtractShipFighterTubes", ball_ops::ACTOR_CMD_TIMEOUT, ship_id,
            |sid, reply| SystemCommand::ExtractShipFighterTubes { ship_id: sid, reply },
            |ctx, sid| { let tr = ctx.actor_st.fighters.extract_ship_tube_transfer(sid); if !tr.is_empty() { ctx.actor_st.mark_fighters_dirty(); } tr },
        )
    }

    pub fn insert_ship_fighter_tubes_sync(&self, ship_id: i64, transfer: ShipFighterTubeTransfer) -> Result<(), String> {
        if transfer.is_empty() { return Ok(()); }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "InsertShipFighterTubes", ball_ops::ACTOR_CMD_TIMEOUT, (ship_id, transfer),
            |(sid, tr), reply| SystemCommand::InsertShipFighterTubes { ship_id: sid, transfer: tr, reply },
            |ctx, (sid, tr)| if !tr.is_empty() { ctx.actor_st.fighters.insert_ship_tube_transfer(sid, tr); ctx.actor_st.mark_fighters_dirty(); },
        )
    }

    pub fn seed_abandoned_fighters_sync(&self, squadrons: Vec<ActiveFighterSquadron>) -> Result<(), String> {
        if squadrons.is_empty() { return Ok(()); }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "SeedAbandonedFighters", ball_ops::ACTOR_CMD_TIMEOUT, squadrons,
            |list, reply| SystemCommand::SeedAbandonedFighters { squadrons: list, reply },
            |ctx, list| {
                for sq in list { ctx.actor_st.fighters.active_fighters.entry(sq.ball_id).or_insert(sq); }
                ctx.actor_st.mark_fighters_dirty();
            },
        )
    }

    pub fn reload_fighter_state_clean_sync(&self, state: SystemFighterState) -> Option<(u64, u64)> {
        let out = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "ReloadFighterStateClean", ball_ops::ACTOR_CMD_TIMEOUT, state,
            |next, reply| SystemCommand::ReloadFighterStateClean { state: next, reply },
            |ctx, next| {
                ctx.actor_st.fighters = next;
                ctx.actor_st.pending_launch_reservations.clear();
                ctx.actor_st.pending_recall_or_scoop_claims.clear();
                ctx.actor_st.fighters_needs_sqlite_hydration = false;
                ctx.actor_st.clear_fighters_clean();
                (ctx.actor_st.fighters_lifecycle_gen, ctx.actor_st.fighters_rev)
            },
        ).ok();
        if out.is_some() {
            self.fighters_needs_hydration.store(false, std::sync::atomic::Ordering::Release);
        }
        out
    }

    pub(crate) fn merge_fighter_state_sync(&self, state: SystemFighterState, mark_dirty: bool, clear_hydration: bool) -> Result<(u64, u64), String> {
        let stamp = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "MergeFighterState", ball_ops::ACTOR_CMD_TIMEOUT, (state, mark_dirty, clear_hydration),
            |(st, md, ch), reply| SystemCommand::MergeFighterState { state: st, mark_dirty: md, clear_hydration: ch, reply },
            |ctx, (st, md, ch)| merge_missing_fighter_entries(ctx.actor_st, st, md, ch),
        )?;
        if clear_hydration {
            self.fighters_needs_hydration.store(false, std::sync::atomic::Ordering::Release);
        }
        Ok(stamp)
    }

    pub(crate) async fn merge_fighter_state_async(&self, state: SystemFighterState, mark_dirty: bool, clear_hydration: bool) -> Result<(u64, u64), String> {
        let stamp = ball_ops::call_actor_reply_with_deadline(
            self, "MergeFighterState", ball_ops::ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::MergeFighterState { state, mark_dirty, clear_hydration, reply },
        ).await?;
        if clear_hydration {
            self.fighters_needs_hydration.store(false, std::sync::atomic::Ordering::Release);
        }
        Ok(stamp)
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn test_helper_fighter_op_sync(&self, op: FighterTestHelperOp) -> FighterTestHelperOut {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "TestHelperFighterOp", ball_ops::ACTOR_CMD_TIMEOUT, op,
            |o, reply| SystemCommand::TestHelperFighterOp { op: o, reply },
            |ctx, o| { let out = apply_test_helper_op_to_state(&mut ctx.actor_st.fighters, o); ctx.actor_st.mark_fighters_dirty(); out },
        ).unwrap_or_default()
    }
}
