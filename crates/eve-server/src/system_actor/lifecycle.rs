//! Explicit solar-system lifecycle types, named wake reasons, and
//! content-provider registration interface (`#1542`).

#![forbid(unsafe_code)]

use super::{SystemActorState, SystemHandle, SystemRegistry};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_destiny::Ballpark;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

/// Explicit runtime lifecycle states for a solar system managed by [`SystemRegistry`] (`#1542`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemLifecycleState {
    /// No live actor or ballpark is resident in memory for this solar system.
    Cold,
    /// A single-flight activation pipeline is populating celestials, persisted objects,
    /// and registered content provider seeds before publishing the live handle.
    Loading,
    /// The solar system actor is running with active entities, subscribers, or pending work.
    Live,
    /// The solar system actor is running at idle LOD 0 hibernation (no active subscribers,
    /// dynamic balls, or pending timers) and may be evicted back to `Cold`.
    Hibernating,
    /// A single-flight shutdown/eviction pipeline is stopping the actor before returning the slot to `Cold`.
    Stopping,
}

impl SystemLifecycleState {
    /// Returns `true` when the solar system is initializing or running in memory (`Loading | Live | Hibernating`).
    pub const fn is_loaded(self) -> bool {
        matches!(self, Self::Loading | Self::Live | Self::Hibernating)
    }
}

/// Recorded lifecycle state transition telemetry per `(solar_system_id, generation)` (`#1542`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleTransitionRecord {
    pub solar_system_id: SolarSystemId,
    pub generation: u64,
    pub from: SystemLifecycleState,
    pub to: SystemLifecycleState,
    pub wake_reason: Option<SystemWakeReason>,
    pub detail: &'static str,
}

/// Typed reasons a client session arrival may wake a `Cold` solar system (`#1542`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionArrivalWakeReason {
    LoginInSpace,
    Undock,
    Jump,
    CloneJump,
}

/// Typed reasons the background cluster tick may wake a `Cold` solar system (`#1542`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClusterTickWakeReason {
    /// A ship self-destruct countdown reached its deadline.
    SelfDestructDue,
    /// A disconnected pilot's ship still has an active emergency warp or timer in space.
    DisconnectedShip,
    /// A pending safe-logoff countdown completed and must finalize despawn.
    SafeLogoffCompleted,
    /// A persisted anchoring mobile warp disruptor timer matured.
    MobileWarpDisruptorActivation,
    /// Other scheduled cluster work queued for the solar system.
    ScheduledWork,
}

/// Explicit reason recorded when a solar system is woken from `Cold` (`#1542`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SystemWakeReason {
    SessionArrival(SessionArrivalWakeReason),
    ClusterTick(ClusterTickWakeReason),
    GmCommand(String),
    /// Transitional compatibility bridge for unmigrated legacy `get_or_spawn` callers.
    LegacyGetOrSpawn,
}

/// Fail-closed error returned by named solar-system wake and lifecycle operations (`#1542`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemActivationError {
    /// No active Tokio runtime reactor was available to spawn the solar system actor task.
    NoTokioRuntime { solar_system_id: SolarSystemId },
    /// Re-entrant wake attempted on the same thread while `solar_system_id` is still `Loading`.
    ReentrantLoading { solar_system_id: SolarSystemId },
    /// A concurrent activation on another thread failed or aborted before reaching `Live`.
    ConcurrentActivationAborted { solar_system_id: SolarSystemId },
    /// A registered [`SystemContentProvider`] failed during `seed`, `tick`, or `expire`.
    ContentProviderFailed {
        solar_system_id: SolarSystemId,
        provider: String,
        message: String,
    },
    /// GM wake was invoked with an empty or blank command string.
    InvalidGmCommand { solar_system_id: SolarSystemId },
    /// Registration failed because a [`SystemContentProvider`] had an empty or mismatched name.
    InvalidContentProvider { provider: String, message: String },
    /// Refused to block waiting for another thread's activation while holding ranked or DB write locks.
    WaitRejectedWhileHoldingLocks { solar_system_id: SolarSystemId },
    /// An internal synchronization lock was poisoned; operation failed closed without partial state.
    PoisonedLock {
        solar_system_id: Option<SolarSystemId>,
        lock: &'static str,
    },
}

impl fmt::Display for SystemActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTokioRuntime { solar_system_id } => {
                write!(f, "no active Tokio runtime to activate solar system {solar_system_id}")
            }
            Self::ReentrantLoading { solar_system_id } => {
                write!(
                    f,
                    "re-entrant activation attempted while solar system {solar_system_id} is Loading"
                )
            }
            Self::ConcurrentActivationAborted { solar_system_id } => {
                write!(
                    f,
                    "concurrent activation aborted before solar system {solar_system_id} became Live"
                )
            }
            Self::ContentProviderFailed {
                solar_system_id,
                provider,
                message,
            } => write!(
                f,
                "content provider '{provider}' failed for solar system {solar_system_id}: {message}"
            ),
            Self::InvalidGmCommand { solar_system_id } => {
                write!(
                    f,
                    "invalid empty GM command for waking solar system {solar_system_id}"
                )
            }
            Self::InvalidContentProvider { provider, message } => {
                write!(f, "invalid content provider '{provider}': {message}")
            }
            Self::WaitRejectedWhileHoldingLocks { solar_system_id } => write!(
                f,
                "refused to block waiting for solar system {solar_system_id} activation while holding locks"
            ),
            Self::PoisonedLock {
                solar_system_id: Some(id),
                lock,
            } => write!(f, "poisoned lifecycle lock '{lock}' while handling solar system {id}"),
            Self::PoisonedLock {
                solar_system_id: None,
                lock,
            } => write!(f, "poisoned lifecycle lock '{lock}'"),
        }
    }
}

impl std::error::Error for SystemActivationError {}

/// Context passed to [`SystemContentProvider::seed`] during the `Cold -> Loading -> Live`
/// activation pipeline after static celestials/gates and persisted space objects have been populated.
pub struct SystemContentSeedContext<'a> {
    pub solar_system_id: SolarSystemId,
    pub wake_reason: &'a SystemWakeReason,
    pub registry: &'a SystemRegistry,
    pub ballpark: &'a mut Ballpark,
    #[allow(dead_code)] // Populated and consumed in Track 1b / Wave F2 content providers (#1542).
    pub(crate) actor_state: &'a mut SystemActorState,
    pub sde: Option<&'a Arc<SdeRegistry>>,
    pub db: Option<&'a Arc<eve_db::WorldRepository>>,
    pub db_read_safe: bool,
    pub belt_respawn_interval_secs: u64,
    pub downtime_utc_secs_of_day: u64,
    pub mechanic_rng: &'a Arc<crate::rng::MechanicRng>,
    pub(in crate::system_actor) staged_special_site_balls: HashSet<i64>,
    pub(in crate::system_actor) staged_wh_ore_anomalies: HashSet<String>,
    pub(in crate::system_actor) staged_post_unlock_ops: &'a mut super::post_unlock::PostUnlockOps,
}

/// Context passed to [`SystemContentProvider::tick`] and [`SystemContentProvider::expire`]
/// for periodic maintenance on an already-loaded solar system (`#1542`).
pub struct SystemContentMaintenanceContext<'a> {
    pub solar_system_id: SolarSystemId,
    pub registry: &'a SystemRegistry,
    pub handle: &'a SystemHandle,
    pub sde: Option<&'a Arc<SdeRegistry>>,
    pub db: Option<&'a Arc<eve_db::WorldRepository>>,
    pub now_unix_secs: u64,
}

/// Declarative metadata describing which lifecycle steps a [`SystemContentProvider`] participates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemContentProviderMetadata {
    /// Stable provider identifier (for diagnostics and duplicate-registration prevention).
    pub name: &'static str,
    /// Whether this provider exposes a periodic `tick` hook.
    pub has_tick: bool,
    /// Whether this provider exposes a periodic `expire` hook.
    pub has_expire: bool,
}

/// Content-domain lifecycle hook registered on a [`SystemRegistry`] instance (`#1542`).
///
/// Providers execute `seed` in registration order during `Cold -> Loading` activation after static
/// celestials/gates and persisted DB space objects are loaded, and optionally expose `tick` and
/// `expire` maintenance hooks for live systems.
pub trait SystemContentProvider: Send + Sync + 'static {
    /// Unique stable provider identifier.
    fn name(&self) -> &'static str;

    /// Capability metadata advertised by this provider.
    fn metadata(&self) -> SystemContentProviderMetadata {
        SystemContentProviderMetadata {
            name: self.name(),
            has_tick: false,
            has_expire: false,
        }
    }

    /// Populates system-specific objects or state during `Cold -> Loading -> Live` activation.
    /// Returning `Err` fails closed and keeps activation from publishing partial state.
    fn seed(&self, ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String>;

    /// Optional periodic tick step invoked only for already-loaded systems.
    fn tick(&self, _ctx: &SystemContentMaintenanceContext<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Optional periodic expiry step invoked only for already-loaded systems.
    fn expire(&self, _ctx: &SystemContentMaintenanceContext<'_>) -> Result<(), String> {
        Ok(())
    }
}

struct RegisteredContentProvider {
    name: String,
    provider: Arc<dyn SystemContentProvider>,
}

/// Instance-owned ordered registry of [`SystemContentProvider`] hooks on [`SystemRegistry`]
/// (`#1542`, zero global mutable state).
pub(crate) struct LifecycleContentProviders {
    providers: RankedRwLock<Vec<RegisteredContentProvider>, RANK_LEAF>,
}

impl Default for LifecycleContentProviders {
    fn default() -> Self {
        Self {
            providers: RankedRwLock::new(Vec::new()),
        }
    }
}

impl LifecycleContentProviders {
    pub(crate) fn register(
        &self,
        provider: Arc<dyn SystemContentProvider>,
    ) -> Result<bool, SystemActivationError> {
        let name = provider.name();
        let meta = provider.metadata();
        if name.trim().is_empty() {
            return Err(SystemActivationError::InvalidContentProvider {
                provider: name.to_string(),
                message: "content provider name must not be empty".to_string(),
            });
        }
        if meta.name != name {
            return Err(SystemActivationError::InvalidContentProvider {
                provider: name.to_string(),
                message: format!(
                    "content provider metadata().name '{}' does not match name() '{name}'",
                    meta.name
                ),
            });
        }
        let name_owned = name.to_string();
        let mut entries =
            self.providers
                .write()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: None,
                    lock: "content_providers",
                })?;
        if entries.iter().any(|existing| existing.name == name_owned) {
            return Ok(false);
        }
        entries.push(RegisteredContentProvider {
            name: name_owned,
            provider,
        });
        Ok(true)
    }

    fn snapshot_entries(
        &self,
        solar_system_id: Option<SolarSystemId>,
    ) -> Result<Vec<(String, Arc<dyn SystemContentProvider>)>, SystemActivationError> {
        let entries =
            self.providers
                .read()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id,
                    lock: "content_providers",
                })?;
        Ok(entries
            .iter()
            .map(|entry| (entry.name.clone(), Arc::clone(&entry.provider)))
            .collect())
    }

    pub(crate) fn seed_all(
        &self,
        ctx: &mut SystemContentSeedContext<'_>,
    ) -> Result<(), SystemActivationError> {
        let providers = self.snapshot_entries(Some(ctx.solar_system_id))?;
        for (name, provider) in providers {
            if let Err(message) = provider.seed(ctx) {
                return Err(SystemActivationError::ContentProviderFailed {
                    solar_system_id: ctx.solar_system_id,
                    provider: name,
                    message,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn tick_all(
        &self,
        ctx: &SystemContentMaintenanceContext<'_>,
    ) -> Result<(), SystemActivationError> {
        self.run_maintenance(ctx, false)
    }

    pub(crate) fn expire_all(
        &self,
        ctx: &SystemContentMaintenanceContext<'_>,
    ) -> Result<(), SystemActivationError> {
        self.run_maintenance(ctx, true)
    }

    fn run_maintenance(
        &self,
        ctx: &SystemContentMaintenanceContext<'_>,
        is_expire: bool,
    ) -> Result<(), SystemActivationError> {
        let providers = self.snapshot_entries(Some(ctx.solar_system_id))?;
        for (name, provider) in providers {
            let meta = provider.metadata();
            let result = if is_expire {
                if !meta.has_expire {
                    continue;
                }
                provider.expire(ctx)
            } else {
                if !meta.has_tick {
                    continue;
                }
                provider.tick(ctx)
            };
            if let Err(message) = result {
                return Err(SystemActivationError::ContentProviderFailed {
                    solar_system_id: ctx.solar_system_id,
                    provider: name,
                    message,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn metadata(
        &self,
    ) -> Result<Vec<SystemContentProviderMetadata>, SystemActivationError> {
        let entries = self.snapshot_entries(None)?;
        Ok(entries
            .into_iter()
            .map(|(_, provider)| provider.metadata())
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn snapshot_for_test(
        &self,
    ) -> Result<Vec<Arc<dyn SystemContentProvider>>, SystemActivationError> {
        let entries = self.snapshot_entries(None)?;
        Ok(entries
            .into_iter()
            .map(|(_, provider)| provider)
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let providers = &self.providers;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = providers.write().unwrap_or_else(|e| e.into_inner());
            panic!("intentional test poison of content_providers lock");
        }));
    }

    #[cfg(test)]
    pub(crate) fn read_held_ranks_for_test(&self) -> Vec<u16> {
        let _guard = self.providers.read().expect("content_providers read");
        eve_db::lock_rank::held_lock_ranks()
    }
}

impl SystemRegistry {
    /// Registers a [`SystemContentProvider`] on this registry instance in deterministic order (`#1542`).
    /// Returns `Ok(false)` if a provider with the same `name()` is already registered.
    pub fn register_content_provider(
        &self,
        provider: Arc<dyn SystemContentProvider>,
    ) -> Result<bool, SystemActivationError> {
        self.content_providers.register(provider)
    }

    /// Returns capability metadata for all content providers registered on this registry (`#1542`).
    pub fn registered_content_providers(
        &self,
    ) -> Result<Vec<SystemContentProviderMetadata>, SystemActivationError> {
        self.content_providers.metadata()
    }

    /// Runs `tick()` for all registered content providers with `has_tick == true` (`#1542`).
    pub fn tick_content_providers(
        &self,
        ctx: &SystemContentMaintenanceContext<'_>,
    ) -> Result<(), SystemActivationError> {
        self.content_providers.tick_all(ctx)
    }

    /// Runs `expire()` for all registered content providers with `has_expire == true` (`#1542`).
    pub fn expire_content_providers(
        &self,
        ctx: &SystemContentMaintenanceContext<'_>,
    ) -> Result<(), SystemActivationError> {
        self.content_providers.expire_all(ctx)
    }

    /// Runs registered `tick` and `expire` hooks for a single loaded solar system handle (`#1542`).
    pub fn run_content_maintenance_for_handle(
        &self,
        solar_system_id: SolarSystemId,
        handle: &SystemHandle,
        now_unix_secs: u64,
    ) -> Result<(), SystemActivationError> {
        let ctx = SystemContentMaintenanceContext {
            solar_system_id,
            registry: self,
            handle,
            sde: self.sde.as_ref(),
            db: self.db.as_ref(),
            now_unix_secs,
        };
        self.content_providers.tick_all(&ctx)?;
        self.content_providers.expire_all(&ctx)
    }

    /// Runs `tick` and `expire` hooks across all currently loaded active systems in sorted order (`#1542`).
    pub fn run_content_maintenance_for_active_systems(
        &self,
        now_unix_secs: u64,
    ) -> Result<usize, SystemActivationError> {
        let active_handles = self.active_system_handles();
        let count = active_handles.len();
        for (solar_system_id, handle) in active_handles {
            self.run_content_maintenance_for_handle(solar_system_id, &handle, now_unix_secs)?;
        }
        Ok(count)
    }
}

impl SystemContentSeedContext<'_> {
    /// Returns the authoritative current server Unix timestamp for activation seeding (`#1542`).
    pub(crate) fn now_unix_secs(&self) -> u64 {
        self.db
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(self.ballpark.sim_unix_secs)
    }

    /// Returns the authoritative BlueOS FILETIME ticks for activation seeding (`#1542`).
    pub(crate) fn now_blueos_filetime_ticks(&self) -> i64 {
        self.db
            .map(|db| db.clock().now_blueos_filetime_ticks())
            .unwrap_or_else(|| crate::sp6_lifecycle::unix_to_filetime(self.ballpark.sim_unix_secs))
    }

    /// Stages a special-site ball claim during activation seeding without mutating shared
    /// [`SystemRegistry`] markers until `Loading -> Live` publish commits (`#1542`).
    pub(crate) fn claim_special_site_ball(&mut self, ball_id: i64) -> bool {
        if self
            .registry
            .is_special_site_ball_spawned(self.solar_system_id, ball_id)
        {
            return false;
        }
        self.staged_special_site_balls.insert(ball_id)
    }

    /// Stages a wormhole ore anomaly hydration claim during activation seeding without mutating
    /// shared [`SystemRegistry`] markers until `Loading -> Live` publish commits (`#1542`).
    pub(crate) fn claim_wh_ore_anomaly_hydration(&mut self, anomaly_id: &str) -> bool {
        if self
            .registry
            .is_wh_ore_anomaly_hydrated(self.solar_system_id, anomaly_id)
        {
            return false;
        }
        self.staged_wh_ore_anomalies.insert(anomaly_id.to_string())
    }

    /// Inserts or replaces a non-combat ball directly inside the activating `Ballpark` (`#1542`).
    pub(crate) fn spawn_ball(&mut self, ball: eve_destiny::Ball) {
        let ball_id = ball.id;
        let existing_meta = self.ballpark.ball_slim_meta.get(&ball_id).cloned();
        if self.ballpark.balls.contains_key(&ball_id) {
            let _ = self.ballpark.remove_ball(ball_id);
        }
        if let Some(meta) = existing_meta {
            self.ballpark.set_ball_slim_meta(ball_id, meta);
        } else {
            let sde_opt = self.sde.cloned();
            super::belts::ensure_default_slim_meta(self.ballpark, &ball, &sde_opt);
        }
        if matches!(ball.mode, eve_destiny::MovementMode::Rigid) {
            let meta = self
                .ballpark
                .ball_slim_meta
                .get(&ball_id)
                .cloned()
                .unwrap_or_default();
            self.ballpark.add_static_ball_with_meta(ball, meta);
            return;
        }
        if super::belts::should_pin_stationary_ball(&ball, self.ballpark.ball_slim_meta.get(&ball_id))
        {
            self.ballpark.mark_ball_non_free(ball_id, false);
        }
        self.ballpark.add_ball(ball);
    }

    /// Inserts or replaces a ball with combat state and optional custom slim metadata directly
    /// inside the activating `Ballpark` (`#1542`).
    pub(crate) fn spawn_ball_with_combat_and_meta(
        &mut self,
        ball: eve_destiny::Ball,
        combat_state: eve_destiny::CombatState,
        slim_meta: Option<eve_destiny::SlimItemMeta>,
    ) {
        let sde_opt = self.sde.cloned();
        let db_opt = self.db.cloned();
        super::cycles_tackle::spawn_ball_with_combat_in_actor_collecting(
            self.ballpark.solar_system_id,
            self.ballpark,
            &mut self.actor_state.active_module_cycles,
            &mut self.actor_state.base_scramble_strengths,
            &mut self.actor_state.base_core_strengths,
            &sde_opt,
            &db_opt,
            None,
            ball,
            combat_state,
            slim_meta,
            self.staged_post_unlock_ops,
        );
    }

    /// Inserts or replaces a ball with combat state and default SDE slim metadata inside the
    /// activating `Ballpark` (`#1542`).
    pub(crate) fn spawn_ball_with_combat(
        &mut self,
        ball: eve_destiny::Ball,
        combat_state: eve_destiny::CombatState,
    ) {
        self.spawn_ball_with_combat_and_meta(ball, combat_state, None);
    }

    /// Inserts or replaces a ball with combat state and explicit slim metadata inside the
    /// activating `Ballpark` (`#1542`).
    pub(crate) fn spawn_ball_with_meta(
        &mut self,
        ball: eve_destiny::Ball,
        combat_state: eve_destiny::CombatState,
        slim_meta: eve_destiny::SlimItemMeta,
    ) {
        self.spawn_ball_with_combat_and_meta(ball, combat_state, Some(slim_meta));
    }

    /// Populates an asteroid directly inside the activating `Ballpark` and `SystemActorState` (`#1542`).
    pub(crate) fn spawn_asteroid(
        &mut self,
        asteroid: super::types::AsteroidState,
        ball: eve_destiny::Ball,
    ) {
        let ball_id = asteroid.asteroid_id;
        if self.actor_state.asteroids.contains_key(&ball_id) {
            return;
        }
        let mut ast_ball = ball;
        let sde_opt = self.sde.cloned();
        super::SystemHandle::spawn_asteroid_ball_in_ballpark(
            self.ballpark,
            &sde_opt,
            &mut ast_ball,
        );
        if self.db.is_some() {
            self.staged_post_unlock_ops.push(
                super::post_unlock::PostUnlockIntent::BeltAsteroidInsertIfAbsent(
                    eve_db::AsteroidRecord {
                        asteroid_id: ball_id,
                        belt_id: asteroid.belt_id,
                        type_id: ast_ball.type_id,
                        solar_system_id: self.solar_system_id,
                        position: (ast_ball.position.x, ast_ball.position.y, ast_ball.position.z),
                        quantity: i64::try_from(asteroid.remaining_units).unwrap_or(i64::MAX),
                        initial_quantity: i64::try_from(asteroid.initial_units).unwrap_or(i64::MAX),
                        radius: ast_ball.radius,
                    },
                ),
            );
        }
        self.actor_state
            .asteroid_templates
            .insert(ball_id, (asteroid.clone(), ast_ball));
        self.actor_state.asteroids.insert(ball_id, asteroid);
    }
}
