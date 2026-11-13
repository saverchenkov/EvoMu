//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

mod celestials;
use celestials::fast_forward_clock_in_actor_with_epochs;
pub use celestials::*;
mod belts;
pub use belts::*;
mod cycles;
pub(crate) use cycles::*;
mod cycles_tackle;
pub(crate) use cycles_tackle::*;
mod disruptors;
pub(crate) use disruptors::*;
mod types;
pub use types::*;

use belts::{
    build_tick_broadcast, ensure_default_slim_meta, handle_mine_asteroid,
    reconcile_spawned_belt_asteroids, respawn_due_asteroids,
};
use eve_db::lock_rank::{
    held_lock_ranks, AnyRwLock, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK,
    RANK_LEAF, RANK_SERVICE_STATE, RANK_SYSTEM_REGISTRY,
};
use eve_destiny::wire::{
    encode_destiny_updates, encode_set_state_payload, encode_set_state_update,
};
use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, SlimItemMeta};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

// parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-4, plan:SP-5
// parity: feat:navigation.celestials-balls, feat:navigation.bubbles-grid
// parity: feat:navigation.destiny-updates, feat:serverops.server-tick
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall, destiny:OnSlimItemChange

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

/// DERIVED default maintenance/downtime belt respawn period in seconds (86,400s = 1 day),
/// configurable via `ServerConfig::belt_respawn_interval_secs` (`D-5`, `MN-03`).
/// Per EVE wiki `Asteroids_and_ore.md:69`, belts respawn after downtime only if there are no asteroids left in the belt.
pub const DEFAULT_BELT_RESPAWN_INTERVAL_SECS: u64 = 86_400;

/// Default daily server downtime UTC time of day in seconds (11:00 UTC = 39,600s per EVE wiki `Evergreen_Daily_Login_Rewards.md:21`, `Skillbook_Program.md:43`),
/// configurable via `ServerConfig::downtime_utc_secs_of_day` (`D-5`, `MN-03`).
pub const DEFAULT_DOWNTIME_UTC_SECS_OF_DAY: u64 = 39_600;

/// Computes the strictly next downtime Unix timestamp (`> now_unix`) aligned to `downtime_utc_secs_of_day`.
pub fn next_downtime_unix_secs(now_unix: u64, interval_secs: u64, downtime_utc_secs_of_day: u64) -> u64 {
    let period = interval_secs.max(1);
    let phase = downtime_utc_secs_of_day % period;
    let rem = now_unix % period;
    let delta = if rem < phase { phase - rem } else { period - (rem - phase) };
    now_unix.saturating_add(delta)
}

pub struct SystemRegistry {
    pub(crate) systems: RankedRwLock<HashMap<SolarSystemId, SystemHandle>, RANK_SYSTEM_REGISTRY>,
    sde: Option<Arc<SdeRegistry>>,
    pub db: Option<Arc<eve_db::WorldRepository>>,
    belt_respawn_interval_secs: Arc<std::sync::atomic::AtomicU64>,
    downtime_utc_secs_of_day: Arc<std::sync::atomic::AtomicU64>,
    /// Game-mechanic RNG (seeded test RNG): OS CSPRNG by default, shared with `ServerContext::mechanic_rng`.
    mechanic_rng: Arc<crate::rng::MechanicRng>,
    pub(crate) npc_death_hook: Arc<RankedRwLock<Option<NpcDeathHook>, RANK_LEAF>>,
    pub(crate) module_deactivation_hook: Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    pub(crate) target_lock_completed_hook: Arc<RankedRwLock<Option<TargetLockCompletedHook>, RANK_LEAF>>,
    pub(crate) system_step_hook: Arc<RankedRwLock<Option<SystemStepHook>, RANK_LEAF>>,
    /// PvE special-site balls (system, ball id) already spawned once into this registry's ballparks.
    /// Runtime-only (lives and dies with the ballparks), so no sentinel ball is needed in space.
    special_site_spawned_balls: RankedMutex<std::collections::HashSet<(SolarSystemId, i64)>, RANK_LEAF>,
    /// Wormhole ore anomalies (system, anomaly_id) whose initial Sleeper guard wave has been hydrated
    /// in this registry's ballparks, preventing player scan RPCs from respawning killed guards.
    hydrated_wh_ore_anomalies: RankedMutex<std::collections::HashSet<(SolarSystemId, String)>, RANK_LEAF>,
    pub tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    pending_notifications_opt: Option<Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>>,
    notify_tx_opt: Option<broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    pub cluster_nodes: RankedRwLock<ClusterNodeRegistry, RANK_SERVICE_STATE>,
}

impl Default for SystemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRegistry {
    pub fn new() -> Self {
        Self {
            systems: RankedRwLock::new(HashMap::new()),
            sde: None,
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_BELT_RESPAWN_INTERVAL_SECS)),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY)),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
            npc_death_hook: Arc::new(RankedRwLock::new(None)),
            module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
            target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
            system_step_hook: Arc::new(RankedRwLock::new(None)),
            special_site_spawned_balls: RankedMutex::new(std::collections::HashSet::new()),
            hydrated_wh_ore_anomalies: RankedMutex::new(std::collections::HashSet::new()),
            tactical_state: Arc::new(RankedRwLock::new(Default::default())),
            pending_notifications_opt: None,
            notify_tx_opt: None,
            cluster_nodes: RankedRwLock::new(ClusterNodeRegistry::default()),
        }
    }

    pub fn with_sde(sde: Arc<SdeRegistry>) -> Self {
        Self { sde: Some(sde), ..Self::new() }
    }

    pub fn with_sde_and_db(sde: Arc<SdeRegistry>, db: Arc<eve_db::WorldRepository>) -> Self {
        Self { sde: Some(sde), db: Some(db), ..Self::new() }
    }

    pub fn with_notifications(
        mut self,
        pending_notifications: Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>,
        notify_tx: broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    ) -> Self {
        self.pending_notifications_opt = Some(pending_notifications);
        self.notify_tx_opt = Some(notify_tx);
        self
    }

    pub fn with_tactical_state(
        mut self,
        tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    ) -> Self {
        self.tactical_state = tactical_state;
        self
    }

    pub fn set_npc_death_hook(&self, hook: NpcDeathHook) {
        *self.npc_death_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Invokes the registered `npc_death_hook` for an NPC ball destroyed outside
    /// standard damage ticks (e.g., GM `/kill <npc_ball_id>`, `#1003`).
    pub async fn invoke_npc_death_hook(
        &self,
        solar_system_id: u32,
        npc_ball_id: i64,
        killer_char_id: Option<i64>,
    ) {
        let hook = self
            .npc_death_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(h) = hook {
            h(solar_system_id, npc_ball_id, killer_char_id).await;
        }
    }

    pub fn set_module_deactivation_hook(&self, hook: ModuleDeactivationHook) {
        *self.module_deactivation_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_target_lock_completed_hook(&self, hook: TargetLockCompletedHook) {
        *self.target_lock_completed_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_system_step_hook(&self, hook: SystemStepHook) {
        *self.system_step_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Shares `rng` as this registry's game-mechanic RNG (seeded test RNG).
    pub fn with_mechanic_rng(mut self, rng: Arc<crate::rng::MechanicRng>) -> Self {
        self.mechanic_rng = rng;
        self
    }

    pub fn belt_respawn_interval_secs(&self) -> u64 {
        self.belt_respawn_interval_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_belt_respawn_interval_secs(&self, secs: u64) {
        self.belt_respawn_interval_secs
            .store(secs.clamp(1, 604_800), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn downtime_utc_secs_of_day(&self) -> u64 {
        self.downtime_utc_secs_of_day
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_downtime_utc_secs_of_day(&self, secs: u64) {
        self.downtime_utc_secs_of_day
            .store(secs % 86_400, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SystemRegistry {
    pub fn get_ball_position_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> Option<DVec3> {
        self.get_handle(solar_system_id)
            .and_then(|h| h.get_ball_position_sync(ball_id))
    }

    pub fn is_ball_scrambled_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.is_ball_scrambled_sync(ball_id))
    }

    pub fn has_active_modules_or_cloak_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.has_active_modules_or_cloak_sync(ball_id))
    }

    pub fn find_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        for (_, handle) in self.active_system_handles() {
            if let Some(pos) = handle.get_ball_position_sync(ball_id) {
                return Some(pos);
            }
        }
        None
    }

    pub fn find_ball_system_sync(&self, ball_id: BallId) -> Option<SolarSystemId> {
        for (sys_id, handle) in self.active_system_handles() {
            if handle.get_ball_position_sync(ball_id).is_some() {
                return Some(sys_id);
            }
        }
        None
    }

    pub fn get_system_sim_elapsed(&self, solar_system_id: SolarSystemId) -> f64 {
        self.get_handle(solar_system_id)
            .map(|h| h.sim_elapsed_secs())
            .unwrap_or(0.0)
    }

    pub fn take_pending_cycle_actions(&self, solar_system_id: SolarSystemId) -> Vec<PendingCycleAction> {
        self.get_handle(solar_system_id)
            .map(|h| h.take_pending_cycle_actions())
            .unwrap_or_default()
    }

    pub fn active_systems_count(&self) -> usize {
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            self.systems.read_unranked().unwrap_or_else(|e| e.into_inner()).len()
        } else {
            self.systems.read().unwrap_or_else(|e| e.into_inner()).len()
        }
    }

    pub fn get_handle(&self, solar_system_id: SolarSystemId) -> Option<SystemHandle> {
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
            guard
                .get(&solar_system_id)
                .filter(|h| !h.cmd_tx.is_closed())
                .cloned()
        } else {
            let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
            guard
                .get(&solar_system_id)
                .filter(|h| !h.cmd_tx.is_closed())
                .cloned()
        }
    }

    pub fn active_system_handles(&self) -> Vec<(SolarSystemId, SystemHandle)> {
        let mut handles: Vec<(SolarSystemId, SystemHandle)> =
            if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
                self.systems
                    .read_unranked()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|(&id, h)| (id, h.clone()))
                    .collect()
            } else {
                self.systems
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|(&id, h)| (id, h.clone()))
                    .collect()
            };
        handles.sort_by_key(|(id, _)| *id);
        handles
    }

    pub fn cancel_nanite_repairs_for_ship_sync(&self, ship_id: BallId) -> Vec<ActiveNaniteRepair> {
        self.active_system_handles()
            .into_iter()
            .flat_map(|(_, h)| h.cancel_nanite_repairs_for_ship_sync(ship_id))
            .collect()
    }

    /// Records that PvE special-site ball `ball_id` has been spawned in `solar_system_id`.
    /// Returns `true` only the first time, so destroyed site NPCs are never re-spawned.
    pub fn claim_special_site_ball(&self, solar_system_id: SolarSystemId, ball_id: i64) -> bool {
        self.special_site_spawned_balls.lock().unwrap_or_else(|e| e.into_inner()).insert((solar_system_id, ball_id))
    }

    /// Marks wormhole ore `anomaly_id` in `solar_system_id` as hydrated for this ballpark registry.
    /// Returns `true` only on first hydration so player scan RPCs never respawn killed Sleeper guards.
    pub fn claim_wh_ore_anomaly_hydration(&self, solar_system_id: SolarSystemId, anomaly_id: &str) -> bool {
        self.hydrated_wh_ore_anomalies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((solar_system_id, anomaly_id.to_string()))
    }

    /// Synchronously flushes dirty condition checkpoints across all active systems to `self.db`
    /// (`RFC-1 E-4`, `T-2`, `T-5`, `#919`).
    pub fn flush_all_condition_checkpoints_to_db(&self) -> usize {
        let Some(ref db) = self.db else {
            return 0;
        };
        if db.is_write_locked_by_current_thread() {
            return 0;
        }
        let mut total = 0usize;
        for (sys_id, handle) in self.active_system_handles() {
            let entries = handle.collect_dirty_condition_checkpoint_sync();
            if !entries.is_empty() {
                total += crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
                    db, sys_id, &entries,
                );
            }
        }
        total
    }

    /// Evicts hibernated systems that have been idle for at least `min_idle_secs` after flushing
    /// a final condition checkpoint (`RFC-1 E-4`, `#919`, `#866`).
    pub async fn evict_idle_systems(&self, min_idle_secs: u64) -> Vec<SolarSystemId> {
        self.evict_idle_systems_with_min_ticks(min_idle_secs, 0).await
    }

    /// Evicts hibernated systems that have reached both `min_idle_secs` and `min_idle_ticks`,
    /// provided no external [`SystemHandle`] clones are still held outside the registry/actor.
    pub async fn evict_idle_systems_with_min_ticks(
        &self,
        min_idle_secs: u64,
        min_idle_ticks: u64,
    ) -> Vec<SolarSystemId> {
        let now_unix = self
            .db
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(1);
        let can_evict_handle = |h: &SystemHandle, max_strong: usize| -> bool {
            if !h.is_hibernated() || Arc::strong_count(&h.ballpark) > max_strong {
                return false;
            }
            if min_idle_ticks > 0
                && h.idle_ticks.load(std::sync::atomic::Ordering::Relaxed) < min_idle_ticks
            {
                return false;
            }
            if min_idle_secs == 0 {
                return true;
            }
            let since = h.hibernated_since_unix();
            since > 0 && now_unix.saturating_sub(since) >= min_idle_secs
        };
        let candidates: Vec<(SolarSystemId, SystemHandle)> = self
            .active_system_handles()
            .into_iter()
            // Refcounts during filter: `self.systems` (1) + actor task (1) + vec element (1) = 3.
            .filter(|(_, h)| can_evict_handle(h, 3))
            .collect();
        let mut evicted = Vec::new();
        for (sys_id, handle) in candidates {
            let _ = handle.flush_condition_checkpoint().await;
            if !handle.is_hibernated() {
                continue;
            }
            let removed = {
                let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
                // Loop holds `handle` (1), `guard` holds (1), actor holds (1) = 3.
                if guard.get(&sys_id).is_some_and(|h| can_evict_handle(h, 3)) {
                    guard.remove(&sys_id).is_some()
                } else {
                    false
                }
            };
            if removed {
                evicted.push(sys_id);
            }
        }
        evicted
    }

    /// Retrieves a running Solar System Actor handle, or spawns one only when a Tokio runtime
    /// reactor is currently active (`None` on headless non-Tokio teardown/unit threads).
    pub fn get_or_spawn_if_runtime(&self, solar_system_id: SolarSystemId) -> Option<SystemHandle> {
        self.get_handle(solar_system_id).or_else(|| {
            tokio::runtime::Handle::try_current()
                .ok()
                .map(|_| self.get_or_spawn(solar_system_id))
        })
    }

    /// Spawns or retrieves a running Solar System Actor for `solar_system_id`.
    /// Populates static SDE celestials, stargates, and stations (`SP-1`), re-materializes persisted
    /// space objects (`SP-7`), and hibernates (`LOD 0`) with zero timer wakes until dynamic balls exist.
    pub fn get_or_spawn(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        if let Some(existing) = self.get_handle(solar_system_id) {
            return existing;
        }

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SystemCommand>(SYSTEM_MAILBOX_CAPACITY);
        let (event_tx, _) = broadcast::channel::<TickBroadcast>(4096);
        let event_tx_actor = event_tx.clone();
        let sde_opt = self.sde.clone();
        let db_opt = self.db.clone();
        let belt_interval_atomic = self.belt_respawn_interval_secs.clone();
        let downtime_utc_atomic = self.downtime_utc_secs_of_day.clone();
        let mechanic_rng = self.mechanic_rng.clone();
        let module_deactivation_hook = self.module_deactivation_hook.clone();
        let module_deactivation_hook_actor = module_deactivation_hook.clone();
        let target_lock_completed_hook = self.target_lock_completed_hook.clone();
        let target_lock_completed_hook_actor = target_lock_completed_hook.clone();
        let mut initial_ballpark = Ballpark::new(solar_system_id);
        if let Some(sde) = &sde_opt {
            populate_system_celestials(&mut initial_ballpark, solar_system_id, sde);
        }
        let db_read_safe = db_opt.as_ref().is_some_and(|db| {
            !db.is_write_locked_by_current_thread()
                && !held_lock_ranks().iter().any(|&r| r >= eve_db::lock_rank::RANK_WORLD_INNER)
        });
        if db_read_safe {
            crate::services::cluster_macho_misc_services::populate_shipcaster_balls_from_db(
                solar_system_id,
                &mut initial_ballpark,
                db_opt.as_ref(),
            );
        }
        let mut initial_asteroids = HashMap::new();
        let mut initial_asteroid_templates = HashMap::new();
        let initial_pending_disruptors = if let (Some(sde), Some(db)) = (&sde_opt, &db_opt) {
            if db_read_safe {
                let first_sde_belt_ast_id =
                    crate::system_actor::belts::belt_asteroid_base_id(solar_system_id);
                let has_persisted_belt_asteroids =
                    db.with_snapshot(|s| s.asteroids.contains_key(&first_sde_belt_ast_id));
                if has_persisted_belt_asteroids {
                    let spawned = populate_asteroid_belt_entities(&mut initial_ballpark, solar_system_id, sde);
                    let _ = reconcile_spawned_belt_asteroids(
                        &mut initial_ballpark,
                        solar_system_id,
                        spawned,
                        &mut initial_asteroids,
                        &mut initial_asteroid_templates,
                        &db_opt,
                        belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                        downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
                let pending = crate::sp6_lifecycle::populate_persisted_space_objects(
                    &mut initial_ballpark,
                    solar_system_id,
                    sde,
                    db,
                );
                crate::services::cluster_macho_misc_services::populate_shipcaster_balls_with_sde(
                    solar_system_id,
                    &mut initial_ballpark,
                    Some(sde),
                    Some(db),
                );
                pending
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let ball_positions: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(HashMap::new()));
        let scrambled_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(std::collections::HashSet::new()));
        let committed_warp_balls: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(HashMap::new()));
        let active_module_ships: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(std::collections::HashSet::new()));
        register_committed_warp_cache(&ball_positions, &committed_warp_balls);
        sync_ball_positions(&initial_ballpark, &ball_positions, &scrambled_balls);
        let ball_positions_actor = ball_positions.clone();
        let scrambled_balls_actor = scrambled_balls.clone();
        let active_module_ships_actor = active_module_ships.clone();
        let hibernated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hibernated_since_unix = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let idle_ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let dynamic_ball_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut initial_actor_state = SystemActorState {
            asteroids: initial_asteroids,
            asteroid_templates: initial_asteroid_templates,
            pending_anchoring_disruptors: initial_pending_disruptors,
            sde_opt,
            db_opt,
            mechanic_rng_opt: Some(mechanic_rng.clone()),
            tactical_state: self.tactical_state.clone(),
            pending_notifications_opt: self.pending_notifications_opt.clone(),
            notify_tx_opt: self.notify_tx_opt.clone(),
            hibernated: Arc::clone(&hibernated),
            hibernated_since_unix: Arc::clone(&hibernated_since_unix),
            idle_ticks: Arc::clone(&idle_ticks),
            dynamic_ball_count: Arc::clone(&dynamic_ball_count),
            ..Default::default()
        };
        if db_read_safe {
            let dyn_ids: Vec<BallId> = initial_ballpark
                .balls
                .keys()
                .copied()
                .filter(|&bid| !initial_ballpark.is_static_ball(bid))
                .collect();
            for bid in dyn_ids {
                celestials::record_spawned_ball_checkpoint_baseline(
                    &mut initial_actor_state,
                    &initial_ballpark,
                    bid,
                );
            }
        }
        celestials::update_system_hibernation_flag(&initial_actor_state, &initial_ballpark, 0);
        let ballpark_shared: Arc<RankedRwLock<Ballpark, RANK_BALLPARK>> =
            Arc::new(RankedRwLock::new(initial_ballpark));
        let ballpark_actor = ballpark_shared.clone();
        let actor_state_shared: Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>> =
            Arc::new(RankedRwLock::new(initial_actor_state));
        let actor_state_actor = actor_state_shared.clone();
        let last_auto_stepped_unix = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let initial_tidi = self.cluster_nodes.read().ok()
            .and_then(|cn| cn.manual_tidi_overrides.get(&solar_system_id).copied())
            .or_else(|| self.db.as_ref().and_then(|db| db.with_snapshot(|snap| {
                let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
                snap.owner_notes.get(&nid).and_then(|n| serde_json::from_str::<f64>(&n.note).ok())
            })))
            .unwrap_or(1.0).clamp(0.10, 1.0);
        let tidi_factor_bits = Arc::new(std::sync::atomic::AtomicU64::new(initial_tidi.to_bits()));
        let tidi_factor_bits_actor = tidi_factor_bits.clone();
        let command_panics = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let command_panics_actor = command_panics.clone();
        #[cfg(test)]
        let panic_next_command_for_test = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let panic_next_command_actor = panic_next_command_for_test.clone();

        tokio::spawn(async move {
            let mut last_active_totals: HashMap<BallId, (Option<i32>, Option<i32>)> = HashMap::new();
            // parity: feat:lifecycle.world-unobserved-system-sim, plan:SP-7
            while let Some(cmd) = cmd_rx.recv().await {
                let cmd_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                if panic_next_command_actor.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    panic!("injected SystemActor command panic for #841 supervision test");
                }
                let mut ballpark = ballpark_actor.write().unwrap_or_else(|e| e.into_inner());
                let mut actor_st = actor_state_actor.write().unwrap_or_else(|e| e.into_inner());
                let SystemActorState {
                    active_module_cycles,
                    pending_anchoring_disruptors,
                    active_nanite_repairs,
                    ship_rack_heat,
                    rah_incoming_damage,
                    base_scramble_strengths,
                    base_core_strengths,
                    asteroids,
                    asteroid_templates,
                    sde_opt,
                    db_opt,
                    mechanic_rng_opt,
                    sim_elapsed_secs,
                    pending_cycle_actions,
                    tactical_state,
                    pending_notifications_opt,
                    notify_tx_opt,
                    ball_space_entry_epochs,
                    fit_projections,
                    weapon_cycle_start_modules,
                    ..
                } = &mut *actor_st;
                {
                    {
                        enum StepReply {
                            Full(tokio::sync::oneshot::Sender<StepOutput>),
                            TickOnly(tokio::sync::oneshot::Sender<StepOutput>),
                        }
                        let mut pending_step: Option<(f64, StepReply)> = None;
                        let mut spawned_checkpoint_ball: Option<BallId> = None;
                        let mut removed_checkpoint_ball: Option<BallId> = None;
                        let mut flush_checkpoint_reply: Option<
                            tokio::sync::oneshot::Sender<Vec<ConditionCheckpointEntry>>,
                        > = None;
                        match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                let existing_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                if let Some(meta) = existing_meta {
                                    ballpark.set_ball_slim_meta(ball_id, meta);
                                } else {
                                    ensure_default_slim_meta(&mut ballpark, &ball, sde_opt);
                                }
                                if should_pin_stationary_ball(&ball, ballpark.ball_slim_meta.get(&ball_id)) {
                                    ballpark.mark_ball_non_free(ball_id, false);
                                }
                                ballpark.add_ball(ball);
                                spawned_checkpoint_ball = Some(ball_id);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SetBallMaxVelocity { ball_id, max_velocity, reply } => {
                                let _ = reply.send(ballpark.set_ball_max_velocity(ball_id, max_velocity));
                            }
                            SystemCommand::SetBallMaxVelocityCap { ball_id, cap, reply } => {
                                let _ = reply.send(ballpark.set_ball_max_velocity_cap(ball_id, cap));
                            }
                            SystemCommand::SpawnBallWithCombat {
                                ball,
                                combat_state,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let tick = cycles_tackle::spawn_ball_with_combat_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    sde_opt,
                                    db_opt,
                                    deact_guard.as_ref(),
                                    ball,
                                    combat_state,
                                    None,
                                );
                                spawned_checkpoint_ball = Some(ball_id);
                                cycles_tackle::snapshot_active_tackle_totals(&ballpark, active_module_cycles, &mut last_active_totals);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(tick));
                            }
                            SystemCommand::SpawnBallWithMeta {
                                ball,
                                combat_state,
                                slim_meta,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let tick = cycles_tackle::spawn_ball_with_combat_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    sde_opt,
                                    db_opt,
                                    deact_guard.as_ref(),
                                    ball,
                                    combat_state,
                                    Some(slim_meta),
                                );
                                spawned_checkpoint_ball = Some(ball_id);
                                cycles_tackle::snapshot_active_tackle_totals(&ballpark, active_module_cycles, &mut last_active_totals);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(tick));
                            }
                            SystemCommand::ChangeSlimItem {
                                ball_id,
                                slim_meta,
                                reply,
                            } => {
                                let changed = ballpark.change_slim_item(ball_id, slim_meta);
                                if changed {
                                    spawned_checkpoint_ball = Some(ball_id);
                                }
                                let _ = reply.send(changed);
                            }
                            SystemCommand::SetBallWormholeStages {
                                ball_id,
                                wormhole_age,
                                wormhole_size,
                                reply,
                            } => {
                                let changed = ballpark.set_ball_wormhole_stages(
                                    ball_id,
                                    wormhole_age,
                                    wormhole_size,
                                );
                                let _ = reply.send(changed);
                            }
                            SystemCommand::GetBallSlimItem { ball_id, reply } => {
                                let slim = ballpark.balls.get(&ball_id).map(|b| {
                                    let meta = eve_destiny::wire::resolve_slim_item_meta(
                                        Some(&ballpark),
                                        b,
                                    );
                                    eve_destiny::wire::encode_slim_item_with_park(
                                        Some(&ballpark),
                                        b,
                                        &meta,
                                    )
                                });
                                let _ = reply.send(slim);
                            }
                            SystemCommand::SpawnAsteroid { asteroid, ball, reply } => {
                                let ball_id = asteroid.asteroid_id;
                                if asteroids.contains_key(&ball_id) {
                                    let _ = reply.send(Ok(ball_id));
                                    return;
                                }
                                let mut ast_ball = ball;
                                ast_ball.mode = eve_destiny::MovementMode::Rigid;
                                let (group_id, category_id, name) = sde_opt
                                    .as_ref()
                                    .and_then(|s| s.types.get(&ast_ball.type_id))
                                    .map(|t| {
                                        (
                                            t.group_id,
                                            t.category_id,
                                            (!t.name.is_empty()).then(|| t.name.clone()),
                                        )
                                    })
                                    .unwrap_or((25, 25, None));
                                ballpark.add_static_ball_with_meta(
                                    ast_ball.clone(),
                                    SlimItemMeta {
                                        group_id,
                                        category_id,
                                        owner_id: 1,
                                        char_id: None,
                                        corp_id: -1,
                                        alliance_id: -1,
                                        security_status: 0.0,
                                        is_cloaked: false,
                                        name,
                                        ..Default::default()
                                    },
                                );
                                ballpark.queue_event_for_all_grid_observers(
                                    Some(ball_id),
                                    &DestinyEvent::AddBalls2 {
                                        balls: vec![ast_ball.clone()],
                                    },
                                );
                                if let Some(db) = &db_opt {
                                    if db.get_asteroid(ball_id).is_none() {
                                        let _ = db.spawn_asteroid(eve_db::AsteroidRecord {
                                            asteroid_id: ball_id,
                                            belt_id: asteroid.belt_id,
                                            type_id: ast_ball.type_id,
                                            solar_system_id,
                                            position: (ast_ball.position.x, ast_ball.position.y, ast_ball.position.z),
                                            quantity: i64::try_from(asteroid.remaining_units).unwrap_or(i64::MAX),
                                            initial_quantity: i64::try_from(asteroid.initial_units).unwrap_or(i64::MAX),
                                            radius: ast_ball.radius,
                                        });
                                    }
                                }
                                asteroid_templates.insert(ball_id, (asteroid.clone(), ast_ball));
                                asteroids.insert(ball_id, asteroid);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ball_id));
                            }
                            SystemCommand::MineAsteroid {
                                asteroid_id,
                                requested_units,
                                waste_probability,
                                persist_db,
                                reply,
                            } => {
                                let res = handle_mine_asteroid(
                                    solar_system_id,
                                    asteroid_id,
                                    requested_units,
                                    waste_probability,
                                    persist_db,
                                    &mut ballpark,
                                    asteroids,
                                    &mechanic_rng,
                                    &db_opt,
                                    &belt_interval_atomic,
                                    &downtime_utc_atomic,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetAsteroidState { asteroid_id, reply } => {
                                let _ = reply.send(asteroids.get(&asteroid_id).cloned());
                            }
                            SystemCommand::BreakGateCloak { ball_id, reply } => {
                                let broken = ballpark.deactivate_cloak(ball_id);
                                if let Some(mut meta) = ballpark.ball_slim_meta.get(&ball_id).cloned() {
                                    meta.is_cloaked = false;
                                    ballpark.change_slim_item(ball_id, meta);
                                }
                                let _ = reply.send(broken);
                            }
                            SystemCommand::PopulateAsteroids { reply } => {
                                let first_sde_belt_ast_id =
                                    crate::system_actor::belts::belt_asteroid_base_id(solar_system_id);
                                let mut populated = if !asteroid_templates.contains_key(&first_sde_belt_ast_id) {
                                    if let Some(sde) = &sde_opt {
                                        let spawned = populate_asteroid_belt_entities(&mut ballpark, solar_system_id, sde);
                                        let reconciled = reconcile_spawned_belt_asteroids(
                                            &mut ballpark,
                                            solar_system_id,
                                            spawned,
                                            asteroids,
                                            asteroid_templates,
                                            &db_opt,
                                            belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                            downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                        );
                                        let newly_added_balls: Vec<Ball> = reconciled
                                            .iter()
                                            .filter(|a| !a.depleted)
                                            .filter_map(|a| ballpark.balls.get(&a.asteroid_id).cloned())
                                            .collect();
                                        if !newly_added_balls.is_empty() {
                                            ballpark.queue_event_for_all_grid_observers(
                                                None,
                                                &DestinyEvent::AddBalls2 {
                                                    balls: newly_added_balls,
                                                },
                                            );
                                        }
                                        reconciled
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    Vec::new()
                                };
                                for ast in asteroids.values() {
                                    if !populated.iter().any(|p| p.asteroid_id == ast.asteroid_id) {
                                        populated.push(ast.clone());
                                    }
                                }
                                populated.sort_by_key(|a| a.asteroid_id);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(populated);
                            }
                            SystemCommand::RemoveBall { ball_id, reply } => {
                                crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                    &mut ballpark,
                                    &db_opt,
                                    tactical_state.as_ref(),
                                    ball_id,
                                );
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_all_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ball_id,
                                    deact_guard.as_ref(),
                                );
                                base_scramble_strengths.remove(&ball_id);
                                base_core_strengths.remove(&ball_id);
                                asteroids.remove(&ball_id);
                                let docking_evs: Vec<DestinyEvent> = ballpark
                                    .observer_events
                                    .get(&ball_id)
                                    .map(|evs| {
                                        evs.iter()
                                            .filter(|e| matches!(e, DestinyEvent::OnDockingAccepted { .. }))
                                            .cloned()
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let removed = ballpark.remove_ball(ball_id);
                                fit_projections.remove(&ball_id);
                                removed_checkpoint_ball = Some(ball_id);
                                if !docking_evs.is_empty() {
                                    ballpark.observer_events.insert(ball_id, docking_evs);
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::AcceptDocking {
                                ship_id,
                                station_id,
                                station_pos,
                                reply,
                            } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_all_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ship_id,
                                    deact_guard.as_ref(),
                                );
                                base_scramble_strengths.remove(&ship_id);
                                base_core_strengths.remove(&ship_id);
                                asteroids.remove(&ship_id);
                                fit_projections.remove(&ship_id);
                                removed_checkpoint_ball = Some(ship_id);
                                let removed = if ballpark.balls.contains_key(&station_id) {
                                    ballpark.accept_docking(ship_id, station_id)
                                } else {
                                    ballpark.accept_docking_at(ship_id, station_id, station_pos)
                                };
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::TakeBall {
                                ball_id,
                                accept_docking_target,
                                reply,
                            } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                    let _ = reply.send(None);
                                    return;
                                }
                                let tick = ballpark.current_tick;
                                let before_cycles = active_module_cycles.len();
                                crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                    &mut ballpark,
                                    &db_opt,
                                    tactical_state.as_ref(),
                                    ball_id,
                                );
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_all_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ball_id,
                                    deact_guard.as_ref(),
                                );
                                let cycles_cleared = before_cycles.saturating_sub(active_module_cycles.len());
                                let lock_count = ballpark.clear_target_locks(ball_id);
                                if lock_count > 0 {
                                    on_target_unlocked_in_actor(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        ball_id,
                                        None,
                                        deact_guard.as_ref(),
                                    );
                                }
                                base_scramble_strengths.remove(&ball_id);
                                base_core_strengths.remove(&ball_id);
                                asteroids.remove(&ball_id);
                                fit_projections.remove(&ball_id);
                                removed_checkpoint_ball = Some(ball_id);
                                let combat_state = ballpark.combat_states.get(&ball_id).cloned();
                                let slim_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
                                let removed = if let Some((station_id, station_pos)) = accept_docking_target {
                                    if ballpark.balls.contains_key(&station_id) {
                                        ballpark.accept_docking(ball_id, station_id)
                                    } else {
                                        ballpark.accept_docking_at(ball_id, station_id, station_pos)
                                    }
                                } else {
                                    let docking_evs: Vec<DestinyEvent> = ballpark
                                        .observer_events
                                        .get(&ball_id)
                                        .map(|evs| {
                                            evs.iter()
                                                .filter(|e| matches!(e, DestinyEvent::OnDockingAccepted { .. }))
                                                .cloned()
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    let r = ballpark.remove_ball(ball_id);
                                    if !docking_evs.is_empty() {
                                        ballpark.observer_events.insert(ball_id, docking_evs);
                                    }
                                    r
                                };
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(removed.map(|ball| TakenBall {
                                    tick,
                                    ball,
                                    combat_state,
                                    slim_meta,
                                    cycles_cleared,
                                }));
                            }
                            SystemCommand::RestoreBall { taken, reply } => {
                                let ball_id = taken.ball.id;
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let tick = cycles_tackle::spawn_ball_with_combat_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    &sde_opt,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                    taken.ball,
                                    taken.combat_state.unwrap_or_default(),
                                    taken.slim_meta,
                                );
                                spawned_checkpoint_ball = Some(ball_id);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(tick));
                            }
                            SystemCommand::EmitPackagedActions {
                                ball_id,
                                events,
                                reply,
                            } => {
                                let known = ballpark.balls.contains_key(&ball_id);
                                if known {
                                    ballpark.emit_packaged_actions(Some(ball_id), &events);
                                }
                                let _ = reply.send(known);
                            }
                            SystemCommand::SetMovementMode { ball_id, mode, reply } => {
                                if ballpark.balls.contains_key(&ball_id) {
                                    let is_stop = matches!(mode, eve_destiny::MovementMode::Stop);
                                    match ballpark.set_mode(ball_id, mode) {
                                        Ok(()) => {
                                            if is_stop && active_module_cycles.iter().any(|c| c.ship_id == ball_id) {
                                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                                reconcile_tackle_and_bubbles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths, &db_opt, deact_guard.as_ref());
                                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                            }
                                            persist_ball_positions_in_db(
                                                &ballpark,
                                                &[ball_id],
                                                &db_opt,
                                                ball_space_entry_epochs,
                                            );
                                            let _ = reply.send(Ok(ballpark.current_tick));
                                        }
                                        Err(e) => {
                                            let _ = reply.send(Err(e));
                                        }
                                    }
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetWarpScrambleStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_scramble_strength(ball_id, strength) {
                                    base_scramble_strengths.insert(ball_id, strength);
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        deact_guard.as_ref(),
                                    );
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::SetWarpCoreStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_core_strength(ball_id, strength) {
                                    base_core_strengths.insert(ball_id, strength);
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        deact_guard.as_ref(),
                                    );
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::AddWarpBubble { id, position, radius, reply } => {
                                ballpark.add_warp_bubble(id, position, radius);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::RemoveWarpBubble { id, reply } => {
                                let removed = ballpark.remove_warp_bubble(id).is_some();
                                let _ = reply.send(removed);
                            }
                            SystemCommand::SetSpeedFraction {
                                ball_id,
                                fraction,
                                reply,
                            } => {
                                if ballpark.set_speed_fraction(ball_id, fraction) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetBallPosition {
                                ball_id,
                                position,
                                reply,
                            } => {
                                let updated = ballpark.set_ball_position(ball_id, position);
                                if updated {
                                    persist_ball_positions_in_db(
                                        &ballpark,
                                        &[ball_id],
                                        &db_opt,
                                        ball_space_entry_epochs,
                                    );
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(updated);
                            }
                            SystemCommand::EmitDestinyEvent { ball_id, event, reply } => {
                                let known = cycles_tackle::apply_destiny_event_in_actor(
                                    &mut ballpark,
                                    ball_id,
                                    event,
                                );
                                let _ = reply.send(known);
                            }
                            SystemCommand::EmitObserverDestinyEvent {
                                observer_ball_id,
                                event,
                                reply,
                            } => {
                                let ok = ballpark.balls.contains_key(&observer_ball_id);
                                if ok {
                                    ballpark
                                        .observer_events
                                        .entry(observer_ball_id)
                                        .or_default()
                                        .push(event);
                                }
                                let _ = reply.send(ok);
                            }
                            SystemCommand::GotoPoint { ball_id, point, reply } => {
                                let res = ballpark
                                    .goto_point(ball_id, point)
                                    .map(|()| ballpark.current_tick);
                                if res.is_ok() {
                                    persist_ball_positions_in_db(
                                        &ballpark,
                                        &[ball_id],
                                        &db_opt,
                                        ball_space_entry_epochs,
                                    );
                                }
                                let _ = reply.send(res);
                            }
                            SystemCommand::PushSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.queue_set_state(observer_ball_id) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBallSnapshot { ball_id, reply } => {
                                let res = ballpark.balls.get(&ball_id).cloned().map(|b| (ballpark.current_tick, b));
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetBallGroupId { ball_id, reply } => {
                                let _ = reply.send(ballpark.ball_group_id(ball_id));
                            }
                            SystemCommand::ListBalls { reply } => {
                                let balls: Vec<Ball> = ballpark.balls.values().cloned().collect();
                                let _ = reply.send(balls);
                            }
                            SystemCommand::ListBallsInBubble {
                                observer_ball_id,
                                dynamic_only,
                                reply,
                            } => {
                                let balls: Vec<Ball> = if dynamic_only {
                                    ballpark
                                        .dynamic_balls_in_bubble(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                } else {
                                    ballpark
                                        .visible_balls_for_observer(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                };
                                let _ = reply.send(balls);
                            }
                            SystemCommand::GetSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.balls.contains_key(&observer_ball_id) {
                                    let payload = encode_set_state_payload(&ballpark, observer_ball_id);
                                    let update_list = PyRep::List(vec![encode_set_state_update(
                                        &ballpark,
                                        observer_ball_id,
                                    )]);
                                    let _ = reply.send(Ok((ballpark.current_tick, payload, update_list)));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBubbleIdForBall { ball_id, reply } => {
                                let bid = ballpark.bubble_id_for_ball(ball_id);
                                let _ = reply.send(bid);
                            }
                            SystemCommand::DrainObserverUpdates {
                                observer_ball_id,
                                reply,
                            } => {
                                sync_player_slim_items(&mut ballpark, &db_opt);
                                let events = ballpark.drain_observer_events(observer_ball_id);
                                if events
                                    .iter()
                                    .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
                                {
                                    ballpark.current_tick = ballpark.current_tick.saturating_add(1);
                                }
                                let stamp = ballpark.current_tick as u32;
                                let pyrep = encode_destiny_updates(stamp, &events, Some(&ballpark));
                                let _ = reply.send((ballpark.current_tick, events, pyrep));
                            }
                            SystemCommand::ApplyDamage { ball_id, damage, reply } => {
                                let is_invuln_stronghold = db_opt
                                    .as_ref()
                                    .map(|db| crate::services::insurgency_services::is_stronghold_invulnerable_in_db(db, solar_system_id, ball_id))
                                    .unwrap_or(false);
                                let now_unix = db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or_else(|| {
                                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                                });
                                if is_invuln_stronghold {
                                    if let Some(b) = ballpark.balls.get(&ball_id).cloned() {
                                        let _ = reply.send(Some((0.0, false, b)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else if let Some((applied, destroyed, ball_opt)) = crate::services::mobile_deployables::handle_ball_damage_in_actor(&db_opt, &sde_opt, solar_system_id, ball_id, damage, &mut ballpark, now_unix)
                                    .or_else(|| crate::services::fw_gameplay::fw_gameplay_insurgency::handle_fob_ball_damage_in_actor(db_opt.as_deref(), solar_system_id, ball_id, damage, &mut ballpark, now_unix))
                                    .or_else(|| ballpark.apply_damage_to_ball(ball_id, damage)) {
                                    if applied > 0.0 || damage.total() > 0.0 {
                                        let entry = rah_incoming_damage.entry(ball_id).or_insert([0.0; 4]);
                                        entry[0] += damage.em;
                                        entry[1] += damage.thermal;
                                        entry[2] += damage.kinetic;
                                        entry[3] += damage.explosive;
                                    }
                                    if destroyed {
                                        let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                        remove_all_ship_module_cycles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths, &db_opt, ball_id, deact_guard.as_ref());
                                        crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                            &mut ballpark,
                                            &db_opt,
                                            tactical_state.as_ref(),
                                            ball_id,
                                        );
                                    }
                                    if let (Some(db), Some(cs)) = (&db_opt, ballpark.combat_states.get(&ball_id)) {
                                        let not_stale = db.with_snapshot(|snap| {
                                            !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                                                snap,
                                                ball_id,
                                                Some(solar_system_id),
                                                None,
                                            )
                                        });
                                        if not_stale {
                                            crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, cs);
                                        }
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    if let Some(ball_snap) = ball_opt {
                                        let _ = reply.send(Some((applied, destroyed, ball_snap)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else {
                                    let _ = reply.send(None);
                                }
                            }
                            SystemCommand::ApplyTrueDamage { ball_id, amount, reply } => {
                                if let Some((applied, destroyed, ball_opt)) = ballpark.apply_true_damage_to_ball(ball_id, amount) {
                                    if destroyed {
                                        let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                        remove_all_ship_module_cycles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths, &db_opt, ball_id, deact_guard.as_ref());
                                        crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                            &mut ballpark,
                                            &db_opt,
                                            tactical_state.as_ref(),
                                            ball_id,
                                        );
                                    }
                                    if let (Some(db), Some(cs)) = (&db_opt, ballpark.combat_states.get(&ball_id)) {
                                        let not_stale = db.with_snapshot(|snap| {
                                            !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                                                snap,
                                                ball_id,
                                                Some(solar_system_id),
                                                None,
                                            )
                                        });
                                        if not_stale {
                                            crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, cs);
                                        }
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    if let Some(ball_snap) = ball_opt {
                                        let _ = reply.send(Some((applied, destroyed, ball_snap)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else {
                                    let _ = reply.send(None);
                                }
                            }
                            SystemCommand::StepManual { dt, reply } => {
                                pending_step = Some((dt, StepReply::Full(reply)));
                            }
                            SystemCommand::StepManualTickOnly { dt, reply } => {
                                pending_step = Some((dt, StepReply::TickOnly(reply)));
                            }
                            SystemCommand::FlushConditionCheckpoint { reply } => {
                                flush_checkpoint_reply = Some(reply);
                            }
                            SystemCommand::GetTargetLocks { ball_id, reply } => {
                                let locks = ballpark.get_target_locks(ball_id);
                                let _ = reply.send(locks);
                            }
                            SystemCommand::AddTargetLock { source_id, lock, reply } => {
                                if ballpark.combat_states.get(&lock.target_id).is_some_and(|tcs| tcs.is_tethered) {
                                    let _ = reply.send(Err("Target is tethered and invulnerable to target locks".into()));
                                    return;
                                }
                                if let Some(cs) = ballpark.combat_states.get_mut(&source_id) {
                                    cs.undock_invulnerable_remaining_ticks = 0;
                                    cs.is_tethered = false;
                                }
                                let res = if ballpark.add_target_lock(source_id, lock) {
                                    Ok(())
                                } else {
                                    Err("Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay, or target is untargetable".to_string())
                                };
                                let _ = reply.send(res);
                            }
                            SystemCommand::RemoveTargetLock { source_id, target_id, reply } => {
                                let removed = ballpark.remove_target_lock(source_id, target_id);
                                if removed {
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    on_target_unlocked_in_actor(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        source_id,
                                        Some(target_id),
                                        deact_guard.as_ref(),
                                    );
                                }
                                let _ = reply.send(removed);
                            }
                            SystemCommand::ClearTargetLocks { source_id, reply } => {
                                let count = ballpark.clear_target_locks(source_id);
                                if count > 0 {
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    on_target_unlocked_in_actor(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        source_id,
                                        None,
                                        deact_guard.as_ref(),
                                    );
                                }
                                let _ = reply.send(count);
                            }
                            SystemCommand::SetCapacitor { ball_id, capacitor, reply } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    let _ = reply.send(Err(format!("Ball {ball_id} is not in space")));
                                    return;
                                }
                                let Some(cs) = ballpark.combat_states.get_mut(&ball_id) else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} has no combat state")));
                                    return;
                                };
                                cs.capacitor = capacitor;
                                if let Some(db) = &db_opt {
                                    let cloned = cs.clone();
                                    crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
                                        db,
                                        solar_system_id,
                                        std::iter::once((ball_id, &cloned)),
                                    );
                                }
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::HealCombatState { ball_id, reply } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    let _ = reply.send(Err(format!("Ball {ball_id} is not in space")));
                                    return;
                                }
                                let grid_id = ballpark.ball_grid_ids.get(&ball_id).copied();
                                let Some(cs) = ballpark.combat_states.get_mut(&ball_id) else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} has no combat state")));
                                    return;
                                };
                                cs.shield_hp = cs.max_shield_hp;
                                cs.armor_hp = cs.max_armor_hp;
                                cs.structure_hp = cs.max_structure_hp;
                                cs.capacitor = cs.max_capacitor;
                                let cloned = cs.clone();
                                // Queue authoritative Destiny `OnDamageStateChange` / `OnFleetDamageStateChange`
                                // (`[1.0, 1.0, 1.0]`) in the ballpark so V24 in-space clients receive `DoDestinyUpdate`
                                // when `/heal` restores ship HP (standalone MachoNet `OnDamageStateChange` is suppressed).
                                ballpark.emit_damage_and_destruction_events(
                                    ball_id, grid_id, 1.0, 1.0, 1.0, false,
                                );
                                if let Some(db) = &db_opt {
                                    let not_stale = db.with_snapshot(|snap| {
                                        !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                                            snap,
                                            ball_id,
                                            Some(solar_system_id),
                                            None,
                                        )
                                    });
                                    if not_stale {
                                        crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, &cloned);
                                    }
                                }
                                let _ = reply.send(Ok(cloned));
                            }
                            SystemCommand::SetCombatState { ball_id, combat_state, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                cycles_tackle::set_combat_state_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    &mut last_active_totals,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                    ball_id,
                                    combat_state,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::GetCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.get(&ball_id).cloned();
                                let _ = reply.send(cs);
                            }
                            SystemCommand::ModifyCombatState { ball_id, f, reply } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    let _ = reply.send(None);
                                    return;
                                }
                                let Some(mut cs) = ballpark.combat_states.get(&ball_id).cloned() else {
                                    let _ = reply.send(None);
                                    return;
                                };
                                f(&mut cs);
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                cycles_tackle::set_combat_state_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    &mut last_active_totals,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                    ball_id,
                                    cs,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let updated = ballpark.combat_states.get(&ball_id).cloned();
                                if let (Some(db), Some(ref ucs)) = (&db_opt, &updated) {
                                    crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
                                        db,
                                        solar_system_id,
                                        std::iter::once((ball_id, ucs)),
                                    );
                                }
                                let _ = reply.send(updated);
                            }
                            SystemCommand::AddModuleCycle { cycle, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let res = handle_add_module_cycle(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &sde_opt,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                    cycle,
                                );
                                cycles_tackle::snapshot_active_tackle_totals(&ballpark, active_module_cycles, &mut last_active_totals);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
                                let mut remaining_cooldown = None;
                                weapon_cycle_start_modules.remove(&(ship_id, module_id));
                                cycles_tackle::snapshot_active_tackle_totals(&ballpark, active_module_cycles, &mut last_active_totals);
                                if let Some(pos) = active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                                    let removed = active_module_cycles.remove(pos);
                                    let rem_cycle = (removed.duration_s - removed.elapsed_s).max(0.0);
                                    remaining_cooldown = Some(rem_cycle + removed.reactivation_delay_s);
                                    cleanup_removed_module_cycle(&mut ballpark, &removed, &db_opt);
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        deact_guard.as_ref(),
                                    );
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                {
                                    let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                    mod_guard.clear();
                                    for c in active_module_cycles.iter() {
                                        mod_guard.insert(c.ship_id);
                                    }
                                }
                                let _ = reply.send(Ok(remaining_cooldown));
                            }
                            SystemCommand::RequestCycleDeactivation { ship_id, module_id, reply } => {
                                let updated = if let Some(cycle) = active_module_cycles
                                    .iter_mut()
                                    .find(|c| c.ship_id == ship_id && c.module_id == module_id)
                                {
                                    cycle.repeat = 0;
                                    true
                                } else {
                                    false
                                };
                                let _ = reply.send(updated);
                            }
                            SystemCommand::FireWeaponCycleVolley { ship_id, module_id, reply } => {
                                let mut volley_destroyed = Vec::new();
                                let mut volley_actions = Vec::new();
                                if let Some(cycle) = active_module_cycles
                                    .iter()
                                    .find(|c| c.ship_id == ship_id && c.module_id == module_id)
                                    .cloned()
                                {
                                    weapon_cycle_start_modules.insert((ship_id, module_id));
                                    cycles::apply_weapon_cycle_damage(
                                        &cycle,
                                        &mut ballpark,
                                        rah_incoming_damage,
                                        sde_opt,
                                        db_opt,
                                        &mut volley_actions,
                                        &mut volley_destroyed,
                                    );
                                    cycles::flush_pending_cycle_damage_notifications(
                                        db_opt.as_ref(),
                                        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
                                        notify_tx_opt.as_ref(),
                                        &mut volley_actions,
                                    );
                                    if let Some(db) = db_opt.as_ref() {
                                        for &(target_ball_id, _) in &volley_destroyed {
                                            crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                                &mut ballpark,
                                                db_opt,
                                                tactical_state.as_ref(),
                                                target_ball_id,
                                            );
                                            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(
                                                &mut ballpark,
                                                db,
                                                solar_system_id,
                                                target_ball_id,
                                            );
                                        }
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                }
                                let _ = reply.send((volley_destroyed, volley_actions));
                            }
                            SystemCommand::HasModuleCycle { ship_id, module_id, reply } => {
                                let exists = active_module_cycles
                                    .iter()
                                    .any(|c| c.ship_id == ship_id && c.module_id == module_id);
                                let _ = reply.send(exists);
                            }
                            SystemCommand::UpdateModuleOverload { ship_id, module_id, is_overloaded, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let updated = handle_update_module_overload(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &sde_opt,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                    ship_id,
                                    module_id,
                                    is_overloaded,
                                );
                                let _ = reply.send(updated);
                            }
                            SystemCommand::StartNaniteRepair { repair, reply } => {
                                if !active_nanite_repairs.iter().any(|r| r.ship_id == repair.ship_id && r.module_id == repair.module_id) {
                                    active_nanite_repairs.push(repair);
                                }
                                let _ = reply.send(());
                            }
                            SystemCommand::StopNaniteRepair { ship_id, module_id, reply } => {
                                let mut rem = None;
                                if let Some(pos) = active_nanite_repairs.iter().position(|r| r.ship_id == ship_id && r.module_id == module_id) {
                                    let r = active_nanite_repairs.remove(pos);
                                    let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                                    let rem_dmg = (r.initial_damage_hp * (1.0 - frac)).max(0.0);
                                    let used_units = if r.elapsed_s <= 1e-6 {
                                        0
                                    } else {
                                        ((r.paste_total_needed.max(1) as f64) * frac - 1e-9)
                                            .ceil()
                                            .clamp(0.0, r.paste_consumed_so_far as f64) as i32
                                    };
                                    let paste_refund = r.paste_consumed_so_far.saturating_sub(used_units);
                                    rem = Some((rem_dmg, paste_refund));
                                }
                                let _ = reply.send(rem);
                            }
                            SystemCommand::RecordIncomingArmorDamage { ship_id, damage, reply } => {
                                let entry = rah_incoming_damage.entry(ship_id).or_insert([0.0; 4]);
                                for (dst, src) in entry.iter_mut().zip(damage.iter()) {
                                    *dst += *src;
                                }
                                let _ = reply.send(());
                            }
                            SystemCommand::StopNonWarpModuleCycles { ship_id, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_non_warp_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ship_id,
                                    deact_guard.as_ref(),
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                {
                                    let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                    mod_guard.clear();
                                    for c in active_module_cycles.iter() {
                                        mod_guard.insert(c.ship_id);
                                    }
                                }
                                let _ = reply.send(());
                            }
                            SystemCommand::SetInterdictionNullified { ball_id, nullified, reply } => {
                                let _ = reply.send(ballpark.set_interdiction_nullified(ball_id, nullified));
                            }
                            SystemCommand::FastForwardClock { ship_id, secs, reply } => {
                                fast_forward_clock_in_actor_with_epochs(solar_system_id, &mut ballpark, ship_id, secs, sim_elapsed_secs, &db_opt, ball_space_entry_epochs, &ball_positions_actor, &scrambled_balls_actor, &event_tx_actor);
                                let _ = reply.send(());
                            }
                            SystemCommand::MicroJump { ball_id, distance_meters, reply } => {
                                let res = ballpark.micro_jump(ball_id, distance_meters);
                                if res.is_ok() {
                                    persist_ball_positions_in_db(
                                        &ballpark,
                                        &[ball_id],
                                        &db_opt,
                                        ball_space_entry_epochs,
                                    );
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::MicroJumpField { ball_id, distance_meters, field_radius_meters, max_jumped_ships, reply } => {
                                let res = ballpark.micro_jump_field(ball_id, distance_meters, field_radius_meters, max_jumped_ships);
                                if let Ok(ref jumped_ids) = res {
                                    persist_ball_positions_in_db(
                                        &ballpark,
                                        jumped_ids,
                                        &db_opt,
                                        ball_space_entry_epochs,
                                    );
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::IsBallScrambled { ball_id, reply } => {
                                let _ = reply.send(ballpark.is_ball_scrambled(ball_id));
                            }
                            SystemCommand::AddInterdictionSphere { id, position, radius, reply } => {
                                let ok = ballpark.add_interdiction_sphere(id, position, radius);
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                reconcile_tackle_and_bubbles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths, &db_opt, deact_guard.as_ref());
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply } => {
                                let ok = ballpark.add_mobile_warp_disruptor(id, position, radius);
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                reconcile_tackle_and_bubbles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths, &db_opt, deact_guard.as_ref());
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ScheduleMobileWarpDisruptor { id, type_id, position, radius, anchoring_delay_s, reply } => {
                                let ok = handle_schedule_mobile_warp_disruptor(&mut ballpark, pending_anchoring_disruptors, id, type_id, position, radius, anchoring_delay_s);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ApplyEcmJam { target_id, duration_s, reply } => {
                                let target_ewar_immune = active_module_cycles.iter().any(|c| c.ship_id == target_id && c.self_ewar_immune);
                                let exists = ballpark.combat_states.contains_key(&target_id) && !target_ewar_immune;
                                if exists {
                                    ballpark.apply_ecm_jam(target_id, duration_s);
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    on_target_unlocked_in_actor(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                        &db_opt,
                                        target_id,
                                        None,
                                        deact_guard.as_ref(),
                                    );
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                }
                                let _ = reply.send(exists);
                            }
                            SystemCommand::ApplyEcmBurst { source_id, burst_radius_m, jam_strengths, duration_s, roll, ship_eval_overrides, reply } => {
                                let jammed = handle_apply_ecm_burst(&mut ballpark, &active_module_cycles, &sde_opt, &mechanic_rng_opt, source_id, burst_radius_m, jam_strengths, duration_s, roll, ship_eval_overrides);
                                if !jammed.is_empty() {
                                    let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                    for &tid in &jammed {
                                        on_target_unlocked_in_actor(
                                            &mut ballpark,
                                            active_module_cycles,
                                            &base_scramble_strengths,
                                            &base_core_strengths,
                                            &db_opt,
                                            tid,
                                            None,
                                            deact_guard.as_ref(),
                                        );
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                }
                                let _ = reply.send(jammed);
                            }
                            SystemCommand::FlushPeerObserverUpdates {
                                exclude_ball_id,
                                reply,
                            } => {
                                let mut flushed = 0usize;
                                if event_tx_actor.receiver_count() > 0 {
                                    sync_player_slim_items(&mut ballpark, &db_opt);
                                    let stamp = ballpark.current_tick as u32;
                                    let ready_obs_ids: Vec<BallId> = ballpark
                                        .observer_events
                                        .iter()
                                        .filter_map(|(&obs_id, evs)| {
                                            if Some(obs_id) == exclude_ball_id
                                                || evs.is_empty()
                                                || ballpark
                                                    .pending_handoff_observers
                                                    .contains(&obs_id)
                                            {
                                                return None;
                                            }
                                            if evs
                                                .iter()
                                                .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
                                            {
                                                return None;
                                            }
                                            Some(obs_id)
                                        })
                                        .collect();
                                    if !ready_obs_ids.is_empty() {
                                        let mut observer_events = std::collections::BTreeMap::new();
                                        let mut observer_updates_pyrep =
                                            std::collections::BTreeMap::new();
                                        for obs_id in ready_obs_ids {
                                            let obs_evs = ballpark.drain_observer_events(obs_id);
                                            if !obs_evs.is_empty() {
                                                observer_updates_pyrep.insert(
                                                    obs_id,
                                                    encode_destiny_updates(
                                                        stamp,
                                                        &obs_evs,
                                                        Some(&ballpark),
                                                    ),
                                                );
                                                observer_events.insert(obs_id, obs_evs);
                                            }
                                        }
                                        flushed = observer_events.len();
                                        if flushed > 0 {
                                            let _ = event_tx_actor.send(TickBroadcast {
                                                solar_system_id,
                                                tick: ballpark.current_tick,
                                                events: Vec::new(),
                                                observer_events,
                                                observer_updates_pyrep,
                                            });
                                        }
                                    }
                                }
                                let _ = reply.send(flushed);
                            }
                            SystemCommand::ApplyFitProjection {
                                ship_id,
                                projection,
                                reply,
                            } => {
                                let applied = apply_fit_projection_in_actor(
                                    &mut ballpark,
                                    fit_projections,
                                    ship_id,
                                    projection,
                                );
                                if let Some(tx) = reply {
                                    let _ = tx.send(applied);
                                }
                            }
                        }
                        if let Some((dt, step_reply)) = pending_step {
                            let raw_tidi = f64::from_bits(tidi_factor_bits_actor.load(std::sync::atomic::Ordering::Relaxed));
                            let tidi = if raw_tidi.is_finite() { raw_tidi.clamp(0.10, 1.0) } else { 1.0 };
                            let dt_sim = if dt > 0.0 && dt.is_finite() { dt * tidi } else { dt };
                            if dt_sim > 0.0 && dt_sim.is_finite() {
                                *sim_elapsed_secs += dt_sim;
                            }
                            respawn_due_asteroids(
                                &mut ballpark,
                                asteroids,
                                &asteroid_templates,
                                &sde_opt,
                                &db_opt,
                            );
                            sync_player_slim_items(&mut ballpark, &db_opt);
                            if let Some(db) = db_opt.as_ref() {
                                let now_unix = db.clock().now_unix_secs();
                                if now_unix > ballpark.sim_unix_secs {
                                    ballpark.sim_unix_secs = now_unix;
                                }
                            }
                            let pre_tick_caps = cycles::snapshot_player_ship_capacitors(&ballpark, db_opt.as_ref());
                            let events = ballpark.step(dt_sim);
                            let landed_warps = celestials::collect_landed_warp_positions(&ballpark, &events);
                            persist_landed_warps_in_db(
                                &ballpark,
                                &events,
                                &db_opt,
                                ball_space_entry_epochs,
                            );
                            cycles::flush_target_lost_notifications(
                                &ballpark,
                                &events,
                                db_opt.as_ref(),
                                pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
                                notify_tx_opt.as_ref(),
                            );
                            let mut lock_completed_events = Vec::new();
                            let mut target_lost_events = Vec::new();
                            let lock_hook_opt = target_lock_completed_hook_actor
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                            for ev in &events {
                                if let DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
                                    if mode == "lost" {
                                        on_target_unlocked_in_actor(
                                            &mut ballpark,
                                            active_module_cycles,
                                            &base_scramble_strengths,
                                            &base_core_strengths,
                                            &db_opt,
                                            *source_id,
                                            Some(*target_id),
                                            deact_guard.as_ref(),
                                        );
                                    }
                                    if mode == "locked" || mode == "lost" {
                                        let char_id_opt = ballpark
                                            .ball_slim_meta
                                            .get(source_id)
                                            .and_then(|m| m.char_id)
                                            .or_else(|| {
                                                db_opt.as_ref().and_then(|db| {
                                                    db.with_snapshot(|s| {
                                                        s.characters
                                                            .values()
                                                            .find(|c| c.active_ship_id == *source_id)
                                                            .map(|c| c.character_id)
                                                            .or_else(|| {
                                                                crate::services::station_structure::structure_upwell_ops::load_ext_state(s)
                                                                    .active_pilot_by_structure
                                                                    .get(source_id)
                                                                    .copied()
                                                            })
                                                    })
                                                })
                                            });
                                        if let Some(cid) = char_id_opt.filter(|&c| c > 0) {
                                            if mode == "locked" {
                                                lock_completed_events.push((cid, *target_id));
                                                if let Some(ref lock_hook) = lock_hook_opt {
                                                    lock_hook(cid, *target_id);
                                                }
                                            } else {
                                                target_lost_events.push((cid, *target_id));
                                            }
                                        }
                                    }
                                }
                            }
                            let mut destroyed_balls = step_cycles_and_capacitor_with_epochs(
                                &mut ballpark, active_module_cycles, pending_anchoring_disruptors,
                                active_nanite_repairs, ship_rack_heat, rah_incoming_damage,
                                &base_scramble_strengths, &base_core_strengths, &sde_opt, &db_opt,
                                ball_space_entry_epochs, fit_projections, weapon_cycle_start_modules,
                                mechanic_rng_opt, tactical_state.as_ref(), pending_cycle_actions, dt_sim, deact_guard.as_ref(),
                            );
                            cycles::record_player_ship_capacitor_deltas(&ballpark, &pre_tick_caps, pending_cycle_actions);
                            cycles::flush_pending_cycle_damage_notifications(
                                db_opt.as_ref(),
                                pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
                                notify_tx_opt.as_ref(),
                                pending_cycle_actions,
                            );
                            let cycle_actions = pending_cycle_actions.clone();
                            if let Some(db) = db_opt.as_ref() {
                                for &(target_ball_id, _) in &destroyed_balls {
                                    crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                        &mut ballpark,
                                        &db_opt,
                                        tactical_state.as_ref(),
                                        target_ball_id,
                                    );
                                    crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(
                                        &mut ballpark, db, solar_system_id, target_ball_id,
                                    );
                                }
                            }
                            for bid in crate::services::pve_abyssal_pylons::step_abyssal_hazards_and_pylons_in_ballpark_for_tick(
                                &mut ballpark,
                                sde_opt.as_deref(),
                                db_opt.as_ref(),
                                solar_system_id,
                            ) {
                                destroyed_balls.push((bid, None));
                            }
                            for bid in crate::services::pvp_filament_arena::step_proving_grounds_arena_hazards_in_ballpark_for_tick(
                                &mut ballpark,
                                sde_opt.as_deref(),
                                db_opt.as_ref(),
                                solar_system_id,
                                pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
                                notify_tx_opt.as_ref(),
                            ) {
                                destroyed_balls.push((bid, None));
                            }
                            for bid in crate::services::space_navigation_advanced::step_zarzakh_gravity_hazard_in_ballpark(
                                &mut ballpark,
                                sde_opt.as_deref(),
                                solar_system_id,
                            ) {
                                destroyed_balls.push((bid, None));
                            }
                            {
                                let step_now_unix = db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or_else(|| {
                                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                                });
                                crate::services::mobile_deployables::step_mobile_deployables_in_actor(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &mechanic_rng,
                                    deact_guard.as_ref(),
                                    sde_opt.as_deref(),
                                    db_opt.as_ref(),
                                    mechanic_rng_opt.as_deref(),
                                    solar_system_id,
                                    dt_sim,
                                    step_now_unix,
                                );
                                crate::services::doomsday_aoe_ops::step_bosonic_doomsday_in_ballpark(
                                    &mut ballpark,
                                    base_scramble_strengths,
                                    sde_opt.as_ref(),
                                    db_opt.as_ref(),
                                    solar_system_id,
                                    step_now_unix,
                                    &mut destroyed_balls,
                                );
                                cycles_tackle::snapshot_active_tackle_totals(&ballpark, active_module_cycles, &mut last_active_totals);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    deact_guard.as_ref(),
                                );
                            }
                            sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                            {
                                let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                mod_guard.clear();
                                for c in active_module_cycles.iter() {
                                    mod_guard.insert(c.ship_id);
                                }
                            }
                            let current_tick = ballpark.current_tick;
                            let has_subscribers = event_tx_actor.receiver_count() > 0;
                            let broadcast_opt = if has_subscribers || matches!(step_reply, StepReply::Full(_)) {
                                let broadcast = build_tick_broadcast(solar_system_id, &mut ballpark, events);
                                if has_subscribers {
                                    let _ = event_tx_actor.send(broadcast.clone());
                                }
                                Some(broadcast)
                            } else {
                                belts::drain_tick_events_without_encode(&mut ballpark);
                                None
                            };
                            let condition_checkpoint = if is_condition_checkpoint_tick(solar_system_id, current_tick) {
                                Some(celestials::collect_dirty_condition_checkpoint_entries(&mut actor_st, &ballpark))
                            } else {
                                celestials::collect_immediate_armor_hull_checkpoint_entries(&mut actor_st, &ballpark)
                            };
                            celestials::update_system_hibernation_flag(&actor_st, &ballpark, has_subscribers as usize);
                            let step_out = StepOutput {
                                solar_system_id,
                                tick: current_tick,
                                dt_sim,
                                landed_warps,
                                destroyed_balls,
                                lock_completed_events,
                                target_lost_events,
                                cycle_actions,
                                condition_checkpoint,
                                broadcast: broadcast_opt,
                            };
                            match step_reply {
                                StepReply::Full(reply) | StepReply::TickOnly(reply) => {
                                    let _ = reply.send(step_out);
                                }
                            }
                        } else {
                            {
                                let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                mod_guard.clear();
                                for c in active_module_cycles.iter() {
                                    mod_guard.insert(c.ship_id);
                                }
                            }
                            if let Some(bid) = spawned_checkpoint_ball {
                                celestials::record_spawned_ball_checkpoint_baseline(&mut actor_st, &ballpark, bid);
                            }
                            if let Some(bid) = removed_checkpoint_ball {
                                celestials::remove_ball_checkpoint_tracking(&mut actor_st, bid);
                            }
                            if let Some(reply) = flush_checkpoint_reply {
                                let entries = celestials::collect_dirty_condition_checkpoint_entries(&mut actor_st, &ballpark);
                                let _ = reply.send(entries);
                            }
                            celestials::update_system_hibernation_flag(
                                &actor_st,
                                &ballpark,
                                event_tx_actor.receiver_count(),
                            );
                        }
                    }
                }
                }));
                if let Err(panic_payload) = cmd_outcome {
                    let panic_count = command_panics_actor.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let msg = panic_payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("non-string panic payload");
                    tracing::error!(
                        solar_system_id,
                        panic_count,
                        panic = %msg,
                        "SystemActor caught panic while handling command; continuing actor loop"
                    );
                }
            }
        });

        let handle = SystemHandle {
            solar_system_id,
            cmd_tx,
            event_tx,
            ball_positions,
            scrambled_balls,
            committed_warp_balls,
            active_module_ships,
            ballpark: ballpark_shared,
            actor_state: actor_state_shared,
            last_auto_stepped_unix,
            tidi_factor_bits,
            command_panics,
            hibernated,
            hibernated_since_unix,
            idle_ticks,
            dynamic_ball_count,
            #[cfg(test)]
            panic_next_command_for_test,
            npc_death_hook: Arc::clone(&self.npc_death_hook),
            module_deactivation_hook: Arc::clone(&self.module_deactivation_hook),
            target_lock_completed_hook: Arc::clone(&self.target_lock_completed_hook),
            system_step_hook: Arc::clone(&self.system_step_hook),
        };
        if let Some(ref db) = self.db {
            register_db_system_ballpark(db, solar_system_id, &handle);
        }
        let insert_handle = |guard: &mut HashMap<SolarSystemId, SystemHandle>| -> SystemHandle {
            if let Some(existing) = guard.get(&solar_system_id) {
                if !existing.cmd_tx.is_closed() {
                    return existing.clone();
                }
                tracing::warn!(
                    solar_system_id,
                    "replacing terminated SystemActor handle with freshly spawned actor"
                );
            }
            guard.insert(solar_system_id, handle.clone());
            handle.clone()
        };
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            match self.systems.try_write_unranked() {
                Ok(mut guard) => insert_handle(&mut guard),
                Err(std::sync::TryLockError::Poisoned(e)) => insert_handle(&mut e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => handle,
            }
        } else {
            let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
            insert_handle(&mut guard)
        }
    }
}

/// Applies an immutable [`eve_dogma::FitProjection`] onto `ship_id` inside the system actor,
/// enforcing monotonic [`eve_dogma::FitRev`] ordering and recomputing derived `Ball` / `CombatState`
/// attributes from the new baselines without overwriting actor-managed multipliers (`#922`, `#834`).
pub(crate) fn apply_fit_projection_in_actor(
    ballpark: &mut Ballpark,
    fit_projections: &mut HashMap<BallId, eve_dogma::FitProjection>,
    ship_id: BallId,
    projection: eve_dogma::FitProjection,
) -> bool {
    if fit_projections
        .get(&ship_id)
        .is_some_and(|cur| projection.rev < cur.rev)
    {
        return false;
    }
    let prev_proj = fit_projections.get(&ship_id);
    let hull_changed = prev_proj.is_some_and(|p| p.ship_type_id != projection.ship_type_id);
    let prev_baseline_cap = prev_proj.map(|p| p.baseline.max_capacitor);
    let baseline = projection.baseline;
    fit_projections.insert(ship_id, projection);
    let prev_max_vel = ballpark.balls.get(&ship_id).map(|b| b.max_velocity);
    let Some(cs) = ballpark.combat_states.get_mut(&ship_id) else {
        return false;
    };
    let shield_regen_disabled = cs.shield_recharge_time_ms <= 0.0;
    let cap_regen_disabled = cs.capacitor_recharge_time_ms <= 0.0
        || cs.base_capacitor_recharge_time_ms.is_some_and(|b| b <= 0.0);
    cs.base_max_velocity = Some(baseline.max_velocity.max(0.0));
    cs.base_mass = Some(baseline.mass.max(1.0));
    cs.base_inertia = Some(baseline.agility.max(0.001));
    cs.base_signature_radius = Some(baseline.signature_radius.max(0.0));
    if !shield_regen_disabled {
        cs.base_max_shield_hp = Some(baseline.max_shield_hp.max(0.0));
        cs.base_max_armor_hp = Some(baseline.max_armor_hp.max(0.0));
        cs.base_max_structure_hp = Some(baseline.max_structure_hp.max(0.0));
        cs.shield_recharge_time_ms = baseline.shield_recharge_time_ms.max(1.0);
    }
    if !cap_regen_disabled {
        let cap_manually_overridden = prev_baseline_cap.is_some_and(|prev_b| {
            cs.base_max_capacitor
                .is_some_and(|cur_b| (cur_b - prev_b).abs() > 1e-2)
                && (baseline.max_capacitor - prev_b).abs() <= 1e-2
        }) || (prev_baseline_cap.is_none()
            && (baseline.max_capacitor - 250.0).abs() < 1e-2
            && cs
                .base_max_capacitor
                .is_some_and(|cur_b| (cur_b - 250.0).abs() > 1e-2));
        if !cap_manually_overridden {
            let cap_floor = if hull_changed { cs.capacitor } else { 0.0 };
            cs.base_max_capacitor = Some(baseline.max_capacitor.max(cap_floor).max(0.0));
        }
        cs.base_capacitor_recharge_time_ms = Some(baseline.capacitor_recharge_time_ms.max(1.0));
    }
    cs.base_shield_resonance = Some(baseline.shield_resonance);
    cs.base_armor_resonance = Some(baseline.armor_resonance);
    cs.base_structure_resonance = Some(baseline.structure_resonance);
    ballpark.recompute_ball_attributes(ship_id);
    if let (Some(prev), Some(new_v)) =
        (prev_max_vel, ballpark.balls.get(&ship_id).map(|b| b.max_velocity))
    {
        if (prev - new_v).abs() > 1e-6 {
            ballpark.emit_event(
                Some(ship_id),
                DestinyEvent::SetMaxSpeed {
                    ball_id: ship_id,
                    max_speed: new_v,
                },
            );
        }
    }
    true
}

impl SystemHandle {
    /// Pushes an immutable [`eve_dogma::FitProjection`] to the system actor and awaits completion (`#922`).
    pub async fn apply_fit_projection(
        &self,
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
    ) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::ApplyFitProjection {
                ship_id,
                projection,
                reply: Some(tx),
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Non-blocking enqueue of [`SystemCommand::ApplyFitProjection`] for synchronous fit-mutation paths (`#922`).
    ///
    /// Releases/requires no held locks (`cmd_sender` asserts lock-free caller). Returns `false`
    /// only if the actor mailbox is closed or currently full.
    pub fn try_send_fit_projection(
        &self,
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
    ) -> bool {
        self.cmd_sender()
            .try_send(SystemCommand::ApplyFitProjection {
                ship_id,
                projection,
                reply: None,
            })
            .is_ok()
    }

    /// Reads the latest [`eve_dogma::FitProjection`] stored in actor state for `ship_id`, if any.
    pub fn latest_fit_projection(&self, ship_id: BallId) -> Option<eve_dogma::FitProjection> {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .fit_projections
            .get(&ship_id)
            .cloned()
    }
}

#[cfg(test)] #[path = "system_actor_tests.rs"] mod system_actor_tests;
#[cfg(test)] #[path = "system_actor_extra_tests.rs"] mod system_actor_extra_tests;
#[cfg(test)] #[path = "system_actor/cycles_extra_unit_tests.rs"] mod cycles_extra_unit_tests;
#[cfg(test)] #[path = "system_actor_combat_ai_deep_extra_unit_tests.rs"] mod system_actor_combat_ai_deep_extra_unit_tests;

