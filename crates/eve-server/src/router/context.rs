//! Server execution context, runtime state caches, and simulation clock management.

use crate::living_world::LivingWorldDirector;
use crate::system_actor::SystemRegistry;
use crate::world_seeder;
use eve_db::lock_rank::{
    AnyRwLock, RankedMutex, RankedRwLock, RankedRwLockReadGuard, RankedRwLockWriteGuard,
    RANK_CLOCK, RANK_LEAF, RANK_NPC_SPAWNS,
};
use eve_db::WorldRepository;
use eve_dogma::ShipFittingEngine;
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, LockResult, PoisonError, RwLock, TryLockError, TryLockResult};

pub static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

pub const EMBEDDED_RPC_CATALOG_JSON: &str =
    include_str!("../../../../docs/index/rpc_service_catalog.json");
pub const EMBEDDED_QUASAR_CATALOG_JSON: &str =
    include_str!("../../../../docs/index/quasar_protobuf_catalog.json");

#[derive(Debug, Clone)]
pub struct BoundObjectEntry {
    pub bound_id: u64,
    pub session_id: u64,
    pub user_id: u64,
    pub character_id: i64,
    pub service: String,
    pub object_id: i64,
    pub oid_str: String,
    pub ref_id: i64,
}

static NEXT_CTX_INSTANCE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone)]
pub struct ServerContext {
    pub instance_id: u64,
    pub sde: Arc<SdeRegistry>,
    pub db: Arc<WorldRepository>,
    pub clock: Arc<RankedRwLock<Arc<dyn eve_db::ServerClock>, RANK_CLOCK>>,
    pub systems: Arc<SystemRegistry>,
    pub living_world: Arc<LivingWorldDirector>,
    pub quasar_gateway: Arc<crate::quasar_gateway::PublicGatewayRouter>,
    pub cataloged_services: Arc<HashSet<String>>,
    pub quasar_domains: Arc<HashSet<String>>,
    pub bound_objects: Arc<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    pub object_cache: Arc<RankedRwLock<HashMap<String, crate::router::cache_notifications::CachedObjectRecord>, RANK_LEAF>>,
    pub object_cache_timestamps: Arc<RankedRwLock<HashMap<String, i64>, RANK_LEAF>>,
    pub notification_records: Arc<RankedRwLock<Vec<crate::router::cache_notifications::NotificationRecord>, RANK_LEAF>>,
    pub pending_notifications: Arc<RankedRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>, RANK_LEAF>>,
    pub notify_tx: tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    pub map_landmarks: Arc<RankedRwLock<Vec<(i64, String, f64, f64, f64)>, RANK_LEAF>>,
    pub session_closers: Arc<RankedRwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>, RANK_LEAF>>,
    pub ship_fittings: Arc<RankedRwLock<HashMap<i64, ShipFittingEngine>, RANK_LEAF>>,
    pub projected_ewar: Arc<RankedRwLock<HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>, RANK_LEAF>>,
    pub projected_ewar_sources: Arc<RankedRwLock<HashMap<i64, i64>, RANK_LEAF>>,
    pub active_ewar_starts: Arc<RankedRwLock<HashMap<(i64, i64), Vec<(i64, &'static str)>>, RANK_LEAF>>,
    pub weapon_cycle_state: Arc<RankedRwLock<HashMap<i64, (Option<u64>, u32, Option<i64>)>, RANK_LEAF>>,
    pub fighters: Arc<RankedRwLock<FighterRuntimeState, RANK_LEAF>>,
    pub fighter_cooldown_state: FighterStateLens<HashMap<(i64, u32), u64>>,
    pub fighter_charge_state: FighterStateLens<HashMap<(i64, u32), u32>>,
    pub active_fighters: FighterStateLens<HashMap<i64, ActiveFighterSquadron>>,
    pub fighter_tube_loads: FighterStateLens<HashMap<(i64, usize), i64>>,
    pub active_scanner_probes: Arc<RankedRwLock<HashMap<i64, ActiveScannerProbe>, RANK_LEAF>>,
    pub active_moon_probes: Arc<RankedRwLock<Vec<ActiveMoonSurveyProbe>, RANK_LEAF>>,
    pub scanned_signatures: Arc<RankedRwLock<HashSet<(i64, u32, i64)>, RANK_LEAF>>,
    pub character_scan_progress: Arc<RankedRwLock<HashMap<(i64, u32, String), f64>, RANK_LEAF>>,
    pub invention_roll_override: Arc<RankedRwLock<Option<f64>, RANK_LEAF>>,
    pub mechanic_rng: Arc<crate::rng::MechanicRng>, // seeded test RNG: shared with `systems`; OS CSPRNG unless seeded
    pub module_reactivation_cooldowns: Arc<RankedRwLock<HashMap<(i64, i64, u32), ModuleCooldownEntry>, RANK_LEAF>>,
    /// Repeating mining-module cycles keyed by module item ID (`MN-04`).
    pub mining_cycles: Arc<RankedRwLock<HashMap<i64, crate::services::mining_cycle::ActiveMiningCycle>, RANK_LEAF>>,
    /// Active Phased Asteroid Fields keyed by solar system ID (`MN-08`).
    pub phased_asteroid_fields: Arc<RankedRwLock<HashMap<u32, crate::services::space_mining_harvest::PhasedAsteroidFieldState>, RANK_LEAF>>,
    pub crimewatch: Arc<crate::combat::crimewatch::CrimeWatchLock>,
    pub(crate) combat_ephemeral: Arc<RankedRwLock<crate::services::combat_crimewatch::CombatRuntimeState, RANK_LEAF>>,
    pub tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    pub session_change_timers: Arc<RankedMutex<HashMap<i64, crate::services::space_destiny::SessionChangeTimerState>, RANK_LEAF>>,
    pub active_warps: Arc<RankedMutex<HashMap<i64, i64>, RANK_LEAF>>,
    pub return_warps: Arc<RankedMutex<HashSet<i64>, RANK_LEAF>>,
    pub location_change_invuln: Arc<RankedMutex<HashMap<i64, u64>, RANK_LEAF>>,
    pub feature_flags: Arc<RwLock<crate::config::FeatureFlagsConfig>>,
    pub esi_port: Arc<AtomicU16>,
    pub xmpp_port: Arc<AtomicU16>,
    pub esi_state: Arc<crate::esi::runtime_state::EsiRuntimeState>,
    /// Per-world fleet state not persisted in `WorldRepository` (names, invites, adverts, watchlists).
    pub(crate) fleet_extras: Arc<RankedMutex<crate::services::fleet_social::fleet_social_fleets::FleetExtrasState, RANK_LEAF>>,
    /// Per-world social state not persisted in `WorldRepository` (mailing lists, calendar, chat moderation).
    pub(crate) social_extras: Arc<RankedMutex<crate::services::fleet_social::fleet_social_chat::SocialExtrasState, RANK_LEAF>>,
    /// Per-world cluster/macho/misc service state (`cluster_macho_misc_services`).
    pub(crate) cluster_misc_state: Arc<RankedMutex<crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState, RANK_LEAF>>,
    pub cluster_state: Arc<RankedRwLock<crate::services::macho_cluster_mgmt::MachoClusterState, RANK_LEAF>>,
    pub global_config_values: Arc<RankedRwLock<HashMap<String, eve_proto::PyRep>, RANK_LEAF>>,
    pub alert_telemetry: Arc<RankedMutex<crate::services::client_login::AlertTelemetryState, RANK_LEAF>>,
    pub flight_recorder: Arc<crate::flight_recorder::FlightRecorder>,
    pub bug_reporting: Arc<crate::bug_reporting::BugReportingRuntime>,
    pub npc_spawns: Arc<RankedRwLock<HashMap<u32, crate::combat::npc_ai::NpcSpawnManager>, RANK_NPC_SPAWNS>>,
    pub combat_rng_seq: Arc<AtomicU64>,
    pub cluster_tick_last_unix: Arc<AtomicU64>,
    pub cluster_tick_persisted_unix: Arc<AtomicU64>,
    pub dedicated_physics_ticker_active: Arc<AtomicBool>,
    pub fw_idle_last_step_unix: Arc<RankedRwLock<HashMap<i64, u64>, RANK_LEAF>>,
}

/// Non-owning weak handle to all shared [`ServerContext`] state (`RFC-1 E-4`, `#839`, `#919`).
///
/// Holds [`std::sync::Weak`] pointers to every `Arc`-managed runtime map and service so hooks
/// registered inside [`SystemRegistry`] cannot create reference cycles while upgrading to the
/// single authoritative [`ServerContext`] without constructing a shadow context.
#[derive(Clone)]
pub(crate) struct ServerContextWeak {
    instance_id: u64,
    sde: std::sync::Weak<SdeRegistry>,
    db: std::sync::Weak<WorldRepository>,
    clock: std::sync::Weak<RankedRwLock<Arc<dyn eve_db::ServerClock>, RANK_CLOCK>>,
    systems: std::sync::Weak<SystemRegistry>,
    living_world: std::sync::Weak<LivingWorldDirector>,
    quasar_gateway: std::sync::Weak<crate::quasar_gateway::PublicGatewayRouter>,
    cataloged_services: std::sync::Weak<HashSet<String>>,
    quasar_domains: std::sync::Weak<HashSet<String>>,
    bound_objects: std::sync::Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    object_cache: std::sync::Weak<RankedRwLock<HashMap<String, crate::router::cache_notifications::CachedObjectRecord>, RANK_LEAF>>,
    object_cache_timestamps: std::sync::Weak<RankedRwLock<HashMap<String, i64>, RANK_LEAF>>,
    notification_records: std::sync::Weak<RankedRwLock<Vec<crate::router::cache_notifications::NotificationRecord>, RANK_LEAF>>,
    pending_notifications: std::sync::Weak<RankedRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>, RANK_LEAF>>,
    notify_tx: tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    map_landmarks: std::sync::Weak<RankedRwLock<Vec<(i64, String, f64, f64, f64)>, RANK_LEAF>>,
    session_closers: std::sync::Weak<RankedRwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>, RANK_LEAF>>,
    ship_fittings: std::sync::Weak<RankedRwLock<HashMap<i64, ShipFittingEngine>, RANK_LEAF>>,
    projected_ewar: std::sync::Weak<RankedRwLock<HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>, RANK_LEAF>>,
    projected_ewar_sources: std::sync::Weak<RankedRwLock<HashMap<i64, i64>, RANK_LEAF>>,
    active_ewar_starts: std::sync::Weak<RankedRwLock<HashMap<(i64, i64), Vec<(i64, &'static str)>>, RANK_LEAF>>,
    weapon_cycle_state: std::sync::Weak<RankedRwLock<HashMap<i64, (Option<u64>, u32, Option<i64>)>, RANK_LEAF>>,
    fighters: std::sync::Weak<RankedRwLock<FighterRuntimeState, RANK_LEAF>>,
    active_scanner_probes: std::sync::Weak<RankedRwLock<HashMap<i64, ActiveScannerProbe>, RANK_LEAF>>,
    active_moon_probes: std::sync::Weak<RankedRwLock<Vec<ActiveMoonSurveyProbe>, RANK_LEAF>>,
    scanned_signatures: std::sync::Weak<RankedRwLock<HashSet<(i64, u32, i64)>, RANK_LEAF>>,
    character_scan_progress: std::sync::Weak<RankedRwLock<HashMap<(i64, u32, String), f64>, RANK_LEAF>>,
    invention_roll_override: std::sync::Weak<RankedRwLock<Option<f64>, RANK_LEAF>>,
    mechanic_rng: std::sync::Weak<crate::rng::MechanicRng>,
    module_reactivation_cooldowns: std::sync::Weak<RankedRwLock<HashMap<(i64, i64, u32), ModuleCooldownEntry>, RANK_LEAF>>,
    mining_cycles: std::sync::Weak<RankedRwLock<HashMap<i64, crate::services::mining_cycle::ActiveMiningCycle>, RANK_LEAF>>,
    phased_asteroid_fields: std::sync::Weak<RankedRwLock<HashMap<u32, crate::services::space_mining_harvest::PhasedAsteroidFieldState>, RANK_LEAF>>,
    crimewatch: std::sync::Weak<crate::combat::crimewatch::CrimeWatchLock>,
    combat_ephemeral: std::sync::Weak<RankedRwLock<crate::services::combat_crimewatch::CombatRuntimeState, RANK_LEAF>>,
    tactical_state: std::sync::Weak<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    session_change_timers: std::sync::Weak<RankedMutex<HashMap<i64, crate::services::space_destiny::SessionChangeTimerState>, RANK_LEAF>>,
    active_warps: std::sync::Weak<RankedMutex<HashMap<i64, i64>, RANK_LEAF>>,
    return_warps: std::sync::Weak<RankedMutex<HashSet<i64>, RANK_LEAF>>,
    location_change_invuln: std::sync::Weak<RankedMutex<HashMap<i64, u64>, RANK_LEAF>>,
    feature_flags: std::sync::Weak<RwLock<crate::config::FeatureFlagsConfig>>,
    esi_port: std::sync::Weak<AtomicU16>,
    xmpp_port: std::sync::Weak<AtomicU16>,
    esi_state: std::sync::Weak<crate::esi::runtime_state::EsiRuntimeState>,
    fleet_extras: std::sync::Weak<RankedMutex<crate::services::fleet_social::fleet_social_fleets::FleetExtrasState, RANK_LEAF>>,
    social_extras: std::sync::Weak<RankedMutex<crate::services::fleet_social::fleet_social_chat::SocialExtrasState, RANK_LEAF>>,
    cluster_misc_state: std::sync::Weak<RankedMutex<crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState, RANK_LEAF>>,
    cluster_state: std::sync::Weak<RankedRwLock<crate::services::macho_cluster_mgmt::MachoClusterState, RANK_LEAF>>,
    global_config_values: std::sync::Weak<RankedRwLock<HashMap<String, eve_proto::PyRep>, RANK_LEAF>>,
    alert_telemetry: std::sync::Weak<RankedMutex<crate::services::client_login::AlertTelemetryState, RANK_LEAF>>,
    flight_recorder: std::sync::Weak<crate::flight_recorder::FlightRecorder>,
    bug_reporting: std::sync::Weak<crate::bug_reporting::BugReportingRuntime>,
    npc_spawns: std::sync::Weak<RankedRwLock<HashMap<u32, crate::combat::npc_ai::NpcSpawnManager>, RANK_NPC_SPAWNS>>,
    combat_rng_seq: std::sync::Weak<AtomicU64>,
    cluster_tick_last_unix: std::sync::Weak<AtomicU64>,
    cluster_tick_persisted_unix: std::sync::Weak<AtomicU64>,
    dedicated_physics_ticker_active: std::sync::Weak<AtomicBool>,
    fw_idle_last_step_unix: std::sync::Weak<RankedRwLock<HashMap<i64, u64>, RANK_LEAF>>,
}

impl ServerContextWeak {
    pub(crate) fn upgrade(&self) -> Option<ServerContext> {
        let fighters = self.fighters.upgrade()?;
        let (fighter_cooldown_state, fighter_charge_state, active_fighters, fighter_tube_loads) =
            make_fighter_lenses(&fighters);
        Some(ServerContext {
            instance_id: self.instance_id,
            sde: self.sde.upgrade()?,
            db: self.db.upgrade()?,
            clock: self.clock.upgrade()?,
            systems: self.systems.upgrade()?,
            living_world: self.living_world.upgrade()?,
            quasar_gateway: self.quasar_gateway.upgrade()?,
            cataloged_services: self.cataloged_services.upgrade()?,
            quasar_domains: self.quasar_domains.upgrade()?,
            bound_objects: self.bound_objects.upgrade()?,
            object_cache: self.object_cache.upgrade()?,
            object_cache_timestamps: self.object_cache_timestamps.upgrade()?,
            notification_records: self.notification_records.upgrade()?,
            pending_notifications: self.pending_notifications.upgrade()?,
            notify_tx: self.notify_tx.clone(),
            map_landmarks: self.map_landmarks.upgrade()?,
            session_closers: self.session_closers.upgrade()?,
            ship_fittings: self.ship_fittings.upgrade()?,
            projected_ewar: self.projected_ewar.upgrade()?,
            projected_ewar_sources: self.projected_ewar_sources.upgrade()?,
            active_ewar_starts: self.active_ewar_starts.upgrade()?,
            weapon_cycle_state: self.weapon_cycle_state.upgrade()?,
            fighters,
            fighter_cooldown_state,
            fighter_charge_state,
            active_fighters,
            fighter_tube_loads,
            active_scanner_probes: self.active_scanner_probes.upgrade()?,
            active_moon_probes: self.active_moon_probes.upgrade()?,
            scanned_signatures: self.scanned_signatures.upgrade()?,
            character_scan_progress: self.character_scan_progress.upgrade()?,
            invention_roll_override: self.invention_roll_override.upgrade()?,
            mechanic_rng: self.mechanic_rng.upgrade()?,
            module_reactivation_cooldowns: self.module_reactivation_cooldowns.upgrade()?,
            mining_cycles: self.mining_cycles.upgrade()?,
            phased_asteroid_fields: self.phased_asteroid_fields.upgrade()?,
            crimewatch: self.crimewatch.upgrade()?,
            combat_ephemeral: self.combat_ephemeral.upgrade()?,
            tactical_state: self.tactical_state.upgrade()?,
            session_change_timers: self.session_change_timers.upgrade()?,
            active_warps: self.active_warps.upgrade()?,
            return_warps: self.return_warps.upgrade()?,
            location_change_invuln: self.location_change_invuln.upgrade()?,
            feature_flags: self.feature_flags.upgrade()?,
            esi_port: self.esi_port.upgrade()?,
            xmpp_port: self.xmpp_port.upgrade()?,
            esi_state: self.esi_state.upgrade()?,
            fleet_extras: self.fleet_extras.upgrade()?,
            social_extras: self.social_extras.upgrade()?,
            cluster_misc_state: self.cluster_misc_state.upgrade()?,
            cluster_state: self.cluster_state.upgrade()?,
            global_config_values: self.global_config_values.upgrade()?,
            alert_telemetry: self.alert_telemetry.upgrade()?,
            flight_recorder: self.flight_recorder.upgrade()?,
            bug_reporting: self.bug_reporting.upgrade()?,
            npc_spawns: self.npc_spawns.upgrade()?,
            combat_rng_seq: self.combat_rng_seq.upgrade()?,
            cluster_tick_last_unix: self.cluster_tick_last_unix.upgrade()?,
            cluster_tick_persisted_unix: self.cluster_tick_persisted_unix.upgrade()?,
            dedicated_physics_ticker_active: self.dedicated_physics_ticker_active.upgrade()?,
            fw_idle_last_step_unix: self.fw_idle_last_step_unix.upgrade()?,
        })
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ModuleCooldownEntry { pub recorded_unix_f64: f64, pub delay_s: f64, pub solar_system_id: Option<u32>, pub recorded_sys_sim_s: f64 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveFighterSquadron {
    pub ball_id: i64, pub item_id: i64, pub type_id: u32, pub owner_char_id: i64, pub parent_ship_id: i64,
    pub tube_index: usize, pub squadron_size: u32, pub target_id: Option<i64>, pub is_abandoned: bool, pub mwd_active: bool,
}

/// Consolidated carrier fighter runtime maps guarded under a single `RANK_LEAF` lock so
/// multi-map fighter operations cannot invert or trigger same-rank assertions (`RFC-1 §4.3`).
#[derive(Debug, Default)]
pub struct FighterRuntimeState {
    pub cooldown_state: HashMap<(i64, u32), u64>,
    pub charge_state: HashMap<(i64, u32), u32>,
    pub active_fighters: HashMap<i64, ActiveFighterSquadron>,
    pub tube_loads: HashMap<(i64, usize), i64>,
}

/// Safe, zero-`unsafe` projection wrapper exposing a single map inside [`FighterRuntimeState`]
/// with `.read()` / `.write()` ergonomics compatible with existing call sites and tests.
pub struct FighterStateLens<T: 'static> {
    lock: Arc<RankedRwLock<FighterRuntimeState, RANK_LEAF>>,
    project_ref: fn(&FighterRuntimeState) -> &T,
    project_mut: fn(&mut FighterRuntimeState) -> &mut T,
}

impl<T: 'static> Clone for FighterStateLens<T> {
    fn clone(&self) -> Self {
        Self {
            lock: Arc::clone(&self.lock),
            project_ref: self.project_ref,
            project_mut: self.project_mut,
        }
    }
}

pub struct FighterLensReadGuard<'a, T: 'static> {
    guard: RankedRwLockReadGuard<'a, FighterRuntimeState, RANK_LEAF>,
    project: fn(&FighterRuntimeState) -> &T,
}

impl<T: 'static> std::ops::Deref for FighterLensReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        (self.project)(&self.guard)
    }
}

pub struct FighterLensWriteGuard<'a, T: 'static> {
    guard: RankedRwLockWriteGuard<'a, FighterRuntimeState, RANK_LEAF>,
    project_ref: fn(&FighterRuntimeState) -> &T,
    project_mut: fn(&mut FighterRuntimeState) -> &mut T,
}

impl<T: 'static> std::ops::Deref for FighterLensWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        (self.project_ref)(&self.guard)
    }
}

impl<T: 'static> std::ops::DerefMut for FighterLensWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        (self.project_mut)(&mut self.guard)
    }
}

impl<T: 'static> FighterStateLens<T> {
    pub fn new(
        lock: Arc<RankedRwLock<FighterRuntimeState, RANK_LEAF>>,
        project_ref: fn(&FighterRuntimeState) -> &T,
        project_mut: fn(&mut FighterRuntimeState) -> &mut T,
    ) -> Self {
        Self {
            lock,
            project_ref,
            project_mut,
        }
    }

    #[track_caller]
    pub fn read(&self) -> LockResult<FighterLensReadGuard<'_, T>> {
        let project = self.project_ref;
        match (*self.lock).read() {
            Ok(guard) => Ok(FighterLensReadGuard { guard, project }),
            Err(poisoned) => Err(PoisonError::new(FighterLensReadGuard {
                guard: poisoned.into_inner(),
                project,
            })),
        }
    }

    #[track_caller]
    pub fn try_read(&self) -> TryLockResult<FighterLensReadGuard<'_, T>> {
        let project = self.project_ref;
        match self.lock.try_read() {
            Ok(guard) => Ok(FighterLensReadGuard { guard, project }),
            Err(TryLockError::Poisoned(poisoned)) => Err(TryLockError::Poisoned(PoisonError::new(
                FighterLensReadGuard {
                    guard: poisoned.into_inner(),
                    project,
                },
            ))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    #[track_caller]
    pub fn write(&self) -> LockResult<FighterLensWriteGuard<'_, T>> {
        let project_ref = self.project_ref;
        let project_mut = self.project_mut;
        match (*self.lock).write() {
            Ok(guard) => Ok(FighterLensWriteGuard {
                guard,
                project_ref,
                project_mut,
            }),
            Err(poisoned) => Err(PoisonError::new(FighterLensWriteGuard {
                guard: poisoned.into_inner(),
                project_ref,
                project_mut,
            })),
        }
    }

    #[track_caller]
    pub fn try_write(&self) -> TryLockResult<FighterLensWriteGuard<'_, T>> {
        let project_ref = self.project_ref;
        let project_mut = self.project_mut;
        match self.lock.try_write() {
            Ok(guard) => Ok(FighterLensWriteGuard {
                guard,
                project_ref,
                project_mut,
            }),
            Err(TryLockError::Poisoned(poisoned)) => Err(TryLockError::Poisoned(PoisonError::new(
                FighterLensWriteGuard {
                    guard: poisoned.into_inner(),
                    project_ref,
                    project_mut,
                },
            ))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }
}

pub(crate) fn make_fighter_lenses(
    fighters: &Arc<RankedRwLock<FighterRuntimeState, RANK_LEAF>>,
) -> (
    FighterStateLens<HashMap<(i64, u32), u64>>,
    FighterStateLens<HashMap<(i64, u32), u32>>,
    FighterStateLens<HashMap<i64, ActiveFighterSquadron>>,
    FighterStateLens<HashMap<(i64, usize), i64>>,
) {
    (
        FighterStateLens::new(
            Arc::clone(fighters),
            |s| &s.cooldown_state,
            |s| &mut s.cooldown_state,
        ),
        FighterStateLens::new(
            Arc::clone(fighters),
            |s| &s.charge_state,
            |s| &mut s.charge_state,
        ),
        FighterStateLens::new(
            Arc::clone(fighters),
            |s| &s.active_fighters,
            |s| &mut s.active_fighters,
        ),
        FighterStateLens::new(
            Arc::clone(fighters),
            |s| &s.tube_loads,
            |s| &mut s.tube_loads,
        ),
    )
}

pub type ActiveScannerProbe = eve_db::PersistedScannerProbe;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveMoonSurveyProbe {
    pub probe_id: i64, pub probe_type_id: u32, pub owner_char_id: i64, pub solar_system_id: u32, pub hit_moon_id: Option<i64>, pub arrival_unix_secs: u64,
}

impl Default for ServerContext { fn default() -> Self { Self::new() } }

impl ServerContext {
    pub(crate) fn default_cataloged_services() -> &'static Arc<HashSet<String>> {
        static CATALOGED_SERVICES: std::sync::OnceLock<Arc<HashSet<String>>> =
            std::sync::OnceLock::new();
        CATALOGED_SERVICES.get_or_init(|| {
            let catalog_map: HashMap<String, serde_json::Value> =
                serde_json::from_str(EMBEDDED_RPC_CATALOG_JSON).unwrap_or_default();
            Arc::new(catalog_map.into_keys().collect())
        })
    }

    pub(crate) fn default_quasar_domains() -> &'static Arc<HashSet<String>> {
        static QUASAR_DOMAINS: std::sync::OnceLock<Arc<HashSet<String>>> =
            std::sync::OnceLock::new();
        QUASAR_DOMAINS.get_or_init(|| {
            let quasar_map: HashMap<String, serde_json::Value> =
                serde_json::from_str(EMBEDDED_QUASAR_CATALOG_JSON).unwrap_or_default();
            Arc::new(quasar_map.into_keys().collect())
        })
    }

    #[inline]
    pub fn feature_flags_snapshot(&self) -> crate::config::FeatureFlagsConfig {
        self.feature_flags
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[inline]
    pub fn esi_port(&self) -> u16 {
        self.esi_port.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_esi_port(&self, port: u16) {
        self.esi_port.store(port, Ordering::Release);
    }

    #[inline]
    pub fn xmpp_port(&self) -> u16 {
        self.xmpp_port.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_xmpp_port(&self, port: u16) {
        self.xmpp_port.store(port, Ordering::Release);
    }

    /// Creates a non-owning weak handle referencing all shared runtime state on this `ServerContext`
    /// (`RFC-1 E-4`, `#839`, `#919`).
    pub(crate) fn downgrade(&self) -> ServerContextWeak {
        ServerContextWeak {
            instance_id: self.instance_id,
            sde: Arc::downgrade(&self.sde),
            db: Arc::downgrade(&self.db),
            clock: Arc::downgrade(&self.clock),
            systems: Arc::downgrade(&self.systems),
            living_world: Arc::downgrade(&self.living_world),
            quasar_gateway: Arc::downgrade(&self.quasar_gateway),
            cataloged_services: Arc::downgrade(&self.cataloged_services),
            quasar_domains: Arc::downgrade(&self.quasar_domains),
            bound_objects: Arc::downgrade(&self.bound_objects),
            object_cache: Arc::downgrade(&self.object_cache),
            object_cache_timestamps: Arc::downgrade(&self.object_cache_timestamps),
            notification_records: Arc::downgrade(&self.notification_records),
            pending_notifications: Arc::downgrade(&self.pending_notifications),
            notify_tx: self.notify_tx.clone(),
            map_landmarks: Arc::downgrade(&self.map_landmarks),
            session_closers: Arc::downgrade(&self.session_closers),
            ship_fittings: Arc::downgrade(&self.ship_fittings),
            projected_ewar: Arc::downgrade(&self.projected_ewar),
            projected_ewar_sources: Arc::downgrade(&self.projected_ewar_sources),
            active_ewar_starts: Arc::downgrade(&self.active_ewar_starts),
            weapon_cycle_state: Arc::downgrade(&self.weapon_cycle_state),
            fighters: Arc::downgrade(&self.fighters),
            active_scanner_probes: Arc::downgrade(&self.active_scanner_probes),
            active_moon_probes: Arc::downgrade(&self.active_moon_probes),
            scanned_signatures: Arc::downgrade(&self.scanned_signatures),
            character_scan_progress: Arc::downgrade(&self.character_scan_progress),
            invention_roll_override: Arc::downgrade(&self.invention_roll_override),
            mechanic_rng: Arc::downgrade(&self.mechanic_rng),
            module_reactivation_cooldowns: Arc::downgrade(&self.module_reactivation_cooldowns),
            mining_cycles: Arc::downgrade(&self.mining_cycles),
            phased_asteroid_fields: Arc::downgrade(&self.phased_asteroid_fields),
            crimewatch: Arc::downgrade(&self.crimewatch),
            combat_ephemeral: Arc::downgrade(&self.combat_ephemeral),
            tactical_state: Arc::downgrade(&self.tactical_state),
            session_change_timers: Arc::downgrade(&self.session_change_timers),
            active_warps: Arc::downgrade(&self.active_warps),
            return_warps: Arc::downgrade(&self.return_warps),
            location_change_invuln: Arc::downgrade(&self.location_change_invuln),
            feature_flags: Arc::downgrade(&self.feature_flags),
            esi_port: Arc::downgrade(&self.esi_port),
            xmpp_port: Arc::downgrade(&self.xmpp_port),
            esi_state: Arc::downgrade(&self.esi_state),
            fleet_extras: Arc::downgrade(&self.fleet_extras),
            social_extras: Arc::downgrade(&self.social_extras),
            cluster_misc_state: Arc::downgrade(&self.cluster_misc_state),
            cluster_state: Arc::downgrade(&self.cluster_state),
            global_config_values: Arc::downgrade(&self.global_config_values),
            alert_telemetry: Arc::downgrade(&self.alert_telemetry),
            flight_recorder: Arc::downgrade(&self.flight_recorder),
            bug_reporting: Arc::downgrade(&self.bug_reporting),
            npc_spawns: Arc::downgrade(&self.npc_spawns),
            combat_rng_seq: Arc::downgrade(&self.combat_rng_seq),
            cluster_tick_last_unix: Arc::downgrade(&self.cluster_tick_last_unix),
            cluster_tick_persisted_unix: Arc::downgrade(&self.cluster_tick_persisted_unix),
            dedicated_physics_ticker_active: Arc::downgrade(&self.dedicated_physics_ticker_active),
            fw_idle_last_step_unix: Arc::downgrade(&self.fw_idle_last_step_unix),
        }
    }

    /// Initializes a `ServerContext` with the provided `WorldRepository` and `SdeRegistry`, and seeds NPC regional markets.
    pub fn with_sde_and_repository(sde: Arc<SdeRegistry>, db: Arc<WorldRepository>) -> Self {
        let _ = world_seeder::seed_dual_anchor_markets(&db, &sde);
        let clock = db.clock();
        let cataloged_services = Arc::clone(Self::default_cataloged_services());
        let quasar_domains = Arc::clone(Self::default_quasar_domains());
        let (notify_tx, _) = tokio::sync::broadcast::channel(256);
        let instance_id = NEXT_CTX_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pending_notifications = Arc::new(RankedRwLock::new(HashMap::new()));
        let ship_fittings = Arc::new(RankedRwLock::new(HashMap::new()));
        let projected_ewar = Arc::new(RankedRwLock::new(HashMap::new()));
        let projected_ewar_sources = Arc::new(RankedRwLock::new(HashMap::new()));
        let active_ewar_starts = Arc::new(RankedRwLock::new(HashMap::new()));
        let invention_roll_override = Arc::new(RankedRwLock::new(None));
        let mechanic_rng = Arc::new(crate::rng::MechanicRng::os());
        let tactical_state = Arc::new(RankedRwLock::new(
            crate::services::tactical_modules::TacticalModulesState::default(),
        ));
        let systems = Arc::new(
            SystemRegistry::with_sde_and_db(sde.clone(), Arc::clone(&db))
                .with_mechanic_rng(Arc::clone(&mechanic_rng))
                .with_tactical_state(Arc::clone(&tactical_state))
                .with_notifications(Arc::clone(&pending_notifications), notify_tx.clone()),
        );
        let combat_ephemeral = {
            let mut rt = crate::services::combat_crimewatch::CombatRuntimeState::default();
            db.with_snapshot(|snap| rt.sync_from_persisted(&snap.combat_crimewatch_state));
            Arc::new(RankedRwLock::new(rt))
        };
        let quasar_gateway = Arc::new(crate::quasar_gateway::PublicGatewayRouter::new(
            eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty(),
            Arc::clone(&db),
            Arc::clone(&sde),
        ));
        let cluster_misc_arc = Arc::new(RankedMutex::new(
            crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&db),
        ));
        let module_reactivation_cooldowns = Arc::new(RankedRwLock::new(HashMap::new()));
        let fighters = Arc::new(RankedRwLock::new(FighterRuntimeState::default()));
        let persisted_notifications = db.with_snapshot(|snap| {
            let mut list: Vec<crate::router::cache_notifications::NotificationRecord> = snap.owner_notes.values()
                .filter(|n| n.character_id == -79)
                .filter_map(|n| serde_json::from_str(&n.note).ok())
                .collect();
            list.sort_by_key(|r| r.notification_id);
            list
        });
        if db.is_ephemeral_in_memory() {
            db.with_mut_snapshot(|snap| {
                snap.next_corporation_id = snap.next_corporation_id.max(98_100_000 + (instance_id as i64) * 10_000);
                snap.next_alliance_id = snap.next_alliance_id.max(99_000_000 + (instance_id as i64) * 1_000);
            });
        }
        let crimewatch = Arc::new(crate::combat::crimewatch::CrimeWatchLock::new(Arc::clone(&db)));
        let _cluster_misc = crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&db);
        let persisted_global_config: HashMap<String, eve_proto::PyRep> = db.with_snapshot(|snap| {
            snap.cluster_and_runtime.macho_global_config.clone()
        });
        let persisted_probes: HashMap<i64, ActiveScannerProbe> = db.with_snapshot(|snap| {
            crate::sp6_lifecycle::load_sp6_state_from_snap(snap)
                .active_scanner_probes
                .into_iter()
                .filter(|(_, p)| p.state >= 0)
                .map(|(id, mut p)| {
                    // On cluster restart, launched probes lose connection (INACTIVE = 0) and
                    // cancel any in-flight warp timer so ReconnectToLostProbes recovers them.
                    if p.state > 0 {
                        p.state = 0;
                    }
                    p.warp_end_ft = None;
                    (id, p)
                })
                .collect()
        });
        let (fighter_cooldown_state, fighter_charge_state, active_fighters, fighter_tube_loads) =
            make_fighter_lenses(&fighters);
        let ctx = Self {
            instance_id,
            sde,
            db,
            clock: Arc::new(RankedRwLock::new(clock)),
            systems,
            living_world: Arc::new(LivingWorldDirector::new()),
            quasar_gateway,
            cataloged_services,
            quasar_domains,
            bound_objects: Arc::new(RankedRwLock::new(HashMap::new())),
            object_cache: Arc::new(RankedRwLock::new(HashMap::new())),
            object_cache_timestamps: Arc::new(RankedRwLock::new(HashMap::new())),
            notification_records: Arc::new(RankedRwLock::new(persisted_notifications)),
            pending_notifications,
            notify_tx,
            map_landmarks: Arc::new(RankedRwLock::new(Vec::new())),
            session_closers: Arc::new(RankedRwLock::new(HashMap::new())),
            ship_fittings,
            projected_ewar,
            projected_ewar_sources,
            active_ewar_starts,
            weapon_cycle_state: Arc::new(RankedRwLock::new(HashMap::new())),
            fighters,
            fighter_cooldown_state,
            fighter_charge_state,
            active_fighters,
            fighter_tube_loads,
            active_scanner_probes: Arc::new(RankedRwLock::new(persisted_probes)),
            active_moon_probes: Arc::new(RankedRwLock::new(Vec::new())),
            scanned_signatures: Arc::new(RankedRwLock::new(HashSet::new())),
            character_scan_progress: Arc::new(RankedRwLock::new(HashMap::new())),
            invention_roll_override,
            mechanic_rng,
            module_reactivation_cooldowns,
            mining_cycles: Arc::new(RankedRwLock::new(HashMap::new())),
            phased_asteroid_fields: Arc::new(RankedRwLock::new(HashMap::new())),
            crimewatch,
            combat_ephemeral,
            tactical_state,
            session_change_timers: Arc::new(RankedMutex::new(HashMap::new())),
            active_warps: Arc::new(RankedMutex::new(HashMap::new())),
            return_warps: Arc::new(RankedMutex::new(HashSet::new())),
            location_change_invuln: Arc::new(RankedMutex::new(HashMap::new())),
            feature_flags: Arc::new(RwLock::new(crate::config::FeatureFlagsConfig::default())),
            esi_port: Arc::new(AtomicU16::new(26_004)),
            xmpp_port: Arc::new(AtomicU16::new(5222)),
            esi_state: Arc::new(crate::esi::runtime_state::EsiRuntimeState::new()),
            fleet_extras: Arc::new(RankedMutex::new(Default::default())),
            social_extras: Arc::new(RankedMutex::new(crate::services::fleet_social::fleet_social_chat::SocialExtrasState::new())),
            cluster_misc_state: cluster_misc_arc,
            cluster_state: Arc::new(RankedRwLock::new(crate::services::macho_cluster_mgmt::MachoClusterState::default())),
            global_config_values: Arc::new(RankedRwLock::new(persisted_global_config)),
            alert_telemetry: Arc::new(RankedMutex::new(crate::services::client_login::AlertTelemetryState::default())),
            flight_recorder: Arc::new(crate::flight_recorder::FlightRecorder::new()),
            bug_reporting: Arc::new(crate::bug_reporting::BugReportingRuntime::new()),
            npc_spawns: Arc::new(RankedRwLock::new(HashMap::new())),
            combat_rng_seq: Arc::new(AtomicU64::new(1)),
            cluster_tick_last_unix: Arc::new(AtomicU64::new(0)),
            cluster_tick_persisted_unix: Arc::new(AtomicU64::new(0)),
            dedicated_physics_ticker_active: Arc::new(AtomicBool::new(false)),
            fw_idle_last_step_unix: Arc::new(RankedRwLock::new(HashMap::new())),
        };
        let npc_ctx_w = ctx.downgrade();
        ctx.systems.set_npc_death_hook(Arc::new(move |sys_id, npc_id, killer_opt| {
            let ctx_w = npc_ctx_w.clone();
            Box::pin(async move {
                let Some(ctx) = ctx_w.upgrade() else { return; };
                let sys = &ctx.systems;
                let db = &ctx.db;
                let sde = &ctx.sde;
                let tx = &ctx.notify_tx;
                let pw_any: Option<&dyn AnyRwLock<_>> = Some(&*ctx.pending_notifications);
                let inc = crate::services::pve_incursion_sites::on_incursion_boss_destroyed_core(sys, db, sde, sys_id, npc_id, killer_opt).await;
                if let Some(Ok(r)) = &inc {
                    crate::services::pve_incursion_sites::notify_incursion_site_completed(&ctx.quasar_gateway, db, sys_id, r);
                    crate::services::notify_emitters_tournaments_anomalies::emit_incursion_completion_combat_notifications(
                        db,
                        sde,
                        pw_any,
                        Some(tx),
                        r,
                    );
                }
                let acct_changes = crate::services::pve_special_sites::on_special_site_npc_destroyed_core(sys, db, sde, sys_id, npc_id, killer_opt).await;
                let _ = crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(sys, db, sys_id, npc_id).await;
                crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(sys, db, sys_id, npc_id, killer_opt).await;
                crate::services::pve_special_sites::notify_special_site_account_changes(db, &*ctx.pending_notifications, tx, &acct_changes);
                crate::services::agents_missions::on_mission_npc_destroyed_core(db, sys_id, npc_id, killer_opt);
                crate::services::shipcaster_ops::on_landing_pad_ball_destroyed_core(
                    &ctx.cluster_misc_state,
                    sys,
                    db,
                    sde,
                    pw_any,
                    tx,
                    npc_id,
                )
                .await;
                crate::services::combat_npc_encounters::handle_npc_destroyed_in_space(sys, db, sde, &*ctx.combat_ephemeral, sys_id, npc_id, killer_opt).await;
                crate::services::fw_gameplay::on_fw_complex_guard_destroyed(sys, db, sys_id, npc_id).await;
                if let Some(kid) = killer_opt {
                    let now_s = db.clock().now_unix_secs();
                    let (clr, esc, step) = crate::services::pve_exploration::handle_pve_site_npc_destruction_core(sys, db, sde, &ctx.mechanic_rng, &*ctx.combat_ephemeral, sys_id, npc_id, kid, true).await;
                    ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(kid as u32, &["destroyHostileNPC"], 1, now_s, &ctx.quasar_gateway);
                    if let Some(ref sid) = clr {
                        let (cat, did) = crate::services::pve_exploration::pve_site_category_and_dungeon_id(db, now_s, sys_id, sid);
                        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(sys_id, &cat, did, 1, now_s, &ctx.quasar_gateway);
                    }
                    crate::services::pve_exploration::notify_pve_destruction_events(db, &*ctx.pending_notifications, tx, kid, clr, esc, step);
                }
                crate::services::pve_dungeon_runtime::on_dungeon_npc_killed_core(
                    sys,
                    db,
                    sde,
                    pw_any,
                    Some(tx),
                    sys_id,
                    npc_id,
                    killer_opt.unwrap_or(0),
                )
                .await;
                if let Some((sh_info, res_opt)) = crate::services::insurgency_services::on_insurgency_stronghold_destroyed_with_sde(sys, db, Some(sde), sys_id, npc_id, killer_opt).await {
                    if let Some(res) = res_opt {
                        crate::services::fw_gameplay::fw_gameplay_insurgency::emit_campaign_resolution_effects_core(
                            db,
                            Some(&*ctx.quasar_gateway),
                            pw_any,
                            Some(tx),
                            &res,
                        );
                    }
                    crate::services::insurgency_services::notify_stronghold_destruction(
                        db,
                        Some(&*ctx.quasar_gateway),
                        pw_any,
                        Some(tx),
                        &sh_info,
                    );
                }
            })
        }));
        let step_ctx_w = ctx.downgrade();
        ctx.systems.set_system_step_hook(Arc::new(move |sys_id, dt, destroyed_balls| {
            let ctx_w = step_ctx_w.clone();
            Box::pin(async move {
                let Some(ctx) = ctx_w.upgrade() else { return; };
                for &(tid, killer_opt) in &destroyed_balls {
                    crate::services::fw_gameplay::on_space_entity_destroyed(&ctx, sys_id, killer_opt, tid, 0).await;
                    crate::services::notify_emitters_seasons_tactical::on_deployable_deactivated(&ctx, None, tid);
                    crate::services::shipcaster_ops::on_landing_pad_ball_destroyed(&ctx, tid).await;
                    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(&ctx, 0, Some(sys_id), tid);
                    let _ = crate::services::wormhole_pve_escalations::on_decloaked_relay_attacked_sync(&ctx, sys_id, tid);
                    if let Ok(Some(death_res)) = crate::combat::death_pipeline::on_ball_destroyed_sync(
                        &ctx,
                        sys_id,
                        tid,
                        killer_opt,
                        glam::DVec3::ZERO,
                    ) {
                        if let Some(vcid) = ctx.db.with_snapshot(|s| {
                            death_res
                                .ejected_capsule_id
                                .and_then(|cid_ship| s.items.get(&cid_ship).map(|it| it.owner_id))
                                .or_else(|| {
                                    s.killmails
                                        .get(&death_res.killmail_id)
                                        .map(|km| km.victim_character_id)
                                })
                        }) {
                            crate::services::pve_abyssal_pylons::on_abyssal_boundary_death_hook(
                                &ctx,
                                sys_id,
                                vcid,
                                tid,
                                death_res.ejected_capsule_id,
                            );
                        }
                    }
                }
                crate::services::combat_weapons_spool::step_system_entropic_spools(&ctx, sys_id, dt).await;
                crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;
                if !destroyed_balls.is_empty() {
                    let now_unix = ctx.now_unix();
                    if let Ok(mut spawns) = ctx.npc_spawns.write() {
                        if let Some(mgr) = spawns.get_mut(&sys_id) {
                            for (tid, killer_opt) in destroyed_balls {
                                if mgr.entities.contains_key(&tid) {
                                    let killer_char_id = killer_opt
                                        .and_then(|kid| {
                                            ctx.db.with_snapshot(|s| {
                                                if s.characters.contains_key(&kid) {
                                                    Some(kid)
                                                } else {
                                                    s.characters
                                                        .values()
                                                        .find(|c| c.active_ship_id == kid)
                                                        .map(|c| c.character_id)
                                                }
                                            })
                                        })
                                        .unwrap_or(0);
                                    let _ = mgr.handle_npc_destroyed(&ctx, sys_id, tid, killer_char_id, now_unix);
                                }
                            }
                        }
                    }
                }
                crate::services::structure_control_ops::step_structure_auto_defense(&ctx, sys_id, dt).await;
                crate::services::pve_crab_beacons::tick_crab_beacons_in_system(&ctx, sys_id).await;
                crate::services::wormhole_pve_escalations::step_wormhole_pve_system(&ctx, sys_id, dt).await;
                crate::services::ice_heist_ops::step_ice_heist_system(&ctx, sys_id).await;
                crate::services::doomsday_aoe_ops::step_bosonic_doomsday_system(&ctx, sys_id).await;
                crate::services::structure_standup_weapons::step_guided_bombs_in_system(&ctx, sys_id, dt).await;
            })
        }));
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.clear();
            for item in snap.items.values_mut() {
                if !item.dynamic_attributes.contains_key(&2114) && ctx.sde.types.get(&item.type_id).is_some_and(|t| t.category_id == 6) {
                    for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112, 113, 1175, 1176, 1177] { item.dynamic_attributes.remove(&aid); }
                }
            }
        });
        crate::services::fleet_social::hydrate_social_state_from_db(&ctx);
        crate::services::fleet_social::fleet_social_notify::prune_offline_fleets_and_channels(&ctx);
        ctx.hydrate_server_runtime_from_db();
        crate::services::combat_crimewatch::sync_combat_runtime_from_db(&ctx);
        crate::sp6_lifecycle::seed_persisted_abandoned_drones(&ctx);
        crate::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
        ctx.install_tx_post_commit_listener(); ctx.wire_module_deactivation_hook();
        ctx
    }

    /// Initializes a `ServerContext` with the provided `WorldRepository` and seeds the NPC regional markets.
    pub fn with_repository(db: Arc<WorldRepository>) -> Self {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        Self::with_sde_and_repository(sde, db)
    }

    /// Initializes a default `ServerContext` for unit tests.
    pub fn for_tests() -> Self {
        Self::new()
    }

    /// Alias for `drain_notifications_for_char` used by unit tests.
    pub fn drain_notifications(
        &self,
        char_id: i64,
    ) -> Vec<crate::router::cache_notifications::PendingMachoNotification> {
        self.drain_notifications_for_char(char_id)
    }

    /// Applies active dbuffs and system effects to a ship's evaluated dogma attribute map.
    pub fn apply_active_dbuffs_and_system_effects_to_ship_attrs(
        &self,
        ship_id: i64,
        attrs: &mut HashMap<u16, f64>,
    ) {
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            self,
            ship_id,
            attrs,
        );
    }

    /// Initializes a `ServerContext` backed by a SQLite world database at `path`.
    pub fn from_sqlite(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let p = path.as_ref();
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("sqlite" | "sqlite3" | "db")) {
            return Err(format!(
                "world_db_path '{}' must use a SQLite file extension (.sqlite, .sqlite3, or .db)",
                p.display()
            ));
        }
        let repo = WorldRepository::open_sqlite(p)?;
        repo.ensure_auth_sqlite_connection()?;
        let db = Arc::new(repo);
        Ok(Self::with_repository(db))
    }

    /// Initializes a `ServerContext` from a [`crate::config::ServerConfig`] (`D-5`, `#877`).
    pub fn from_config(cfg: &crate::config::ServerConfig) -> Result<Self, String> {
        let backend = cfg.validate_storage_backend()?;
        let sde = match SdeRegistry::open(&cfg.sde_path) {
            Ok(s) => Arc::new(s),
            #[cfg(test)]
            Err(_) if cfg.sde_path == "data/evomu_sde.sqlite" => {
                Arc::new(SdeRegistry::with_canonical_seed())
            }
            Err(e) => return Err(e.to_string()),
        };
        let db = match backend {
            "sqlite" => {
                tracing::info!(
                    storage_backend = "sqlite",
                    world_db_path = %cfg.world_db_path,
                    "opening SQLite world repository"
                );
                let repo = WorldRepository::open_sqlite(&cfg.world_db_path)?;
                repo.ensure_auth_sqlite_connection()?;
                Arc::new(repo)
            }
            "memory" => {
                tracing::info!(
                    storage_backend = "memory",
                    "opening ephemeral in-memory world repository"
                );
                Arc::new(WorldRepository::new_empty_with_snapshot_path(None)?)
            }
            other => {
                return Err(format!("Unsupported storage backend '{other}'"));
            }
        };
        for p in &cfg.player_accounts {
            if !p.username.trim().is_empty() && !p.password_token.trim().is_empty() {
                db.provision_player_account(p.username.trim(), p.password_token.trim());
            }
        }
        for a in &cfg.admin_accounts {
            if !a.username.trim().is_empty() && !a.password_token.trim().is_empty() {
                db.provision_gm_account(a.username.trim(), a.password_token.trim(), a.character_id, a.character_name.trim());
            }
        }
        let ctx = Self::with_sde_and_repository(sde, db);
        ctx.systems.set_belt_respawn_interval_secs(cfg.belt_respawn_interval_secs);
        ctx.systems.set_downtime_utc_secs_of_day(cfg.downtime_utc_secs_of_day);
        *ctx.feature_flags.write().unwrap_or_else(|e| e.into_inner()) = cfg.feature_flags.clone();
        if !ctx
            .quasar_gateway
            .has_persisted_free_fast_checkout_plex
            .load(Ordering::Relaxed)
        {
            ctx.quasar_gateway.free_fast_checkout_plex.store(
                cfg.feature_flags.free_fast_checkout_plex,
                Ordering::Relaxed,
            );
        } else {
            ctx.feature_flags
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .free_fast_checkout_plex = ctx
                .quasar_gateway
                .free_fast_checkout_plex
                .load(Ordering::Relaxed);
        }
        ctx.set_esi_port(cfg.esi_port);
        ctx.set_xmpp_port(cfg.xmpp_port);
        if cfg.feature_flags.seed_demo {
            ctx.seed_demo_world_fixtures();
            ctx.db.with_mut_snapshot(|snap| {
                crate::services::quasar_payment_store::ensure_store_catalog_seeded(snap, &ctx.sde);
            });
        }
        Ok(ctx)
    }

    pub fn set_free_fast_checkout_plex(&mut self, enabled: bool) {
        self.feature_flags
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .free_fast_checkout_plex = enabled;
        self.quasar_gateway
            .free_fast_checkout_plex
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self.quasar_gateway
            .has_persisted_free_fast_checkout_plex
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.quasar_gateway.sync_meta_to_db();
    }

    /// Explicitly seeds demo convoys (`800_000_001..=800_000_003`), nullsec ESS bank balances (`10M`/`25M` ISK),
    /// and demo wormhole/FW-complex fixtures when `--seed-demo` or a test fixture explicitly requests them (`S-5`).
    pub fn seed_demo_world_fixtures(&self) {
        self.living_world.seed_default_convoys();
        self.db.with_mut_snapshot(|snap| {
            if snap.characters.is_empty() {
                eve_db::WorldRepository::populate_demo_snapshot(snap);
            }
            if let Some(st) = snap.system_states.get_mut(&30_002_813) {
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(31_000_005);
                st.signature_position = Some([15_000.0, 0.0, 0.0]);
                st.fw_complex_position = Some([250_000.0, 0.0, -150_000.0]);
                st.fw_contested_pct = 45.0;
                st.fw_capture_seconds_elapsed = 270;
                st.fw_capture_duration_seconds = 600;
            }
            if let Some(st) = snap.system_states.get_mut(&31_000_005) {
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(30_002_904);
                st.signature_position = Some([10_500.0, 0.0, 0.0]);
            }
            if let Some(st) = snap.system_states.get_mut(&30_002_904) {
                st.ess_main_bank = eve_db::IskCents::from_isk(10_000_000);
                st.ess_reserve_bank = eve_db::IskCents::from_isk(25_000_000);
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(30_002_813);
                st.signature_position = Some([12_000.0, 0.0, 0.0]);
            }
            snap.items.entry(100_000_200).or_insert_with(|| eve_db::InventoryItemRecord {
                item_id: 100_000_200,
                type_id: 53977, // Noise-5 'Needlejack' Filament
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: std::collections::HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            });
        });
    }

    /// Initializes the production server context (`R22-P1-5`, `D-4`): always starts with an empty
    /// account/character table (`WorldRepository::new_empty()`) without demo secrets, and provisions
    /// operator-supplied player and admin accounts from `EVOMU_ACCOUNTS` / `EVOMU_GM_ACCOUNT`.
    pub fn new_production() -> Self {
        let _env_read_guard = acquire_test_env_read_guard();
        let db = Arc::new(WorldRepository::new_empty());
        if let Ok(accounts_spec) = std::env::var("EVOMU_ACCOUNTS") {
            for entry in accounts_spec.split(',') {
                if let Some((user, pass)) = entry.trim().split_once(':') {
                    let u = user.trim();
                    let p = pass.trim();
                    if !u.is_empty() && !p.is_empty() {
                        db.provision_player_account(u, p);
                    }
                }
            }
        }
        if let Ok(gm_spec) = std::env::var("EVOMU_GM_ACCOUNT") {
            if let Some((user, pass)) = gm_spec.trim().split_once(':') {
                let u = user.trim();
                let p = pass.trim();
                if !u.is_empty() && !p.is_empty() {
                    db.provision_gm_account(u, p, 90_000_003, "ISD Polaris");
                }
            }
        }
        Self::with_repository(db)
    }

    pub fn new() -> Self {
        let _env_read_guard = acquire_test_env_read_guard();
        #[cfg(any(test, feature = "demo-seed"))]
        let db = {
            let disable_demo = std::env::var("EVOMU_DISABLE_DEMO_SEED").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
            if disable_demo { Arc::new(WorldRepository::new_empty()) } else { Arc::new(WorldRepository::with_demo_seed()) }
        };
        #[cfg(not(any(test, feature = "demo-seed")))]
        let db = Arc::new(WorldRepository::new_empty());
        let ctx = Self::with_repository(db);
        #[cfg(any(test, feature = "demo-seed"))]
        let ctx = ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        ctx
    }

    pub fn mark_signature_scanned(&self, char_id: i64, solar_system_id: u32, sig_ball_id: i64) {
        let inserted = self
            .scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, solar_system_id, sig_ball_id));
        if inserted {
            self.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id as u32, 3, 1, &self.quasar_gateway);
            self.quasar_gateway.progression.career_goals.on_gameplay_event(char_id as u32, "exploration", 1, &self.quasar_gateway);
            self.quasar_gateway.progression.extra.on_campaign_gameplay_event(char_id as u32, crate::services::quasar_campaign_state::CampaignEvent { method: "ScanSignature", type_id: None, solar_system_id: Some(solar_system_id), faction_id: None }, 1, self.now_unix(), &self.quasar_gateway);
            crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(self, char_id, &["ScanSignatures"], 1, Some(solar_system_id as i64));
        }
    }

    pub fn has_scanned_signature(
        &self,
        char_id: i64,
        solar_system_id: u32,
        sig_ball_id: i64,
    ) -> bool {
        self.scanned_signatures
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(char_id, solar_system_id, sig_ball_id))
    }

    pub fn record_character_scan_progress(
        &self,
        char_id: i64,
        solar_system_id: u32,
        signature_id: &str,
        strength: f64,
    ) {
        self.character_scan_progress
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (char_id, solar_system_id, signature_id.to_string()),
                strength,
            );
    }

    pub fn get_character_scan_progress(
        &self,
        char_id: i64,
        solar_system_id: u32,
        signature_id: &str,
    ) -> Option<f64> {
        self.character_scan_progress
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(char_id, solar_system_id, signature_id.to_string()))
            .copied()
    }

    pub fn effective_signature_signal_strength(
        &self,
        char_id: i64,
        solar_system_id: u32,
        sig: &eve_db::CosmicSignatureRecord,
    ) -> f64 {
        self.get_character_scan_progress(char_id, solar_system_id, &sig.signature_id)
            .unwrap_or(sig.signal_strength)
    }

    pub fn character_skill_levels(&self, char_id: i64) -> HashMap<u32, u8> {
        let now_ft = self.now_blueos_filetime_ticks();
        self.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id);
            let is_alpha = ch.map(|c| c.clone_grade <= 1).unwrap_or(false);
            let race_id = ch.map(|c| c.race_id).unwrap_or(1);
            let mut levels: HashMap<u32, u8> = snap
                .skills
                .get(&char_id)
                .map(|m| {
                    m.iter()
                        .map(|(&tid, rec)| {
                            let lvl = if is_alpha {
                                let cap = crate::services::character_skills::alpha_skill_cap_for_character(
                                    &self.sde, race_id, tid,
                                );
                                rec.level.min(cap)
                            } else {
                                rec.level
                            };
                            (tid, lvl)
                        })
                        .collect()
                })
                .unwrap_or_default();

            let es_skills = crate::services::character_expert_systems::get_active_expert_system_skills(
                snap, char_id, now_ft,
            );
            for (skill_id, level) in es_skills {
                let entry = levels.entry(skill_id).or_insert(0);
                *entry = (*entry).max(level);
            }
            levels
        })
    }

    fn resolve_ship_cooldown_clock(&self, ship_id: i64, module_id: i64) -> (u32, Option<u32>, f64) {
        let (type_id, db_sys_opt) = self.db.with_snapshot(|s| {
            let tid = s.items.get(&module_id).map(|i| i.type_id).unwrap_or(0);
            let char_sys = s
                .characters
                .values()
                .find(|c| c.active_ship_id == ship_id)
                .map(|c| c.solar_system_id);
            let item_sys = s.items.get(&ship_id).and_then(|it| {
                (30_000_000..=33_000_000)
                    .contains(&it.location_id)
                    .then_some(it.location_id as u32)
            });
            (tid, char_sys.or(item_sys))
        });
        let sys_opt = self.systems.find_ball_system_sync(ship_id).or(db_sys_opt);
        let cur_sys_sim_s = sys_opt
            .map(|sid| self.systems.get_system_sim_elapsed(sid))
            .unwrap_or(0.0);
        (type_id, sys_opt, cur_sys_sim_s)
    }

    fn remaining_cooldown_secs(
        entry: &ModuleCooldownEntry,
        now_unix_f64: f64,
        sys_opt: Option<u32>,
        cur_sys_sim_s: f64,
    ) -> f64 {
        let wall_elapsed = (now_unix_f64 - entry.recorded_unix_f64).max(0.0);
        let sim_elapsed = if entry.solar_system_id.is_some() && entry.solar_system_id == sys_opt {
            (cur_sys_sim_s - entry.recorded_sys_sim_s).max(0.0)
        } else {
            0.0
        };
        entry.delay_s - wall_elapsed.max(sim_elapsed)
    }

    /// Checks if a module on a ship is currently under reactivation delay (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn is_module_in_reactivation_delay(&self, ship_id: i64, module_id: i64) -> bool {
        let now = self.now_unix_secs() as f64;
        let (type_id, sys_opt, cur_sys_sim_s) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let guard = self
            .module_reactivation_cooldowns
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&(ship_id, module_id, type_id)) {
            Self::remaining_cooldown_secs(entry, now, sys_opt, cur_sys_sim_s) > 1e-6
        } else {
            false
        }
    }

    /// Records module reactivation delay until `now + delay_s` (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn record_module_reactivation_delay(&self, ship_id: i64, module_id: i64, delay_s: f64) {
        if delay_s <= 0.0 {
            return;
        }
        let now = self.now_unix_secs() as f64;
        let (type_id, sys_opt, cur_sys_sim_s) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let mut guard = self
            .module_reactivation_cooldowns
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let effective_delay = guard
            .get(&(ship_id, module_id, type_id))
            .map(|e| Self::remaining_cooldown_secs(e, now, sys_opt, cur_sys_sim_s).max(delay_s))
            .unwrap_or(delay_s);
        guard.insert(
            (ship_id, module_id, type_id),
            ModuleCooldownEntry {
                recorded_unix_f64: now,
                delay_s: effective_delay,
                solar_system_id: sys_opt,
                recorded_sys_sim_s: cur_sys_sim_s,
            },
        );
        drop(guard);
        self.persist_server_runtime_to_db();
    }

    /// Returns the active [`eve_db::ServerClock`] implementation (`T-7`).
    pub fn clock(&self) -> Arc<dyn eve_db::ServerClock> {
        self.clock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Injects a custom [`eve_db::ServerClock`] into both `ServerContext` and `WorldRepository` (`T-7`).
    pub fn set_clock(&self, clock: Arc<dyn eve_db::ServerClock>) {
        let now_u = clock.now_unix_secs();
        self.db.set_clock(clock.clone());
        *self.clock.write().unwrap_or_else(|e| e.into_inner()) = clock;
        self.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("__server_now_unix".into(), now_u.to_string());
        });
    }

    /// Returns the current server Unix timestamp in seconds (`T-7`).
    pub fn now_unix_secs(&self) -> u64 {
        self.clock().now_unix_secs()
    }

    /// Alias for [`Self::now_unix_secs`] (`T-7`).
    pub fn now_unix(&self) -> u64 {
        self.now_unix_secs()
    }

    /// Synchronizes a live `SessionState` with current DB character spatial and vessel state.
    pub fn sync_session(&self, session: &mut eve_machonet::SessionState) {
        crate::services::structure_control_ops::sync_character_session(self, session);
    }

    /// Advances virtual server time by `secs` seconds and runs time-driven skill queue,
    /// jetcan despawn (3600s lifetime), moon field natural decay progression (`T-7`), and
    /// SP-6 / SP-7 background cluster lifecycle timers (`plan:SP-6`, `plan:SP-7`).
    pub fn advance_secs(&self, secs: u64) {
        self.advance_secs_inner(secs, true);
    }

    /// Advances the server clock by `secs` seconds without emitting `DoSimClockRebase`.
    ///
    /// Used by `timeControl.AdvanceClock`, which fast-forwards server-side physics via
    /// `fast_forward_clock` while live clients continue ticking `blue.os.GetSimTime()` at real-time
    /// rate; sending `DoSimClockRebase` would shift `Ballpark.AdjustTimes` into the future and
    /// stall client `Evolve()` updates.
    pub fn advance_secs_without_sim_clock_rebase(&self, secs: u64) {
        self.advance_secs_inner(secs, false);
    }

    fn advance_secs_inner(&self, secs: u64, emit_sim_clock_rebase: bool) {
        let prev_unix = self.now_unix_secs();
        self.clock().advance_secs(secs);
        let cur_unix = self.now_unix_secs();
        self.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("__server_now_unix".into(), cur_unix.to_string());
        });
        if secs > 0 {
            self.run_clock_advanced_sync_pipeline(prev_unix, secs, emit_sim_clock_rebase, true);
        }
    }

    /// Unified synchronous server-clock advancement pipeline shared across live 1 Hz cluster ticks
    /// (`tick_background_cluster` / `tick_wall_second`), accelerated ticks (`step_clock_tick`),
    /// and synthetic time jumps (`advance_secs`) (`#735`).
    pub(crate) fn run_clock_advanced_sync_pipeline(
        &self,
        prev_unix: u64,
        secs: u64,
        emit_sim_clock_rebase: bool,
        is_synthetic_jump: bool,
    ) {
        if secs == 0 {
            return;
        }
        crate::sp6_lifecycle::on_advance_secs_internal(
            self,
            prev_unix,
            secs,
            emit_sim_clock_rebase,
            is_synthetic_jump,
        );
        crate::services::pve_special_sites::on_server_clock_advanced(self, secs);
        let prev_metenox = if is_synthetic_jump {
            prev_unix
        } else {
            let now_u = self.now_unix_secs();
            self.db.with_snapshot(|snap| {
                if snap.mining_and_ess_state.metenox_last_tick_unix > 0 {
                    snap.mining_and_ess_state.metenox_last_tick_unix
                } else {
                    now_u
                }
            })
        };
        crate::services::mining_advanced::tick_all_metenox_moon_drills(
            self,
            prev_metenox,
            self.now_unix_secs(),
        );
        if is_synthetic_jump {
            let now_after = self.now_unix_secs();
            let dt_sec = self.systems.downtime_utc_secs_of_day();
            if secs >= 86_400
                || (prev_unix.saturating_sub(dt_sec) / 86_400)
                    < (now_after.saturating_sub(dt_sec) / 86_400)
            {
                crate::services::fleet_social::execute_pending_auto_kicks_on_downtime(self);
                crate::services::fw_services::process_pending_fw_captures_on_downtime(self);
            }
        }
        crate::services::implant_plugging::purge_expired_boosters_all(self, self.now_unix_secs());
        crate::services::character_expert_systems::sweep_expired_expert_systems_all(
            self,
            self.now_blueos_filetime_ticks(),
        );
        self.advance_skill_queues(secs as f64);
        crate::services::fleet_social::fleet_social_notify::tick_fleet_lifecycle(self);
        crate::services::space_mining_harvest::tick_all_ice_belt_respawns_sync(self);
        let now = self.now_unix_secs();
        crate::sp6_lifecycle::sweep_expired_cosmic_anomalies_and_signatures(self, now);
        crate::services::fleet_social::sweep_expired_bookmarks_and_notify(self, now);
        self.decay_expired_jetcans_and_moon_extractions(now);
        self.process_arrived_moon_survey_probes(now);
        crate::services::social_corp_fleet::sync_all_alliance_timers(self);
        crate::services::fleet_social::fleet_social_corps::promote_all_due_corp_grants(self);
        crate::services::social_corp_fleet::social_wars::social_wars_helpers::sync_wars(self);
        crate::services::contracts_service::sweep_expired_contracts_and_notify(self, now);
        crate::services::combat_crimewatch::combat_wrecks_killmails::sweep_expired_insurance_contracts(self, now);
        let _ = crate::services::station_structure::structure_lifecycle::sync_all_structures(self);
    }

    fn decay_expired_jetcans_and_moon_extractions(&self, now: u64) {
        let mut balls_to_remove: Vec<(u32, i64)> = Vec::new();
        let has_space_decay = self.db.with_snapshot(|snap| {
            snap.items.values().any(|it| {
                it.type_id == 23
                    && it.flag == eve_db::InventoryFlag::None
                    && (30_000_000..=33_000_000).contains(&it.location_id)
                    && snap
                        .sp6_lifecycle
                        .space_object_meta
                        .get(&it.item_id)
                        .and_then(|m| m.created_unix)
                        .is_some_and(|c| now >= c.saturating_add(3600))
            }) || snap.moon_extractions.values().any(|e| {
                e.status != "Decayed" && e.status != "Cancelled" && now >= e.natural_decay_unix
            })
        });
        if has_space_decay {
            self.db.with_mut_snapshot(|snap| {
                // Despawn Cargo Containers (`type_id = 23`) in space (`InventoryFlag::None`) after 3600s (`feat:mining.jetcan-mining`).
                let expired_jetcans: Vec<(i64, u32)> = snap
                    .items
                    .values()
                    .filter(|item| {
                        item.type_id == 23
                            && item.flag == eve_db::InventoryFlag::None
                            && (30_000_000..=33_000_000).contains(&item.location_id)
                            && snap
                                .sp6_lifecycle
                                .space_object_meta
                                .get(&item.item_id)
                                .and_then(|m| m.created_unix)
                                .is_some_and(|created| now >= created.saturating_add(3600))
                    })
                    .map(|item| (item.item_id, item.location_id as u32))
                    .collect();

                for (jetcan_id, sys_id) in expired_jetcans {
                    snap.items.remove(&jetcan_id);
                    snap.sp6_lifecycle.space_object_meta.remove(&jetcan_id);
                    snap.items.retain(|_, item| item.location_id != jetcan_id);
                    balls_to_remove.push((sys_id, jetcan_id));
                }

                // Expire moon extractions that have passed `natural_decay_unix` (`feat:mining.moon-field-decay`).
                for ext in snap.moon_extractions.values_mut() {
                    if ext.status != "Decayed"
                        && ext.status != "Cancelled"
                        && now >= ext.natural_decay_unix
                    {
                        let struct_sys = snap
                            .structures
                            .get(&ext.structure_id)
                            .map(|s| s.solar_system_id);
                        for &ast_id in &ext.spawned_asteroid_ids {
                            let ast_sys = snap
                                .asteroids
                                .remove(&ast_id)
                                .map(|a| a.solar_system_id)
                                .or(struct_sys);
                            if let Some(sys_id) = ast_sys {
                                balls_to_remove.push((sys_id, ast_id));
                            }
                        }
                        ext.spawned_asteroid_ids.clear();
                        ext.status = "Decayed".to_string();
                    }
                }
            });
        }

        for (sys_id, ball_id) in balls_to_remove {
            let handle = self.systems.get_or_spawn(sys_id);
            handle.try_remove_ball(ball_id);
        }
    }

    fn process_arrived_moon_survey_probes(&self, now: u64) {
        let has_arrived = self
            .active_moon_probes
            .read()
            .is_ok_and(|probes| probes.iter().any(|p| now >= p.arrival_unix_secs));
        if !has_arrived {
            return;
        }
        let arrived_moon_probes: Vec<ActiveMoonSurveyProbe> =
            if let Ok(mut probes) = self.active_moon_probes.write() {
                let mut arrived = Vec::new();
                probes.retain(|p| {
                    if now >= p.arrival_unix_secs {
                        arrived.push(p.clone());
                        false
                    } else {
                        true
                    }
                });
                arrived
            } else {
                Vec::new()
            };

        for p in arrived_moon_probes {
            let stay_in_space = self.db.with_snapshot(|snap| {
                snap.characters.get(&p.owner_char_id).is_some_and(|c| {
                    c.solar_system_id == p.solar_system_id
                        && c.station_id.is_none()
                        && c.structure_id.is_none()
                })
            });
            if stay_in_space {
                if let Some(moon_id) = p.hit_moon_id {
                    let composition = crate::services::mining_advanced::derive_moon_ores_from_sde(
                        &self.sde,
                        p.solar_system_id,
                        moon_id,
                    );
                    if composition.is_empty() {
                        self.notify(
                            crate::router::NotificationTarget::Character(p.owner_char_id),
                            "OnScanNoMaterials",
                            "charid",
                            vec![eve_proto::PyRep::Integer(moon_id)],
                        );
                    } else {
                        let resources_dict = eve_proto::PyRep::Dict(
                            composition
                                .iter()
                                .map(|&(tid, frac)| {
                                    (
                                        eve_proto::PyRep::Integer(tid as i64),
                                        eve_proto::PyRep::Real(frac),
                                    )
                                })
                                .collect(),
                        );
                        self.notify(
                            crate::router::NotificationTarget::Character(p.owner_char_id),
                            "OnMoonScanComplete",
                            "charid",
                            vec![eve_proto::PyRep::Integer(moon_id), resources_dict],
                        );
                    }
                }
            }
            self.notify(
                crate::router::NotificationTarget::Character(p.owner_char_id),
                "OnRemoveMoonProbe",
                "charid",
                vec![eve_proto::PyRep::Integer(p.probe_id)],
            );
        }
    }

    /// Advances the virtual server clock by `secs` seconds (`T-7`).
    pub fn advance_virtual_clock(&self, secs: u64) {
        self.advance_secs(secs);
    }

    /// Alias for [`Self::advance_secs`] (`T-7`).
    pub fn advance_time_seconds(&self, secs: u64) {
        self.advance_secs(secs);
    }

    /// Returns the active time-acceleration multiplier (`>= 1`) (`T-7`).
    pub fn time_acceleration_multiplier(&self) -> u64 {
        self.clock().acceleration_multiplier()
    }

    /// Configures the time-acceleration multiplier (`>= 1`) (`T-7`).
    pub fn set_time_acceleration_multiplier(&self, multiplier: u64) {
        self.clock().set_acceleration_multiplier(multiplier);
    }

    /// Returns `true` if the server clock is currently paused (`T-7`).
    pub fn is_clock_paused(&self) -> bool {
        self.clock().is_paused()
    }

    /// Pauses or unpauses the server clock (`T-7`).
    pub fn set_clock_paused(&self, paused: bool) {
        self.clock().set_paused(paused);
    }

    /// Returns `true` if pause-on-tick mode is enabled (`T-7`).
    pub fn pause_on_tick(&self) -> bool {
        self.clock().pause_on_tick()
    }

    /// Enables or disables automatic clock pause at the end of each tick (`T-7`).
    pub fn set_pause_on_tick(&self, enabled: bool) {
        self.clock().set_pause_on_tick(enabled);
    }

    /// Steps the server clock by `base_secs` real seconds scaled by the time-acceleration multiplier,
    /// running the unified synchronous tick pipeline (`#735`).
    pub fn step_clock_tick(&self, base_secs: u64) -> u64 {
        let prev_unix = self.now_unix_secs();
        let advanced = self.clock().step_tick(base_secs);
        if advanced > 0 {
            self.run_clock_advanced_sync_pipeline(prev_unix, advanced, false, true);
        }
        advanced
    }

    /// Steps a 1 Hz live server wall-clock tick, running the unified synchronous tick pipeline (`#735`).
    pub fn tick_wall_second(&self) -> u64 {
        let prev_unix = self.now_unix_secs();
        let advanced = self.clock().tick_wall_second();
        if advanced > 0 {
            self.run_clock_advanced_sync_pipeline(prev_unix, advanced, false, true);
        }
        advanced
    }

    pub fn now_blueos_filetime_ticks(&self) -> i64 {
        self.clock().now_blueos_filetime_ticks()
    }

    /// Advances all active character skill queues by `elapsed_secs` of virtual time (`T-7`).
    pub fn advance_skill_queues(&self, elapsed_secs: f64) {
        if elapsed_secs <= 0.0 || !elapsed_secs.is_finite() {
            return;
        }
        if !self.db.with_snapshot(|snap| snap.skill_queues.values().any(|q| !q.is_empty())) {
            return;
        }
        let sde = self.sde.clone();
        let now_ft = self.now_blueos_filetime_ticks();
        let now_unix = self.now_unix();
        let (invalidated_ships, completed_events, paused_chars) = self.db.with_mut_snapshot(|snap| {
            let mut ships_to_invalidate = Vec::new();
            let mut completed_events = Vec::new();
            let mut paused_chars = Vec::new();
            let char_ids: Vec<i64> = snap.skill_queues.iter().filter(|(_, q)| !q.is_empty()).map(|(&cid, _)| cid).collect();
            for char_id in char_ids {
                if snap.paused_skill_queues.contains(&char_id) {
                    continue;
                }
                let Some(ch) = snap.characters.get(&char_id).cloned() else { continue };
                let exp_opt = crate::services::account_services::subscription_expiry_for_user(snap, ch.user_id);
                let omega_lapsed_at = exp_opt.filter(|&until| until <= now_unix && ch.clone_grade >= 2);
                let is_alpha = omega_lapsed_at.is_none() && ch.clone_grade <= 1;
                let current_char_sp = ch.skill_points + ch.free_skill_points;
                if is_alpha && current_char_sp >= 5_000_000.0 {
                    snap.paused_skill_queues.insert(char_id);
                    paused_chars.push(char_id);
                    continue;
                }
                let eff_attrs = crate::services::character_skills::effective_character_attributes(&sde, snap, char_id);
                let queue = snap.skill_queues.get(&char_id).cloned().unwrap_or_default();
                let mut rem_secs = if let Some(exp_t) = omega_lapsed_at {
                    let prev_unix = now_unix.saturating_sub(elapsed_secs as u64);
                    let start_t = queue.first().and_then(|q| q.started_at_unix).unwrap_or(prev_unix).max(prev_unix);
                    exp_t.saturating_sub(start_t) as f64
                } else {
                    elapsed_secs
                };
                let mut remaining_queue = Vec::new();
                let mut total_sp_gained = 0.0;
                let mut history_to_append = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();

                for mut q in queue {
                    if rem_secs <= 0.0 {
                        remaining_queue.push(q);
                        continue;
                    }
                    if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                        rem_secs = 0.0;
                        snap.paused_skill_queues.insert(char_id);
                        remaining_queue.push(q);
                        continue;
                    }
                    if is_alpha {
                        let alpha_cap = crate::services::character_skills::alpha_skill_cap_for_character(&sde, ch.race_id, q.type_id);
                        if q.target_level > alpha_cap {
                            rem_secs = 0.0;
                            snap.paused_skill_queues.insert(char_id);
                            remaining_queue.push(q);
                            continue;
                        }
                    }

                    let skill_type = sde.types.get(&q.type_id);
                    let p_attr_id = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::PRIMARY_ATTRIBUTE)).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::PERCEPTION as u32);
                    let s_attr_id = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SECONDARY_ATTRIBUTE)).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::WILLPOWER as u32);
                    let p_val = crate::services::character_skills::lookup_effective_attr_from_tuple(eff_attrs, p_attr_id);
                    let s_val = crate::services::character_skills::lookup_effective_attr_from_tuple(eff_attrs, s_attr_id);
                    let clone_mult = if !is_alpha { 1.0 } else { 0.5 };
                    let sp_sec = crate::formulas::sp_per_minute(p_val, s_val, clone_mult).max(1e-6) / 60.0;

                    let needed_sp = (q.end_sp - q.start_sp).max(0.0);
                    let time_for_needed = (needed_sp / sp_sec).max(0.0);
                    let max_allowed_sp = if is_alpha { (5_000_000.0 - (current_char_sp + total_sp_gained)).max(0.0) } else { f64::INFINITY };

                    let (existing_lvl, existing_sp) = skills_map.get(&q.type_id).map(|s| (s.level, s.skill_points)).unwrap_or((0, 0.0));
                    if rem_secs >= time_for_needed && needed_sp <= max_allowed_sp {
                        rem_secs = (rem_secs - time_for_needed).max(0.0);
                        total_sp_gained += needed_sp;
                        let final_lvl = q.target_level.max(existing_lvl);
                        let final_sp = q.end_sp.max(existing_sp);
                        skills_map.insert(q.type_id, eve_db::TrainedSkillRecord { type_id: q.type_id, level: final_lvl, skill_points: final_sp });
                        completed_events.push((char_id, q.type_id, final_lvl));
                        history_to_append.push(eve_db::SkillHistoryEntry { log_date_ft: now_ft, event_type_id: 37, skill_type_id: q.type_id, skill_level: final_lvl, absolute_points: final_sp });
                    } else {
                        let sp_advance = (rem_secs * sp_sec).min(needed_sp).min(max_allowed_sp);
                        rem_secs = 0.0;
                        q.start_sp += sp_advance;
                        q.duration_seconds = ((q.end_sp - q.start_sp).max(0.0)) / sp_sec;
                        q.started_at_unix = Some(now_unix);
                        total_sp_gained += sp_advance;

                        let current_lvl = existing_lvl;
                        let mut achieved_lvl = current_lvl;
                        if let Some(rank) = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT).copied()) {
                            for lvl in (current_lvl + 1)..=q.target_level {
                                if q.start_sp + 0.5 >= crate::formulas::sp_for_skill_level(rank, lvl) {
                                    achieved_lvl = lvl;
                                } else {
                                    break;
                                }
                            }
                        }
                        if achieved_lvl > current_lvl {
                            completed_events.push((char_id, q.type_id, achieved_lvl));
                            history_to_append.push(eve_db::SkillHistoryEntry { log_date_ft: now_ft, event_type_id: 37, skill_type_id: q.type_id, skill_level: achieved_lvl, absolute_points: q.start_sp.max(existing_sp) });
                        }
                        skills_map.insert(q.type_id, eve_db::TrainedSkillRecord { type_id: q.type_id, level: achieved_lvl.max(existing_lvl), skill_points: q.start_sp.max(existing_sp) });
                        remaining_queue.push(q);
                    }
                }
                if let Some(exp_t) = omega_lapsed_at {
                    if let Some(head) = remaining_queue.first_mut() {
                        head.started_at_unix = Some(exp_t);
                    }
                    for c in snap.characters.values_mut().filter(|c| c.user_id == ch.user_id) {
                        if c.clone_grade != 1 {
                            c.clone_grade = 1;
                            c.version += 1;
                        }
                        snap.paused_skill_queues.insert(c.character_id);
                    }
                    paused_chars.push(char_id);
                } else if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                    snap.paused_skill_queues.insert(char_id);
                    paused_chars.push(char_id);
                } else if let Some(head) = remaining_queue.first_mut() {
                    if head.started_at_unix.is_none() {
                        head.started_at_unix = Some(now_unix);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);
                let level_completed = !history_to_append.is_empty();
                if !history_to_append.is_empty() {
                    snap.skill_history.entry(char_id).or_default().extend(history_to_append);
                }
                if total_sp_gained > 0.0 {
                    if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.skill_points += total_sp_gained;
                        if level_completed && ch_mut.active_ship_id > 0 {
                            let in_space = ch_mut.station_id.is_none() && ch_mut.structure_id.is_none();
                            ships_to_invalidate.push((char_id, ch_mut.active_ship_id, in_space));
                        }
                    }
                }
            }
            (ships_to_invalidate, completed_events, paused_chars)
        });
        for cid in paused_chars {
            let _ = self.notify_skill_queue_paused_server(cid);
        }
        let mut trained_by_char: std::collections::BTreeMap<i64, Vec<u32>> =
            std::collections::BTreeMap::new();
        for (cid, skill_type_id, lvl) in completed_events {
            let _ = self.notify_skill_trained(cid, 0, skill_type_id, lvl);
            let entry = trained_by_char.entry(cid).or_default();
            if !entry.contains(&skill_type_id) {
                entry.push(skill_type_id);
            }
        }
        for (cid, skill_ids) in trained_by_char {
            crate::services::jump_clone_names::emit_server_skills_changed(
                self,
                cid,
                &skill_ids,
                Some("SkillTrained"),
            );
            let _ = self.notify_new_skill_queue_saved(
                cid,
                crate::services::jump_clone_names::skill_queue_rowset(self, cid),
            );
        }
        for (cid, ship_id, in_space) in invalidated_ships {
            if in_space {
                self.sync_ship_ballpark_stats(cid, ship_id);
            } else {
                self.invalidate_ship_fitting(ship_id);
            }
        }
    }

    /// Invalidates cached Dogma fitting for `ship_id` and pushes recomputed kinematics and
    /// `CombatState` baselines to the active `SystemActor` containing the ship when in space (`#736`, `#840`).
    pub fn sync_ship_ballpark_stats(&self, char_id: i64, ship_id: i64) {
        if ship_id <= 0 {
            return;
        }
        self.invalidate_ship_fitting(ship_id);
        let (type_id, in_space_sys_id) = self.db.with_snapshot(|s| {
            let Some(item) = s.items.get(&ship_id) else {
                return (0, None);
            };
            let sys_id = if let Some(ch) = s.characters.get(&char_id) {
                if ch.station_id.is_none() && ch.structure_id.is_none() {
                    u32::try_from(ch.solar_system_id).ok()
                } else {
                    None
                }
            } else {
                u32::try_from(item.location_id)
                    .ok()
                    .filter(|&loc| (30_000_000..40_000_000).contains(&loc))
            };
            (item.type_id, sys_id)
        });
        if type_id == 0 {
            return;
        }
        let Some(sys_id) = in_space_sys_id else {
            return;
        };
        let Some(handle) = self.systems.get_handle(sys_id) else {
            return;
        };
        let fresh = self.build_ship_combat_state(char_id, ship_id, type_id);
        let _ = handle.try_sync_ship_ballpark_stats(ship_id, &fresh);
    }

    pub fn invalidate_ship_fitting(&self, ship_id: i64) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
    }

    pub fn store_ship_fitting(&self, ship_id: i64, fit: ShipFittingEngine) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id, fit);
    }

    pub fn get_or_rebuild_ship_fitting(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        let skill_levels = self.character_skill_levels(char_id);
        let active_implants = self.db.with_snapshot(|s| {
            s.active_implants.get(&char_id).cloned().unwrap_or_default()
        });
        let (active_boosters, active_booster_side_effects) =
            crate::services::implant_plugging::active_combat_boosters_for_char(
                &self.db,
                char_id,
                self.now_unix(),
            );
        let active_projected = self
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
            .unwrap_or_default();
        let active_ship_dbuffs =
            crate::services::inventory_fitting::fitting_scanners_superweapons::active_ship_dbuff_gang_bursts(
                self,
                ship_id,
                self.now_unix_secs(),
            );
        let mut active_probes: Vec<(i64, u32)> = self
            .active_scanner_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|p| p.owner_char_id == char_id && p.state >= 0)
            .map(|p| (p.probe_id, p.probe_type_id))
            .collect();
        active_probes.sort_by_key(|&(id, _)| id);
        let cached = self
            .ship_fittings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
            .filter(|e| {
                e.boosters == active_boosters
                    && e.booster_side_effects == active_booster_side_effects
            });
        if let Some(mut existing) = cached {
            let mut desired_gang_bursts: Vec<eve_dogma::GangBurstEffect> = existing
                .gang_bursts
                .iter()
                .filter(|b| b.dbuff_id.is_none())
                .cloned()
                .collect();
            desired_gang_bursts.extend(
                crate::services::insurgency_complex_ops::active_insurgency_pocket_dbuffs(
                    self,
                    char_id,
                    ship_id,
                    existing.ship.type_id,
                ),
            );
            desired_gang_bursts.extend(active_ship_dbuffs);
            let mut drones_changed = false;
            // Keep cached slot charge types synchronized with live inventory items in case charges
            // were swapped or depleted without invalidating `ship_fittings`.
            let charges_changed = !existing.modules.is_empty()
                && self.db.with_snapshot(|s| {
                    let mut slot_charges: HashMap<u16, (u32, u32)> = HashMap::new();
                    for it in s.items.values() {
                        if it.location_id == ship_id
                            && (11..=34).contains(&(it.flag as u16))
                            && it.quantity > 0
                            && self.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 8)
                        {
                            slot_charges.insert(it.flag as u16, (it.type_id, it.quantity as u32));
                        }
                    }
                    let mut changed = false;
                    for m in existing.modules.values_mut() {
                        let (want_tid, want_qty) =
                            if let Some(&(ctid, cqty)) = slot_charges.get(&m.flag_id) {
                                (Some(ctid), cqty)
                            } else if let Some(&act_tid) = s.module_active_charges.get(&m.item_id) {
                                (Some(act_tid), m.charge_quantity.max(1))
                            } else {
                                (None, 0)
                            };
                        if m.charge_type_id != want_tid || m.charge_quantity != want_qty {
                            m.charge_type_id = want_tid;
                            m.charge_quantity = want_qty;
                            changed = true;
                        }
                    }
                    changed
                });
            crate::services::combat_crimewatch::with_combat_ephemeral(self, |rt| {
                for d in rt.active_drones_in_space.values() {
                    if d.parent_ship_id == ship_id && !d.is_abandoned {
                        if !existing.drones.iter().any(|&(id, _)| id == d.drone_item_id) {
                            existing.drones.push((d.drone_item_id, d.type_id));
                            drones_changed = true;
                        }
                        if !existing.drones.iter().any(|&(id, _)| id == d.ball_id) {
                            existing.drones.push((d.ball_id, d.type_id));
                            drones_changed = true;
                        }
                    }
                }
            });
            for &(probe_id, probe_type_id) in &active_probes {
                if !existing.drones.iter().any(|&(id, _)| id == probe_id) {
                    existing.drones.push((probe_id, probe_type_id));
                    drones_changed = true;
                }
            }
            let mut ship_dyn_changed = false;
            if let Some(ship_dyn) = self
                .db
                .with_snapshot(|s| s.items.get(&ship_id).map(|it| it.dynamic_attributes.clone()))
            {
                for (attr_id, val) in ship_dyn {
                    if !matches!(attr_id, 3 | 18 | 264 | 265 | 266)
                        && existing.ship.base_attributes.get(&attr_id).copied() != Some(val)
                    {
                        existing.ship.base_attributes.insert(attr_id, val);
                        ship_dyn_changed = true;
                    }
                }
            }
            let needs_recalc = existing.persisted_context().is_none()
                || existing.character_skills != skill_levels
                || existing.implants != active_implants
                || existing.projected_ewar != active_projected
                || existing.gang_bursts != desired_gang_bursts
                || drones_changed
                || ship_dyn_changed
                || charges_changed;
            existing.gang_bursts = desired_gang_bursts;
            existing.set_character_skills(skill_levels);
            existing.set_implants(active_implants);
            if needs_recalc {
                existing.projected_ewar = active_projected;
                existing.recalculate_stats(&self.sde);
                self.ship_fittings
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(ship_id, existing.clone());
            }
            return Ok(existing);
        }

        let items = self.db.get_items_for_character(char_id);
        let ship_item = items.iter().find(|i| i.item_id == ship_id).cloned().or_else(|| {
            self.db.with_snapshot(|s| s.structures.contains_key(&ship_id).then(|| s.items.get(&ship_id).cloned()).flatten())
        });
        let Some(ship_item) = ship_item else {
            return Err(BridgeRpcError { code: "ShipNotFound".into(), message: format!("Ship item {ship_id} not found for character {char_id}"), client_error: None });
        };

        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, &self.sde)
            .with_character_skills(skill_levels)
            .with_implants(active_implants)
            .with_boosters(active_boosters);
        fit.booster_side_effects = active_booster_side_effects;
        fit.projected_ewar = active_projected;
        let mut gang_bursts = crate::services::insurgency_complex_ops::active_insurgency_pocket_dbuffs(
            self,
            char_id,
            ship_id,
            ship_item.type_id,
        );
        gang_bursts.extend(active_ship_dbuffs);
        fit.gang_bursts = gang_bursts;
        for (&attr_id, &val) in &ship_item.dynamic_attributes {
            fit.ship.base_attributes.insert(attr_id, val);
        }
        let (online_states, active_states, active_charges) = self.db.with_snapshot(|s| {
            (s.module_online_states.clone(), s.module_active_states.clone(), s.module_active_charges.clone())
        });

        let mut fittable_items: Vec<_> = items
            .iter()
            .filter(|item| {
                item.location_id == ship_id && (item.flag as u16) >= 11 && {
                    let cat = self.sde.types.get(&item.type_id).map(|t| t.category_id);
                    cat == Some(7) || cat == Some(32)
                }
            })
            .cloned()
            .collect();

        // Subsystems (125..=129), Rigs (92..=94), and Low-slot fitting mods (11..=18) first
        fittable_items.sort_by_key(|i| match i.flag as u16 {
            125..=129 => 0,
            92..=94 => 1,
            11..=18 => 2,
            _ => 3,
        });

        let mut auto_offlined = Vec::new();
        let mut fitted_items = Vec::with_capacity(fittable_items.len());
        for item in fittable_items {
            let want_online = online_states.get(&item.item_id).copied().unwrap_or(true);
            if fit
                .fit_module_with_state(
                    item.item_id,
                    item.type_id,
                    item.flag as u16,
                    false,
                    &self.sde,
                )
                .is_err()
            {
                continue;
            }
            if let Some(m) = fit.modules.get_mut(&item.item_id) {
                if let Some(&dmg) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE) {
                    m.damage_hp = dmg;
                }
                if let Some(&hp) = item.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT) {
                    m.max_hp = hp;
                }
                let mut overrides = item.dynamic_attributes.clone();
                overrides.remove(&eve_sde::attr_ids::DAMAGE);
                overrides.remove(&eve_sde::attr_ids::HP_STRUCT);
                if !overrides.is_empty() {
                    m.dynamic_attribute_overrides = overrides;
                }
                if let Some(&active_charge) = active_charges.get(&item.item_id) {
                    m.charge_type_id = Some(active_charge);
                    m.charge_quantity = m.charge_quantity.max(1);
                }
            }
            fitted_items.push((item.item_id, item.type_id, want_online));
        }
        fit.recalculate_stats(&self.sde);
        // Pass 1: Online fitted modules whose skill prerequisites are met so skilled modules
        // receive hull CPU/PG priority, and record genuine post-unfit CPU/PG overloads.
        let mut unskilled_want_online = Vec::new();
        for (item_id, type_id, want_online) in fitted_items {
            if !want_online {
                continue;
            }
            if !fit.has_required_skills_for_type(type_id, &self.sde) {
                unskilled_want_online.push(item_id);
                continue;
            }
            match fit.set_module_online(item_id, &self.sde) {
                Ok(()) => {
                    if active_states.contains(&item_id) {
                        let _ = fit.activate_module(item_id, false, &self.sde);
                    }
                }
                Err(ref err_msg) => {
                    if err_msg.contains("Insufficient CPU")
                        || err_msg.contains("Insufficient Powergrid")
                    {
                        auto_offlined.push(item_id);
                    }
                }
            }
        }
        if !unskilled_want_online.is_empty() {
            fit.recalculate_stats(&self.sde);
            // Pass 2: DB-seeded modules with `want_online == true` but missing pilot skills come
            // online if remaining CPU/PG permits; otherwise drop them from `fit.modules` without
            // mutating `snap.module_online_states` (matching pre-fit skill rejection for seeded items
            // while keeping explicitly offline `want_online == false` modules in `fit.modules` for `GetAllInfo`).
            for item_id in unskilled_want_online {
                match fit.set_module_online_skip_skills(item_id, &self.sde) {
                    Ok(()) => {
                        if active_states.contains(&item_id) {
                            let _ = fit.activate_module(item_id, false, &self.sde);
                        }
                    }
                    Err(_) => {
                        let _ = fit.unfit_module(item_id, &self.sde);
                    }
                }
            }
        }
        fit.recalculate_stats(&self.sde);
        let post_offlined = fit.enforce_post_unfit_or_offline_constraints(&self.sde);
        auto_offlined.extend(post_offlined);
        if !auto_offlined.is_empty() {
            self.db.with_mut_snapshot(|snap| {
                for mid in auto_offlined {
                    snap.module_online_states.insert(mid, false);
                    snap.module_active_states.remove(&mid);
                }
            });
        }

        // Attach loaded charges / scripts, scanner probes, and owned drones
        for item in &items {
            let t_opt = self.sde.types.get(&item.type_id);
            let cat = t_opt.map(|t| t.category_id);
            if item.location_id == ship_id && cat == Some(8) {
                let slot_flag = item.flag as u16;
                if let Some(m) = fit.modules.values_mut().find(|m| m.flag_id == slot_flag) {
                    m.charge_type_id = Some(item.type_id);
                    m.charge_quantity = item.quantity.max(0) as u32;
                } else if t_opt.is_some_and(|t| t.group_id == 479) {
                    fit.drones.push((item.item_id, item.type_id));
                }
            } else if item.location_id == ship_id && cat == Some(18) {
                fit.drones.push((item.item_id, item.type_id));
            }
        }
        crate::services::combat_crimewatch::with_combat_runtime(self, |rt| {
            for d in rt.active_drones_in_space.values() {
                if d.parent_ship_id == ship_id && !d.is_abandoned {
                    if !fit.drones.iter().any(|&(id, _)| id == d.drone_item_id) {
                        fit.drones.push((d.drone_item_id, d.type_id));
                    }
                    if !fit.drones.iter().any(|&(id, _)| id == d.ball_id) {
                        fit.drones.push((d.ball_id, d.type_id));
                    }
                }
            }
        });
        for &(probe_id, probe_type_id) in &active_probes {
            if !fit.drones.iter().any(|&(id, _)| id == probe_id) {
                fit.drones.push((probe_id, probe_type_id));
            }
        }
        fit.recalculate_stats(&self.sde);

        self.store_ship_fitting(ship_id, fit.clone());
        Ok(fit)
    }

    pub fn has_fleet_extras_for(&self, fleet_id: i64) -> bool {
        let ex = self.fleet_extras.lock().unwrap();
        ex.target_tags.contains_key(&fleet_id) || ex.watchlist.contains_key(&fleet_id)
            || ex.options.contains_key(&fleet_id) || ex.wing_squad_hierarchy.contains_key(&fleet_id)
            || ex.fleet_names.contains_key(&fleet_id) || ex.max_sizes.contains_key(&fleet_id)
            || ex.invite_details.keys().any(|&(fid, _)| fid == fleet_id)
            || ex.invite_timestamps.keys().any(|&(fid, _)| fid == fleet_id)
    }
}

#[cfg(not(test))]
#[inline]
fn acquire_test_env_read_guard() {}

#[cfg(test)]
fn acquire_test_env_read_guard() -> crate::TestEnvReadGuard {
    crate::lock_test_env_read()
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
#[cfg(test)]
#[path = "context_edges_tests.rs"]
mod context_edges_tests;

