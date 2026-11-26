//! Server execution context, runtime state caches, and simulation clock management.

use super::notify_registry::{OnMoonScanComplete, OnRemoveMoonProbe, OnScanNoMaterials};
use crate::living_world::LivingWorldDirector;
use crate::system_actor::SystemRegistry;
use crate::world_seeder;
use eve_db::lock_rank::{AnyRwLock, RankedMutex, RankedRwLock, RANK_CLOCK, RANK_LEAF, RANK_NPC_SPAWNS};
use eve_db::WorldRepository;
use eve_dogma::ShipFittingEngine;
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[path = "rpc_rate_limit.rs"]
pub(crate) mod rpc_rate_limit;
pub use rpc_rate_limit::{
    RpcRateLimitPolicy, RPC_BOOTSTRAP_BURST_ALLOWANCE, RPC_BOOTSTRAP_GRANT_COOLDOWN,
    RPC_BOOTSTRAP_TRANSITION_ALLOWANCE, RPC_RATE_BURST_TOKENS, RPC_RATE_IDLE_TTL,
    RPC_RATE_PRUNE_THRESHOLD, RPC_RATE_REFILL_PER_SEC,
};

pub static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

/// Returns current host wallclock time as EVE BlueOS 100ns FILETIME ticks.
///
/// Live clients keep `blue.os.GetSimTime()` anchored to host wallclock when
/// `/advancetime` advances server timers without emitting `DoSimClockRebase`.
pub fn host_wall_blueos_filetime_ticks() -> i64 {
    let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
    (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100)
}

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
    pub rpc_registry: Arc<crate::router::rpc_registry::RpcRegistry>,
    pub bound_objects: Arc<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    pub(crate) transition_grace_store: Arc<
        RankedRwLock<
            crate::router::binding_transition_policy::ContextGraceStore,
            { crate::router::binding_transition_policy::RANK_TRANSITION_GRACE },
        >,
    >,
    pub object_cache: Arc<RankedRwLock<HashMap<String, crate::router::cache_notifications::CachedObjectRecord>, RANK_LEAF>>,
    pub object_cache_timestamps: Arc<RankedRwLock<HashMap<String, i64>, RANK_LEAF>>,
    pub notification_records: Arc<RankedRwLock<Vec<crate::router::cache_notifications::NotificationRecord>, RANK_LEAF>>,
    pub pending_notifications: Arc<RankedRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>, RANK_LEAF>>,
    pub notify_tx: tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    pub map_landmarks: Arc<RankedRwLock<Vec<(i64, String, f64, f64, f64)>, RANK_LEAF>>,
    pub session_closers: Arc<RankedRwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>, RANK_LEAF>>,
    pub ship_fittings: Arc<RankedRwLock<HashMap<i64, ShipFittingEngine>, RANK_LEAF>>,
    pub ship_fit_revs: Arc<RankedRwLock<HashMap<i64, eve_dogma::FitRev>, RANK_LEAF>>,
    pub brain_versions: Arc<RankedRwLock<HashMap<i64, i64>, RANK_LEAF>>,
    pub active_scanner_probes: Arc<RankedRwLock<HashMap<i64, ActiveScannerProbe>, RANK_LEAF>>,
    pub(crate) active_probe_scan_contexts: Arc<RankedRwLock<HashMap<i64, Vec<crate::services::space_destiny::space_probe_scanning::ActiveProbeScanContext>>, RANK_LEAF>>,
    pub active_moon_probes: Arc<RankedRwLock<Vec<ActiveMoonSurveyProbe>, RANK_LEAF>>,
    pub scanned_signatures: Arc<RankedRwLock<HashSet<(i64, u32, i64)>, RANK_LEAF>>,
    pub character_scan_progress: Arc<RankedRwLock<HashMap<(i64, u32, String), f64>, RANK_LEAF>>,
    pub invention_roll_override: Arc<RankedRwLock<Option<f64>, RANK_LEAF>>,
    pub mechanic_rng: Arc<crate::rng::MechanicRng>, // seeded test RNG: shared with `systems`; OS CSPRNG unless seeded
    /// Repeating mining-module cycles keyed by module item ID (`MN-04`).
    pub mining_cycles: Arc<RankedRwLock<HashMap<i64, crate::services::mining_cycle::ActiveMiningCycle>, RANK_LEAF>>,
    /// Active Phased Asteroid Fields keyed by solar system ID (`MN-08`).
    pub phased_asteroid_fields: Arc<RankedRwLock<HashMap<u32, crate::services::space_mining_harvest::PhasedAsteroidFieldState>, RANK_LEAF>>,
    pub crimewatch: Arc<crate::combat::crimewatch::CrimeWatchLock>,
    pub(crate) combat_ephemeral: Arc<RankedRwLock<crate::services::combat_crimewatch::CombatRuntimeState, RANK_LEAF>>,
    pub tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    pub(crate) active_fuel_reserves: Arc<crate::services::tactical_modules::ActiveFuelReserveTracker>,
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
    pub(crate) unrebased_clock_offset_secs: Arc<AtomicU64>,
    pub fw_idle_last_step_unix: Arc<RankedRwLock<HashMap<i64, u64>, RANK_LEAF>>,
    /// Tracks `(session_id, char_id) -> solar_system_id` whose initial space ballpark `SetState`
    /// bootstrap has been emitted for the current space stay (`#1083`).
    pub space_ballpark_bootstrapped: Arc<RankedRwLock<HashMap<(u64, i64), u32>, RANK_LEAF>>,
    /// Tracks one-shot non-destructive View Outside `SetState` requests per `(session_id, char_id)`
    /// storing `(structure_id, recorded_at)` so `macho_listener` snapshots `get_set_state(structure_id)`
    /// without mutating shared `Ballpark::observer_events[structure_id]` (`#1461`).
    pub pending_structure_view_outside_set_state:
        Arc<RankedRwLock<HashMap<(u64, i64), (i64, std::time::Instant)>, RANK_LEAF>>,
    /// Tracks `(session_id, char_id)` pairs that already emitted their initial `OnServerBrainUpdated`
    /// on first `dogmaIM.GetAllInfo` (`#1083`).
    pub brain_primed_sessions: Arc<RankedRwLock<HashSet<(u64, i64)>, RANK_LEAF>>,
    /// Per-context RPC rate limiter configured once at construction (`#1186`, `#1540`).
    pub(crate) rpc_rate_limiter: Arc<rpc_rate_limit::RpcRateLimiter>,
    pub scanners_superweapons: Arc<
        crate::services::inventory_fitting::fitting_scanners_superweapons::ScannersSuperweaponsRuntime,
    >,
    pub(crate) cyno_portal_state: Arc<
        crate::services::inventory_fitting::fitting_cyno_portal::CynoPortalSlot,
    >,
    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fixture_census: Arc<super::fixture_census::FixtureCensusCollector>,
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
    rpc_registry: std::sync::Weak<crate::router::rpc_registry::RpcRegistry>,
    bound_objects: std::sync::Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    transition_grace_store: std::sync::Weak<
        RankedRwLock<
            crate::router::binding_transition_policy::ContextGraceStore,
            { crate::router::binding_transition_policy::RANK_TRANSITION_GRACE },
        >,
    >,
    object_cache: std::sync::Weak<RankedRwLock<HashMap<String, crate::router::cache_notifications::CachedObjectRecord>, RANK_LEAF>>,
    object_cache_timestamps: std::sync::Weak<RankedRwLock<HashMap<String, i64>, RANK_LEAF>>,
    notification_records: std::sync::Weak<RankedRwLock<Vec<crate::router::cache_notifications::NotificationRecord>, RANK_LEAF>>,
    pending_notifications: std::sync::Weak<RankedRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>, RANK_LEAF>>,
    notify_tx: tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    map_landmarks: std::sync::Weak<RankedRwLock<Vec<(i64, String, f64, f64, f64)>, RANK_LEAF>>,
    session_closers: std::sync::Weak<RankedRwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>, RANK_LEAF>>,
    ship_fittings: std::sync::Weak<RankedRwLock<HashMap<i64, ShipFittingEngine>, RANK_LEAF>>,
    ship_fit_revs: std::sync::Weak<RankedRwLock<HashMap<i64, eve_dogma::FitRev>, RANK_LEAF>>,
    brain_versions: std::sync::Weak<RankedRwLock<HashMap<i64, i64>, RANK_LEAF>>,
    active_scanner_probes: std::sync::Weak<RankedRwLock<HashMap<i64, ActiveScannerProbe>, RANK_LEAF>>,
    active_probe_scan_contexts: std::sync::Weak<RankedRwLock<HashMap<i64, Vec<crate::services::space_destiny::space_probe_scanning::ActiveProbeScanContext>>, RANK_LEAF>>,
    active_moon_probes: std::sync::Weak<RankedRwLock<Vec<ActiveMoonSurveyProbe>, RANK_LEAF>>,
    scanned_signatures: std::sync::Weak<RankedRwLock<HashSet<(i64, u32, i64)>, RANK_LEAF>>,
    character_scan_progress: std::sync::Weak<RankedRwLock<HashMap<(i64, u32, String), f64>, RANK_LEAF>>,
    invention_roll_override: std::sync::Weak<RankedRwLock<Option<f64>, RANK_LEAF>>,
    mechanic_rng: std::sync::Weak<crate::rng::MechanicRng>,
    mining_cycles: std::sync::Weak<RankedRwLock<HashMap<i64, crate::services::mining_cycle::ActiveMiningCycle>, RANK_LEAF>>,
    phased_asteroid_fields: std::sync::Weak<RankedRwLock<HashMap<u32, crate::services::space_mining_harvest::PhasedAsteroidFieldState>, RANK_LEAF>>,
    crimewatch: std::sync::Weak<crate::combat::crimewatch::CrimeWatchLock>,
    combat_ephemeral: std::sync::Weak<RankedRwLock<crate::services::combat_crimewatch::CombatRuntimeState, RANK_LEAF>>,
    tactical_state: std::sync::Weak<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    active_fuel_reserves: std::sync::Weak<crate::services::tactical_modules::ActiveFuelReserveTracker>,
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
    unrebased_clock_offset_secs: std::sync::Weak<AtomicU64>,
    fw_idle_last_step_unix: std::sync::Weak<RankedRwLock<HashMap<i64, u64>, RANK_LEAF>>,
    space_ballpark_bootstrapped: std::sync::Weak<RankedRwLock<HashMap<(u64, i64), u32>, RANK_LEAF>>,
    pending_structure_view_outside_set_state:
        std::sync::Weak<RankedRwLock<HashMap<(u64, i64), (i64, std::time::Instant)>, RANK_LEAF>>,
    brain_primed_sessions: std::sync::Weak<RankedRwLock<HashSet<(u64, i64)>, RANK_LEAF>>,
    rpc_rate_limiter: std::sync::Weak<rpc_rate_limit::RpcRateLimiter>,
    scanners_superweapons: std::sync::Weak<
        crate::services::inventory_fitting::fitting_scanners_superweapons::ScannersSuperweaponsRuntime,
    >,
    cyno_portal_state: std::sync::Weak<
        crate::services::inventory_fitting::fitting_cyno_portal::CynoPortalSlot,
    >,
    #[cfg(any(test, feature = "testkit"))]
    fixture_census: std::sync::Weak<super::fixture_census::FixtureCensusCollector>,
}

impl ServerContextWeak {
    pub(crate) fn upgrade(&self) -> Option<ServerContext> {
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
            rpc_registry: self.rpc_registry.upgrade()?,
            bound_objects: self.bound_objects.upgrade()?,
            transition_grace_store: self.transition_grace_store.upgrade()?,
            object_cache: self.object_cache.upgrade()?,
            object_cache_timestamps: self.object_cache_timestamps.upgrade()?,
            notification_records: self.notification_records.upgrade()?,
            pending_notifications: self.pending_notifications.upgrade()?,
            notify_tx: self.notify_tx.clone(),
            map_landmarks: self.map_landmarks.upgrade()?,
            session_closers: self.session_closers.upgrade()?,
            ship_fittings: self.ship_fittings.upgrade()?,
            ship_fit_revs: self.ship_fit_revs.upgrade()?,
            brain_versions: self.brain_versions.upgrade()?,
            active_scanner_probes: self.active_scanner_probes.upgrade()?,
            active_probe_scan_contexts: self.active_probe_scan_contexts.upgrade()?,
            active_moon_probes: self.active_moon_probes.upgrade()?,
            scanned_signatures: self.scanned_signatures.upgrade()?,
            character_scan_progress: self.character_scan_progress.upgrade()?,
            invention_roll_override: self.invention_roll_override.upgrade()?,
            mechanic_rng: self.mechanic_rng.upgrade()?,
            mining_cycles: self.mining_cycles.upgrade()?,
            phased_asteroid_fields: self.phased_asteroid_fields.upgrade()?,
            crimewatch: self.crimewatch.upgrade()?,
            combat_ephemeral: self.combat_ephemeral.upgrade()?,
            tactical_state: self.tactical_state.upgrade()?,
            active_fuel_reserves: self.active_fuel_reserves.upgrade()?,
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
            unrebased_clock_offset_secs: self.unrebased_clock_offset_secs.upgrade()?,
            fw_idle_last_step_unix: self.fw_idle_last_step_unix.upgrade()?,
            space_ballpark_bootstrapped: self.space_ballpark_bootstrapped.upgrade()?,
            pending_structure_view_outside_set_state: self
                .pending_structure_view_outside_set_state
                .upgrade()?,
            brain_primed_sessions: self.brain_primed_sessions.upgrade()?,
            rpc_rate_limiter: self.rpc_rate_limiter.upgrade()?,
            scanners_superweapons: self.scanners_superweapons.upgrade()?,
            cyno_portal_state: self.cyno_portal_state.upgrade()?,
            #[cfg(any(test, feature = "testkit"))]
            fixture_census: self.fixture_census.upgrade()?,
        })
    }
}

pub use crate::system_actor::ModuleCooldownEntry;

pub use crate::system_actor::ActiveFighterSquadron;

pub type ActiveScannerProbe = eve_db::PersistedScannerProbe;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveMoonSurveyProbe {
    pub probe_id: i64, pub probe_type_id: u32, pub owner_char_id: i64, pub solar_system_id: u32, pub hit_moon_id: Option<i64>, pub arrival_unix_secs: u64,
}

#[cfg(any(test, feature = "testkit"))]
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

    /// Returns the active global skill training speed multiplier (`1.0` = vanilla, `#1123`).
    #[inline]
    pub fn skill_training_multiplier(&self) -> f64 {
        let raw = self
            .feature_flags
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .skill_training_multiplier;
        crate::formulas::sanitize_rate_multiplier(raw)
    }

    /// Returns the active global mining yield multiplier (`1.0` = vanilla, `#1123`).
    #[inline]
    pub fn mining_yield_multiplier(&self) -> f64 {
        let raw = self
            .feature_flags
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mining_yield_multiplier;
        crate::formulas::sanitize_rate_multiplier(raw)
    }

    /// Returns `true` when `--gm` mode is enabled, unlocking loopback-only `/ops` endpoints (`#1077 §A`).
    pub fn is_gm_ops_enabled(&self) -> bool {
        self.feature_flags
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .gm_mode
    }

    /// Enables or disables `--gm` ops mode at runtime (primarily for tests, `#1077 §A`).
    pub fn set_gm_ops_enabled(&self, enabled: bool) {
        self.feature_flags
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .gm_mode = enabled;
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
            rpc_registry: Arc::downgrade(&self.rpc_registry),
            bound_objects: Arc::downgrade(&self.bound_objects),
            transition_grace_store: Arc::downgrade(&self.transition_grace_store),
            object_cache: Arc::downgrade(&self.object_cache),
            object_cache_timestamps: Arc::downgrade(&self.object_cache_timestamps),
            notification_records: Arc::downgrade(&self.notification_records),
            pending_notifications: Arc::downgrade(&self.pending_notifications),
            notify_tx: self.notify_tx.clone(),
            map_landmarks: Arc::downgrade(&self.map_landmarks),
            session_closers: Arc::downgrade(&self.session_closers),
            ship_fittings: Arc::downgrade(&self.ship_fittings),
            ship_fit_revs: Arc::downgrade(&self.ship_fit_revs),
            brain_versions: Arc::downgrade(&self.brain_versions),
            active_scanner_probes: Arc::downgrade(&self.active_scanner_probes),
            active_probe_scan_contexts: Arc::downgrade(&self.active_probe_scan_contexts),
            active_moon_probes: Arc::downgrade(&self.active_moon_probes),
            scanned_signatures: Arc::downgrade(&self.scanned_signatures),
            character_scan_progress: Arc::downgrade(&self.character_scan_progress),
            invention_roll_override: Arc::downgrade(&self.invention_roll_override),
            mechanic_rng: Arc::downgrade(&self.mechanic_rng),
            mining_cycles: Arc::downgrade(&self.mining_cycles),
            phased_asteroid_fields: Arc::downgrade(&self.phased_asteroid_fields),
            crimewatch: Arc::downgrade(&self.crimewatch),
            combat_ephemeral: Arc::downgrade(&self.combat_ephemeral),
            tactical_state: Arc::downgrade(&self.tactical_state),
            active_fuel_reserves: Arc::downgrade(&self.active_fuel_reserves),
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
            unrebased_clock_offset_secs: Arc::downgrade(&self.unrebased_clock_offset_secs),
            fw_idle_last_step_unix: Arc::downgrade(&self.fw_idle_last_step_unix),
            space_ballpark_bootstrapped: Arc::downgrade(&self.space_ballpark_bootstrapped),
            pending_structure_view_outside_set_state: Arc::downgrade(
                &self.pending_structure_view_outside_set_state,
            ),
            brain_primed_sessions: Arc::downgrade(&self.brain_primed_sessions),
            rpc_rate_limiter: Arc::downgrade(&self.rpc_rate_limiter),
            scanners_superweapons: Arc::downgrade(&self.scanners_superweapons),
            cyno_portal_state: Arc::downgrade(&self.cyno_portal_state),
            #[cfg(any(test, feature = "testkit"))]
            fixture_census: Arc::downgrade(&self.fixture_census),
        }
    }

    /// Initializes a `ServerContext` with explicit `SdeRegistry`, `WorldRepository`, and
    /// `QuasarDescriptorRegistry`, using [`RpcRateLimitPolicy::Default`] and seeding NPC regional markets.
    pub fn with_sde_repository_and_descriptors(
        sde: Arc<SdeRegistry>,
        db: Arc<WorldRepository>,
        quasar_descriptors: Arc<eve_proto::quasar::QuasarDescriptorRegistry>,
    ) -> Self {
        Self::with_sde_repository_descriptors_and_rate_limit(
            sde,
            db,
            quasar_descriptors,
            RpcRateLimitPolicy::Default,
        )
    }

    /// Initializes a `ServerContext` with explicit `SdeRegistry`, `WorldRepository`,
    /// `QuasarDescriptorRegistry`, and an immutable [`RpcRateLimitPolicy`], and seeds NPC regional markets.
    pub fn with_sde_repository_descriptors_and_rate_limit(
        sde: Arc<SdeRegistry>,
        db: Arc<WorldRepository>,
        quasar_descriptors: Arc<eve_proto::quasar::QuasarDescriptorRegistry>,
        rate_limit_policy: RpcRateLimitPolicy,
    ) -> Self {
        let _ = world_seeder::seed_dual_anchor_markets(&db, &sde);
        let clock = db.clock();
        let unrebased_clock_offset_secs = Arc::new(AtomicU64::new(
            db.persisted_unrebased_clock_offset_secs()
                .unwrap_or(0)
                .min(clock.offset_secs()),
        ));
        let cataloged_services = Arc::clone(Self::default_cataloged_services());
        let quasar_domains = Arc::clone(Self::default_quasar_domains());
        let rpc_registry = Arc::new(crate::router::rpc_registry::RpcRegistry::default_compiled());
        let (notify_tx, _) = tokio::sync::broadcast::channel(256);
        let instance_id = NEXT_CTX_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pending_notifications = Arc::new(RankedRwLock::new(HashMap::new()));
        let ship_fittings = Arc::new(RankedRwLock::new(HashMap::new()));
        let ship_fit_revs = Arc::new(RankedRwLock::new(HashMap::new()));
        let invention_roll_override = Arc::new(RankedRwLock::new(None));
        let mechanic_rng = Arc::new(crate::rng::MechanicRng::os());
        let tactical_state = Arc::new(RankedRwLock::new(
            crate::services::tactical_modules::TacticalModulesState::default(),
        ));
        let active_fuel_reserves = Arc::new(
            crate::services::tactical_modules::ActiveFuelReserveTracker::default(),
        );
        let systems = Arc::new(
            SystemRegistry::with_sde_and_db(sde.clone(), Arc::clone(&db))
                .with_mechanic_rng(Arc::clone(&mechanic_rng))
                .with_tactical_state(Arc::clone(&tactical_state))
                .with_notifications(Arc::clone(&pending_notifications), notify_tx.clone())
                .with_unrebased_clock_offset_secs(Arc::clone(&unrebased_clock_offset_secs)),
        );
        let combat_ephemeral = Arc::new(RankedRwLock::new(
            crate::services::combat_crimewatch::init_combat_runtime_state(&db),
        ));
        let quasar_gateway = Arc::new(crate::quasar_gateway::PublicGatewayRouter::new(
            quasar_descriptors,
            Arc::clone(&db),
            Arc::clone(&sde),
        ));
        let cluster_misc_arc = Arc::new(RankedMutex::new(
            crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&db),
        ));
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
        let esi_port = Arc::clone(&quasar_gateway.esi_port);
        let cyno_portal_state =
            crate::services::inventory_fitting::fitting_cyno_portal::init_cyno_portal_slot(&db);
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
            rpc_registry,
            bound_objects: Arc::new(RankedRwLock::new(HashMap::new())),
            transition_grace_store: Arc::new(RankedRwLock::new(
                crate::router::binding_transition_policy::ContextGraceStore::default(),
            )),
            object_cache: Arc::new(RankedRwLock::new(HashMap::new())),
            object_cache_timestamps: Arc::new(RankedRwLock::new(HashMap::new())),
            notification_records: Arc::new(RankedRwLock::new(persisted_notifications)),
            pending_notifications,
            notify_tx,
            map_landmarks: Arc::new(RankedRwLock::new(Vec::new())),
            session_closers: Arc::new(RankedRwLock::new(HashMap::new())),
            ship_fittings,
            ship_fit_revs,
            brain_versions: Arc::new(RankedRwLock::new(HashMap::new())),
            active_scanner_probes: Arc::new(RankedRwLock::new(persisted_probes)),
            active_probe_scan_contexts: Arc::new(RankedRwLock::new(HashMap::new())),
            active_moon_probes: Arc::new(RankedRwLock::new(Vec::new())),
            scanned_signatures: Arc::new(RankedRwLock::new(HashSet::new())),
            character_scan_progress: Arc::new(RankedRwLock::new(HashMap::new())),
            invention_roll_override,
            mechanic_rng,
            mining_cycles: Arc::new(RankedRwLock::new(HashMap::new())),
            phased_asteroid_fields: Arc::new(RankedRwLock::new(HashMap::new())),
            crimewatch,
            combat_ephemeral,
            tactical_state,
            active_fuel_reserves,
            feature_flags: Arc::new(RwLock::new(crate::config::FeatureFlagsConfig::default())),
            esi_port,
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
            unrebased_clock_offset_secs,
            fw_idle_last_step_unix: Arc::new(RankedRwLock::new(HashMap::new())),
            space_ballpark_bootstrapped: Arc::new(RankedRwLock::new(HashMap::new())),
            pending_structure_view_outside_set_state: Arc::new(RankedRwLock::new(HashMap::new())),
            brain_primed_sessions: Arc::new(RankedRwLock::new(HashSet::new())),
            rpc_rate_limiter: Arc::new(rpc_rate_limit::RpcRateLimiter::new(rate_limit_policy)),
            scanners_superweapons: Arc::new(
                crate::services::inventory_fitting::fitting_scanners_superweapons::ScannersSuperweaponsRuntime::new(),
            ),
            cyno_portal_state,
            #[cfg(any(test, feature = "testkit"))]
            fixture_census: Arc::new(super::fixture_census::FixtureCensusCollector::new()),
        };
        if let Err(err) = crate::services::pve_content_providers::register_default_pve_content_providers(
            &ctx.systems,
            Arc::clone(&ctx.sde),
            Arc::clone(&ctx.db),
            Arc::clone(&ctx.combat_ephemeral),
            Arc::clone(&ctx.mechanic_rng),
            Arc::clone(&ctx.phased_asteroid_fields),
        ) {
            tracing::warn!(error = %err, "Failed to register default Track-2 content providers");
        }
        let npc_ctx_w = ctx.downgrade();
        ctx.systems.set_npc_death_hook(Arc::new(move |sys_id, npc_id, killer_opt| {
            let ctx_w = npc_ctx_w.clone();
            Box::pin(async move {
                let Some(ctx) = ctx_w.upgrade() else { return; };
                let is_capsuleer_or_corp_ship = !crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
                    rt.npc_entities.contains_key(&npc_id)
                }) && ctx.db.with_snapshot(|snap| {
                    crate::combat::death_pipeline::resolve_capsuleer_ship_char_in_snap(snap, &ctx.sde, npc_id).is_some()
                        || crate::combat::death_pipeline::resolve_corp_ship_owner_in_snap(snap, &ctx.sde, npc_id).is_some()
                });
                if is_capsuleer_or_corp_ship {
                    return;
                }
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
                let _ = crate::services::pve_special_sites::on_special_site_npc_destroyed_core(sys, db, sde, sys_id, npc_id, killer_opt).await;
                let _ = crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(sys, db, sys_id, npc_id).await;
                crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(sys, db, sys_id, npc_id, killer_opt).await;
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
                crate::services::combat_npc_encounters::handle_npc_destroyed_in_space_with_ctx(&ctx, sys_id, npc_id, killer_opt).await;
                crate::services::fw_gameplay::on_fw_complex_guard_destroyed(sys, db, sys_id, npc_id).await;
                if let Some(kid) = killer_opt {
                    let now_s = db.clock().now_unix_secs();
                    let (clr, esc, step) = crate::services::pve_exploration::handle_pve_site_npc_destruction_core(sys, db, sde, &ctx.mechanic_rng, &*ctx.combat_ephemeral, sys_id, npc_id, kid, true).await;
                    crate::quasar_gateway::isolate_side_effect("router.context.npc_destroyed_progression", || {
                        ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(kid as u32, &["destroyHostileNPC"], 1, now_s, &ctx.quasar_gateway);
                        if let Some(ref sid) = clr {
                            let (cat, did) = crate::services::pve_exploration::pve_site_category_and_dungeon_id(db, now_s, sys_id, sid);
                            ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(sys_id, &cat, did, 1, now_s, &ctx.quasar_gateway);
                            crate::services::quasar_career_goals::record_combat_site_completion_in_system(
                                &ctx.quasar_gateway,
                                sde,
                                sys_id,
                                kid,
                                &cat,
                            );
                        }
                    });
                    crate::services::pve_exploration::notify_pve_destruction_events(db, &*ctx.pending_notifications, tx, kid, clr, esc, step);
                }
                crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(
                    &ctx,
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
                    if let Ok(Some(death_res)) = crate::combat::death_pipeline::on_ball_destroyed_sync(&ctx, sys_id, tid, killer_opt, glam::DVec3::ZERO) {
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
                if let Some(h) = (dt > 0.0).then(|| ctx.systems.get_handle(sys_id)).flatten() {
                    crate::services::combat_crimewatch::combat_npcs_drones::step_engaged_drones_in_system(&ctx, &h, ctx.now_unix()).await;
                }
                crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
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
        crate::system_actor::migrate_legacy_active_drones_at_startup(&ctx);
        crate::sp6_lifecycle::seed_persisted_abandoned_drones(&ctx);
        crate::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
        {
            let ctx_weak = ctx.downgrade();
            let sink: crate::quasar_gateway::UiViewActivatedSink =
                Arc::new(move |session, view_name| {
                    if let Some(ctx) = ctx_weak.upgrade() {
                        ctx.handle_quasar_ui_view_activated(session, view_name);
                    }
                });
            *ctx.quasar_gateway
                .ui_view_activated_sink
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(sink);
        }
        super::startup_seeds::run_startup_global_seeds(&ctx);
        ctx.install_tx_post_commit_listener();
        ctx.wire_module_deactivation_hook();
        crate::router::dispatch::ensure_dispatch_tx_hooks_installed(&ctx.db);
        ctx
    }

    /// Test-only helper initializing `ServerContext` with `SdeRegistry`, `WorldRepository`, and
    /// `QuasarDescriptorRegistry::open_default_or_empty()`.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_sde_and_repository(sde: Arc<SdeRegistry>, db: Arc<WorldRepository>) -> Self {
        Self::with_sde_repository_and_descriptors(
            sde,
            db,
            eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty(),
        )
    }

    #[cfg(any(test, feature = "testkit"))]
    fn canonical_test_sde() -> Arc<SdeRegistry> {
        let _env_read_guard = acquire_test_env_read_guard();
        Arc::new(eve_testkit::TestSde::with_canonical_seed())
    }

    /// Initializes a `ServerContext` with the provided `WorldRepository` and seeds the NPC regional markets.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_repository(db: Arc<WorldRepository>) -> Self {
        let sde = Self::canonical_test_sde();
        Self::with_sde_and_repository(sde, db)
    }

    /// Initializes a default `ServerContext` for unit tests.
    #[cfg(any(test, feature = "testkit"))]
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
    #[cfg(any(test, feature = "testkit"))]
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

    /// Initializes a `ServerContext` from a [`crate::config::ServerConfig`] (`D-5`, `#877`) using
    /// pre-validated `SdeRegistry` and `QuasarDescriptorRegistry` instances.
    pub fn from_config_with_static_data(
        cfg: &crate::config::ServerConfig,
        sde: Arc<SdeRegistry>,
        quasar_descriptors: Arc<eve_proto::quasar::QuasarDescriptorRegistry>,
    ) -> Result<Self, String> {
        cfg.validate_rates()?;
        let backend = cfg.validate_storage_backend()?;
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
                Arc::new(WorldRepository::new_in_memory()?)
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
        let ctx = crate::services::quasar_payment_store::suppress_auto_store_seed_for_db(
            db, !cfg.feature_flags.seed_demo,
            |db| Self::with_sde_repository_and_descriptors(sde, db, quasar_descriptors),
        );
        ctx.systems.set_belt_respawn_interval_secs(cfg.belt_respawn_interval_secs);
        ctx.systems.set_downtime_utc_secs_of_day(cfg.downtime_utc_secs_of_day);
        crate::services::serverops_cluster::set_scheduled_downtime_enabled(
            &ctx,
            cfg.scheduled_downtime_enabled,
        );
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
        ctx.systems.set_lazy_stargate_activation(true);
        if cfg.feature_flags.seed_demo {
            ctx.seed_demo_world_fixtures();
            ctx.db.with_mut_snapshot(|snap| {
                crate::services::quasar_payment_store::ensure_store_catalog_seeded(snap, &ctx.sde);
            });
        }
        Ok(ctx)
    }

    /// Initializes a `ServerContext` from a [`crate::config::ServerConfig`] (`D-5`, `#877`).
    pub fn from_config(cfg: &crate::config::ServerConfig) -> Result<Self, String> {
        #[cfg(any(test, feature = "testkit"))]
        let open_sde = eve_testkit::TestSde::open(&cfg.sde_path).map(Arc::new);
        #[cfg(not(any(test, feature = "testkit")))]
        let open_sde = SdeRegistry::open_shared(&cfg.sde_path);
        let sde = match open_sde {
            Ok(s) => s,
            #[cfg(test)]
            Err(_) if cfg.sde_path == "data/evomu_sde.sqlite" => {
                let _env_read_guard = acquire_test_env_read_guard();
                Arc::new(eve_testkit::TestSde::with_canonical_seed())
            }
            Err(e) => return Err(e.to_string()),
        };
        #[cfg(any(test, feature = "testkit"))]
        let quasar_descriptors = eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty();
        #[cfg(not(any(test, feature = "testkit")))]
        let quasar_descriptors = eve_proto::quasar::QuasarDescriptorRegistry::open_default()
            .map_err(|e| e.to_string())?;
        Self::from_config_with_static_data(cfg, sde, quasar_descriptors)
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
                eve_db::WorldRepository::populate_demo_snapshot_with_sde(snap, &self.sde);
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
            let starter_ship_owned = snap
                .items
                .get(&100_000_001)
                .is_some_and(|ship| ship.owner_id == 90_000_001);
            if starter_ship_owned {
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
            } else if !crate::services::inventory_bound::snapshot_contains_entity_id(
                snap,
                100_000_001,
            ) {
                // Purge items left orphaned inside removed starter ship `100_000_001`
                // across persistent SQLite `--seed-demo` restarts (GH `#1371`).
                snap.items.retain(|_, item| item.location_id != 100_000_001);
                snap.transient_items
                    .retain(|_, item| item.location_id != 100_000_001);
            }
        });
    }

    /// Initializes the production server context (`R22-P1-5`, `D-4`): always starts with an empty
    /// account/character table (`WorldRepository::new_empty()`) without demo secrets, and provisions
    /// operator-supplied player and admin accounts from `EVOMU_ACCOUNTS` / `EVOMU_GM_ACCOUNT`.
    #[cfg(any(test, feature = "testkit"))]
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
        let ctx = Self::with_repository(db);
        ctx.systems.set_lazy_stargate_activation(true);
        ctx
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fn test_sde_and_db_for_context(
    ) -> (crate::TestEnvReadGuard, Arc<SdeRegistry>, Arc<WorldRepository>) {
        let env_read_guard = acquire_test_env_read_guard();
        let sde = Self::canonical_test_sde();
        #[cfg(any(test, feature = "demo-seed"))]
        let db = {
            let disable_demo = std::env::var("EVOMU_DISABLE_DEMO_SEED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if disable_demo {
                Arc::new(WorldRepository::new_empty())
            } else {
                Arc::new(WorldRepository::with_demo_seed_for_sde(&sde))
            }
        };
        #[cfg(not(any(test, feature = "demo-seed")))]
        let db = Arc::new(WorldRepository::new_empty());
        (env_read_guard, sde, db)
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn new() -> Self {
        let (_env_read_guard, sde, db) = Self::test_sde_and_db_for_context();
        let ctx = Self::with_sde_and_repository(sde, db);
        #[cfg(any(test, feature = "demo-seed"))]
        let ctx = ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        ctx
    }

    pub fn mark_signature_scanned(&self, char_id: i64, solar_system_id: u32, sig_ball_id: i64) -> bool {
        let inserted = self
            .scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, solar_system_id, sig_ball_id));
        if inserted {
            crate::quasar_gateway::isolate_side_effect("router.context.mark_signature_scanned", || {
                self.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id as u32, 3, 1, &self.quasar_gateway);
                self.quasar_gateway.progression.career_goals.on_career_gameplay_event(
                    char_id as u32,
                    crate::services::quasar_career_goals::solar_system_career_event(
                        &self.sde,
                        "exploration",
                        solar_system_id,
                    ),
                    1,
                    &self.quasar_gateway,
                );
                self.quasar_gateway.progression.extra.on_campaign_gameplay_event(char_id as u32, crate::services::quasar_campaign_state::CampaignEvent { method: "ScanSignature", type_id: None, solar_system_id: Some(solar_system_id), faction_id: None }, 1, self.now_unix(), &self.quasar_gateway);
                crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(self, char_id, &["ScanSignatures"], 1, Some(solar_system_id as i64));
            });
        }
        inserted
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

    /// Returns the character's current Brain-in-a-Jar monotonic version counter (`0` before first update).
    pub fn current_brain_version(&self, char_id: i64) -> i64 {
        self.brain_versions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .copied()
            .unwrap_or(0)
    }

    /// Advances and returns the character's monotonic Brain-in-a-Jar version counter.
    pub fn bump_brain_version(&self, char_id: i64) -> i64 {
        let mut guard = self
            .brain_versions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(char_id).or_insert(0);
        *entry = entry.saturating_add(1).max(1);
        *entry
    }

    /// Advances the character's brain version and pushes `OnServerBrainUpdated` to the client.
    pub fn push_brain_update(&self, char_id: i64) {
        let version = self.bump_brain_version(char_id);
        crate::services::notify_emitters_missing2::emit_server_brain_updated(
            self, char_id, version,
        );
    }

    pub fn is_space_ballpark_bootstrapped(
        &self,
        session_id: u64,
        char_id: i64,
        solar_system_id: u32,
    ) -> bool {
        self.space_ballpark_bootstrapped
            .read()
            .map(|m| m.get(&(session_id, char_id)).copied() == Some(solar_system_id))
            .unwrap_or(false)
    }

    pub fn mark_space_ballpark_bootstrapped(
        &self,
        session_id: u64,
        char_id: i64,
        solar_system_id: u32,
    ) {
        let mut m = self
            .space_ballpark_bootstrapped
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.insert((session_id, char_id), solar_system_id);
    }

    pub fn clear_space_ballpark_bootstrapped(&self, char_id: i64) {
        {
            let mut m = self
                .space_ballpark_bootstrapped
                .write()
                .unwrap_or_else(|e| e.into_inner());
            m.retain(|&(_, cid), _| cid != char_id);
        }
        self.clear_pending_structure_view_outside_set_state_for_char(char_id);
    }

    pub fn clear_space_ballpark_bootstrapped_for_session(&self, session_id: u64, char_id: i64) {
        {
            let mut m = self
                .space_ballpark_bootstrapped
                .write()
                .unwrap_or_else(|e| e.into_inner());
            m.remove(&(session_id, char_id));
        }
        self.clear_pending_structure_view_outside_set_state(session_id, char_id);
    }

    pub(crate) fn set_pending_structure_view_outside_set_state(
        &self,
        session_id: u64,
        char_id: i64,
        structure_id: i64,
    ) {
        let now = std::time::Instant::now();
        let mut m = self
            .pending_structure_view_outside_set_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if m.len() > 4_096 {
            // Prune only stale non-listener entries; never drop active live sessions.
            m.retain(|_, (_, recorded_at)| {
                now.duration_since(*recorded_at) < std::time::Duration::from_secs(600)
            });
        }
        m.insert((session_id, char_id), (structure_id, now));
    }

    pub(crate) fn take_pending_structure_view_outside_set_state(
        &self,
        session_id: u64,
        char_id: i64,
    ) -> Option<i64> {
        let mut m = self
            .pending_structure_view_outside_set_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.remove(&(session_id, char_id)).map(|(sid, _)| sid)
    }

    pub fn clear_pending_structure_view_outside_set_state(&self, session_id: u64, char_id: i64) {
        let mut m = self
            .pending_structure_view_outside_set_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.remove(&(session_id, char_id));
    }

    pub fn clear_pending_structure_view_outside_set_state_for_session(&self, session_id: u64) {
        let mut m = self
            .pending_structure_view_outside_set_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.retain(|&(sid, _), _| sid != session_id);
    }

    pub fn clear_pending_structure_view_outside_set_state_for_char(&self, char_id: i64) {
        let mut m = self
            .pending_structure_view_outside_set_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.retain(|&(_, cid), _| cid != char_id);
    }

    /// Removes all bound objects for `(session_id, service)` without emitting
    /// `OnMachoObjectDisconnect` and purges matching transition-grace entries (`#1477`).
    pub(crate) fn prune_bound_service_without_notify(
        &self,
        session_id: u64,
        service: &str,
    ) -> usize {
        let target_bids: Vec<u64> = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(&bid, e)| {
                (e.session_id == session_id && e.service == service).then_some(bid)
            })
            .collect();
        for &bid in &target_bids {
            self.remove_bound_object_without_notify(session_id, bid);
        }
        target_bids.len()
    }

    /// Clears docked structure View Outside state when Quasar telemetry reports the client
    /// activated a non-space, non-structure primary UI view (`"hangar"`, `"station"`, etc.).
    /// CCP's `ObjectCallGPCS.__ReleaseObjectHelper` delays `OID-` / `ClientHasReleasedTheseObjects`
    /// by 30s after `michelle.RemoveBallpark()`, whereas `viewStateSvc._LogViewActivated` fires
    /// immediately over Quasar (`/eve_public.gateway.Events/Publish`) (`#1477`).
    pub fn handle_quasar_ui_view_activated(
        &self,
        session: &eve_machonet::SessionState,
        view_unique_name: &str,
    ) {
        let clears_ballpark = matches!(
            view_unique_name.trim().to_ascii_lowercase().as_str(),
            "hangar" | "station" | "charsel" | "charactercreation" | "login" | "dockpanelview"
        );
        if !clears_ballpark {
            return;
        }
        let Some(uid) = session.user_id else {
            return;
        };
        let Some(os) = self.db.get_online_session(uid) else {
            return;
        };
        let Some(cid) = os.character_id else {
            return;
        };
        if session.character_id != Some(cid) {
            return;
        }
        let docked_in_structure = self.db.with_snapshot(|s| {
            s.characters.get(&cid).is_some_and(|c| {
                c.user_id == uid && c.station_id.is_none() && c.structure_id.is_some()
            })
        });
        if !docked_in_structure {
            return;
        }
        self.clear_space_ballpark_bootstrapped_for_session(os.session_id, cid);
        self.prune_bound_service_without_notify(os.session_id, "beyonce");
        crate::services::space_destiny::reset_update_state_rate_limit(
            self,
            os.session_id,
            cid,
        );
    }

    /// Marks `(session_id, char_id)` as having received its initial `OnServerBrainUpdated`;
    /// returns `true` only on the first call for that session login (`#1083`).
    pub fn mark_brain_primed_for_session(&self, session_id: u64, char_id: i64) -> bool {
        let mut m = self
            .brain_primed_sessions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.insert((session_id, char_id))
    }

    pub fn clear_brain_primed_for_char(&self, char_id: i64) {
        let mut m = self
            .brain_primed_sessions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        m.retain(|&(_, cid)| cid != char_id);
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
        self.unrebased_clock_offset_secs.store(0, Ordering::Release);
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

    fn persist_clock_advance_meta(&self, cur_unix: u64, offset_secs: u64) {
        self.cluster_tick_last_unix
            .fetch_max(cur_unix, Ordering::AcqRel);
        self.cluster_tick_persisted_unix
            .fetch_max(cur_unix, Ordering::AcqRel);
        if self.db.is_write_locked_by_current_thread() {
            return;
        }
        let unrebased_offset_secs = self.unrebased_clock_offset_secs.load(Ordering::Acquire);
        let _ = self.db.tx("clock_advance", |wtx| {
            wtx.snap
                .service_state_blobs
                .insert("__server_now_unix".into(), cur_unix.to_string());
            wtx.snap
                .service_state_blobs
                .insert("cluster_tick_last_unix".into(), cur_unix.to_string());
            wtx.set_clock_offset_secs(offset_secs);
            wtx.set_unrebased_clock_offset_secs(unrebased_offset_secs);
            Ok::<_, eve_db::TxError>(())
        });
    }

    fn advance_secs_inner(&self, secs: u64, emit_sim_clock_rebase: bool) {
        let prev_unix = self.now_unix_secs();
        self.clock().advance_secs(secs);
        if !emit_sim_clock_rebase && secs > 0 {
            self.unrebased_clock_offset_secs
                .fetch_add(secs, Ordering::AcqRel);
        }
        let cur_unix = self.now_unix_secs();
        let offset_secs = self.clock().offset_secs();
        self.persist_clock_advance_meta(cur_unix, offset_secs);
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
        crate::services::inventory_fitting::fitting_scanners_superweapons::purge_expired_ship_dbuffs_all(
            self,
            self.now_unix_secs(),
        );
        crate::services::character_expert_systems::sweep_expired_expert_systems_all(
            self,
            self.now_blueos_filetime_ticks(),
        );
        self.advance_skill_queues_for_clock(prev_unix, secs as f64, is_synthetic_jump);
        crate::services::fleet_social::fleet_social_notify::tick_fleet_lifecycle(self);
        crate::services::space_mining_harvest::tick_all_ice_belt_respawns_sync(self);
        let now = self.now_unix_secs();
        let _ = self.systems.run_content_maintenance_for_active_systems(now);
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
                        self.emit_typed(
                            crate::router::NotificationTarget::Character(p.owner_char_id),
                            OnScanNoMaterials { moon_id },
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
                        self.emit_typed(
                            crate::router::NotificationTarget::Character(p.owner_char_id),
                            OnMoonScanComplete {
                                moon_id,
                                results: resources_dict,
                            },
                        );
                    }
                }
            }
            self.emit_typed(
                crate::router::NotificationTarget::Character(p.owner_char_id),
                OnRemoveMoonProbe {
                    probe_id: p.probe_id,
                },
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
        let prev_offset = self.clock().offset_secs();
        let advanced = self.clock().step_tick(base_secs);
        if advanced > 0 {
            let cur_unix = self.now_unix_secs();
            let cur_offset = self.clock().offset_secs();
            self.cluster_tick_last_unix
                .fetch_max(cur_unix, Ordering::AcqRel);
            if cur_offset != prev_offset {
                self.persist_clock_advance_meta(cur_unix, cur_offset);
            }
            self.run_clock_advanced_sync_pipeline(prev_unix, advanced, false, true);
        }
        advanced
    }

    /// Steps a 1 Hz live server wall-clock tick, running the unified synchronous tick pipeline (`#735`).
    pub fn tick_wall_second(&self) -> u64 {
        let prev_unix = self.now_unix_secs();
        let prev_offset = self.clock().offset_secs();
        let advanced = self.clock().tick_wall_second();
        if advanced > 0 {
            let cur_unix = self.now_unix_secs();
            let cur_offset = self.clock().offset_secs();
            self.cluster_tick_last_unix
                .fetch_max(cur_unix, Ordering::AcqRel);
            if cur_offset != prev_offset {
                self.persist_clock_advance_meta(cur_unix, cur_offset);
            }
            self.run_clock_advanced_sync_pipeline(prev_unix, advanced, false, true);
        }
        advanced
    }

    pub fn now_blueos_filetime_ticks(&self) -> i64 {
        self.clock().now_blueos_filetime_ticks()
    }

    /// Returns a BlueOS 100ns FILETIME timestamp aligned with the client's MachoNet clock
    /// (`ctx.clock()` minus `unrebased_clock_offset_secs`), so `/advancetime` calls via
    /// [`Self::advance_secs_without_sim_clock_rebase`] stay client-anchored while still
    /// tracking [`VirtualServerClock`] instead of reading OS `SystemTime::now()` (`#1196`).
    pub fn now_client_sim_filetime_ticks(&self) -> i64 {
        self.now_machonet_wallclock_filetime_ticks()
    }

    /// Returns a BlueOS 100ns FILETIME timestamp anchored to `solar_system_id`'s sim clock
    /// (`sim_clock_anchoring`) when present, clamping monotonically against
    /// `last_sim_activation_ft_by_system` so `0.5 -> 1.0` transitions preserve the current sim
    /// stamp and immediate post-transition activations emit history-safe timestamps (`#1081`).
    /// Uses client-aligned wallclock (`now_machonet_wallclock_filetime_ticks`) so unrebased
    /// `/advancetime` jumps never push anchored sim timestamps >24h into the client's future
    /// and trigger `godma.py ActualStopEffect` sleep overflows (`#1143`, `#1144`).
    pub fn now_system_sim_filetime_ticks(&self, solar_system_id: Option<u32>) -> i64 {
        let Some(sys_id) = solar_system_id else {
            return self.now_client_sim_filetime_ticks();
        };
        let now_wall_ft = self.now_machonet_wallclock_filetime_ticks();
        let mut cn = self
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let Some(&(anchor_wall_ft, anchor_sim_ft, factor)) = cn.sim_clock_anchoring.get(&sys_id)
        else {
            drop(cn);
            return self.now_client_sim_filetime_ticks();
        };
        let elapsed_wall_ft = now_wall_ft.saturating_sub(anchor_wall_ft).max(0);
        let dilated_ft = ((elapsed_wall_ft as f64) * factor.clamp(0.10, 1.0)).round() as i64;
        let base_ft = anchor_sim_ft.saturating_add(dilated_ft);
        let prev = cn
            .last_sim_activation_ft_by_system
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        let safe_ft = if base_ft <= prev {
            prev.saturating_add(1)
        } else {
            base_ft
        };
        cn.last_sim_activation_ft_by_system.insert(sys_id, safe_ft);
        safe_ft
    }

    /// Resolves `(char_id, ship_id)` to a solar system if in space and returns
    /// [`Self::now_system_sim_filetime_ticks`] (`#1081`).
    pub fn now_system_sim_filetime_ticks_for_ship(&self, char_id: i64, ship_id: i64) -> i64 {
        let sys_opt = self.systems.find_ball_system_sync(ship_id).or_else(|| {
            if char_id > 0 {
                self.db.with_snapshot(|s| {
                    s.characters
                        .get(&char_id)
                        .filter(|c| c.station_id.is_none() && c.structure_id.is_none())
                        .map(|c| c.solar_system_id)
                })
            } else {
                None
            }
        });
        self.now_system_sim_filetime_ticks(sys_opt)
    }

    /// Returns a BlueOS 100ns FILETIME timestamp for `machoNet.GetTime` (`blue.os.GetWallclockTimeNow()`),
    /// tracking [`Self::clock`] (`set_clock` and `advance_secs`) while subtracting offsets added via
    /// [`Self::advance_secs_without_sim_clock_rebase`] (`timeControl.AdvanceClock` / `/advancetime`)
    /// so live client `ClockSyncDaemon` never skews `timeSyncAdjustFactor` above 1.0x (#1031).
    pub fn now_machonet_wallclock_filetime_ticks(&self) -> i64 {
        let clock = self.clock();
        crate::system_actor::machonet_wallclock_filetime_ticks(
            clock.as_ref(),
            &self.unrebased_clock_offset_secs,
        )
    }

    /// Returns wall-clock milliseconds derived from [`Self::now_machonet_wallclock_filetime_ticks`] (`#1080`).
    pub fn now_machonet_wallclock_ms(&self) -> u64 {
        let clock = self.clock();
        crate::system_actor::machonet_wallclock_ms(
            clock.as_ref(),
            &self.unrebased_clock_offset_secs,
        )
    }

    /// Seconds added via [`Self::advance_secs_without_sim_clock_rebase`] (`/advancetime`)
    /// by which the server's virtual clock leads the live client's `blue.os.GetSimTime()`.
    pub fn unrebased_clock_offset_secs(&self) -> u64 {
        self.unrebased_clock_offset_secs.load(Ordering::Acquire)
    }

    /// Advances all active character skill queues by `elapsed_secs` of virtual time (`T-7`).
    pub fn advance_skill_queues(&self, elapsed_secs: f64) {
        self.advance_skill_queues_inner(elapsed_secs, None, false);
    }

    /// Advances active character skill queues for a clock tick/jump, clamping against persisted
    /// `skill_queue_last_applied_unix` so restart recovery and GM jumps never double-credit SP (`#863`).
    pub(crate) fn advance_skill_queues_for_clock(&self, prev_unix: u64, elapsed_secs: f64, is_synthetic_jump: bool) {
        self.advance_skill_queues_inner(elapsed_secs, Some(prev_unix), is_synthetic_jump);
    }

    fn advance_skill_queues_inner(&self, elapsed_secs: f64, clock_prev_unix: Option<u64>, is_synthetic_jump: bool) {
        if elapsed_secs <= 0.0 || !elapsed_secs.is_finite() {
            return;
        }
        if !self.db.with_snapshot(|snap| snap.skill_queues.values().any(|q| !q.is_empty())) {
            return;
        }
        let sde = self.sde.clone();
        let train_mult = self.skill_training_multiplier();
        let now_ft = self.now_blueos_filetime_ticks();
        let now_unix = self.now_unix();
        let (invalidated_ships, completed_events, paused_chars, partial_sp_events) = self.db.with_mut_snapshot(|snap| {
            let effective_elapsed = if let Some(clock_prev) = clock_prev_unix {
                let last_applied = snap
                    .service_state_blobs
                    .get("skill_queue_last_applied_unix")
                    .and_then(|s| s.parse::<u64>().ok());
                let clamped = match last_applied {
                    Some(last_ts) if now_unix <= last_ts => 0.0,
                    Some(last_ts) => {
                        let start_ts = last_ts.max(clock_prev);
                        (now_unix.saturating_sub(start_ts) as f64).min(elapsed_secs)
                    }
                    None => elapsed_secs,
                };
                if clamped <= 0.0 {
                    return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                }
                snap.service_state_blobs
                    .insert("skill_queue_last_applied_unix".into(), now_unix.to_string());
                clamped
            } else {
                elapsed_secs
            };
            let mut ships_to_invalidate = Vec::new();
            let mut completed_events = Vec::new();
            let mut paused_chars = Vec::new();
            let mut partial_sp_events = Vec::new();
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
                    let prev_unix = now_unix.saturating_sub(effective_elapsed as u64);
                    let start_t = queue.first().and_then(|q| q.started_at_unix).unwrap_or(prev_unix).max(prev_unix);
                    exp_t.saturating_sub(start_t) as f64
                } else {
                    effective_elapsed
                };
                let mut remaining_queue = Vec::new();
                let mut total_sp_gained = 0.0;
                let mut history_to_append = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();

                for mut q in queue {
                    if !crate::services::character_skills::is_trainable_injected_skill(
                        &sde, skills_map, q.type_id,
                    ) {
                        continue;
                    }
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
                    let sp_sec = crate::formulas::sp_per_minute_with_multiplier(
                        p_val,
                        s_val,
                        clone_mult,
                        train_mult,
                    )
                    .max(1e-6)
                        / 60.0;

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
                        if is_synthetic_jump && sp_advance > 0.0 {
                            partial_sp_events.push((char_id, q.type_id));
                        }

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
                if remaining_queue.is_empty() {
                    snap.skill_queues.remove(&char_id);
                } else {
                    snap.skill_queues.insert(char_id, remaining_queue);
                }
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
            (ships_to_invalidate, completed_events, paused_chars, partial_sp_events)
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
        // On synthetic clock jumps (`/advancetime`, `advance_secs`), push SP & queue updates even
        // when no level finished, while keeping 1 Hz ticks silent and avoiding duplicate emits (#1066).
        let mut partial_only_by_char: std::collections::BTreeMap<i64, Vec<u32>> =
            std::collections::BTreeMap::new();
        if is_synthetic_jump {
            for (cid, skill_type_id) in partial_sp_events {
                if let Some(trained_ids) = trained_by_char.get_mut(&cid) {
                    if !trained_ids.contains(&skill_type_id) {
                        trained_ids.push(skill_type_id);
                    }
                } else {
                    let entry = partial_only_by_char.entry(cid).or_default();
                    if !entry.contains(&skill_type_id) {
                        entry.push(skill_type_id);
                    }
                }
            }
        }
        for (cid, skill_ids, event) in trained_by_char
            .into_iter()
            .map(|(cid, ids)| (cid, ids, Some("SkillTrained")))
            .chain(
                partial_only_by_char
                    .into_iter()
                    .map(|(cid, ids)| (cid, ids, None)),
            )
        {
            crate::services::jump_clone_names::emit_server_skills_changed(
                self, cid, &skill_ids, event,
            );
            self.push_brain_update(cid);
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
        let rev = self.current_fit_rev(ship_id);
        let fresh = self.build_ship_combat_state(char_id, ship_id, type_id);
        let proj_opt = self.build_fit_projection(char_id, ship_id, rev);
        if eve_db::lock_rank::held_lock_ranks().is_empty() {
            let _ = handle.try_sync_ship_ballpark_stats(ship_id, &fresh);
            if let Some(proj) = proj_opt {
                let _ = handle.try_send_fit_projection(ship_id, proj);
            }
        } else {
            let h = handle.clone();
            h.enqueue_post_unlock_action(Box::new(move || { let _ = handle.try_sync_ship_ballpark_stats(ship_id, &fresh); if let Some(proj) = proj_opt { let _ = handle.try_send_fit_projection(ship_id, proj); } }));
        }
    }

    fn resolve_ship_fit_char_id(&self, ship_id: i64) -> Option<i64> {
        self.db.with_snapshot(|s| {
            s.characters
                .values()
                .find(|c| c.active_ship_id == ship_id)
                .map(|c| c.character_id)
                .or_else(|| s.items.get(&ship_id).map(|i| i.owner_id))
        })
    }

    /// Computes a [`eve_dogma::FitProjection`] for `(char_id, ship_id)` at `rev`, applying active
    /// command-burst dbuffs and system-wide phenomena once onto the `ActorBaseline` (which
    /// excludes propulsion, cloak, HIC, and `gang_bursts` so nothing is double-applied, `#834`, `#922`).
    pub(crate) fn build_fit_projection(
        &self,
        char_id: i64,
        ship_id: i64,
        rev: eve_dogma::FitRev,
    ) -> Option<eve_dogma::FitProjection> {
        let fit = self.fit(char_id, ship_id).ok()?;
        if fit.ship.type_id == 0 || !self.sde.types.contains_key(&fit.ship.type_id) {
            return None;
        }
        let mut proj = fit.to_projection(rev, &self.sde);
        use eve_sde::attr_ids as a;
        let b = &mut proj.baseline;
        let mut eff_attrs: HashMap<u16, f64> = [
            (a::SHIELD_CAPACITY, b.max_shield_hp),
            (a::ARMOR_HP, b.max_armor_hp),
            (a::HP_STRUCT, b.max_structure_hp),
            (a::CAPACITOR_CAPACITY, b.max_capacitor),
            (a::SIGNATURE_RADIUS, b.signature_radius),
            (a::MAX_VELOCITY, b.max_velocity),
            (a::MASS, b.mass),
            (a::AGILITY, b.agility),
            (a::SHIELD_RECHARGE_RATE, b.shield_recharge_time_ms),
            (a::RECHARGE_RATE, b.capacitor_recharge_time_ms),
            (a::SHIELD_EM_DAMAGE_RESONANCE, b.shield_resonance[0]),
            (a::SHIELD_THERMAL_DAMAGE_RESONANCE, b.shield_resonance[1]),
            (a::SHIELD_KINETIC_DAMAGE_RESONANCE, b.shield_resonance[2]),
            (a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, b.shield_resonance[3]),
            (a::ARMOR_EM_DAMAGE_RESONANCE, b.armor_resonance[0]),
            (a::ARMOR_THERMAL_DAMAGE_RESONANCE, b.armor_resonance[1]),
            (a::ARMOR_KINETIC_DAMAGE_RESONANCE, b.armor_resonance[2]),
            (a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, b.armor_resonance[3]),
            (a::EM_DAMAGE_RESONANCE, b.structure_resonance[0]),
            (a::THERMAL_DAMAGE_RESONANCE, b.structure_resonance[1]),
            (a::KINETIC_DAMAGE_RESONANCE, b.structure_resonance[2]),
            (a::EXPLOSIVE_DAMAGE_RESONANCE, b.structure_resonance[3]),
            (a::WARP_SPEED_MULTIPLIER, b.warp_speed_au_s),
            (a::MAX_LOCKED_TARGETS, b.max_locked_targets as f64),
            (a::MAX_TARGET_RANGE, b.max_target_range),
            (a::SCAN_RESOLUTION, b.scan_resolution),
        ]
        .into_iter()
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
            self,
            ship_id,
            &[2141],
            &mut eff_attrs,
        );
        b.max_shield_hp = eff_attrs[&a::SHIELD_CAPACITY].max(0.0);
        b.max_armor_hp = eff_attrs[&a::ARMOR_HP].max(0.0);
        b.max_structure_hp = eff_attrs[&a::HP_STRUCT].max(0.0);
        let raw_cap = eff_attrs[&a::CAPACITOR_CAPACITY];
        b.max_capacitor = if raw_cap <= 0.0 {
            if proj.ship_type_id == 670 { 100.0 } else { 250.0 }
        } else {
            raw_cap
        };
        let tp_mult = self.effective_target_painter_multiplier(ship_id);
        let sig = eff_attrs[&a::SIGNATURE_RADIUS].max(0.0);
        b.signature_radius = if tp_mult > 1.0 + 1e-6 { sig / tp_mult } else { sig };
        b.max_velocity = eff_attrs[&a::MAX_VELOCITY].max(0.0);
        b.mass = eff_attrs[&a::MASS].max(1.0);
        b.agility = eff_attrs[&a::AGILITY].max(0.001);
        b.shield_recharge_time_ms = eff_attrs[&a::SHIELD_RECHARGE_RATE].max(1.0);
        b.capacitor_recharge_time_ms = eff_attrs[&a::RECHARGE_RATE].max(1.0);
        b.shield_resonance = [
            eff_attrs[&a::SHIELD_EM_DAMAGE_RESONANCE],
            eff_attrs[&a::SHIELD_THERMAL_DAMAGE_RESONANCE],
            eff_attrs[&a::SHIELD_KINETIC_DAMAGE_RESONANCE],
            eff_attrs[&a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE],
        ];
        b.armor_resonance = [
            eff_attrs[&a::ARMOR_EM_DAMAGE_RESONANCE],
            eff_attrs[&a::ARMOR_THERMAL_DAMAGE_RESONANCE],
            eff_attrs[&a::ARMOR_KINETIC_DAMAGE_RESONANCE],
            eff_attrs[&a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE],
        ];
        b.structure_resonance = [
            eff_attrs[&a::EM_DAMAGE_RESONANCE],
            eff_attrs[&a::THERMAL_DAMAGE_RESONANCE],
            eff_attrs[&a::KINETIC_DAMAGE_RESONANCE],
            eff_attrs[&a::EXPLOSIVE_DAMAGE_RESONANCE],
        ];
        b.warp_speed_au_s = eff_attrs[&a::WARP_SPEED_MULTIPLIER].max(0.0);
        b.max_locked_targets = eff_attrs[&a::MAX_LOCKED_TARGETS].max(0.0).round() as usize;
        b.max_target_range = eff_attrs[&a::MAX_TARGET_RANGE].max(0.0);
        b.scan_resolution = eff_attrs[&a::SCAN_RESOLUTION].max(0.0);
        if let Some(ref mut prop) = proj.propulsion {
            prop.speed_boost = eve_dogma::compute_propulsion_speed_boost(
                prop.speed_factor,
                prop.speed_boost_factor,
                b.mass,
                prop.mass_addition,
            );
        }
        Some(proj)
    }

    /// Invalidates the cached fit for `ship_id`, advances `FitRev`, and pushes a fresh
    /// `FitProjection` to the system actor when the ship is in space (`#834`, `#922`).
    pub(crate) fn invalidate_and_push_fit(&self, ship_id: i64) -> Option<eve_dogma::FitRev> {
        self.invalidate_ship_fitting_cache_only(ship_id);
        let rev = self.push_fit_projection_for_ship(ship_id);
        // Drop cached fit without bumping FitRev again so invalidation contract holds.
        self.ship_fittings.write().unwrap_or_else(|e| e.into_inner()).remove(&ship_id);
        rev
    }

    /// Resolves the owner/pilot for `ship_id` and invokes [`Self::push_fit_projection`].
    pub fn push_fit_projection_for_ship(&self, ship_id: i64) -> Option<eve_dogma::FitRev> {
        if ship_id <= 0 {
            return None;
        }
        if let Some(char_id) = self.resolve_ship_fit_char_id(ship_id) {
            self.push_fit_projection(char_id, ship_id)
        } else {
            Some(self.bump_fit_rev(ship_id))
        }
    }

    /// Returns the current monotonic [`eve_dogma::FitRev`] for `ship_id` (`FitRev(0)` if never bumped).
    pub fn current_fit_rev(&self, ship_id: i64) -> eve_dogma::FitRev {
        self.ship_fit_revs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .copied()
            .unwrap_or_default()
    }

    /// Advances and returns the monotonic [`eve_dogma::FitRev`] for `ship_id` (`#922`).
    pub fn bump_fit_rev(&self, ship_id: i64) -> eve_dogma::FitRev {
        let mut revs = self.ship_fit_revs.write().unwrap_or_else(|e| e.into_inner());
        let next = revs.get(&ship_id).copied().unwrap_or_default().next();
        revs.insert(ship_id, next);
        next
    }

    /// Bumps `FitRev` for `ship_id` and, when the ship is currently in space with an active
    /// `SystemActor`, computes and enqueues an immutable [`eve_dogma::FitProjection`] (`#922`).
    ///
    /// Drops all locks before calling [`SystemHandle::try_send_fit_projection`].
    pub fn push_fit_projection(&self, char_id: i64, ship_id: i64) -> Option<eve_dogma::FitRev> {
        if ship_id <= 0 {
            return None;
        }
        let in_space_sys_id = self.db.with_snapshot(|s| {
            let item = s.items.get(&ship_id)?;
            if let Some(ch) = s.characters.get(&char_id) {
                if ch.station_id.is_none() && ch.structure_id.is_none() {
                    u32::try_from(ch.solar_system_id).ok()
                } else {
                    None
                }
            } else {
                u32::try_from(item.location_id)
                    .ok()
                    .filter(|&loc| (30_000_000..40_000_000).contains(&loc))
            }
        });
        if let Some(sys_id) = in_space_sys_id {
            if let Some(handle) = self.systems.get_handle(sys_id) {
                let rev = self.bump_fit_rev(ship_id);
                if let Some(projection) = self.build_fit_projection(char_id, ship_id, rev) {
                    if eve_db::lock_rank::held_lock_ranks().is_empty() {
                        let _ = handle.try_send_fit_projection(ship_id, projection);
                    } else {
                        let h = handle.clone();
                        h.enqueue_post_unlock_action(Box::new(move || { let _ = handle.try_send_fit_projection(ship_id, projection); }));
                    }
                }
                return Some(rev);
            }
        }
        Some(self.bump_fit_rev(ship_id))
    }

    /// Persists durable ship fit runtime inputs (`active_stance_type_id`, `last_stance_switch_ms`,
    /// `overloaded_modules`) into `WorldStateSnapshot::ship_fit_inputs` (`#861`, `#922`).
    pub(crate) fn persist_fit_inputs(&self, ship_id: i64, inputs: &eve_dogma::PersistedFitInputs) {
        let rec = eve_db::ShipFitInputsRecord {
            active_stance_type_id: inputs.active_stance_type_id,
            last_stance_switch_ms: inputs.last_stance_switch_ms,
            overloaded_modules: inputs.overloaded_modules.clone(),
        };
        self.db.with_mut_snapshot(|snap| {
            if rec.active_stance_type_id.is_none()
                && rec.last_stance_switch_ms.is_none()
                && rec.overloaded_modules.is_empty()
            {
                snap.ship_fit_inputs.remove(&ship_id);
            } else {
                snap.ship_fit_inputs.insert(ship_id, rec);
            }
        });
    }

    /// Removes a single module from `ship_id`'s persisted `overloaded_modules` while keeping
    /// stance state intact, pruning the entry when no persisted fit inputs remain.
    pub(crate) fn clear_persisted_module_overload(
        snap: &mut eve_db::WorldStateSnapshot,
        ship_id: i64,
        module_id: i64,
    ) {
        let should_prune = if let Some(rec) = snap.ship_fit_inputs.get_mut(&ship_id) {
            rec.overloaded_modules.remove(&module_id);
            rec.active_stance_type_id.is_none()
                && rec.last_stance_switch_ms.is_none()
                && rec.overloaded_modules.is_empty()
        } else {
            false
        };
        if should_prune {
            snap.ship_fit_inputs.remove(&ship_id);
        }
    }

    /// Clears all persisted `overloaded_modules` for `ship_id` while keeping stance state intact,
    /// returning `true` if any overloaded module entry was cleared.
    pub(crate) fn clear_all_persisted_ship_overloads(
        snap: &mut eve_db::WorldStateSnapshot,
        ship_id: i64,
    ) -> bool {
        let (cleared, should_prune) = if let Some(rec) = snap.ship_fit_inputs.get_mut(&ship_id) {
            let had = !rec.overloaded_modules.is_empty();
            rec.overloaded_modules.clear();
            let empty = rec.active_stance_type_id.is_none() && rec.last_stance_switch_ms.is_none();
            (had, empty)
        } else {
            (false, false)
        };
        if should_prune {
            snap.ship_fit_inputs.remove(&ship_id);
        }
        cleared
    }

    /// Captures cached rack heat and module heat damage for `ship_id` before cross-system transit.
    pub(crate) fn snapshot_rack_heat_and_damage(
        &self,
        ship_id: i64,
    ) -> Option<([f64; 3], Vec<(i64, f64)>)> {
        self.ship_fittings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .map(|f| {
                let dmg = f
                    .modules
                    .iter()
                    .filter(|(_, m)| m.damage_hp.is_finite() && m.damage_hp > 0.0)
                    .map(|(&mid, m)| (mid, m.damage_hp))
                    .collect();
                (f.rack_heat, dmg)
            })
    }

    /// Updates cached rack heat (and clears module damage flags when resetting heat) for `ship_id`.
    pub(crate) fn update_ship_rack_heat(
        &self,
        char_id: i64,
        ship_id: i64,
        rack_heat: [f64; 3],
        clear_module_damage: bool,
    ) {
        if let Ok(mut fit) = self.fit(char_id, ship_id) {
            fit.rack_heat = rack_heat;
            if clear_module_damage {
                for m in fit.modules.values_mut() {
                    m.damage_hp = 0.0;
                }
            }
            self.ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(ship_id, fit);
            let _ = self.bump_fit_rev(ship_id);
        }
    }

    pub(crate) fn invalidate_ship_fitting_cache_only(&self, ship_id: i64) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
        let _ = self.bump_fit_rev(ship_id);
    }

    pub fn invalidate_ship_fitting(&self, ship_id: i64) {
        let _ = self.invalidate_and_push_fit(ship_id);
    }

    pub fn store_ship_fitting(&self, ship_id: i64, fit: ShipFittingEngine) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id, fit);
        let _ = self.bump_fit_rev(ship_id);
    }

    /// Verifies that `char_id` owns or is currently piloting `ship_id` (or `ship_id` is a valid
    /// structure item) before serving any cached or rebuilt fit (`#861`, `#922`).
    fn verify_ship_fit_access(&self, char_id: i64, ship_id: i64) -> Result<(), BridgeRpcError> {
        let allowed = self.db.with_snapshot(|s| {
            let Some(item) = s.items.get(&ship_id) else {
                return false;
            };
            item.owner_id == char_id
                || s.characters
                    .get(&char_id)
                    .is_some_and(|c| c.active_ship_id == ship_id)
                || s.structures.contains_key(&ship_id)
        });
        if allowed {
            Ok(())
        } else {
            Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Ship item {ship_id} not found for character {char_id}"),
                client_error: None,
            })
        }
    }

    /// Single checked accessor for a character's ship fit (`#861`, `#922`).
    ///
    /// Legacy callers still reach this via [`Self::get_or_rebuild_ship_fitting`] until Phase 2
    /// migrates all callers and restricts `store_ship_fitting` / `invalidate_ship_fitting`
    /// visibility.
    pub fn get_or_rebuild_ship_fitting(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        self.fit(char_id, ship_id)
    }

    pub fn fit(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        self.verify_ship_fit_access(char_id, ship_id)?;
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
        let active_projected = self.systems.ship_ewar_projections(ship_id);
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
            for (_, d) in self.systems.all_active_drones() {
                if d.ball_id > 0 && d.parent_ship_id == ship_id && !d.is_abandoned {
                    for id in [d.drone_item_id, d.ball_id] {
                        if !existing.drones.iter().any(|&(eid, _)| eid == id) {
                            existing.drones.push((id, d.type_id));
                            drones_changed = true;
                        }
                    }
                }
            }
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
            self.db.with_snapshot(|s| {
                (s.structures.contains_key(&ship_id)
                    || s.characters
                        .get(&char_id)
                        .is_some_and(|c| c.active_ship_id == ship_id))
                    .then(|| s.items.get(&ship_id).cloned())
                    .flatten()
            })
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
        let (online_states, active_states, active_charges, persisted_fit_inputs) =
            self.db.with_snapshot(|s| {
                (
                    s.module_online_states.clone(),
                    s.module_active_states.clone(),
                    s.module_active_charges.clone(),
                    s.ship_fit_inputs.get(&ship_id).cloned(),
                )
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
        for (_, d) in self.systems.all_active_drones() {
            if d.ball_id > 0 && d.parent_ship_id == ship_id && !d.is_abandoned {
                for id in [d.drone_item_id, d.ball_id] {
                    if !fit.drones.iter().any(|&(eid, _)| eid == id) {
                        fit.drones.push((id, d.type_id));
                    }
                }
            }
        }
        for &(probe_id, probe_type_id) in &active_probes {
            if !fit.drones.iter().any(|&(id, _)| id == probe_id) {
                fit.drones.push((probe_id, probe_type_id));
            }
        }
        if let Some(pfi) = persisted_fit_inputs {
            fit.apply_persisted_inputs(
                &eve_dogma::PersistedFitInputs {
                    active_stance_type_id: pfi.active_stance_type_id,
                    last_stance_switch_ms: pfi.last_stance_switch_ms,
                    overloaded_modules: pfi.overloaded_modules,
                },
                &self.sde,
            );
        }
        fit.recalculate_stats(&self.sde);

        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id, fit.clone());
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

    /// Deterministically advances the injected server clock and steps every active `SystemActor`
    /// in increments of `<= 100 ms` (`GH #1077 §C`).
    ///
    /// Whenever a step crosses one or more whole Unix seconds, the unified synchronous clock
    /// pipeline (`persist_clock_advance_meta` + `run_clock_advanced_sync_pipeline`) is invoked
    /// before stepping the `Ballpark` actors for that sub-second slice.
    #[cfg(any(test, feature = "testkit"))]
    pub async fn advance_sim_ms(&self, ms: u64) {
        let mut remaining = ms;
        while remaining > 0 {
            let step_ms = remaining.min(100);
            remaining -= step_ms;

            let prev_unix = self.clock().now_unix_secs();
            self.clock().advance_millis(step_ms);
            let cur_unix = self.clock().now_unix_secs();
            if cur_unix > prev_unix {
                let secs = cur_unix - prev_unix;
                let offset_secs = self.clock().offset_secs();
                self.persist_clock_advance_meta(cur_unix, offset_secs);
                self.run_clock_advanced_sync_pipeline(prev_unix, secs, true, true);
            }

            let dt = step_ms as f64 / 1_000.0;
            for (_sys_id, handle) in self.systems.active_system_handles() {
                let _ = handle.step_manual_tick_only(dt).await;
            }
        }
    }

    /// Advances simulation time in `<= 100 ms` steps via [`Self::advance_sim_ms`] until `pred()`
    /// returns `true` or `max_ms` elapses (`GH #1077 §C`).
    #[cfg(any(test, feature = "testkit"))]
    pub async fn advance_until<F>(&self, mut pred: F, max_ms: u64) -> Result<u64, String>
    where
        F: FnMut() -> bool,
    {
        if pred() {
            return Ok(0);
        }
        let mut elapsed_ms = 0u64;
        while elapsed_ms < max_ms {
            let step_ms = (max_ms - elapsed_ms).min(100);
            self.advance_sim_ms(step_ms).await;
            elapsed_ms += step_ms;
            if pred() {
                return Ok(elapsed_ms);
            }
        }
        Err(format!(
            "advance_until timed out after {elapsed_ms} ms without predicate becoming true"
        ))
    }

    /// Async variant of [`Self::advance_until`] for predicates that query async actors or drain
    /// test harness frames (`GH #1077 §C`).
    #[cfg(any(test, feature = "testkit"))]
    pub async fn advance_until_async<F, Fut>(
        &self,
        mut pred: F,
        max_ms: u64,
    ) -> Result<u64, String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        if pred().await {
            return Ok(0);
        }
        let mut elapsed_ms = 0u64;
        while elapsed_ms < max_ms {
            let step_ms = (max_ms - elapsed_ms).min(100);
            self.advance_sim_ms(step_ms).await;
            elapsed_ms += step_ms;
            if pred().await {
                return Ok(elapsed_ms);
            }
        }
        Err(format!(
            "advance_until_async timed out after {elapsed_ms} ms without predicate becoming true"
        ))
    }

    /// Returns the Unix timestamp when `module_id`'s pending reload completes, if scheduled (`§C`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn pending_reload_complete_at(&self, module_id: i64) -> Option<u64> {
        let now = self.now_unix_secs();
        self.db
            .with_snapshot(|s| s.cluster_and_runtime.module_reload_until_unix.get(&module_id).copied())
            .filter(|&until| now < until)
    }

    /// Returns the Unix timestamp when `char_id`'s session-change timer expires, if active (`§C`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn session_change_timer_until(&self, char_id: i64) -> Option<u64> {
        let now = self.now_unix_secs();
        self.db
            .with_snapshot(|s| {
                s.sp6_lifecycle
                    .session_change_timers
                    .get(&char_id)
                    .map(|t| t.expires_unix)
            })
            .filter(|&until| now < until)
    }

    /// Returns the immutable RPC rate-budget policy configured on this `ServerContext` (`#1540`).
    #[inline]
    pub fn rpc_rate_limit_policy(&self) -> RpcRateLimitPolicy {
        self.rpc_rate_limiter.policy()
    }

    /// Attempts to consume one RPC rate-budget token for `session_id` at `now`.
    pub(crate) fn try_consume_rpc_token_at(
        &self,
        session_id: u64,
        now: std::time::Instant,
        is_bootstrap: bool,
    ) -> bool {
        self.rpc_rate_limiter
            .try_consume_at(session_id, now, is_bootstrap)
    }

    /// Refreshes the bootstrap burst allowance for `session_id` on major transitions,
    /// subject to `RPC_BOOTSTRAP_GRANT_COOLDOWN`.
    pub(crate) fn grant_session_bootstrap_burst_at(
        &self,
        session_id: u64,
        now: std::time::Instant,
    ) -> bool {
        self.rpc_rate_limiter.grant_bootstrap_at(session_id, now)
    }

    /// Refreshes the bootstrap burst allowance for `session_id` at `Instant::now()`.
    pub(crate) fn grant_session_bootstrap_burst(&self, session_id: u64) -> bool {
        self.grant_session_bootstrap_burst_at(session_id, std::time::Instant::now())
    }

    #[cfg(test)]
    pub(crate) fn rpc_rate_bucket_count(&self) -> usize {
        self.rpc_rate_limiter.bucket_count()
    }

    #[cfg(test)]
    pub(crate) fn bootstrap_tokens_remaining(&self, session_id: u64) -> Option<u32> {
        self.rpc_rate_limiter.bootstrap_tokens_remaining(session_id)
    }

    /// Removes the per-session RPC rate bucket when a client connection closes.
    pub fn release_session_rate_bucket(&self, session_id: u64) {
        self.rpc_rate_limiter.release_session(session_id);
    }

    /// Returns the remaining lock acquisition milliseconds for `ship_id -> target_id`, if locking (`§C`).
    #[cfg(any(test, feature = "testkit"))]
    pub async fn pending_lock_remaining_ms(
        &self,
        solar_system_id: u32,
        ship_id: i64,
        target_id: i64,
    ) -> Option<f64> {
        let handle = self.systems.get_handle(solar_system_id)?;
        let locks = handle.get_target_locks(ship_id).await.ok()?;
        locks
            .into_iter()
            .find(|l| l.target_id == target_id && !l.locked)
            .map(|l| l.remaining_lock_ms)
    }
}

#[cfg(any(test, feature = "testkit"))]
fn acquire_test_env_read_guard() -> crate::TestEnvReadGuard {
    crate::lock_test_env_read()
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
#[cfg(test)]
#[path = "context_edges_tests.rs"]
mod context_edges_tests;

