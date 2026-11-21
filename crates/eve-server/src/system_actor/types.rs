//! Types, command enum, and state definitions for `system_actor`.

#![forbid(unsafe_code)]

use eve_db::lock_rank::{RankedMutex, RankedRwLock, RANK_LEAF};
use eve_destiny::{
    Ball, BallId, Ballpark, CombatProtectionSnapshot, CombatState, DamageVector, DestinyEvent,
    GridBubble, MovementMode, SlimItemMeta, TargetLock, WarpBubbleMgr,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Origin classification of an active ship warp (`#1175`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpProvenance {
    /// Solo player warp or fleet warp initiated via client or fleet RPCs.
    PlayerOrFleetWarp,
    /// SP-6 outbound emergency warp triggered on mid-space disconnect.
    DisconnectedEmergencyWarp,
    /// SP-6 return warp restoring a reconnecting pilot to pre-disconnect coordinates.
    ReconnectReturnWarp,
}

/// Shared `ServerContext` warp-tracking tables wired into spawned solar systems (`#1175`).
#[derive(Clone, Default)]
pub(crate) struct SharedWarpTracking {
    pub active_warps: Option<Arc<RankedMutex<HashMap<i64, i64>, RANK_LEAF>>>,
    pub return_warps: Option<Arc<RankedMutex<HashSet<i64>, RANK_LEAF>>>,
    pub pending_warp_finished: Option<Arc<RankedMutex<HashSet<i64>, RANK_LEAF>>>,
}

/// State of an individual harvestable asteroid entity in space (`SP-4`, `MN-01`, `MN-02`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsteroidState {
    pub asteroid_id: BallId,
    pub belt_id: BallId,
    pub type_id: u32,
    pub initial_units: u64,
    pub remaining_units: u64,
    pub unit_volume: f64,
    pub depleted: bool,
    pub respawn_at_tick: Option<u64>,
}

/// Result of an individual mining/harvesting extraction cycle (`MN-01`, `MN-02`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningExtractionResult {
    pub mined_units: u64,
    pub wasted_units: u64,
    pub remaining_units: u64,
    pub ore_type_id: u32,
    pub depleted: bool,
}

#[derive(Debug, Clone)]
pub struct TickBroadcast {
    pub solar_system_id: SolarSystemId,
    pub tick: u64,
    /// Monotonic per-system broadcast sequence number assigned at `event_tx.send` time (`GH #1172`).
    pub broadcast_seq: u64,
    pub events: Vec<DestinyEvent>,
    pub observer_events: BTreeMap<BallId, Vec<DestinyEvent>>,
    pub observer_updates_pyrep: BTreeMap<BallId, PyRep>,
}

#[derive(Debug, Clone, Default)]
pub struct ActiveModuleCycle {
    pub ship_id: BallId,
    pub module_id: i64,
    pub module_type_id: u32,
    pub owner_char_id: Option<CharacterId>,
    pub effect_name: String,
    pub target_id: Option<BallId>,
    pub repeat: i32,
    pub duration_s: f64,
    pub elapsed_s: f64,
    pub cap_need_per_cycle: f64,
    pub speed_boost_factor: f64,
    pub linked_turrets: Vec<i64>,
    pub ammo_type_id: Option<u32>,
    pub damage_vector: DamageVector,
    pub cap_drain_target: f64,
    pub cap_transfer_target: f64,
    pub nosferatu_blood_raider_override: bool,
    pub shield_boost_amount: f64,
    pub armor_repair_amount: f64,
    pub hull_repair_amount: f64,
    pub remote_shield_amount: f64,
    pub remote_armor_amount: f64,
    pub remote_hull_amount: f64,
    pub remote_cap_amount: f64,
    pub remote_optimal: f64,
    pub remote_falloff: f64,
    pub mutadaptive_spool_step: f64,
    pub mutadaptive_spool_max: f64,
    pub current_spool_cycles: u32,
    pub cap_booster_inject_amount: f64,
    pub is_asb: bool,
    pub asb_fallback_cap_need: f64,
    pub is_aar: bool,
    pub aar_paste_rate: i32,
    pub aar_base_armor_repair: f64,
    pub is_rah: bool,
    pub rah_shift_amount: f64,
    pub is_adc: bool,
    pub ehe_hull_resists: Option<[f64; 4]>,
    pub target_velocity_multiplier: Option<f64>,
    pub immobilize_self: bool,
    pub self_disallow_assistance: bool,
    pub self_ewar_immune: bool,
    pub self_disable_mwd_mjd: bool,
    pub self_mass_multiplier: Option<f64>,
    pub is_auto_targeter: bool,
    pub auto_target_max_range: f64,
    pub auto_target_max_locks: usize,
    pub auto_target_scan_res: f64,
    pub crystal_volatility_chance: f64,
    pub crystal_volatility_damage: f64,
    pub is_overloaded: bool,
    pub pending_overloaded: Option<bool>,
    pub rack_idx: usize,
    pub heat_damage_per_cycle: f64,
    pub reactivation_delay_s: f64,
    pub is_mwd_or_mjd: bool,
    pub warp_scramble_points: i32,
    pub disables_target_mwd_mjd: bool,
    pub warp_core_stab_bonus: i32,
    pub hic_bubble_radius_m: Option<f64>,
    pub hic_velocity_multiplier: Option<f64>,
    pub hic_sig_multiplier: Option<f64>,
    pub hic_mass_addition: Option<f64>,
    pub mjd_jump_distance_m: Option<f64>,
    pub mjfg_field_params: Option<(f64, usize, usize)>,
    pub grants_interdiction_nullification: bool,
    pub effective_max_capacitor: Option<f64>,
    /// Maximum effective range in meters for targeted modules (`plan:DG-4`).
    /// Deactivates module cycle if target moves outside this range.
    pub max_effective_range_m: Option<f64>,
    pub lance_beam_dir: Option<DVec3>,
    pub grappler_optimal_m: f64,
    pub grappler_falloff_m: f64,
    pub grappler_speed_factor_pct: f64,
    pub weapon_is_turret: bool,
    pub weapon_is_missile: bool,
    pub weapon_is_guided_missile: bool,
    pub weapon_optimal_m: f64,
    pub weapon_falloff_m: f64,
    pub weapon_tracking_speed: f64,
    pub weapon_sig_resolution_m: f64,
    pub missile_explosion_radius_m: f64,
    pub missile_explosion_velocity_ms: f64,
    pub missile_drf: f64,
    pub missile_max_velocity_ms: f64,
    pub missile_flight_time_s: f64,
    pub missile_ball_radius: f64,
    pub missile_reload_ms: f64,
    pub missile_charge_type_id: Option<TypeId>,
    pub missile_charge_capacity: u32,
    pub reload_remaining_sim_s: f64,
    /// Unix timestamp (seconds) when this module cycle was added or began its current cycle (`#498`).
    pub started_at_unix: u64,
}

impl ActiveModuleCycle {
    /// Returns `true` when this cycle paid upfront at activation for an end-of-cycle
    /// repair or capacitor-vampire transfer (`#890`) that must finish its current cycle
    /// on `Deactivate` rather than being dropped mid-cycle.
    pub fn has_paid_end_of_cycle_effect(&self) -> bool {
        self.armor_repair_amount > 0.0
            || self.hull_repair_amount > 0.0
            || (self.remote_armor_amount > 0.0 && self.mutadaptive_spool_step <= 0.0)
            || self.remote_hull_amount > 0.0
            || (self.is_aar && self.aar_base_armor_repair > 0.0)
            || (self.cap_transfer_target > 0.0 && self.remote_cap_amount <= 0.0)
    }
}

#[derive(Debug, Clone)]
pub struct ActiveNaniteRepair {
    pub char_id: CharacterId,
    pub ship_id: BallId,
    pub module_id: i64,
    pub initial_damage_hp: f64,
    pub paste_total_needed: i32,
    pub paste_consumed_so_far: i32,
    pub duration_s: f64,
    pub elapsed_s: f64,
}

#[derive(Debug, Clone)]
pub struct PendingAnchoringDisruptor {
    pub id: BallId,
    pub type_id: u32,
    pub position: DVec3,
    pub radius: f64,
    pub remaining_s: f64,
}

#[derive(Debug, Clone)]
pub enum PendingCycleAction {
    SalvageEnd {
        char_id: CharacterId,
        ship_id: BallId,
        module_id: i64,
        module_type_id: u32,
        wreck_id: BallId,
        repeat: i32,
        effect_name: String,
        next_cycle_cap_deducted: bool,
        deducted_cap_gj: f64,
    },
    LanceBeamFired {
        attacker_char_id: CharacterId,
        attacker_ship_id: BallId,
        attacker_ship_type_id: u32,
        module_id: i64,
        module_type_id: u32,
        hits: Vec<(BallId, f64)>,
    },
    RepeatCycleStart {
        char_id: CharacterId,
        ship_id: BallId,
        module_id: i64,
        module_type_id: u32,
        target_id: Option<BallId>,
        duration_s: f64,
        is_overloaded: bool,
        effect_name: String,
    },
    SyncOverloadState {
        char_id: Option<CharacterId>,
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
    },
    InvalidateShipFitting {
        char_id: Option<CharacterId>,
        ship_id: BallId,
    },
    CycleWeaponDamage {
        attacker_char_id: Option<CharacterId>,
        attacker_ship_id: BallId,
        attacker_ship_type_id: u32,
        target_id: BallId,
        target_char_id: Option<CharacterId>,
        module_type_id: u32,
        applied_dmg: f64,
        notified: bool,
    },
    CycleWeaponMiss {
        attacker_char_id: Option<CharacterId>,
        attacker_ship_id: BallId,
        target_id: BallId,
        target_char_id: Option<CharacterId>,
        module_type_id: u32,
        notified: bool,
    },
    CycleChargeItemMutations {
        char_id: CharacterId,
        ship_id: BallId,
        diffs: Vec<(
            Option<eve_db::InventoryItemRecord>,
            Option<eve_db::InventoryItemRecord>,
        )>,
    },
    ShipCapacitorChanged {
        char_id: CharacterId,
        ship_id: BallId,
        new_cap: f64,
        max_cap: f64,
        old_cap: f64,
        notified: bool,
    },
    AutoReloadStarted {
        char_id: CharacterId,
        module_ids: Vec<ItemId>,
        charge_type_id: TypeId,
        time_ms: f64,
        reload_until_unix: u64,
        sim_deadline_s: f64,
        notified: bool,
    },
    NoChargesDeactivated {
        char_id: CharacterId,
        module_type_id: TypeId,
        notified: bool,
    },
    CareerProgressEvent {
        char_id: CharacterId,
        source_ship_id: BallId,
        target_id: BallId,
        activity_kind: &'static str,
        sub_kind: &'static str,
        amount: f64,
    },
}

/// Queued guided missile launch awaiting next-tick `AddBalls2` acquisition batching.
#[derive(Debug, Clone)]
pub(crate) struct PendingMissileLaunch {
    pub queued_tick: u64,
    pub char_id: CharacterId,
    pub owner_id: i64,
    pub corp_id: i64,
    pub alliance_id: i64,
    pub attacker_ship_id: BallId,
    pub target_ball_id: BallId,
    pub master_module_id: ItemId,
    pub launcher_module_id: ItemId,
    pub module_type_id: TypeId,
    pub ammo_type_id: TypeId,
    pub max_velocity_ms: f64,
    pub flight_time_s: f64,
    pub ball_radius: f64,
    pub explosion_radius_m: f64,
    pub explosion_velocity_ms: f64,
    pub drf: f64,
    pub damage_vector: DamageVector,
    pub duration_ms: f64,
}

/// Queued deployment `OnSpecialFX` emitted on the tick after missile `AddBalls2`.
#[derive(Debug, Clone)]
pub(crate) struct PendingMissileDeployFx {
    pub emit_at_tick: u64,
    pub attacker_ship_id: BallId,
    pub target_ball_id: BallId,
    pub master_module_id: ItemId,
    pub module_type_id: TypeId,
    pub ammo_type_id: TypeId,
    pub duration_ms: f64,
}

/// Live in-flight guided missile ball tracked in `SystemActorState`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ActiveGuidedMissile {
    pub ball_id: BallId,
    pub spawn_tick: u64,
    pub char_id: CharacterId,
    pub attacker_ship_id: BallId,
    pub target_ball_id: BallId,
    pub module_id: ItemId,
    pub module_type_id: TypeId,
    pub ammo_type_id: TypeId,
    pub max_velocity_ms: f64,
    pub flight_time_s: f64,
    pub elapsed_sim_s: f64,
    pub min_visual_sim_s: f64,
    pub impacted: bool,
    pub explosion_radius_m: f64,
    pub explosion_velocity_ms: f64,
    pub drf: f64,
    pub damage_vector: DamageVector,
}

/// Staggered 10-tick cadence for per-system condition checkpoints (`RFC-1 E-4`, `T-5`, `#919`).
pub const CONDITION_CHECKPOINT_CADENCE_TICKS: u64 = 10;

/// Returns `true` when `tick` is a staggered condition-checkpoint tick for `solar_system_id`
/// (`(tick + solar_system_id) % 10 == 0`).
#[inline]
pub const fn is_condition_checkpoint_tick(solar_system_id: SolarSystemId, tick: u64) -> bool {
    tick.wrapping_add(solar_system_id as u64) % CONDITION_CHECKPOINT_CADENCE_TICKS == 0
}

/// Dirty-only in-space ship condition checkpoint entry emitted in [`StepOutput`] (`RFC-1 E-4`, `T-2`, `T-5`).
#[derive(Debug, Clone)]
pub struct ConditionCheckpointEntry {
    pub ball_id: BallId,
    pub space_entry_epoch: u64,
    pub combat_state: Option<CombatState>,
    pub position: [f64; 3],
}

/// Cached last-checkpointed ball state inside [`SystemActorState`] for dirty-only filtering (`T-5`).
#[derive(Debug, Clone, PartialEq)]
pub struct LastCheckpointedBallState {
    pub space_entry_epoch: u64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub structure_hp: f64,
    pub capacitor: f64,
    pub position: [f64; 3],
}

#[derive(Default)]
struct PendingDestroyedJournalInner {
    pending: Vec<(BallId, Option<CharacterId>)>,
    in_flight: HashMap<u64, Vec<(BallId, Option<CharacterId>)>>,
    next_delivery_id: u64,
    #[cfg(test)]
    recorded_total: u64,
    #[cfg(test)]
    acked_total: u64,
}

/// Durable actor-side outbox for destroyed balls awaiting downstream commit acknowledgement.
#[derive(Clone)]
pub(crate) struct PendingDestroyedOutbox {
    journal: Arc<RankedMutex<PendingDestroyedJournalInner, RANK_LEAF>>,
    non_empty: Arc<AtomicBool>,
}

impl Default for PendingDestroyedOutbox {
    fn default() -> Self {
        Self {
            journal: Arc::new(RankedMutex::new(PendingDestroyedJournalInner::default())),
            non_empty: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl std::fmt::Debug for PendingDestroyedOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingDestroyedOutbox")
            .field("non_empty", &self.is_non_empty())
            .finish()
    }
}

impl PendingDestroyedOutbox {
    fn refresh_non_empty(inner: &PendingDestroyedJournalInner, non_empty: &AtomicBool) {
        non_empty.store(
            !inner.pending.is_empty() || !inner.in_flight.is_empty(),
            Ordering::Release,
        );
    }

    fn push_unique_into(
        pending: &mut Vec<(BallId, Option<CharacterId>)>,
        in_flight: &HashMap<u64, Vec<(BallId, Option<CharacterId>)>>,
        entry: (BallId, Option<CharacterId>),
    ) -> bool {
        let already_tracked = pending.iter().any(|&(bid, _)| bid == entry.0)
            || in_flight
                .values()
                .flatten()
                .any(|&(bid, _)| bid == entry.0);
        if !already_tracked {
            pending.push(entry);
            true
        } else {
            false
        }
    }

    fn record_entries(
        &self,
        entries: impl IntoIterator<Item = (BallId, Option<CharacterId>)>,
    ) {
        let mut guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        for entry in entries {
            let inserted = {
                let PendingDestroyedJournalInner {
                    pending,
                    in_flight,
                    ..
                } = &mut *guard;
                Self::push_unique_into(pending, in_flight, entry)
            };
            #[cfg(test)]
            if inserted {
                guard.recorded_total = guard.recorded_total.saturating_add(1);
            }
            #[cfg(not(test))]
            let _ = inserted;
        }
        Self::refresh_non_empty(&guard, &self.non_empty);
    }

    /// Returns true when any destroyed ball is pending checkout or awaiting delivery ack.
    /// Lock-free so hibernation/eviction checks can query it under arbitrary lock ranks.
    pub(crate) fn is_non_empty(&self) -> bool {
        self.non_empty.load(Ordering::Acquire)
    }

    pub(crate) fn record_one(&self, ball_id: BallId, attacker_char_id: Option<CharacterId>) {
        self.record_entries(std::iter::once((ball_id, attacker_char_id)));
    }

    pub(crate) fn record_batch(&self, entries: &[(BallId, Option<CharacterId>)]) {
        if entries.is_empty() {
            return;
        }
        self.record_entries(entries.iter().copied());
    }

    fn issue_lease_locked(
        &self,
        guard: &mut PendingDestroyedJournalInner,
        batch: Vec<(BallId, Option<CharacterId>)>,
    ) -> Option<Arc<PendingDestroyedDeliveryGuard>> {
        if batch.is_empty() {
            Self::refresh_non_empty(guard, &self.non_empty);
            return None;
        }
        guard.next_delivery_id = guard.next_delivery_id.wrapping_add(1);
        let delivery_id = guard.next_delivery_id;
        guard.in_flight.insert(delivery_id, batch);
        Self::refresh_non_empty(guard, &self.non_empty);
        Some(Arc::new(PendingDestroyedDeliveryGuard {
            outbox: self.clone(),
            delivery_id,
            acked: AtomicBool::new(false),
        }))
    }

    /// Drains currently pending destroyed balls into an in-flight lease that rolls back on Drop
    /// unless [`PendingDestroyedDeliveryGuard::ack`] is called after downstream persistence succeeds.
    pub(crate) fn checkout_for_step(
        &self,
    ) -> (
        Vec<(BallId, Option<CharacterId>)>,
        Option<Arc<PendingDestroyedDeliveryGuard>>,
    ) {
        let mut guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let batch = std::mem::take(&mut guard.pending);
        let lease = self.issue_lease_locked(&mut guard, batch.clone());
        (batch, lease)
    }

    /// Checks out a specific sub-batch (such as immediate volley deaths) from `pending` into an
    /// independent in-flight lease without disturbing unrelated pending entries.
    pub(crate) fn checkout_entries(
        &self,
        entries: &[(BallId, Option<CharacterId>)],
    ) -> (
        Vec<(BallId, Option<CharacterId>)>,
        Option<Arc<PendingDestroyedDeliveryGuard>>,
    ) {
        if entries.is_empty() {
            return (Vec::new(), None);
        }
        let mut guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let mut checked_out = Vec::with_capacity(entries.len());
        for &(ball_id, _) in entries {
            if let Some(pos) = guard.pending.iter().position(|&(bid, _)| bid == ball_id) {
                checked_out.push(guard.pending.remove(pos));
            }
        }
        let lease = self.issue_lease_locked(&mut guard, checked_out.clone());
        (checked_out, lease)
    }

    #[cfg(test)]
    pub(crate) fn pending_and_in_flight_counts(&self) -> (usize, usize) {
        let guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let in_flight_items: usize = guard.in_flight.values().map(Vec::len).sum();
        (guard.pending.len(), in_flight_items)
    }

    #[cfg(test)]
    pub(crate) fn totals(&self) -> (u64, u64) {
        let guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        (guard.recorded_total, guard.acked_total)
    }
}

/// RAII lease tying an emitted [`StepOutput`] batch to downstream commit completion.
/// Dropping without [`Self::ack`] returns the batch to the front of [`PendingDestroyedOutbox`].
pub(crate) struct PendingDestroyedDeliveryGuard {
    outbox: PendingDestroyedOutbox,
    delivery_id: u64,
    acked: AtomicBool,
}

impl std::fmt::Debug for PendingDestroyedDeliveryGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingDestroyedDeliveryGuard")
            .field("delivery_id", &self.delivery_id)
            .field("acked", &self.acked.load(Ordering::Acquire))
            .finish()
    }
}

impl PendingDestroyedDeliveryGuard {
    pub(crate) fn ack(&self) {
        if !self.acked.swap(true, Ordering::AcqRel) {
            let mut guard = self.outbox.journal.lock().unwrap_or_else(|e| e.into_inner());
            let removed = guard.in_flight.remove(&self.delivery_id);
            #[cfg(test)]
            if let Some(batch) = removed {
                guard.acked_total = guard.acked_total.saturating_add(batch.len() as u64);
            }
            #[cfg(not(test))]
            let _ = removed;
            PendingDestroyedOutbox::refresh_non_empty(&guard, &self.outbox.non_empty);
        }
    }
}

impl Drop for PendingDestroyedDeliveryGuard {
    fn drop(&mut self) {
        if self.acked.load(Ordering::Acquire) {
            return;
        }
        let mut guard = self.outbox.journal.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(batch) = guard.in_flight.remove(&self.delivery_id) {
            let existing = std::mem::take(&mut guard.pending);
            let mut restored = Vec::with_capacity(batch.len().saturating_add(existing.len()));
            for entry in batch.into_iter().chain(existing) {
                PendingDestroyedOutbox::push_unique_into(&mut restored, &guard.in_flight, entry);
            }
            guard.pending = restored;
        }
        PendingDestroyedOutbox::refresh_non_empty(&guard, &self.outbox.non_empty);
    }
}

/// Structured actor outbox produced by a single [`SystemActor`] step (`RFC-1 E-4`, `#919`).
#[derive(Debug, Clone, Default)]
pub struct StepOutput {
    pub solar_system_id: SolarSystemId,
    pub tick: u64,
    pub dt_sim: f64,
    pub landed_warps: Vec<(BallId, [f64; 3])>,
    pub destroyed_balls: Vec<(BallId, Option<CharacterId>)>,
    pub(crate) destroyed_lease: Option<Arc<PendingDestroyedDeliveryGuard>>,
    pub lock_completed_events: Vec<(CharacterId, BallId)>,
    pub target_lost_events: Vec<(CharacterId, BallId)>,
    pub cycle_actions: Vec<PendingCycleAction>,
    pub condition_checkpoint: Option<Vec<ConditionCheckpointEntry>>,
    pub broadcast: Option<TickBroadcast>,
}

/// Structured actor outbox produced by an immediate [`SystemCommand::FireWeaponCycleVolley`].
#[derive(Debug, Clone, Default)]
pub struct WeaponVolleyOutput {
    pub destroyed: Vec<(BallId, Option<CharacterId>)>,
    pub(crate) destroyed_lease: Option<Arc<PendingDestroyedDeliveryGuard>>,
    pub actions: Vec<PendingCycleAction>,
}

/// Per-session `beyonce.UpdateStateRequest` cooldown state stored per `ServerContext` on `SystemRegistry` (`#1169`).
#[derive(Debug, Clone, Copy)]
pub struct ExplicitUpdateStateGuard {
    pub sys_id: u32,
    pub ship_id: i64,
    pub last_push_tick: u64,
    pub last_push_at: std::time::Instant,
}

/// Sim-clock state for an active module reload (`#1081`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleReloadSimEntry {
    pub remaining_sim_s: f64,
    pub min_wall_until_unix: u64,
    pub tidi_stretched: bool,
}

/// Canonical reason why ship cloak and/or invulnerability protection is evaluated or broken (`#1486`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectionBreakReason {
    /// Activating a non-exempt module breaks gate cloak and undock/warp-exit/location invuln,
    /// while leaving an active non-gate cloak module intact.
    ModuleActivation,
    /// Locking a target breaks gate cloak and undock/warp-exit/location invuln while preserving non-gate cloak.
    TargetLock,
    /// Initiating warp breaks gate cloak, non-covert active cloak, and invuln while preserving covert cloak.
    WarpInitiation,
    /// Issuing subwarp movement breaks gate cloak and invuln while preserving active module cloaks.
    SubwarpMovement,
    /// Generic non-cloak space action breaks gate cloak and invuln while preserving active non-gate cloak.
    SessionAction,
    /// ESS acceleration gate entry forcibly strips all cloak types (gate, non-covert, covert) and cloak cycles.
    EssGateEntry,
    /// Explicit manual protection clear breaks all cloaks, cloak cycles, and invulnerability timers.
    Manual,
}

impl ProtectionBreakReason {
    /// Returns `true` when this reason forcibly strips an active Covert Ops cloak module.
    pub const fn breaks_covert_cloak(self) -> bool {
        matches!(self, Self::EssGateEntry | Self::Manual)
    }

    /// Returns `true` when this reason breaks an active non-covert cloak module (`!gate && !covert`).
    pub const fn breaks_non_covert_module_cloak(self) -> bool {
        matches!(self, Self::WarpInitiation | Self::EssGateEntry | Self::Manual)
    }

    /// Returns `true` when this reason clears post-jump/undock/warp-exit/location invulnerability timers.
    pub const fn clears_invuln(self) -> bool {
        !matches!(self, Self::EssGateEntry)
    }
}

/// Summary of an active cloak `ActiveModuleCycle` stripped atomically by forced decloak (`#1486`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedCloakCycleInfo {
    pub module_id: ItemId,
    pub module_type_id: u32,
    pub is_covert: bool,
    pub effect_name: String,
}

/// Structured transition events returned atomically by actor-owned `break_protection` / `force_decloak` (`#1486`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProtectionBreakEvents {
    /// `true` if a post-jump/post-session gate cloak broke (`OnJumpCloakCancelled`).
    pub gate_cloak_broken: bool,
    /// `true` if an active non-gate module cloak (`!gate`) broke.
    pub non_gate_cloak_broken: bool,
    /// `true` if an active Covert Ops cloak (`!gate && covert`) broke.
    pub covert_cloak_broken: bool,
    /// `true` if ship-side `undock_invulnerable_remaining_ticks` cancelled.
    pub undock_invuln_cancelled: bool,
    /// `true` if either `undock_invulnerable_remaining_ticks` or `location_change_invuln` cancelled
    /// (`OnInvulnCancelled` must emit at most once when `true`).
    pub invuln_cancelled: bool,
    /// `true` if `warp_exit_invulnerable_remaining_ticks` cleared on this transition.
    pub warp_exit_invuln_cancelled: bool,
    /// `true` if `is_tethered` cleared on this transition.
    pub tether_broken: bool,
    /// `true` if `slim_meta.is_cloaked` cleared or `deactivate_cloak` ran.
    pub slim_cloak_cleared: bool,
    /// Cloak `ActiveModuleCycle`s removed atomically when decloaking (`#1486`).
    pub removed_cloak_cycles: Vec<RemovedCloakCycleInfo>,
    /// `true` if the matching `location_change_invuln` grant was already consumed by a concurrent
    /// actor command so service-level post-await cleanup must not re-emit `OnInvulnCancelled` (`#1486`).
    pub location_invuln_already_consumed: bool,
    /// Post-decloak sensor recalibration targeting delay active after break (seconds).
    pub decloak_targeting_delay_s: f64,
    /// Monotonic simulation clock elapsed seconds when this break ran (`sim_elapsed_secs`).
    pub sim_elapsed_secs: f64,
}

impl ProtectionBreakEvents {
    /// Returns `true` if any cloak (gate or module) transitioned out of cloaked state.
    pub fn any_cloak_broken(&self) -> bool {
        self.gate_cloak_broken
            || self.non_gate_cloak_broken
            || self.slim_cloak_cleared
            || !self.removed_cloak_cycles.is_empty()
    }

    /// Returns `true` when client `OnJumpCloakCancelled` should fire for a broken gate cloak.
    pub fn should_emit_jump_cloak_cancelled(&self) -> bool {
        self.gate_cloak_broken
    }
}

/// Actor-owned unified protection record tracked per ship ball on the actor sim clock (`#1486`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ActorShipProtection {
    pub owner_char_id: Option<CharacterId>,
    pub snapshot: CombatProtectionSnapshot,
    pub has_active_cloak_cycle: bool,
    pub location_change_invuln_until_unix: Option<u64>,
    pub updated_sim_elapsed_secs: f64,
    pub updated_sim_unix_secs: u64,
}

impl ActorShipProtection {
    /// Returns `true` when location-change invulnerability is active at `now_unix_secs`.
    pub fn has_location_change_invuln(&self, now_unix_secs: u64) -> bool {
        self.location_change_invuln_until_unix
            .is_some_and(|until| until > now_unix_secs)
    }

    /// Returns `true` when any cloak state (gate or module) is engaged.
    pub fn has_any_cloak(&self) -> bool {
        self.snapshot.has_any_cloak() || self.has_active_cloak_cycle
    }

    /// Returns `true` when an active module cloak (excluding gate cloak) is engaged.
    pub fn has_active_non_gate_cloak(&self) -> bool {
        self.snapshot.has_active_non_gate_cloak_or_vel() || self.has_active_cloak_cycle
    }

    /// Returns `true` when any protection (tether, gate cloak, undock, warp exit, or location change) is active.
    pub fn is_protected(&self, now_unix_secs: u64) -> bool {
        self.snapshot.is_invulnerable() || self.has_location_change_invuln(now_unix_secs)
    }
}

/// Atomic module-activation request executed inside the system actor loop (`#1486`).
#[derive(Debug, Clone)]
pub struct AtomicActivateCycleRequest {
    pub char_id: CharacterId,
    pub cycle: ActiveModuleCycle,
    pub allow_while_warping: bool,
    pub allow_while_cloaked: bool,
    pub break_protection: bool,
    pub break_reason: ProtectionBreakReason,
    /// Optional caller-observed `location_change_invuln` token folded atomically.
    pub location_change_invuln: Option<LocationInvulnToken>,
    pub real_now_unix: u64,
}

/// Lock rank for [`LocationInvulnLedger`], strictly between `RANK_WAL_LOCK (43)` and `RANK_LEAF (60)`.
pub const RANK_INVULN_LEDGER: u16 = 55;

/// Monotonically-sequenced `location_change_invuln` token (`#1486`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationInvulnToken {
    pub seq: u64,
    pub until_unix: u64,
}

/// Authoritative sequence ledger pairing `ctx.location_change_invuln` with monotonic tokens (`#1486`).
#[derive(Debug, Clone, Default)]
pub struct LocationInvulnLedger {
    pub next_seq: u64,
    pub by_char: HashMap<CharacterId, LocationInvulnToken>,
    pub notified_seq_by_char: HashMap<CharacterId, u64>,
}

/// Actor mirror sync operation for `location_change_invuln` (`#1486`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationChangeInvulnSyncOp {
    Grant(LocationInvulnToken),
    ClearUpTo { seq: u64 },
}

/// Structured outcome returned by [`SystemCommand::ActivateModuleCycleAtomic`] (`#1486`).
#[derive(Debug, Clone, Default)]
pub struct AtomicActivateCycleOutcome {
    pub protection_events: ProtectionBreakEvents,
    pub capacitor_after: f64,
}

#[derive(Default)]
pub(crate) struct SystemActorState {
    pub active_module_cycles: Vec<ActiveModuleCycle>,
    pub protection_registry: HashMap<BallId, ActorShipProtection>,
    pub location_invuln_by_char: HashMap<CharacterId, LocationInvulnToken>,
    pub location_invuln_consumed_seq_by_char: HashMap<CharacterId, u64>,
    pub pending_anchoring_disruptors: Vec<PendingAnchoringDisruptor>,
    pub active_nanite_repairs: Vec<ActiveNaniteRepair>,
    pub ship_rack_heat: HashMap<BallId, [f64; 3]>,
    pub rah_incoming_damage: HashMap<BallId, [f64; 4]>,
    pub base_scramble_strengths: HashMap<BallId, i32>,
    pub base_core_strengths: HashMap<BallId, i32>,
    pub asteroids: HashMap<BallId, AsteroidState>,
    pub asteroid_templates: HashMap<BallId, (AsteroidState, Ball)>,
    pub sde_opt: Option<Arc<SdeRegistry>>,
    pub db_opt: Option<Arc<eve_db::WorldRepository>>,
    pub mechanic_rng_opt: Option<Arc<crate::rng::MechanicRng>>,
    pub sim_elapsed_secs: f64,
    pub pending_cycle_actions: Vec<PendingCycleAction>,
    pub tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    pub module_reload_sim_deadlines: Arc<RankedRwLock<HashMap<i64, (SolarSystemId, f64, f64)>, RANK_LEAF>>,
    pub pending_notifications_opt: Option<Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>>,
    pub notify_tx_opt: Option<tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    pub ball_space_entry_epochs: HashMap<BallId, u64>,
    pub fit_projections: HashMap<BallId, eve_dogma::FitProjection>,
    pub weapon_cycle_start_modules: std::collections::HashSet<(BallId, i64)>,
    /// Active sim-clock-governed turret/launcher reload countdowns (`#1081`).
    pub module_reload_sim_remaining: HashMap<i64, ModuleReloadSimEntry>,
    pub last_checkpointed_balls: HashMap<BallId, LastCheckpointedBallState>,
    pub hibernated: Arc<std::sync::atomic::AtomicBool>,
    pub hibernated_since_unix: Arc<std::sync::atomic::AtomicU64>,
    pub idle_ticks: Arc<std::sync::atomic::AtomicU64>,
    pub dynamic_ball_count: Arc<std::sync::atomic::AtomicUsize>,
    pub pending_missile_launches: Vec<PendingMissileLaunch>,
    pub pending_missile_deploy_fx: Vec<PendingMissileDeployFx>,
    pub active_guided_missiles: Vec<ActiveGuidedMissile>,
    pub next_missile_ball_id: i64,
    pub(crate) pending_destroyed_balls: PendingDestroyedOutbox,
    pub(crate) warp_provenance: HashMap<BallId, WarpProvenance>,
    pub(crate) warp_tracking: SharedWarpTracking,
    /// Serializes monotonic `TickBroadcast.broadcast_seq` assignment with `event_tx.send` (`GH #1172`).
    pub(crate) broadcast_seq_gate: Arc<std::sync::Mutex<u64>>,
}

/// Pre-cleanup runtime `CombatState` flags preserved across DB-preserving `TakeBall`
/// so `RestoreBall` restores exact origin-actor state (`#1237`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TakenBallPreCleanupCombatFlags {
    pub rah_armor_resonance: Option<[f64; 4]>,
    pub is_covert_cloak: bool,
    pub cloak_stabilization_remaining_s: f64,
    pub decloak_targeting_delay_remaining_s: f64,
    pub cloak_velocity_multiplier: Option<f64>,
}

/// Atomic snapshot of a ball removed from a solar system (`#842`, `#917` E-2).
#[derive(Debug, Clone)]
pub struct TakenBall {
    pub tick: u64,
    pub ball: Ball,
    pub combat_state: Option<CombatState>,
    pub slim_meta: Option<SlimItemMeta>,
    pub cycles_cleared: usize,
    pub removed_own_cycles: Vec<ActiveModuleCycle>,
    pub pre_cleanup_combat_flags: Option<TakenBallPreCleanupCombatFlags>,
}

/// Result of non-blocking [`SystemHandle::try_repair_combat_state_sync`] (`#1167`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRepairCombatOutcome {
    /// Target ball has no live combat state in this system (e.g., docked ship).
    NotPresent,
    /// Repaired synchronously on `Ballpark`, emitted `OnDamageStateChange`, and persisted to DB.
    Applied,
    /// `Ballpark` was write-contended (`WouldBlock`); repair command queued via `try_send`.
    QueuedAsync,
    /// `Ballpark` was write-contended and mailbox/locks prevented `try_send`; caller should `.await`
    /// [`SystemHandle::repair_combat_state`] outside DB locks.
    NeedsAsyncFallback,
}

pub enum SystemCommand {
    SpawnBall {
        ball: Ball,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SpawnBallWithCombat {
        ball: Ball,
        combat_state: CombatState,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SpawnBallWithMeta {
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SpawnBallWithMetaTagged {
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        spawn_token: u64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    ChangeSlimItem {
        ball_id: BallId,
        slim_meta: SlimItemMeta,
        reply: oneshot::Sender<bool>,
    },
    SetBallWormholeStages {
        ball_id: BallId,
        wormhole_age: i64,
        wormhole_size: f64,
        reply: oneshot::Sender<bool>,
    },
    SetBallMaxVelocity {
        ball_id: BallId,
        max_velocity: f64,
        reply: oneshot::Sender<bool>,
    },
    SetBallMaxVelocityCap {
        ball_id: BallId,
        cap: Option<f64>,
        reply: oneshot::Sender<bool>,
    },
    GetBallSlimItem {
        ball_id: BallId,
        reply: oneshot::Sender<Option<eve_proto::PyRep>>,
    },
    SpawnAsteroid {
        asteroid: AsteroidState,
        ball: Ball,
        reply: oneshot::Sender<Result<BallId, String>>,
    },
    MineAsteroid {
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
        persist_db: bool,
        reply: oneshot::Sender<Result<MiningExtractionResult, String>>,
    },
    GetAsteroidState {
        asteroid_id: BallId,
        reply: oneshot::Sender<Option<AsteroidState>>,
    },
    BreakGateCloak {
        ball_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    PopulateAsteroids {
        reply: oneshot::Sender<Vec<AsteroidState>>,
    },
    RemoveBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<Ball>>,
    },
    /// Atomically removes `ball_id` only when its active spawn token matches `spawn_token`.
    RemoveBallIfToken {
        ball_id: BallId,
        spawn_token: u64,
        reply: oneshot::Sender<Option<Ball>>,
    },
    AcceptDocking {
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
        reply: oneshot::Sender<Option<Ball>>,
    },
    TakeBall {
        ball_id: BallId,
        accept_docking_target: Option<(BallId, DVec3)>,
        preserve_db_active_states: bool,
        reply: oneshot::Sender<Option<TakenBall>>,
    },
    RestoreBall {
        taken: TakenBall,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    /// Atomically restores `taken` only when `taken.ball.id` is absent from `ballpark.balls`.
    RestoreBallIfAbsent {
        taken: TakenBall,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    EmitPackagedActions {
        ball_id: BallId,
        events: Vec<DestinyEvent>,
        reply: oneshot::Sender<bool>,
    },
    SetMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetWarpMovementMode {
        ball_id: BallId,
        mode: MovementMode,
        provenance: WarpProvenance,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetWarpScrambleStrength {
        ball_id: BallId,
        strength: i32,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetWarpCoreStrength {
        ball_id: BallId,
        strength: i32,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    AddWarpBubble {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    RemoveWarpBubble {
        id: BallId,
        reply: oneshot::Sender<bool>,
    },
    SetSpeedFraction {
        ball_id: BallId,
        fraction: f64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    SetBallPosition {
        ball_id: BallId,
        position: glam::DVec3,
        reply: oneshot::Sender<bool>,
    },
    EmitDestinyEvent {
        ball_id: BallId,
        event: DestinyEvent,
        reply: oneshot::Sender<bool>,
    },
    EmitObserverDestinyEvent {
        observer_ball_id: BallId,
        event: DestinyEvent,
        reply: oneshot::Sender<bool>,
    },
    GotoPoint {
        ball_id: BallId,
        point: glam::DVec3,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    PushSetState {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    TrackModuleReloadSim {
        module_id: i64,
        ship_id: BallId,
        remaining_sim_s: f64,
    },
    GetBallSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<(u64, Ball)>>,
    },
    GetBallGroupId {
        ball_id: BallId,
        reply: oneshot::Sender<Option<u32>>,
    },
    ListBalls {
        reply: oneshot::Sender<Vec<Ball>>,
    },
    ListBallsInBubble {
        observer_ball_id: BallId,
        dynamic_only: bool,
        reply: oneshot::Sender<Vec<Ball>>,
    },
    GetSetState {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<(u64, PyRep, PyRep), String>>,
    },
    /// Like [`GetSetState`], and also returns the actor's `TickBroadcast.broadcast_seq`
    /// watermark at the instant the `SetState` snapshot was encoded (`#1461`).
    GetSetStateWithSeq {
        observer_ball_id: BallId,
        reply: oneshot::Sender<Result<(u64, PyRep, PyRep, u64), String>>,
    },
    CurrentTick {
        reply: oneshot::Sender<u64>,
    },
    GetBubbleIdForBall {
        ball_id: BallId,
        reply: oneshot::Sender<Option<u64>>,
    },
    DrainObserverUpdates {
        observer_ball_id: BallId,
        reply: oneshot::Sender<(u64, Vec<DestinyEvent>, PyRep)>,
    },
    DrainObserverUpdatesWithSeq {
        observer_ball_id: BallId,
        reply: oneshot::Sender<(u64, Vec<DestinyEvent>, PyRep, u64)>,
    },
    ApplyDamage {
        ball_id: BallId,
        damage: DamageVector,
        reply: oneshot::Sender<Option<(f64, bool, Ball)>>,
    },
    ApplyTrueDamage {
        ball_id: BallId,
        amount: f64,
        reply: oneshot::Sender<Option<(f64, bool, Ball)>>,
    },
    StepManual {
        dt: f64,
        reply: oneshot::Sender<StepOutput>,
    },
    StepManualTickOnly {
        dt: f64,
        reply: oneshot::Sender<StepOutput>,
    },
    FlushConditionCheckpoint {
        reply: oneshot::Sender<Vec<ConditionCheckpointEntry>>,
    },
    FastForwardClock {
        ship_id: BallId,
        secs: u64,
        reply: oneshot::Sender<()>,
    },
    GetTargetLocks {
        ball_id: BallId,
        reply: oneshot::Sender<Vec<TargetLock>>,
    },
    AddTargetLock {
        source_id: BallId,
        lock: TargetLock,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveTargetLock {
        source_id: BallId,
        target_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    RemoveTargetLockQuiet {
        source_id: BallId,
        target_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    ClearTargetLocks {
        source_id: BallId,
        reply: oneshot::Sender<usize>,
    },
    ClearTargetLocksQuiet {
        source_id: BallId,
        reply: oneshot::Sender<usize>,
    },
    SetCapacitor {
        ball_id: BallId,
        capacitor: f64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    HealCombatState {
        ball_id: BallId,
        reply: oneshot::Sender<Result<CombatState, String>>,
    },
    SetCombatState {
        ball_id: BallId,
        combat_state: CombatState,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetCombatState {
        ball_id: BallId,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    ModifyCombatState {
        ball_id: BallId,
        f: Box<dyn FnOnce(&mut CombatState) + Send>,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    RepairCombatState {
        ball_id: BallId,
        hull_heal: f64,
        armor_heal: f64,
        shield_heal: f64,
        reply: oneshot::Sender<Option<CombatState>>,
    },
    AddModuleCycle {
        cycle: ActiveModuleCycle,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveModuleCycle {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<Result<Option<f64>, String>>,
    },
    RequestCycleDeactivation {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<bool>,
    },
    FireWeaponCycleVolley {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<WeaponVolleyOutput>,
    },
    HasModuleCycle {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<bool>,
    },
    ReconcileModuleCycleBeforeActivate {
        ship_id: BallId,
        module_id: i64,
        now_unix: u64,
        reply: oneshot::Sender<(Option<(i32, Option<BallId>)>, bool)>,
    },
    SetInterdictionNullified {
        ball_id: BallId,
        nullified: bool,
        reply: oneshot::Sender<bool>,
    },
    MicroJump {
        ball_id: BallId,
        distance_meters: f64,
        reply: oneshot::Sender<Result<DVec3, String>>,
    },
    MicroJumpField {
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
        reply: oneshot::Sender<Result<Vec<BallId>, String>>,
    },
    IsBallScrambled {
        ball_id: BallId,
        reply: oneshot::Sender<bool>,
    },
    AddInterdictionSphere {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<bool>,
    },
    AddMobileWarpDisruptor {
        id: BallId,
        position: DVec3,
        radius: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmJam {
        target_id: BallId,
        duration_s: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmJamFrom {
        source_id: BallId,
        target_id: BallId,
        duration_s: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmJamFromModule {
        source_id: BallId,
        module_id: i64,
        target_id: BallId,
        duration_s: f64,
        reply: oneshot::Sender<bool>,
    },
    ApplyEcmBurst {
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
        ship_eval_overrides: Option<HashMap<BallId, (usize, f64, f64)>>,
        reply: oneshot::Sender<Vec<BallId>>,
    },
    ScheduleMobileWarpDisruptor {
        id: BallId,
        type_id: u32,
        position: DVec3,
        radius: f64,
        anchoring_delay_s: f64,
        reply: oneshot::Sender<bool>,
    },
    UpdateModuleOverload {
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
        reply: oneshot::Sender<bool>,
    },
    StartNaniteRepair {
        repair: ActiveNaniteRepair,
        reply: oneshot::Sender<()>,
    },
    StopNaniteRepair {
        ship_id: BallId,
        module_id: i64,
        reply: oneshot::Sender<Option<(f64, i32)>>,
    },
    RecordIncomingArmorDamage {
        ship_id: BallId,
        damage: [f64; 4],
        reply: oneshot::Sender<()>,
    },
    StopNonWarpModuleCycles {
        ship_id: BallId,
        reply: oneshot::Sender<()>,
    },
    FlushPeerObserverUpdates {
        exclude_ball_id: Option<BallId>,
        reply: oneshot::Sender<usize>,
    },
    ApplyFitProjection {
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
        reply: Option<oneshot::Sender<bool>>,
    },
    BreakProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        location_invuln: Option<LocationInvulnToken>,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<ProtectionBreakEvents, String>>,
    },
    ForceDecloak {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        reason: ProtectionBreakReason,
        location_invuln: Option<LocationInvulnToken>,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<ProtectionBreakEvents, String>>,
    },
    GetProtectionSnapshot {
        ball_id: BallId,
        reply: oneshot::Sender<Option<ActorShipProtection>>,
    },
    SyncLocationChangeInvuln {
        char_id: CharacterId,
        op: LocationChangeInvulnSyncOp,
    },
    ActivateModuleCycleAtomic {
        request: AtomicActivateCycleRequest,
        reply: oneshot::Sender<Result<AtomicActivateCycleOutcome, String>>,
    },
    SetMovementModeBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        reason: ProtectionBreakReason,
        location_invuln: Option<LocationInvulnToken>,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    SetWarpMovementModeBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        provenance: WarpProvenance,
        reason: ProtectionBreakReason,
        location_invuln: Option<LocationInvulnToken>,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
    GotoPointBreakingProtection {
        ball_id: BallId,
        char_id: Option<CharacterId>,
        point: glam::DVec3,
        reason: ProtectionBreakReason,
        location_invuln: Option<LocationInvulnToken>,
        real_now_unix: u64,
        reply: oneshot::Sender<Result<(u64, ProtectionBreakEvents), String>>,
    },
}

pub type CharacterId = i64;
pub type ItemId = i64;
pub type TypeId = u32;
pub type NpcDeathFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type NpcDeathHook = Arc<dyn Fn(SolarSystemId, BallId, Option<CharacterId>) -> NpcDeathFuture + Send + Sync>;
pub type ModuleDeactivationHook = Arc<dyn Fn(CharacterId, &ActiveModuleCycle) + Send + Sync>;
pub type TargetLockCompletedHook = Arc<dyn Fn(CharacterId, BallId) + Send + Sync>;
pub type SystemStepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub type SystemStepHook =
    Arc<dyn Fn(SolarSystemId, f64, Vec<(BallId, Option<CharacterId>)>) -> SystemStepFuture + Send + Sync>;
pub(crate) type PostUnlockHookAction = Box<dyn FnOnce() + Send>;

/// DERIVED: Default active-entity + module-cycle load units per worker node before load-induced Time Dilation engages.
pub const DEFAULT_NODE_LOAD_TIDI_THRESHOLD: usize = 80;

/// DERIVED: Runtime worker-node assignment, fleet-fight reinforcement, and simulation clock rebase tracking
/// (`feat:serverops.node-balancing`, `feat:serverops.time-dilation`, `notify:DoSimClockRebase`).
/// Automatic load-induced TiDi is disabled by default (`auto_tidi_enabled = false`) and must be explicitly
/// enabled by operator/GM command (`set_auto_cluster_tidi_enabled`).
#[derive(Debug, Clone)]
pub struct ClusterNodeRegistry {
    /// DERIVED: Default general-purpose worker node pool (`[1, 2, 3, 4]`).
    pub worker_nodes: Vec<u32>,
    pub system_node_overrides: HashMap<SolarSystemId, u32>,
    /// Dedicated reinforced nodes (`node_id -> exclusive solar_system_id`).
    pub reinforced_nodes: HashMap<u32, SolarSystemId>,
    /// Explicit GM/operator Time Dilation overrides (`solar_system_id -> factor`).
    pub manual_tidi_overrides: HashMap<SolarSystemId, f64>,
    /// Last emitted authoritative destiny `(anchor_wall_ft, anchor_sim_ft, tidi_factor)` per system.
    pub sim_clock_anchoring: HashMap<SolarSystemId, (i64, i64, f64)>,
    /// Preserved per-character continuous jump clock `(anchor_wall_ft, anchor_sim_ft, tidi_factor)` across TiDi jumps (`#1080`).
    pub character_jump_clocks: HashMap<i64, (i64, i64, f64)>,
    pub node_load_tidi_threshold: usize,
    pub next_dedicated_node_id: u32,
    pub auto_tidi_enabled: bool,
    /// Per-system latency-driven TiDi autoscaler master enable (`true` by default per `#1081`).
    pub latency_auto_tidi_enabled: bool,
    /// Per-system latency autoscaler state machines (`#1081`).
    pub system_tidi_autoscalers: HashMap<SolarSystemId, crate::system_actor::tidi_autoscaler::SystemTidiAutoscaler>,
    /// Monotonic rebase generation per system incremented on each emitted `DoSimClockRebase`.
    pub rebase_generation_by_system: HashMap<SolarSystemId, u64>,
    /// Last `(solar_system_id, rebase_generation)` delivered to a character (`DoSimClockRebase` once per entry/change).
    pub char_rebased_system_gen: HashMap<i64, (SolarSystemId, u64)>,
    /// Last monotonic sim FILETIME stamp emitted on space activation/FX per system (preserved across `0.5 -> 1.0`).
    pub last_sim_activation_ft_by_system: HashMap<SolarSystemId, i64>,
    pub scheduled_downtime_enabled: bool,
}

impl Default for ClusterNodeRegistry {
    fn default() -> Self {
        Self {
            worker_nodes: vec![1, 2, 3, 4],
            system_node_overrides: HashMap::new(),
            reinforced_nodes: HashMap::new(),
            manual_tidi_overrides: HashMap::new(),
            sim_clock_anchoring: HashMap::new(),
            character_jump_clocks: HashMap::new(),
            node_load_tidi_threshold: DEFAULT_NODE_LOAD_TIDI_THRESHOLD,
            next_dedicated_node_id: 100,
            auto_tidi_enabled: false,
            latency_auto_tidi_enabled: true,
            system_tidi_autoscalers: HashMap::new(),
            rebase_generation_by_system: HashMap::new(),
            char_rebased_system_gen: HashMap::new(),
            last_sim_activation_ft_by_system: HashMap::new(),
            scheduled_downtime_enabled: true,
        }
    }
}

/// Normalized shield, armor, and structure HP fractions (`0.0..=1.0`) for a ball.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BallDamageFractions {
    pub shield: f64,
    pub armor: f64,
    pub structure: f64,
}

impl BallDamageFractions {
    /// Computes normalized HP fractions `[0.0, 1.0]` from a [`CombatState`].
    pub fn from_combat_state(cs: &CombatState) -> Self {
        let ratio = |hp: f64, max: f64| -> f64 {
            if max > 0.0 && max.is_finite() {
                (hp / max).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        Self {
            shield: ratio(cs.shield_hp, cs.max_shield_hp),
            armor: ratio(cs.armor_hp, cs.max_armor_hp),
            structure: ratio(cs.structure_hp, cs.max_structure_hp),
        }
    }
}

/// Immutable, lock-free snapshot of a solar system's live `Ballpark` and active-module
/// state published by the system actor and synchronous mutators (`RFC-1 §7.3`, `#1342`).
#[derive(Debug, Clone)]
pub struct SystemView {
    pub solar_system_id: SolarSystemId,
    pub current_tick: u64,
    pub sim_unix_secs: u64,
    pub tidi_factor: f64,
    pub sim_elapsed_secs: f64,
    pub balls: BTreeMap<BallId, Ball>,
    pub static_ball_ids: HashSet<BallId>,
    pub ball_group_ids: HashMap<BallId, u32>,
    pub ball_slim_meta: HashMap<BallId, SlimItemMeta>,
    pub combat_states: HashMap<BallId, CombatState>,
    pub scrambled_balls: HashSet<BallId>,
    pub cloaked_balls: HashSet<BallId>,
    pub tethered_balls: HashSet<BallId>,
    pub invulnerable_balls: HashSet<BallId>,
    pub warp_disrupted_balls: HashSet<BallId>,
    pub warp_bubble_mgr: WarpBubbleMgr,
    pub ball_grid_ids: HashMap<BallId, u64>,
    pub grids: BTreeMap<u64, GridBubble>,
    pub target_locks: BTreeMap<BallId, Vec<TargetLock>>,
    pub active_module_ships: HashSet<BallId>,
    pub active_bastion_ships: HashSet<BallId>,
    pub active_triage_ships: HashSet<BallId>,
    pub active_siege_ships: HashSet<BallId>,
    pub active_industrial_core_ships: HashSet<BallId>,
    pub active_microjump_ships: HashSet<BallId>,
}

impl Default for SystemView {
    fn default() -> Self {
        Self {
            solar_system_id: 0,
            current_tick: 0,
            sim_unix_secs: 0,
            tidi_factor: 1.0,
            sim_elapsed_secs: 0.0,
            balls: BTreeMap::new(),
            static_ball_ids: HashSet::new(),
            ball_group_ids: HashMap::new(),
            ball_slim_meta: HashMap::new(),
            combat_states: HashMap::new(),
            scrambled_balls: HashSet::new(),
            cloaked_balls: HashSet::new(),
            tethered_balls: HashSet::new(),
            invulnerable_balls: HashSet::new(),
            warp_disrupted_balls: HashSet::new(),
            warp_bubble_mgr: WarpBubbleMgr::default(),
            ball_grid_ids: HashMap::new(),
            grids: BTreeMap::new(),
            target_locks: BTreeMap::new(),
            active_module_ships: HashSet::new(),
            active_bastion_ships: HashSet::new(),
            active_triage_ships: HashSet::new(),
            active_siege_ships: HashSet::new(),
            active_industrial_core_ships: HashSet::new(),
            active_microjump_ships: HashSet::new(),
        }
    }
}

impl SystemView {
    /// Captures a consistent [`SystemView`] from `ballpark` and optional `actor_state`.
    ///
    /// When `actor_state` is `None` (because the caller only holds `ballpark`), `previous`
    /// supplies the previously observed `sim_elapsed_secs` and active-module ship sets so
    /// synchronous ballpark-only mutations do not erase active module flags.
    pub(crate) fn from_ballpark(
        ballpark: &Ballpark,
        actor_state: Option<&SystemActorState>,
        tidi_factor: f64,
        previous: Option<&SystemView>,
    ) -> Self {
        let mut cloaked_balls = HashSet::new();
        let mut tethered_balls = HashSet::new();
        let mut invulnerable_balls = HashSet::new();
        let mut scrambled_balls = HashSet::new();
        let mut warp_disrupted_balls = HashSet::new();

        for (&ball_id, cs) in &ballpark.combat_states {
            if cs.has_any_cloak() {
                cloaked_balls.insert(ball_id);
            }
            if cs.is_tethered {
                tethered_balls.insert(ball_id);
            }
            if cs.is_protected() {
                invulnerable_balls.insert(ball_id);
            }
            if cs.is_warp_scrambled() {
                scrambled_balls.insert(ball_id);
            }
        }
        for (&ball_id, meta) in &ballpark.ball_slim_meta {
            if meta.is_cloaked {
                cloaked_balls.insert(ball_id);
            }
        }
        for (&ball_id, ball) in &ballpark.balls {
            if !ballpark.is_ball_bubble_immune(ball_id)
                && ballpark.warp_bubble_mgr.is_scrambled_at(ball.position)
            {
                warp_disrupted_balls.insert(ball_id);
                scrambled_balls.insert(ball_id);
            }
        }

        let (
            sim_elapsed_secs,
            active_module_ships,
            active_bastion_ships,
            active_triage_ships,
            active_siege_ships,
            active_industrial_core_ships,
            active_microjump_ships,
        ) = if let Some(st) = actor_state {
            let mut mods = HashSet::new();
            let mut bastion = HashSet::new();
            let mut triage = HashSet::new();
            let mut siege = HashSet::new();
            let mut ind_core = HashSet::new();
            let mut microjump = HashSet::new();
            for c in &st.active_module_cycles {
                mods.insert(c.ship_id);
                let eff_lower = c.effect_name.to_ascii_lowercase();
                if c.module_type_id == 33400 || eff_lower.contains("bastion") {
                    bastion.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 27951 | 4294) || eff_lower.contains("triage") {
                    triage.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 20280 | 4292) || eff_lower.contains("siege") {
                    siege.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
                    || eff_lower.contains("industrial")
                {
                    ind_core.insert(c.ship_id);
                }
                if c.mjd_jump_distance_m.is_some() || eff_lower.contains("microjump") {
                    microjump.insert(c.ship_id);
                }
            }
            (
                st.sim_elapsed_secs,
                mods,
                bastion,
                triage,
                siege,
                ind_core,
                microjump,
            )
        } else if let Some(prev) = previous {
            (
                prev.sim_elapsed_secs,
                prev.active_module_ships.clone(),
                prev.active_bastion_ships.clone(),
                prev.active_triage_ships.clone(),
                prev.active_siege_ships.clone(),
                prev.active_industrial_core_ships.clone(),
                prev.active_microjump_ships.clone(),
            )
        } else {
            (
                0.0,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            )
        };

        Self {
            solar_system_id: ballpark.solar_system_id,
            current_tick: ballpark.current_tick,
            sim_unix_secs: ballpark.sim_unix_secs,
            tidi_factor,
            sim_elapsed_secs,
            balls: ballpark.balls.clone(),
            static_ball_ids: ballpark.static_ball_ids.iter().copied().collect(),
            ball_group_ids: ballpark.ball_group_ids.iter().map(|(&k, &v)| (k, v)).collect(),
            ball_slim_meta: ballpark
                .ball_slim_meta
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
            combat_states: ballpark
                .combat_states
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
            scrambled_balls,
            cloaked_balls,
            tethered_balls,
            invulnerable_balls,
            warp_disrupted_balls,
            warp_bubble_mgr: ballpark.warp_bubble_mgr.clone(),
            ball_grid_ids: ballpark.ball_grid_ids.iter().map(|(&k, &v)| (k, v)).collect(),
            grids: ballpark.grids.clone(),
            target_locks: ballpark.target_locks.clone(),
            active_module_ships,
            active_bastion_ships,
            active_triage_ships,
            active_siege_ships,
            active_industrial_core_ships,
            active_microjump_ships,
        }
    }

    pub fn ball(&self, ball_id: BallId) -> Option<&Ball> {
        self.balls.get(&ball_id)
    }

    pub fn contains_ball(&self, ball_id: BallId) -> bool {
        self.balls.contains_key(&ball_id)
    }

    pub fn ball_position(&self, ball_id: BallId) -> Option<DVec3> {
        self.balls.get(&ball_id).map(|b| b.position)
    }

    pub fn ball_velocity(&self, ball_id: BallId) -> Option<DVec3> {
        self.balls.get(&ball_id).map(|b| b.velocity)
    }

    pub fn movement_mode(&self, ball_id: BallId) -> Option<&MovementMode> {
        self.balls.get(&ball_id).map(|b| &b.mode)
    }

    pub fn group_id(&self, ball_id: BallId) -> Option<u32> {
        self.ball_slim_meta
            .get(&ball_id)
            .map(|m| m.group_id)
            .or_else(|| self.ball_group_ids.get(&ball_id).copied())
    }

    pub fn owner_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.owner_id)
    }

    pub fn corp_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.corp_id)
    }

    pub fn alliance_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.alliance_id)
    }

    pub fn combat_state(&self, ball_id: BallId) -> Option<&CombatState> {
        self.combat_states.get(&ball_id)
    }

    pub fn slim_meta(&self, ball_id: BallId) -> Option<&SlimItemMeta> {
        self.ball_slim_meta.get(&ball_id)
    }

    pub fn damage_fractions(&self, ball_id: BallId) -> Option<BallDamageFractions> {
        self.combat_states
            .get(&ball_id)
            .map(BallDamageFractions::from_combat_state)
    }

    /// Mirrors `Ballpark::is_static_ball`.
    pub fn is_static_ball(&self, ball_id: BallId) -> bool {
        self.static_ball_ids.contains(&ball_id)
            || self
                .balls
                .get(&ball_id)
                .is_some_and(|b| matches!(b.mode, MovementMode::Rigid))
    }

    pub fn is_ball_scrambled(&self, ball_id: BallId) -> bool {
        self.scrambled_balls.contains(&ball_id)
    }

    pub fn is_ball_cloaked(&self, ball_id: BallId) -> bool {
        self.cloaked_balls.contains(&ball_id)
    }

    pub fn is_gate_cloaked(&self, ball_id: BallId) -> bool {
        self.combat_states
            .get(&ball_id)
            .is_some_and(CombatState::has_gate_cloak)
    }

    pub fn is_ball_tethered(&self, ball_id: BallId) -> bool {
        self.tethered_balls.contains(&ball_id)
    }

    pub fn is_ball_invulnerable(&self, ball_id: BallId) -> bool {
        self.invulnerable_balls.contains(&ball_id)
    }

    pub fn is_ball_warp_disrupted(&self, ball_id: BallId) -> bool {
        self.warp_disrupted_balls.contains(&ball_id)
    }

    pub fn has_active_module(&self, ball_id: BallId) -> bool {
        self.active_module_ships.contains(&ball_id)
    }

    pub fn has_active_bastion(&self, ball_id: BallId) -> bool {
        self.active_bastion_ships.contains(&ball_id)
    }

    pub fn has_active_triage(&self, ball_id: BallId) -> bool {
        self.active_triage_ships.contains(&ball_id)
    }

    pub fn has_active_siege(&self, ball_id: BallId) -> bool {
        self.active_siege_ships.contains(&ball_id)
    }

    pub fn has_active_industrial_core(&self, ball_id: BallId) -> bool {
        self.active_industrial_core_ships.contains(&ball_id)
    }

    pub fn has_active_microjump(&self, ball_id: BallId) -> bool {
        self.active_microjump_ships.contains(&ball_id)
    }

    /// Yields every ball ID currently holding a lock (locking or locked) on `target_id`.
    pub fn targeters_of(&self, target_id: BallId) -> impl Iterator<Item = BallId> + '_ {
        self.target_locks
            .iter()
            .filter_map(move |(&source_id, locks)| {
                if source_id != target_id && locks.iter().any(|l| l.target_id == target_id) {
                    Some(source_id)
                } else {
                    None
                }
            })
    }

    pub fn is_ball_targeted(&self, target_id: BallId) -> bool {
        self.targeters_of(target_id).next().is_some()
    }

    /// Mirrors `Ballpark::bubble_id_for_ball`.
    pub fn bubble_id_for_ball(&self, ball_id: BallId) -> Option<u64> {
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            return Some(gid);
        }
        let ball = self.balls.get(&ball_id)?;
        self.grids
            .values()
            .find(|g| g.contains(ball.position))
            .map(|g| g.id)
    }

    pub fn grid_bubble(&self, grid_id: u64) -> Option<&GridBubble> {
        self.grids.get(&grid_id)
    }

    /// Mirrors `Ballpark::visible_balls_for_observer`.
    pub fn visible_balls_for_observer(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let obs_gid = self.ball_grid_ids.get(&observer_ball_id).copied();
        self.balls
            .values()
            .filter(|b| {
                if self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                {
                    return true;
                }
                let Some(gid) = obs_gid else {
                    return false;
                };
                b.id == observer_ball_id
                    || (self.ball_grid_ids.get(&b.id) == Some(&gid)
                        && !self.combat_states.get(&b.id).is_some_and(|c| c.is_cloaked))
            })
            .collect()
    }

    /// Mirrors `Ballpark::dynamic_balls_in_bubble`.
    pub fn dynamic_balls_in_bubble(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let Some(obs_gid) = self.ball_grid_ids.get(&observer_ball_id).copied() else {
            return Vec::new();
        };
        let Some(grid) = self.grids.get(&obs_gid) else {
            return Vec::new();
        };
        grid.members
            .iter()
            .filter_map(|&id| {
                let b = self.balls.get(&id)?;
                if id != observer_ball_id
                    && self.combat_states.get(&id).is_some_and(|c| c.is_cloaked)
                {
                    return None;
                }
                Some(b)
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;


