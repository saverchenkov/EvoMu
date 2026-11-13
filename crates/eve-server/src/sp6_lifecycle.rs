//! `sp6_lifecycle` — Logoff, disconnect & login in space (`plan:SP-6`) and background cluster
//! scheduler, unobserved systems & ballpark reload (`plan:SP-7`).
//!
// parity: plan:SP-6, plan:SP-7
// parity: feat:crimewatch.pvp-logoff-timer-renewal
// parity: feat:lifecycle.world-unobserved-system-sim
// parity: feat:lifecycle.world-restart-space-objects
// parity: feat:lifecycle.wreck-restart
// parity: feat:lifecycle.structure-restart
// parity: feat:lifecycle.mobile_warp_disruptor-lifecycle
// parity: feat:lifecycle.drones-in-space-persist-restart
// parity: feat:lifecycle.self-destruct-server-timer
// parity: feat:lifecycle.abyssal-disconnect
// parity: feat:lifecycle.wreck-downtime
// parity: feat:lifecycle.mission-downtime-respawn

#![forbid(unsafe_code)]

mod cluster_tick;
mod cluster_tick_downtime;
mod disconnect_hold;
mod drone_reconnect_finish;
pub mod drone_return;

pub use cluster_tick::{
    catch_unwind_future, clamp_physics_ticker_backlog, on_advance_secs, on_advance_secs_internal,
    on_advance_secs_with_sim_clock_rebase, populate_persisted_space_objects,
    seed_persisted_abandoned_drones, step_active_systems_physics_tick, tick_background_cluster,
    tick_background_cluster_with_step, MAX_PHYSICS_BACKLOG_STEPS, SYSTEM_STEP_ASK_TIMEOUT,
};
pub(crate) use cluster_tick::sweep_expired_cosmic_anomalies_and_signatures;
pub use cluster_tick_downtime::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime, clear_safe_logoff,
    disconnect_safe_logoff_sessions, recover_scanner_probes_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
pub use drone_return::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, handle_cmd_return_bay, step_repeating_drone_mining,
    step_returning_drones,
};

use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
pub use eve_db::{
    AbyssalPersistedSession, DisconnectedShipEntry, InventoryFlag, PersistedCombatHp,
    PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry, Sp6PersistedState,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub const SAFE_LOGOFF_COUNTDOWN_SECS: u64 = 30;
pub const UNAGGRESSED_LOGOFF_REMOVE_SECS: u64 = 60;
pub const WEAPONS_TIMER_SECS: u64 = 60;
pub const NPC_TIMER_SECS: u64 = 300;
pub const PVP_TIMER_SECS: u64 = 900;
pub const CRIMINAL_TIMER_SECS: u64 = 900;
pub const EMERGENCY_WARP_DISTANCE_M: f64 = 1_000_000_000.0; // 1,000,000 km
pub const MOBILE_WARP_DISRUPTOR_DECAY_SECS: u64 = 2 * 86_400; // 48h (172,800s)
// DERIVED: EVE Online Upwell and legacy container decay rules
pub const SECURE_CONTAINER_DECAY_SECS: u64 = 30 * 86_400; // 30d (2,592,000s)
pub const WRECK_DECAY_LIFETIME_SECS: u64 = 7_200; // 2h
pub const JETCAN_DECAY_LIFETIME_SECS: u64 = 3_600; // 1h
pub const ABYSSAL_COLLAPSE_TIMEOUT_SECS: u64 = 1_200; // 20m
pub const SELF_DESTRUCT_COUNTDOWN_SECS: u64 = 120; // 2m

/// Converts Unix seconds into Windows FILETIME (`100ns` ticks since 1601-01-01 UTC).
pub fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64)
        .saturating_add(11_644_473_600)
        .saturating_mul(10_000_000)
}

pub fn load_sp6_state_from_snap(snap: &eve_db::WorldStateSnapshot) -> Sp6PersistedState {
    snap.sp6_lifecycle.clone()
}

pub fn save_sp6_state_to_snap(snap: &mut eve_db::WorldStateSnapshot, state: &Sp6PersistedState) {
    snap.sp6_lifecycle = state.clone();
}

pub fn with_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&Sp6PersistedState) -> R,
) -> R {
    db.with_snapshot(|snap| f(&snap.sp6_lifecycle))
}

struct Sp6RestoreGuard<'a> {
    snap: &'a mut eve_db::WorldStateSnapshot,
    state: Sp6PersistedState,
}

impl<'a> Sp6RestoreGuard<'a> {
    fn new(snap: &'a mut eve_db::WorldStateSnapshot) -> Self {
        let state = std::mem::take(&mut snap.sp6_lifecycle);
        Self { snap, state }
    }
}

impl<'a> Drop for Sp6RestoreGuard<'a> {
    fn drop(&mut self) {
        self.snap.sp6_lifecycle = std::mem::take(&mut self.state);
        self.snap.mark_other_domains_dirty();
    }
}

pub fn try_with_mut_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&mut Sp6PersistedState, &mut eve_db::WorldStateSnapshot) -> Result<R, String>,
) -> Result<R, String> {
    db.try_with_mut_snapshot(|snap| {
        let mut guard = Sp6RestoreGuard::new(snap);
        f(&mut guard.state, guard.snap)
    })
}

pub fn with_mut_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&mut Sp6PersistedState, &mut eve_db::WorldStateSnapshot) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let mut guard = Sp6RestoreGuard::new(snap);
        f(&mut guard.state, guard.snap)
    })
}

/// Records wreck, jetcan, or corpse creation metadata in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_wreck_or_container_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    created_unix: u64,
    is_npc_wreck: bool,
) {
    let meta = snap
        .sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default();
    if meta.created_unix.is_none() {
        meta.created_unix = Some(created_unix);
    }
    if is_npc_wreck {
        meta.is_npc_wreck = true;
    }
}

/// Records a launched drone's in-space ball ID in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_drone_ball_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    ball_id: i64,
) {
    snap.sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default()
        .drone_ball_id = Some(ball_id);
}

/// Records anchoring metadata for a deployable or Mobile Warp Disruptor in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_anchor_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    anchor_start_unix: u64,
    anchoring_delay_secs: f64,
    bubble_radius_m: Option<f64>,
) {
    let meta = snap
        .sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default();
    meta.anchor_start_unix = Some(anchor_start_unix);
    meta.anchoring_delay_secs = anchoring_delay_secs;
    if let Some(r) = bubble_radius_m {
        meta.bubble_radius_m = Some(r);
    }
}

/// Queries the SDE uncollected decay lifetime in seconds for a Mobile Warp Disruptor type (`group_id == 361`),
/// defaulting to 48 hours (`172,800s`) per EVE mechanics (`feat:lifecycle.mobile_warp_disruptor-lifecycle`).
pub fn sde_mobile_warp_disruptor_decay_secs(sde: &eve_sde::SdeRegistry, type_id: u32) -> u64 {
    if let Some(t) = sde.types.get(&type_id) {
        if let Some(&val) = t
            .default_attributes
            .get(&1237)
            .or_else(|| t.default_attributes.get(&1034))
        {
            if val > 0.0 {
                let secs = if val > 1_000_000.0 {
                    (val / 1_000.0).round() as u64
                } else {
                    val.round() as u64
                };
                if secs >= 3_600 {
                    return secs;
                }
            }
        }
    }
    MOBILE_WARP_DISRUPTOR_DECAY_SECS
}

/// Extracts SDE-derived hull physics `(group_id, category_id, mass, radius, max_velocity, inertia, warp_speed_au_s)` for `ship_type_id`.
pub fn sde_ship_physics(
    sde: &eve_sde::SdeRegistry,
    ship_type_id: u32,
) -> (u32, u32, f64, f64, f64, f64, f64) {
    sde.types
        .get(&ship_type_id)
        .map(|t| {
            let max_vel = t.default_attributes.get(&37).copied().unwrap_or(300.0).max(50.0);
            let inertia = t.default_attributes.get(&70).copied().unwrap_or(2.5).max(0.5);
            let base_warp = t.default_attributes.get(&1281).copied().unwrap_or(1.0).max(0.5);
            let warp_mult = t.default_attributes.get(&600).copied().unwrap_or(3.0).max(1.0);
            let warp_speed_au_s = (base_warp * warp_mult).clamp(1.5, 12.0);
            (
                t.group_id,
                t.category_id,
                t.mass.max(1_000.0),
                t.radius.max(20.0),
                max_vel,
                inertia,
                warp_speed_au_s,
            )
        })
        .unwrap_or((25, 6, 1_200_000.0, 40.0, 300.0, 2.5, 3.0))
}

/// Snapshot of a ship's authoritative condition and space-residency epoch (`#889`, `#917` T-1..T-3).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PersistedShipCondition {
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub capacitor: Option<f64>,
    pub in_space_system: Option<u32>,
    pub space_entry_epoch: u64,
}

/// Reads the persisted condition and space-residency metadata for `ship_id` from `snap`.
pub fn read_ship_condition_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
) -> Option<PersistedShipCondition> {
    if ship_id <= 0 {
        return None;
    }
    let hp = snap.sp6_lifecycle.ship_combat_hp.get(&ship_id);
    let meta = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id));
    if hp.is_none() && meta.is_none() {
        return None;
    }
    let (shield_hp, armor_hp, hull_hp) = hp
        .map(|h| (h.shield_hp, h.armor_hp, h.hull_hp))
        .unwrap_or((0.0, 0.0, 0.0));
    let capacitor = meta.and_then(|m| (m.shield_hp >= 1.0).then_some((m.shield_hp - 1.0).max(0.0)));
    let in_space_system = meta.and_then(|m| (m.armor_hp > 0.0).then_some(m.armor_hp.round() as u32));
    let space_entry_epoch = meta.map(|m| m.hull_hp.max(0.0).round() as u64).unwrap_or(0);
    Some(PersistedShipCondition {
        shield_hp,
        armor_hp,
        hull_hp,
        capacitor,
        in_space_system,
        space_entry_epoch,
    })
}

/// Returns the current `space_entry_epoch` recorded for `ship_id` in `snap` (`0` if never recorded).
#[inline]
pub fn current_ship_space_entry_epoch_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
) -> u64 {
    if ship_id <= 0 {
        return 0;
    }
    snap.sp6_lifecycle
        .ship_combat_hp
        .get(&(-ship_id))
        .map(|m| m.hull_hp.max(0.0).round() as u64)
        .unwrap_or(0)
}

/// Returns the current `space_entry_epoch` recorded for `ship_id` in `db` (`0` if never recorded).
#[inline]
pub fn current_ship_space_entry_epoch(db: &Arc<WorldRepository>, ship_id: i64) -> u64 {
    if ship_id <= 0 {
        return 0;
    }
    db.with_snapshot(|snap| current_ship_space_entry_epoch_in_snap(snap, ship_id))
}

/// Returns `true` if `ship_id` has left space (docked/stored) or moved to a different solar system
/// or space-entry epoch, meaning an in-space tick/damage flush from `expected_system` must not
/// overwrite its persisted condition (`#846`, `#917` T-1/T-2, `#919` E-4).
pub fn is_ship_stale_for_in_space_write_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    expected_system: Option<u32>,
    expected_epoch: Option<u64>,
) -> bool {
    if ship_id <= 0 {
        return false;
    }
    if let Some(meta) = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)) {
        if meta.armor_hp < 0.0 {
            return true;
        }
        if let Some(exp_sys) = expected_system {
            if meta.armor_hp > 0.0 && (meta.armor_hp.round() as u32) != exp_sys {
                return true;
            }
        }
        if let Some(exp_epoch) = expected_epoch {
            if (meta.hull_hp.max(0.0).round() as u64) != exp_epoch {
                return true;
            }
        }
    } else if let Some(exp_epoch) = expected_epoch {
        if exp_epoch != 0 {
            return true;
        }
    }
    if let Some(exp_sys) = expected_system {
        if let Some(it) = snap.items.get(&ship_id) {
            if it.location_id >= 60_000_000 {
                return true;
            }
            if (30_000_000..60_000_000).contains(&it.location_id)
                && it.location_id != i64::from(exp_sys)
            {
                return true;
            }
        }
        if snap.characters.values().any(|c| {
            c.active_ship_id == ship_id
                && (c.station_id.is_some()
                    || c.structure_id.is_some()
                    || c.solar_system_id != exp_sys)
        }) {
            return true;
        }
    }
    false
}

fn sync_ship_item_damage_attrs_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    ship_id: i64,
    armor_dmg: f64,
    hull_dmg: f64,
) {
    if let Some(it) = snap.items.get_mut(&ship_id) {
        let mut touched = false;
        if armor_dmg > 1e-6 {
            if it
                .dynamic_attributes
                .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                .copied()
                .is_none_or(|v| (v - armor_dmg).abs() > 1e-6)
            {
                it.dynamic_attributes
                    .insert(eve_sde::attr_ids::ARMOR_DAMAGE, armor_dmg);
                touched = true;
            }
        } else if it
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
            .is_some()
        {
            touched = true;
        }
        if hull_dmg > 1e-6 {
            if it
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied()
                .is_none_or(|v| (v - hull_dmg).abs() > 1e-6)
            {
                it.dynamic_attributes
                    .insert(eve_sde::attr_ids::DAMAGE, hull_dmg);
                touched = true;
            }
        } else if it
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::DAMAGE)
            .is_some()
        {
            touched = true;
        }
        if touched {
            it.version += 1;
        }
    }
}

/// Authoritative leave-space transition helper (`#842`, `#889`, `#917` T-1..T-3).
///
/// Records the taken ship's `CombatState` (shield, armor, hull, capacitor), updates `in_space_system`
/// (`None` when docking/storing/logging off, `Some(dest_sys)` when jumping to another solar system),
/// increments `space_entry_epoch` when entering a destination system, and writes or clears `item_positions`.
pub fn leave_space_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    ship_id: i64,
    cs: Option<&CombatState>,
    dest_in_space_system: Option<u32>,
    persist_position: Option<[f64; 3]>,
) -> u64 {
    if ship_id <= 0 {
        return 0;
    }
    let prev_meta = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)).copied();
    let prev_epoch = prev_meta
        .map(|m| m.hull_hp.max(0.0).round() as u64)
        .unwrap_or(0);
    let next_epoch = if dest_in_space_system.is_some() {
        prev_epoch.saturating_add(1)
    } else {
        prev_epoch
    };
    let sys_encoded = dest_in_space_system.map(|s| s as f64).unwrap_or(-1.0);
    let cap_encoded = if let Some(cs) = cs {
        let new_hp = PersistedCombatHp {
            shield_hp: cs.shield_hp.max(0.0),
            armor_hp: cs.armor_hp.max(0.0),
            hull_hp: cs.structure_hp.max(0.0),
        };
        snap.sp6_lifecycle.ship_combat_hp.insert(ship_id, new_hp);
        let armor_dmg = if cs.max_armor_hp > 0.0 {
            (cs.max_armor_hp - cs.armor_hp).clamp(0.0, cs.max_armor_hp)
        } else {
            0.0
        };
        let hull_dmg = if cs.max_structure_hp > 0.0 {
            (cs.max_structure_hp - cs.structure_hp).clamp(0.0, cs.max_structure_hp)
        } else {
            0.0
        };
        sync_ship_item_damage_attrs_in_snap(snap, ship_id, armor_dmg, hull_dmg);
        if cs.max_capacitor > 0.0 && (cs.max_capacitor - cs.capacitor.max(0.0)) > 1e-6 {
            cs.capacitor.clamp(0.0, cs.max_capacitor) + 1.0
        } else {
            0.0
        }
    } else {
        prev_meta.map(|m| m.shield_hp).unwrap_or(0.0)
    };
    snap.sp6_lifecycle.ship_combat_hp.insert(
        -ship_id,
        PersistedCombatHp {
            shield_hp: cap_encoded,
            armor_hp: sys_encoded,
            hull_hp: next_epoch as f64,
        },
    );
    if let Some(pos) = persist_position {
        snap.item_positions.insert(ship_id, pos);
    } else if dest_in_space_system.is_none() {
        snap.item_positions.remove(&ship_id);
    }
    next_epoch
}

/// Authoritative enter-space transition helper (`#917` T-1/T-2).
///
/// Marks `ship_id` as in space in `solar_system_id` and increments its `space_entry_epoch`.
pub fn enter_space_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    ship_id: i64,
    solar_system_id: u32,
) -> u64 {
    if ship_id <= 0 {
        return 0;
    }
    let prev_meta = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)).copied();
    let cap_encoded = prev_meta.map(|m| m.shield_hp).unwrap_or(0.0);
    let prev_epoch = prev_meta
        .map(|m| m.hull_hp.max(0.0).round() as u64)
        .unwrap_or(0);
    let next_epoch = prev_epoch.saturating_add(1);
    snap.sp6_lifecycle.ship_combat_hp.insert(
        -ship_id,
        PersistedCombatHp {
            shield_hp: cap_encoded,
            armor_hp: solar_system_id as f64,
            hull_hp: next_epoch as f64,
        },
    );
    next_epoch
}

/// Persists a ship ball's shield/armor/hull HP into `snap.sp6_lifecycle.ship_combat_hp` so damage
/// taken while disconnected or before a server restart is preserved upon reconnect (`plan:SP-6`).
pub fn persist_ship_combat_hp_in_db(
    db: &Arc<WorldRepository>,
    ball_id: i64,
    cs: &CombatState,
) {
    persist_ship_combat_hp_batch_in_db(db, [(ball_id, cs)]);
}

/// Persists multiple ship combat HPs into `snap.sp6_lifecycle.ship_combat_hp` AND mirrors hull/armor
/// deficits into `snap.items[ship_id].dynamic_attributes[DAMAGE | ARMOR_DAMAGE]` in a single mutation,
/// keeping all three ship HP representations synchronized (`#585`), and skipping snapshot write if unchanged.
pub fn persist_ship_combat_hp_batch_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner(db, updates, false);
}

/// Persists combat HPs only for balls backed by persisted `snap.items` or `snap.structures`,
/// preventing ephemeral NPC combat HP updates from triggering per-tick SQLite WAL writes.
pub fn persist_existing_ship_combat_hp_batch_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner(db, updates, true);
}

/// Persists combat HPs and capacitor for balls in `solar_system_id` backed by persisted entities,
/// refusing to overwrite ships that have already docked or jumped out of `solar_system_id` (`#846`, `#917` T-2).
pub fn persist_existing_ship_combat_hp_for_system_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    solar_system_id: u32,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner_for_system(db, Some(solar_system_id), updates, true);
}

fn persist_ship_combat_hp_batch_inner<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
    require_persisted_entity: bool,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner_for_system(db, None, updates, require_persisted_entity);
}

fn persist_ship_combat_hp_batch_inner_for_system<'a, I>(
    db: &Arc<WorldRepository>,
    expected_system: Option<u32>,
    updates: I,
    require_persisted_entity: bool,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    let to_update: Vec<(i64, PersistedCombatHp, Option<PersistedCombatHp>, bool, f64, f64)> =
        db.with_snapshot(|snap| {
            updates
                .into_iter()
                .filter_map(|(bid, cs)| {
                    if require_persisted_entity
                        && !snap.items.contains_key(&bid)
                        && !snap.structures.contains_key(&bid)
                    {
                        return None;
                    }
                    if is_ship_stale_for_in_space_write_in_snap(snap, bid, expected_system, None) {
                        return None;
                    }
                    let new_hp = PersistedCombatHp {
                        shield_hp: cs.shield_hp.max(0.0),
                        armor_hp: cs.armor_hp.max(0.0),
                        hull_hp: cs.structure_hp.max(0.0),
                    };
                    let cap_encoded = if bid > 0
                        && cs.max_capacitor > 0.0
                        && (cs.max_capacitor - cs.capacitor.max(0.0)) > 1e-6
                    {
                        cs.capacitor.clamp(0.0, cs.max_capacitor) + 1.0
                    } else {
                        0.0
                    };
                    let existing_meta =
                        (bid > 0).then(|| snap.sp6_lifecycle.ship_combat_hp.get(&(-bid)).copied()).flatten();
                    let prev_sys = existing_meta.map(|m| m.armor_hp).unwrap_or(0.0);
                    let prev_epoch = existing_meta.map(|m| m.hull_hp).unwrap_or(0.0);
                    let want_meta = if bid > 0 && (cap_encoded > 0.0 || prev_sys != 0.0 || prev_epoch != 0.0) {
                        Some(PersistedCombatHp {
                            shield_hp: cap_encoded,
                            armor_hp: prev_sys,
                            hull_hp: prev_epoch,
                        })
                    } else {
                        None
                    };
                    let meta_changed = match (existing_meta, want_meta) {
                        (None, None) => false,
                        (Some(a), Some(b)) => {
                            (a.shield_hp - b.shield_hp).abs() > 1e-6
                                || a.armor_hp != b.armor_hp
                                || a.hull_hp != b.hull_hp
                        }
                        _ => true,
                    };
                    let armor_dmg = if cs.max_armor_hp > 0.0 {
                        (cs.max_armor_hp - cs.armor_hp).clamp(0.0, cs.max_armor_hp)
                    } else {
                        0.0
                    };
                    let hull_dmg = if cs.max_structure_hp > 0.0 {
                        (cs.max_structure_hp - cs.structure_hp).clamp(0.0, cs.max_structure_hp)
                    } else {
                        0.0
                    };
                    let hp_changed = snap.sp6_lifecycle.ship_combat_hp.get(&bid) != Some(&new_hp);
                    let item_changed = snap.items.get(&bid).is_some_and(|it| {
                        let cur_a = it
                            .dynamic_attributes
                            .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                            .copied();
                        let cur_h = it
                            .dynamic_attributes
                            .get(&eve_sde::attr_ids::DAMAGE)
                            .copied();
                        let want_a = (armor_dmg > 1e-6).then_some(armor_dmg);
                        let want_h = (hull_dmg > 1e-6).then_some(hull_dmg);
                        let diff = |x: Option<f64>, y: Option<f64>| match (x, y) {
                            (None, None) => false,
                            (Some(a), Some(b)) => (a - b).abs() > 1e-6,
                            _ => true,
                        };
                        diff(cur_a, want_a) || diff(cur_h, want_h)
                    });
                    if hp_changed || meta_changed || item_changed {
                        Some((bid, new_hp, want_meta, meta_changed, armor_dmg, hull_dmg))
                    } else {
                        None
                    }
                })
                .collect()
        });

    if to_update.is_empty() {
        return;
    }

    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        for (bid, hp, want_meta, meta_changed, armor_dmg, hull_dmg) in to_update {
            if is_ship_stale_for_in_space_write_in_snap(snap, bid, expected_system, None) {
                continue;
            }
            snap.sp6_lifecycle.ship_combat_hp.insert(bid, hp);
            if bid > 0 && meta_changed {
                if let Some(m) = want_meta {
                    snap.sp6_lifecycle.ship_combat_hp.insert(-bid, m);
                } else {
                    snap.sp6_lifecycle.ship_combat_hp.remove(&(-bid));
                }
            }
            sync_ship_item_damage_attrs_in_snap(snap, bid, armor_dmg, hull_dmg);
        }
        Ok(())
    });
}

/// Applies a dirty-only condition checkpoint from [`crate::system_actor::types::StepOutput`] (`RFC-1 E-4`, `T-2`, `T-5`, `#919`).
///
/// Only writes rows whose `solar_system_id` AND `space_entry_epoch` still match the snapshot (`T-2`)
/// and whose persisted HP, capacitor, or position actually changed (`T-5`). Returns the number of
/// ships updated in the snapshot (`0` when no rows are modified).
pub fn apply_condition_checkpoint_for_system_in_db(
    db: &Arc<WorldRepository>,
    solar_system_id: u32,
    entries: &[crate::system_actor::types::ConditionCheckpointEntry],
) -> usize {
    if entries.is_empty() {
        return 0;
    }

    type CheckpointRowUpdate = (
        i64,
        u64,
        Option<(PersistedCombatHp, Option<PersistedCombatHp>, bool, f64, f64)>,
        Option<[f64; 3]>,
    );

    let to_update: Vec<CheckpointRowUpdate> = db.with_snapshot(|snap| {
        entries
            .iter()
            .filter_map(|entry| {
                let bid = entry.ball_id;
                let has_item = snap.items.contains_key(&bid);
                let has_struct = snap.structures.contains_key(&bid);
                if !has_item && !has_struct {
                    return None;
                }
                if is_ship_stale_for_in_space_write_in_snap(
                    snap,
                    bid,
                    Some(solar_system_id),
                    Some(entry.space_entry_epoch),
                ) {
                    return None;
                }

                let hp_update = entry.combat_state.as_ref().and_then(|cs| {
                    let new_hp = PersistedCombatHp {
                        shield_hp: cs.shield_hp.max(0.0),
                        armor_hp: cs.armor_hp.max(0.0),
                        hull_hp: cs.structure_hp.max(0.0),
                    };
                    let cap_encoded = if bid > 0
                        && cs.max_capacitor > 0.0
                        && (cs.max_capacitor - cs.capacitor.max(0.0)) > 1e-6
                    {
                        cs.capacitor.clamp(0.0, cs.max_capacitor) + 1.0
                    } else {
                        0.0
                    };
                    let existing_meta = (bid > 0)
                        .then(|| snap.sp6_lifecycle.ship_combat_hp.get(&(-bid)).copied())
                        .flatten();
                    let prev_sys = existing_meta.map(|m| m.armor_hp).unwrap_or(0.0);
                    let prev_epoch = existing_meta.map(|m| m.hull_hp).unwrap_or(0.0);
                    let want_meta =
                        if bid > 0 && (cap_encoded > 0.0 || prev_sys != 0.0 || prev_epoch != 0.0) {
                            Some(PersistedCombatHp {
                                shield_hp: cap_encoded,
                                armor_hp: prev_sys,
                                hull_hp: prev_epoch,
                            })
                        } else {
                            None
                        };
                    let meta_changed = match (existing_meta, want_meta) {
                        (None, None) => false,
                        (Some(a), Some(b)) => {
                            (a.shield_hp - b.shield_hp).abs() > 1e-6
                                || a.armor_hp != b.armor_hp
                                || a.hull_hp != b.hull_hp
                        }
                        _ => true,
                    };
                    let armor_dmg = if cs.max_armor_hp > 0.0 {
                        (cs.max_armor_hp - cs.armor_hp).clamp(0.0, cs.max_armor_hp)
                    } else {
                        0.0
                    };
                    let hull_dmg = if cs.max_structure_hp > 0.0 {
                        (cs.max_structure_hp - cs.structure_hp).clamp(0.0, cs.max_structure_hp)
                    } else {
                        0.0
                    };
                    let hp_changed = snap.sp6_lifecycle.ship_combat_hp.get(&bid) != Some(&new_hp);
                    let item_changed = snap.items.get(&bid).is_some_and(|it| {
                        let cur_a = it
                            .dynamic_attributes
                            .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                            .copied();
                        let cur_h = it
                            .dynamic_attributes
                            .get(&eve_sde::attr_ids::DAMAGE)
                            .copied();
                        let want_a = (armor_dmg > 1e-6).then_some(armor_dmg);
                        let want_h = (hull_dmg > 1e-6).then_some(hull_dmg);
                        let diff = |x: Option<f64>, y: Option<f64>| match (x, y) {
                            (None, None) => false,
                            (Some(a), Some(b)) => (a - b).abs() > 1e-6,
                            _ => true,
                        };
                        diff(cur_a, want_a) || diff(cur_h, want_h)
                    });
                    if hp_changed || meta_changed || item_changed {
                        Some((new_hp, want_meta, meta_changed, armor_dmg, hull_dmg))
                    } else {
                        None
                    }
                });

                let pos_update = if has_item {
                    let cur_pos = snap.item_positions.get(&bid).copied();
                    let changed = cur_pos.is_none_or(|p| {
                        (p[0] - entry.position[0]).abs() > 1e-3
                            || (p[1] - entry.position[1]).abs() > 1e-3
                            || (p[2] - entry.position[2]).abs() > 1e-3
                    });
                    changed.then_some(entry.position)
                } else {
                    None
                };

                if hp_update.is_some() || pos_update.is_some() {
                    Some((bid, entry.space_entry_epoch, hp_update, pos_update))
                } else {
                    None
                }
            })
            .collect()
    });

    if to_update.is_empty() {
        return 0;
    }

    let mut written = 0usize;
    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        for (bid, epoch, hp_opt, pos_opt) in to_update {
            if is_ship_stale_for_in_space_write_in_snap(
                snap,
                bid,
                Some(solar_system_id),
                Some(epoch),
            ) {
                continue;
            }
            if let Some((hp, want_meta, meta_changed, armor_dmg, hull_dmg)) = hp_opt {
                snap.sp6_lifecycle.ship_combat_hp.insert(bid, hp);
                if bid > 0 && meta_changed {
                    if let Some(m) = want_meta {
                        snap.sp6_lifecycle.ship_combat_hp.insert(-bid, m);
                    } else {
                        snap.sp6_lifecycle.ship_combat_hp.remove(&(-bid));
                    }
                }
                sync_ship_item_damage_attrs_in_snap(snap, bid, armor_dmg, hull_dmg);
            }
            if let Some(pos) = pos_opt {
                if snap.items.contains_key(&bid) {
                    snap.item_positions.insert(bid, pos);
                }
            }
            written += 1;
        }
        Ok(())
    });
    written
}

/// Clears ship hull/armor/shield/capacitor dynamic damage attributes and persisted `sp6_lifecycle.ship_combat_hp`
/// together when a ship is repaired or healed (`#585`, `#949`).
pub fn clear_ship_combat_damage_in_snap(snap: &mut eve_db::WorldStateSnapshot, ship_id: i64) {
    if let Some(it) = snap.items.get_mut(&ship_id) {
        let removed_h = it
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::DAMAGE)
            .is_some();
        let removed_a = it
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
            .is_some();
        let removed_s = it
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::SHIELD_CHARGE)
            .is_some();
        let removed_c = it.dynamic_attributes.remove(&18u16).is_some();
        if removed_h || removed_a || removed_s || removed_c {
            it.version += 1;
        }
    }
    snap.sp6_lifecycle.ship_combat_hp.remove(&ship_id);
    if ship_id > 0 {
        let remove_meta = if let Some(meta) = snap.sp6_lifecycle.ship_combat_hp.get_mut(&(-ship_id))
        {
            meta.shield_hp = 0.0;
            meta.armor_hp == 0.0 && meta.hull_hp == 0.0
        } else {
            false
        };
        if remove_meta {
            snap.sp6_lifecycle.ship_combat_hp.remove(&(-ship_id));
        }
    }
}

/// Unified ship HP & damage view reconciled across `Ballpark::combat_states`,
/// `snap.sp6_lifecycle.ship_combat_hp`, and `item.dynamic_attributes` (`#585`).
#[derive(Debug, Clone, Copy)]
pub struct UnifiedShipDamageState {
    pub max_shield: f64,
    pub shield_hp: f64,
    pub shield_damage: f64,
    pub max_armor: f64,
    pub armor_damage: f64,
    pub max_hull: f64,
    pub hull_damage: f64,
}

pub fn read_unified_ship_damage_state(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    type_id: u32,
    fallback_maxes_hull_armor_shield: Option<(f64, f64, f64)>,
) -> UnifiedShipDamageState {
    let t_opt = ctx.sde.types.get(&type_id);
    let (def_hull, def_armor, def_shield) = fallback_maxes_hull_armor_shield.unwrap_or_else(|| {
        (
            t_opt
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
                .unwrap_or(0.0),
            t_opt
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::ARMOR_HP).copied())
                .unwrap_or(0.0),
            t_opt
                .and_then(|t| {
                    t.default_attributes
                        .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                        .copied()
                })
                .unwrap_or(400.0),
        )
    });
    let live_cs = ctx.systems.find_ball_system_sync(ship_id).and_then(|sys_id| {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let bp = handle.ballpark.read().ok()?;
        let cs = bp.combat_states.get(&ship_id)?;
        Some((
            cs.max_structure_hp,
            cs.structure_hp,
            cs.max_armor_hp,
            cs.armor_hp,
            cs.max_shield_hp,
            cs.shield_hp,
        ))
    });
    let eff_hull = live_cs.map(|c| c.0).unwrap_or(def_hull).max(def_hull);
    let eff_armor = live_cs.map(|c| c.2).unwrap_or(def_armor).max(def_armor);
    let eff_shield = live_cs.map(|c| c.4).unwrap_or(def_shield).max(def_shield);

    let live_hull_dmg = live_cs
        .map(|(mh, h, _, _, _, _)| if mh > 0.0 { (mh - h).max(0.0) } else { 0.0 })
        .unwrap_or(0.0);
    let live_armor_dmg = live_cs
        .map(|(_, _, ma, a, _, _)| if ma > 0.0 { (ma - a).max(0.0) } else { 0.0 })
        .unwrap_or(0.0);
    let live_shield_dmg = live_cs
        .map(|(_, _, _, _, ms, s)| if ms > 0.0 { (ms - s).max(0.0) } else { 0.0 })
        .unwrap_or(0.0);

    let persisted = snap.sp6_lifecycle.ship_combat_hp.get(&ship_id);
    let p_hull_dmg = persisted
        .map(|hp| {
            if eff_hull > 0.0 {
                (eff_hull - hp.hull_hp).max(0.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    let p_armor_dmg = persisted
        .map(|hp| {
            if eff_armor > 0.0 {
                (eff_armor - hp.armor_hp).max(0.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    let p_shield_dmg = persisted
        .map(|hp| {
            if eff_shield > 0.0 {
                (eff_shield - hp.shield_hp).max(0.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);

    let item_opt = snap.items.get(&ship_id);
    let dyn_hull_dmg = item_opt
        .and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
        .unwrap_or(0.0)
        .max(0.0);
    let dyn_armor_dmg = item_opt
        .and_then(|it| {
            it.dynamic_attributes
                .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                .copied()
        })
        .unwrap_or(0.0)
        .max(0.0);

    let hull_damage = live_hull_dmg.max(p_hull_dmg).max(dyn_hull_dmg);
    let armor_damage = live_armor_dmg.max(p_armor_dmg).max(dyn_armor_dmg);
    let shield_damage = live_shield_dmg.max(p_shield_dmg);
    let shield_hp = (eff_shield - shield_damage).clamp(0.0, eff_shield);

    UnifiedShipDamageState {
        max_shield: eff_shield,
        shield_hp,
        shield_damage,
        max_armor: eff_armor,
        armor_damage,
        max_hull: eff_hull,
        hull_damage,
    }
}

/// Restores persisted shield/armor/hull HP from `snap.sp6_lifecycle.ship_combat_hp` (falling back
/// to item `DAMAGE`/`ARMOR_DAMAGE` attributes if needed) onto `cs`.
pub fn apply_persisted_hp_to_combat_state(
    ctx: &ServerContext,
    ship_id: i64,
    cs: &mut CombatState,
) {
    ctx.db.with_snapshot(|snap| {
        if let Some(hp) = snap.sp6_lifecycle.ship_combat_hp.get(&ship_id) {
            cs.shield_hp = hp.shield_hp.clamp(0.0, cs.max_shield_hp);
            cs.armor_hp = hp.armor_hp.clamp(0.0, cs.max_armor_hp);
            if hp.hull_hp > 0.0 {
                cs.structure_hp = hp.hull_hp.clamp(0.0, cs.max_structure_hp);
            }
        } else if let Some(it) = snap.items.get(&ship_id) {
            if let Some(&ad) = it.dynamic_attributes.get(&eve_sde::attr_ids::ARMOR_DAMAGE) {
                if ad > 0.0 {
                    cs.armor_hp = (cs.max_armor_hp - ad).clamp(0.0, cs.max_armor_hp);
                }
            }
            if let Some(&hd) = it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE) {
                if hd > 0.0 {
                    cs.structure_hp = (cs.max_structure_hp - hd).clamp(0.0, cs.max_structure_hp);
                }
            }
        }
        if ship_id > 0 {
            if let Some(meta) = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)) {
                if meta.shield_hp >= 1.0 {
                    let cap = (meta.shield_hp - 1.0).max(0.0);
                    cs.capacitor = cap.clamp(0.0, cs.max_capacitor);
                }
            }
        }
    });
}

/// Evaluates active Crimewatch / combat timers `(weapons_until, npc_until, pvp_until, criminal_until)` for `char_id`.
pub fn active_timers_for_char(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> (u64, u64, u64, u64) {
    let (cw_weap, cw_pvp, cw_crim) = {
        let guard = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = guard.states.get(&char_id) {
            (
                st.weapon_timer_until_unix,
                st.pvp_timer_until_unix,
                st.criminal_flag_until_unix,
            )
        } else {
            (0, 0, 0)
        }
    };
    let rt_npc = with_combat_runtime(ctx, |rt| {
        rt.npc_timers.get(&char_id).copied().unwrap_or(0)
    });
    let (sp_weap, sp_npc, sp_pvp, sp_crim) = with_sp6_state(&ctx.db, |st| {
        (
            st.weapon_timers_until.get(&char_id).copied().unwrap_or(0),
            st.npc_timers_until.get(&char_id).copied().unwrap_or(0),
            st.pvp_timers_until.get(&char_id).copied().unwrap_or(0),
            st.criminal_timers_until.get(&char_id).copied().unwrap_or(0),
        )
    });
    let weap = cw_weap.max(sp_weap);
    let npc = rt_npc.max(sp_npc);
    let pvp = cw_pvp.max(sp_pvp);
    let crim = cw_crim.max(sp_crim);
    (
        if weap > now_unix { weap } else { 0 },
        if npc > now_unix { npc } else { 0 },
        if pvp > now_unix { pvp } else { 0 },
        if crim > now_unix { crim } else { 0 },
    )
}

pub fn has_active_pvp_timer(ctx: &ServerContext, char_id: i64, now_unix: u64) -> bool {
    let (_, _, pvp, crim) = active_timers_for_char(ctx, char_id, now_unix);
    pvp > now_unix || crim > now_unix
}

/// Blocks Abyssal, Noise/Signal, and Proving Grounds filaments while under an active 15m PvP timer
/// or an active safe-logoff countdown (`feat:crimewatch.pvp-logoff-timer-renewal`, `feat:pve.abyssal-activation-restrictions`).
pub fn require_no_pvp_timer_for_filament(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    if has_active_pvp_timer(ctx, char_id, now_unix) {
        return Err(BridgeRpcError {
            code: "PvpTimerActive".into(),
            message: "Cannot activate filament while a 15-minute Capsuleer Log-Off (PvP) timer is active"
                .into(),
            client_error: None,
        });
    }
    let has_safe_logoff = with_sp6_state(&ctx.db, |st| {
        st.safe_logoffs
            .get(&char_id)
            .is_some_and(|e| !e.completed && now_unix < e.complete_at_unix)
    });
    if has_safe_logoff {
        return Err(BridgeRpcError {
            code: "LogoffTimerActive".into(),
            message: "Cannot activate filament while a safe-logoff countdown timer is active".into(),
            client_error: None,
        });
    }
    Ok(())
}

/// Validates all space/combat/tackle pre-conditions before activating an Abyssal, Void, or Proving Grounds filament (`#76`).
pub fn require_ship_ready_for_filament_jump(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    require_no_pvp_timer_for_filament(ctx, char_id)?;
    let (weap_until, _npc_until, _, _) = active_timers_for_char(ctx, char_id, now_unix);
    if weap_until > now_unix {
        return Err(BridgeRpcError {
            code: "WeaponsTimerActive".into(),
            message: "Cannot activate filament while a Weapons Timer is active".into(),
            client_error: None,
        });
    }
    crate::services::space_nav_movement_guard::validate_pre_movement_locks(
        ctx,
        ship_id,
        &crate::services::space_nav_movement_guard::MovementIntent::AbyssalOrVoidFilament {
            from_sys: sys_id,
            reject_when_cloaked: false,
            reject_when_tethered: false,
        },
    )?;
    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None)
    {
        if !matches!(active_timer.reason.as_str(), "login" | "undock") {
            return Err(BridgeRpcError {
                code: "SessionChangeInProgress".into(),
                message: format!(
                    "Cannot activate filament while 10s session-change timer ({}) is active",
                    active_timer.reason
                ),
                client_error: None,
            });
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.is_ball_scrambled_sync(ship_id) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot activate filament while warp scrambled or disrupted".into(),
            client_error: None,
        });
    }
    if let Ok(bp) = sys_handle.ballpark.read() {
        if let Some(ball) = bp.balls.get(&ship_id) {
            if matches!(ball.mode, MovementMode::Warp { .. }) {
                return Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot activate filament while ship is mid-warp".into(),
                    client_error: None,
                });
            }
        }
        if let Some(cs) = bp.combat_states.get(&ship_id) {
            if cs.lance_disrupted_remaining_s > 0.0 {
                return Err(BridgeRpcError {
                    code: "DisruptiveLanceDebuffActive".into(),
                    message: "Cannot activate filament while disrupted by a Superweapon Lance".into(),
                    client_error: None,
                });
            }
            if cs.warp_scramble_strength > 0 {
                return Err(BridgeRpcError {
                    code: "WarpScrambled".into(),
                    message: "Cannot activate filament while warp scrambled".into(),
                    client_error: None,
                });
            }
            if cs.is_warp_exit_invulnerable() || (cs.gate_cloak_remaining_ticks > 0 && cs.is_invulnerable()) {
                return Err(BridgeRpcError {
                    code: "ShipInvulnerable".into(),
                    message: "Cannot activate filament while session-change or warp-exit invulnerability is active".into(),
                    client_error: None,
                });
            }
        }
    }
    Ok(())
}

/// Records capsuleer-vs-capsuleer aggression, starting/refreshing the 15-minute (`900s`) PvP log-off timer
/// on both attacker and victim, aborting any active safe-logoff countdown, and renewing the victim's
/// in-space removal timer to `now + 900s` if the victim is already disconnected (`feat:crimewatch.pvp-logoff-timer-renewal`).
pub fn record_pvp_aggression_and_renew_logoff_timer(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_id: i64,
    now_unix: u64,
) {
    let pvp_until = now_unix.saturating_add(PVP_TIMER_SECS);
    let weap_until = now_unix.saturating_add(WEAPONS_TIMER_SECS);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let att = cw.get_or_create(attacker_char_id);
        att.weapon_timer_until_unix = att.weapon_timer_until_unix.max(weap_until);
        att.pvp_timer_until_unix = att.pvp_timer_until_unix.max(pvp_until);
        if victim_char_id > 0 {
            let vic = cw.get_or_create(victim_char_id);
            vic.pvp_timer_until_unix = vic.pvp_timer_until_unix.max(pvp_until);
        }
    }
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until
            .insert(attacker_char_id, weap_until);
        st.pvp_timers_until.insert(attacker_char_id, pvp_until);
        if victim_char_id > 0 {
            st.pvp_timers_until.insert(victim_char_id, pvp_until);
            if let Some(disc) = st.disconnected_ships.get_mut(&victim_char_id) {
                if !disc.removed_from_space {
                    disc.remove_at_unix = disc.remove_at_unix.max(pvp_until);
                }
            }
        }
    });
    let _ = maybe_abort_safe_logoff_on_action(ctx, attacker_char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
    crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, attacker_char_id, now_unix);
    if victim_char_id > 0 {
        let _ =
            maybe_abort_safe_logoff_on_action(ctx, victim_char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
        crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, victim_char_id, now_unix);
    }
}

/// Records NPC aggression (`300s` = 5m timer) for an online character.
/// Per `feat:crimewatch.pvp-logoff-timer-renewal`, NPC aggression does **not** renew the log-off
/// timer of a ship whose pilot has already logged off / disconnected.
pub fn record_npc_aggression_timer(ctx: &ServerContext, char_id: i64, now_unix: u64) {
    let is_disconnected = with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships
            .get(&char_id)
            .is_some_and(|d| !d.removed_from_space)
    });
    if is_disconnected {
        return;
    }
    let npc_until = now_unix.saturating_add(NPC_TIMER_SECS);
    with_mut_combat_runtime(ctx, |rt| {
        rt.npc_timers.insert(char_id, npc_until);
    });
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.npc_timers_until.insert(char_id, npc_until);
    });
    let _ = maybe_abort_safe_logoff_on_action(ctx, char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
    crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, char_id, now_unix);
}

/// Checks whether the pilot has active cycling modules, cloak/bastion/siege state, deployed drones/fighters,
/// or deployed scanner/moon probes in space that block `ship.SafeLogoff` (`BF-SP6-SAFE-LOGOFF-CONDITIONS`).
pub fn safe_logoff_blocking_space_conditions(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) -> Vec<&'static str> {
    let mut conds = Vec::new();
    let has_persisted_active_mods = ctx.db.with_snapshot(|snap| {
        snap.module_active_states.iter().any(|&mid| {
            snap.items
                .get(&mid)
                .is_some_and(|i| i.location_id == ship_id)
        })
    });
    let has_nav_lock = !crate::services::structure_control_ops::is_structure_vessel(ctx, ship_id)
        && (crate::services::tactical_modules::check_ship_navigation_lock(
            ctx,
            ship_id,
            "safe logoff",
        )
        .is_err()
            || crate::services::tactical_modules::is_clone_vat_bay_active_required(ctx, ship_id));
    let has_active_mods = has_persisted_active_mods
        || has_nav_lock
        || ctx.systems.has_active_modules_or_cloak_sync(sys_id, ship_id)
        || ctx.mining_cycles.read().is_ok_and(|mc| {
            mc.values()
                .any(|c| c.ship_id == ship_id || c.char_id == char_id)
        })
        || ctx.ship_fittings.read().is_ok_and(|sf| {
            sf.get(&ship_id).is_some_and(|eng| {
                eng.modules.values().any(|m| {
                    matches!(
                        m.state,
                        eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                    )
                })
            })
        });
    if has_active_mods {
        conds.push("SAFE_LOGOFF_ACTIVE_MODULES");
    }

    let in_warp = ctx
        .systems
        .get_or_spawn(sys_id)
        .ballpark
        .read()
        .is_ok_and(|bp| {
            bp.balls
                .get(&ship_id)
                .is_some_and(|b| matches!(b.mode, MovementMode::Warp { .. }))
        });
    if in_warp {
        conds.push("SAFE_LOGOFF_IN_WARP");
    }

    let has_drones_or_fighters = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .any(|d| !d.is_abandoned && (d.owner_char_id == char_id || d.parent_ship_id == ship_id))
    }) || ctx.active_fighters.read().is_ok_and(|af| {
        af.values()
            .any(|f| !f.is_abandoned && (f.owner_char_id == char_id || f.parent_ship_id == ship_id))
    });
    if has_drones_or_fighters {
        conds.push("SAFE_LOGOFF_DRONES_IN_SPACE");
    }

    let has_probes = ctx.active_scanner_probes.read().is_ok_and(|sp| {
        sp.values()
            .any(|p| p.owner_char_id == char_id || p.ship_id == ship_id)
    }) || ctx
        .active_moon_probes
        .read()
        .is_ok_and(|mp| mp.iter().any(|p| p.owner_char_id == char_id));
    if has_probes {
        conds.push("SAFE_LOGOFF_PROBES_IN_SPACE");
    }

    conds
}

/// Handles `ship.SafeLogoff` (0 args) (`plan:SP-6`, `feat:character.safe-logoff`).
/// - Refuses if docked, warp-scrambled, self-destructing, running active modules/cloak, controlling drones/fighters/probes,
///   or under an active Weapons, NPC, PvP, or Criminal timer:
///   emits `OnSafeLogoffFailed` with `vec![PyRep::List(failed_conditions)]` (`arity = 1`) and returns `PyRep::List(failed_conditions)`.
/// - Otherwise starts a 30s countdown (`now_unix + 30`), emits `OnSafeLogoffTimerStarted` with
///   `vec![PyRep::Integer(unix_to_filetime(complete_at_unix))]` (`arity = 1`), and returns `PyRep::List(vec![])`.
// parity: feat:character.safe-logoff
pub async fn handle_safe_logoff(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let mut failed_conditions: Vec<PyRep> = Vec::new();

    if session.station_id.is_some() || session.structure_id.is_some() {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_DOCKED".into()));
    }
    let ship_id = session.ship_id.unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            .unwrap_or(0)
    });
    let sys_id = session.solar_system_id.unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .map(|c| c.solar_system_id)
                .filter(|&id| id > 0)
                .or_else(|| {
                    (ship_id > 0).then(|| s.items.get(&ship_id)).flatten().and_then(|it| {
                        if (30_000_000..40_000_000).contains(&it.location_id) {
                            Some(it.location_id as u32)
                        } else {
                            ctx.sde.location_solar_system(it.location_id)
                        }
                    })
                })
                .unwrap_or(0)
        })
    });

    if ship_id > 0 && sys_id > 0 && session.station_id.is_none() && session.structure_id.is_none() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
            failed_conditions.push(PyRep::String("SAFE_LOGOFF_WARP_SCRAMBLED".into()));
        }
        for code in safe_logoff_blocking_space_conditions(ctx, char_id, ship_id, sys_id) {
            failed_conditions.push(PyRep::String(code.into()));
        }
    }

    let (weap_until, npc_until, pvp_until, crim_until) =
        active_timers_for_char(ctx, char_id, now_unix);
    if weap_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_WEAPONS_TIMER".into()));
    }
    if npc_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_NPC_TIMER".into()));
    }
    if pvp_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_PVP_TIMER".into()));
    }
    if crim_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_CRIMINAL_TIMER".into()));
    }

    let self_destruct_active = with_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns
            .get(&ship_id)
            .is_some_and(|cd| now_unix < cd.detonate_at_unix)
    }) || with_sp6_state(&ctx.db, |st| {
        st.self_destructs
            .get(&ship_id)
            .is_some_and(|cd| now_unix < cd.detonate_at_unix)
    });
    if self_destruct_active {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_SELF_DESTRUCT_ACTIVE".into()));
    }

    if !failed_conditions.is_empty() {
        let list_rep = PyRep::List(failed_conditions);
        let notif_args = vec![list_rep.clone()];
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSafeLogoffFailed",
            "charid",
            notif_args.clone(),
        );
        session.push_notification("OnSafeLogoffFailed", notif_args);
        return Ok(list_rep);
    }

    let complete_at_unix = now_unix.saturating_add(SAFE_LOGOFF_COUNTDOWN_SECS);
    if ship_id > 0 && sys_id > 0 && session.station_id.is_none() && session.structure_id.is_none() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
        let _ = sys_handle.set_speed_fraction(ship_id, 0.0).await;
    }
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.remove(&char_id);
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix,
                completed: false,
            },
        );
    });

    let safe_logoff_ft = unix_to_filetime(complete_at_unix);
    let started_args = vec![PyRep::Integer(safe_logoff_ft)];
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSafeLogoffTimerStarted",
        "charid",
        started_args.clone(),
    );
    session.push_notification("OnSafeLogoffTimerStarted", started_args);

    Ok(PyRep::List(vec![]))
}

/// Aborts an active safe-logoff countdown on explicit request (`ship.AbortSafeLogoff`)
/// and returns `PyRep::Bool(true)` (`plan:SP-6`, `feat:character.safe-logoff`).
// parity: feat:character.safe-logoff
pub fn handle_abort_safe_logoff(
    ctx: &ServerContext,
    session: &mut SessionState,
    reason_code: &str,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.remove(&char_id);
    });
    let abort_args = vec![PyRep::String(reason_code.to_string())];
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSafeLogoffAborted",
        "charid",
        abort_args.clone(),
    );
    session.push_notification("OnSafeLogoffAborted", abort_args);
    Ok(PyRep::Bool(true))
}

/// Aborts an active safe-logoff countdown when the pilot moves, warps, activates a module, or takes/deals aggression (`plan:SP-6`).
pub fn maybe_abort_safe_logoff_on_action(
    ctx: &ServerContext,
    char_id: i64,
    reason_code: &str,
) -> bool {
    if !ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id))
    {
        return false;
    }
    let aborted = with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs
            .remove(&char_id)
            .is_some_and(|entry| !entry.completed)
    });
    if aborted {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSafeLogoffAborted",
            "charid",
            vec![PyRep::String(reason_code.to_string())],
        );
    }
    aborted
}

/// Computes a deterministic 1,000,000 km (`1e9 m`) emergency-warp destination vector in deep space.
pub(super) fn compute_emergency_warp_destination(current_pos: DVec3, char_id: i64, ship_id: i64) -> DVec3 {
    let seed = (char_id as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(ship_id as u64)
        .wrapping_add(1442695040888963407);
    let theta = ((seed & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
    let z_frac = (((seed >> 16) & 0xFFFF) as f64 / 65535.0) * 0.4 - 0.2;
    let r_xy = (1.0 - z_frac * z_frac).sqrt();
    let dir = DVec3::new(r_xy * theta.cos(), z_frac, r_xy * theta.sin()).normalize_or_zero();
    let unit = if dir.length_squared() > 0.5 {
        dir
    } else {
        DVec3::X
    };
    current_pos + unit * EMERGENCY_WARP_DISTANCE_M
}

/// Returns `true` if `char_id` already has a persisted in-space `DisconnectedShipEntry` (`#851` / `B13`).
pub fn has_active_disconnected_ship(db: &Arc<WorldRepository>, char_id: i64) -> bool {
    with_sp6_state(db, |st| st.disconnected_ships.contains_key(&char_id))
}

/// Synchronously handles ungraceful logoff, kick, ban, downtime, or dropped/panicked socket
/// connection while in space (`plan:SP-6`, `#851` / `B13`):
/// - Initiates a single emergency warp to ~1,000,000 km unless the ship is warp-scrambled or criminal in high-sec.
/// - Keeps the ship ball in the `Ballpark` for the active log-off timer (`60s` unaggressed/weapons, `300s` NPC, `900s` PvP/Criminal).
/// - Persists `DisconnectedShipEntry` in SQLite so the ship remains vulnerable until `remove_at_unix` and
///   logs back into space at its final position on next login.
pub fn handle_character_logoff_or_disconnect_sync(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<DisconnectedShipEntry> {
    let now_unix = ctx.now_unix();
    crate::services::campaigns_and_milestones::playtime::pause_milestone_playtime(
        &ctx.db, char_id, now_unix,
    );
    let (weap_until, npc_until, pvp_until, crim_until) =
        active_timers_for_char(ctx, char_id, now_unix);
    let safe_entry = with_mut_sp6_state(&ctx.db, |st, _snap| st.safe_logoffs.remove(&char_id));
    let char_info = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id)?;
        if ch.station_id.is_some() || ch.structure_id.is_some() {
            return None;
        }
        let ship_id = ch.active_ship_id;
        let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
        let saved_pos = snap
            .item_positions
            .get(&ship_id)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        Some((ch.solar_system_id, ship_id, ship_type_id, saved_pos))
    })?;
    let (sys_id, ship_id, ship_type_id, saved_pos) = char_info;
    let sys_handle_opt = ctx.systems.get_or_spawn_if_runtime(sys_id);
    let has_active_combat_timers = weap_until > now_unix
        || npc_until > now_unix
        || pvp_until > now_unix
        || crim_until > now_unix;
    if safe_entry.is_some_and(|e| e.completed)
        && !has_active_combat_timers
        && sys_handle_opt
            .as_ref()
            .is_none_or(|h| h.get_ball_position_sync(ship_id).is_none())
    {
        return None;
    }
    let (group_id, category_id, mass, radius, max_vel, inertia, warp_speed_au_s) =
        sde_ship_physics(&ctx.sde, ship_type_id);

    // Ensure the ship ball exists in the ballpark if it wasn't spawned yet (when a Tokio runtime/handle is active).
    let current_pos = if let Some(pos) = sys_handle_opt
        .as_ref()
        .and_then(|h| h.get_ball_position_sync(ship_id))
    {
        pos
    } else {
        let pos = DVec3::from_array(saved_pos);
        if let Some(ref sys_handle) = sys_handle_opt {
            let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
            let (corp_id, alliance_id, sec_status) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| {
                        (
                            c.corporation_id,
                            c.alliance_id.unwrap_or(-1),
                            c.security_status,
                        )
                    })
                    .unwrap_or((1_000_009, -1, 0.0))
            });
            sys_handle.try_spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: max_vel,
                    inertia,
                    mass,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: char_id,
                    char_id: Some(char_id),
                    corp_id,
                    alliance_id,
                    security_status: sec_status,
                    is_cloaked: false, ..Default::default()
                },
            );
        }
        pos
    };

    if let Some(cs) = sys_handle_opt.as_ref().and_then(|h| {
        h.ballpark
            .read()
            .ok()
            .and_then(|bp| bp.combat_states.get(&ship_id).cloned())
    }) {
        persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);
    }
    abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
    ctx.return_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&ship_id);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.retain(|&(_, c)| c != char_id);
    });

    let entry = disconnect_hold::build_disconnected_ship_entry_sync(
        ctx,
        sys_handle_opt.as_ref(),
        char_id,
        ship_id,
        ship_type_id,
        sys_id,
        current_pos,
        warp_speed_au_s,
        now_unix,
    );

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.item_positions
            .insert(ship_id, [current_pos.x, current_pos.y, current_pos.z]);
        let _ = crate::router::apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            char_id,
            crate::router::TransitionLocation::Space { solar_system_id: sys_id },
            None,
            false,
            false,
        );
        st.disconnected_ships.insert(char_id, entry.clone());
    });

    Some(entry)
}

/// Handles ungraceful logoff (`authentication.Logout`, `userSvc.UserLogOffCharacter`) or dropped socket
/// connection while in space (`plan:SP-6`):
/// - Initiates a single emergency warp to ~1,000,000 km unless the ship is warp-scrambled or criminal in high-sec.
/// - Keeps the ship ball in the `Ballpark` for the active log-off timer (`60s` unaggressed/weapons, `300s` NPC, `900s` PvP/Criminal).
/// - Persists `DisconnectedShipEntry` in SQLite so the ship remains vulnerable until `remove_at_unix` and
///   logs back into space at its final position on next login.
pub async fn handle_character_logoff_or_disconnect(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<DisconnectedShipEntry> {
    handle_character_logoff_or_disconnect_sync(ctx, char_id)
}

/// Synchronously reconnects a character who logged off or disconnected in space (`plan:SP-6`).
pub fn reconnect_character_in_space_sync(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<(u32, i64, DVec3)> {
    let now_unix = ctx.now_unix();
    cluster_tick::on_advance_secs(ctx, now_unix, 0);
    cluster_tick::collapse_stranded_abyssal_if_needed_sync(ctx, char_id, now_unix);

    let (sys_id, ship_id, ship_type_id, pos_arr, had_live_disc, pre_warp_pos_opt, corp_id, alliance_id, sec_status) =
        with_mut_sp6_state(&ctx.db, |st, snap| {
            let ch = snap.characters.get(&char_id)?;
            if ch.station_id.is_some() || ch.structure_id.is_some() {
                st.disconnected_ships.remove(&char_id);
                st.safe_logoffs.remove(&char_id);
                return None;
            }
            let sys_id = ch.solar_system_id;
            let ship_id = ch.active_ship_id;
            let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
            let corp_id = ch.corporation_id;
            let alliance_id = ch.alliance_id.unwrap_or(-1);
            let sec_status = ch.security_status;

            let disc_entry = st
                .disconnected_ships
                .remove(&char_id)
                .filter(|d| d.ship_id == ship_id && snap.items.contains_key(&d.ship_id));
            let pre_warp_pos_opt = disc_entry.as_ref().and_then(|d| d.pre_warp_position);
            let disc_pos = disc_entry.map(|d| d.last_position);
            st.safe_logoffs.remove(&char_id);

            let is_valid_space_pos = |arr: [f64; 3]| -> bool {
                let pos = DVec3::from_array(arr);
                if pos.length() < 10_000.0 {
                    return false;
                }
                if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
                    for st in &sys.stations {
                        let st_radius = ctx
                            .sde
                            .types
                            .get(&st.station_type_id)
                            .map(|t| t.radius)
                            .unwrap_or(30_000.0)
                            .max(20_000.0);
                        if pos.distance(st.position) < st_radius {
                            return false;
                        }
                    }
                }
                true
            };
            let safe_fallback_pos = || -> [f64; 3] {
                if let Some(st) = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.first())
                {
                    let dir = st
                        .orbit_id
                        .and_then(|oid| {
                            ctx.sde
                                .celestials_in_system(sys_id)
                                .into_iter()
                                .find(|c| c.celestial_id == oid)
                        })
                        .and_then(|cel| {
                            let delta = st.position - cel.position;
                            (delta.length_squared() > 1.0).then(|| delta.normalize())
                        })
                        .or_else(|| {
                            (st.position.length_squared() > 1.0).then(|| st.position.normalize())
                        })
                        .unwrap_or(DVec3::X);
                    let st_radius = ctx
                        .sde
                        .types
                        .get(&st.station_type_id)
                        .map(|t| t.radius)
                        .unwrap_or(30_000.0)
                        .max(25_000.0);
                    let p = st.position + dir * (st_radius + 5_000.0);
                    return [p.x, p.y, p.z];
                }
                if let Some(cel) = ctx.sde.celestials_in_system(sys_id).into_iter().next() {
                    let p = cel.position + DVec3::X * (cel.radius.max(10_000.0) + 50_000.0);
                    return [p.x, p.y, p.z];
                }
                [15_000_000_000.0, 0.0, 0.0]
            };
            let pos_arr = disc_pos
                .filter(|p| is_valid_space_pos(*p))
                .or_else(|| {
                    snap.item_positions
                        .get(&ship_id)
                        .copied()
                        .filter(|p| is_valid_space_pos(*p))
                })
                .unwrap_or_else(safe_fallback_pos);
            snap.item_positions.insert(ship_id, pos_arr);
            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                ship_item.location_id = sys_id as i64;
                ship_item.flag = InventoryFlag::None;
            }
            Some((
                sys_id,
                ship_id,
                ship_type_id,
                pos_arr,
                disc_pos.is_some(),
                pre_warp_pos_opt,
                corp_id,
                alliance_id,
                sec_status,
            ))
        })?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let keep_client_warp =
        pre_warp_pos_opt.is_none() && sys_handle.committed_warp_destination_sync(ship_id).is_some();
    if !keep_client_warp {
        ctx.active_warps.lock().unwrap_or_else(|e| e.into_inner()).remove(&ship_id);
    }
    let (group_id, category_id, mass, radius, max_vel, inertia, warp_speed) =
        sde_ship_physics(&ctx.sde, ship_type_id);

    // If the character's ship is already active in space, preserve the live ball position,
    // capacitor, tackle warp scrambles, and combat status instead of resetting to origin or defaults.
    // If the ship emergency-warped off grid, automatically initiate a return warp back to the pre-disconnect grid spot.
    let pre_warp_eligible =
        crate::services::space_nav_movement_guard::check_sync_pre_warp_eligibility(ctx, ship_id);
    if let Some(existing_pos) = sys_handle.get_ball_position_sync(ship_id) {
        let mut started_return_warp = false;
        if let Ok(mut bp) = sys_handle.ballpark.write() {
            bp.observer_events.remove(&ship_id);
            bp.queue_set_state(ship_id);
            if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
                cs.is_emergency_warping = false;
            }
            if pre_warp_eligible
                && crate::services::space_nav_movement_guard::can_warp_in_ballpark(&bp, ship_id)
            {
                if let Some(orig_arr) = pre_warp_pos_opt {
                    let orig_pos = DVec3::from_array(orig_arr);
                    if existing_pos.distance(orig_pos) > 10_000.0 {
                        let _ = bp.set_mode(ship_id, MovementMode::new_warp(existing_pos, orig_pos, warp_speed));
                        started_return_warp = true;
                    }
                }
            }
            if pre_warp_pos_opt.is_some()
                && !started_return_warp
                && matches!(
                    bp.balls.get(&ship_id).map(|b| &b.mode),
                    Some(MovementMode::Warp { .. })
                )
            {
                // Disconnected ship was still in its outbound 1,000,000 km emergency warp and
                // cannot start a return warp (<= 10 km from orig_pos, or tackled/ineligible):
                // stop the outbound warp immediately so reconnect never flings the ship off-grid (#843).
                let _ = bp.force_stop_warp(ship_id);
                bp.queue_set_state(ship_id);
            }
        }
        if started_return_warp {
            ctx.return_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(ship_id);
        } else {
            ctx.return_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&ship_id);
        }
        if keep_client_warp {
            ctx.active_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(ship_id)
                .or_insert(char_id);
        }
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            snap.item_positions
                .insert(ship_id, [existing_pos.x, existing_pos.y, existing_pos.z]);
            Ok(())
        });
        return Some((sys_id, ship_id, existing_pos));
    }

    let final_pos = if had_live_disc {
        sys_handle
            .get_ball_position_sync(ship_id)
            .unwrap_or_else(|| DVec3::from_array(pos_arr))
    } else {
        DVec3::from_array(pos_arr)
    };
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        snap.item_positions
            .insert(ship_id, [final_pos.x, final_pos.y, final_pos.z]);
        enter_space_in_snap(snap, ship_id, sys_id);
        Ok(())
    });

    // Remove any existing ball for `ship_id` so there are never duplicate ghost balls.
    sys_handle.try_remove_ball(ship_id);

    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);

    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: ship_type_id, position: final_pos, velocity: DVec3::ZERO,
            max_velocity: max_vel, inertia, mass, radius, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id, category_id, owner_id: char_id, char_id: Some(char_id),
            corp_id, alliance_id, security_status: sec_status, is_cloaked: false, ..Default::default()
        },
    );
    let mut started_return_warp = false;
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.observer_events.remove(&ship_id);
        bp.queue_set_state(ship_id);
        if pre_warp_eligible
            && crate::services::space_nav_movement_guard::can_warp_in_ballpark(&bp, ship_id)
        {
            if let Some(orig_arr) = pre_warp_pos_opt {
                let orig_pos = DVec3::from_array(orig_arr);
                if final_pos.distance(orig_pos) > 10_000.0 {
                    let _ = bp.set_mode(ship_id, MovementMode::new_warp(final_pos, orig_pos, warp_speed));
                    started_return_warp = true;
                }
            }
        }
    }
    if started_return_warp {
        ctx.return_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id);
    } else {
        ctx.return_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
    }

    Some((sys_id, ship_id, final_pos))
}

/// Reconnects a character who logged off or disconnected in space (`plan:SP-6`).
pub async fn reconnect_character_in_space(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<(u32, i64, DVec3)> {
    reconnect_character_in_space_sync(ctx, char_id)
}

/// Persists a started 120s self-destruct countdown so it detonates via server timer at `T+120s`
/// even if the client disconnects or never calls again (`feat:lifecycle.self-destruct-server-timer`).
pub fn register_self_destruct_countdown(
    ctx: &ServerContext,
    ship_id: i64,
    char_id: i64,
    solar_system_id: u32,
    started_unix: u64,
    detonate_at_unix: u64,
) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.self_destructs.insert(
            ship_id,
            SelfDestructPersistedEntry {
                ship_id,
                char_id,
                solar_system_id,
                started_unix,
                detonate_at_unix,
            },
        );
    });
}

pub fn abort_self_destruct_countdown(ctx: &ServerContext, ship_id: i64) -> bool {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.self_destructs.remove(&ship_id).is_some()
    })
}

/// Aborts any active self-destruct countdown for `ship_id` across both persisted SP6 state
/// and runtime combat state (#539).
pub fn abort_self_destruct_for_ship(ctx: &ServerContext, ship_id: i64) -> bool {
    let sp6_aborted = with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.self_destructs.remove(&ship_id).is_some()
    });
    let rt_aborted = crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns.remove(&ship_id).is_some()
    });
    sp6_aborted || rt_aborted
}

/// Persists an active Abyssal Deadspace pocket session (`20m` = `1200s` collapse timer)
/// so the server collapses the pocket and destroys the ship + pod even if the pilot disconnects
/// (`feat:lifecycle.abyssal-disconnect`).
pub fn register_abyssal_session(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    pocket_system_id: u32,
    origin_system_id: u32,
    entered_unix: u64,
    collapse_at_unix: u64,
) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.abyssal_sessions.insert(
            char_id,
            AbyssalPersistedSession {
                char_id,
                ship_id,
                pocket_system_id,
                origin_system_id,
                entered_unix,
                collapse_at_unix,
            },
        );
    });
}

pub fn clear_abyssal_session(ctx: &ServerContext, char_id: i64) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.abyssal_sessions.remove(&char_id);
    });
}

/// Abandons specified drones (or all drones) in space for the calling character (`rpc:entity.CmdAbandonDrone`).
pub async fn handle_cmd_abandon_drone(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let target_ids: Vec<i64> = match args.first().or_else(|| kwargs.get("droneIDs")) {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    };

    let mut abandoned_balls = Vec::new();
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if d.owner_char_id == char_id && !d.is_abandoned {
                let matches_filter = target_ids.is_empty()
                    || target_ids.contains(&d.ball_id)
                    || target_ids.contains(&d.drone_item_id);
                if matches_filter {
                    d.is_abandoned = true;
                    d.owner_char_id = 0;
                    d.parent_ship_id = 0;
                    d.target_id = None;
                    d.activity_state = 0;
                    abandoned_balls.push((d.ball_id, d.type_id));
                    rt.active_drone_miners.remove(&d.ball_id);
                }
            }
        }
    });

    if let Some(sys_id) = session.solar_system_id {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for &(bid, tid) in &abandoned_balls {
            let _ = sys_handle
                .emit_destiny_event(
                    bid,
                    eve_destiny::DestinyEvent::OnDroneStateChange {
                        drone_id: bid,
                        owner_id: 0,
                        controller_id: 0,
                        activity_state: 0,
                        type_id: tid,
                        controller_owner_id: 0,
                        target_id: None,
                    },
                )
                .await;
        }
    }

    for &(bid, _) in &abandoned_balls {
        let abandon_args = vec![PyRep::Integer(bid)];
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnDroneControlLost",
            "charid",
            abandon_args.clone(),
        );
        session.push_notification("OnDroneControlLost", abandon_args);
    }

    Ok(PyRep::Dict(vec![]))
}

/// Reconnects to abandoned drones within 250km and bandwidth/control count limits (`rpc:entity.CmdReconnectToDrones`, `feat:drones.reconnect-drones`).
pub async fn handle_cmd_reconnect_to_drones(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError { code: "NoActiveShip".into(), message: "Session has no active ship".into(), client_error: None });
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None });
    };
    if let Err(e) = crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id) {
        return Err(e);
    }

    let target_ids: Vec<i64> = match args.first().or_else(|| kwargs.get("droneIDs")) {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => list.iter().filter_map(|x| x.as_i64()).collect(),
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    };

    let mut ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let snap_type_id = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id)).unwrap_or(0);
    if snap_type_id > 0 && ship_fit_opt.as_ref().map(|f| f.ship.type_id) != Some(snap_type_id) {
        ctx.invalidate_ship_fitting(ship_id);
        ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    }
    let (max_drones_allowed, ship_bw, _) = crate::services::combat_crimewatch::combat_npcs_drones::pilot_drone_capacity(ctx, char_id, ship_id, ship_fit_opt.as_ref());
    let (mut cur_count, mut cur_bw) = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        let active: Vec<&crate::services::combat_crimewatch::DroneSpaceState> = rt
            .active_drones_in_space.values().filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned).collect();
        (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>())
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(glam::DVec3::ZERO);

    let candidates: Vec<(i64, i64, f64, u32)> = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| {
            d.solar_system_id == sys_id
                && ((d.is_abandoned && d.original_owner_char_id == char_id) || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && (target_ids.is_empty() || target_ids.contains(&d.ball_id) || target_ids.contains(&d.drone_item_id))
        }).map(|d| (d.ball_id, d.drone_item_id, d.bandwidth_used, d.type_id)).collect()
    });

    let mut reconnected = Vec::new();
    let mut errors: HashMap<String, Vec<i64>> = HashMap::new();

    for (bid, did, bw, tid) in candidates {
        let drone_pos = sys_handle
            .get_ball_snapshot(bid)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position);
        if let Some(pos) = drone_pos {
            if pos.distance(ship_pos) > 250_000.0 {
                errors.entry("OutOfRange".into()).or_default().push(bid);
                continue;
            }
        }
        if cur_count >= max_drones_allowed {
            errors.entry("MaxDronesExceeded".into()).or_default().push(bid);
            continue;
        }
        if cur_bw + bw > ship_bw {
            errors.entry("InsufficientBandwidth".into()).or_default().push(bid);
            continue;
        }

        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            if let Some(d) = rt.active_drones_in_space.get_mut(&bid) {
                d.is_abandoned = false;
                d.owner_char_id = char_id;
                d.parent_ship_id = ship_id;
                d.activity_state = 0;
                d.target_id = None;
                cur_count += 1;
                cur_bw += bw;
                reconnected.push((bid, did, tid));
            }
        });
    }

    drone_reconnect_finish::finish_reconnect_drones(ctx, &sys_handle, char_id, ship_id, &reconnected)
        .await;

    let errors_py: Vec<(PyRep, PyRep)> = errors
        .into_iter()
        .map(|(k, v)| (PyRep::String(k), PyRep::List(v.into_iter().map(PyRep::Integer).collect())))
        .collect();
    Ok(PyRep::Dict(errors_py))
}

#[cfg(test)] #[path = "sp6_lifecycle_unit_tests.rs"] mod sp6_lifecycle_unit_tests;
#[cfg(test)] mod drone_return_extra_unit_tests;
#[cfg(test)] mod cluster_tick_downtime_extra_unit_tests;
#[cfg(test)] #[path = "sp6_lifecycle_deep_extra_unit_tests.rs"] mod sp6_lifecycle_deep_extra_unit_tests;
