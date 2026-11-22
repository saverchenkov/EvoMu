//! Explicit solar-system lifecycle types, named wake reasons, and
//! content-provider registration interface (`#1542`).

#![forbid(unsafe_code)]

use super::{SystemActorState, SystemHandle, SystemRegistry};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_destiny::Ballpark;
use eve_sde::{SdeRegistry, SolarSystemId};
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
}

impl SystemLifecycleState {
    /// Returns `true` when the solar system is initializing or running in memory (`Loading | Live | Hibernating`).
    pub const fn is_loaded(self) -> bool {
        !matches!(self, Self::Cold)
    }
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

    pub(crate) fn metadata(
        &self,
    ) -> Result<Vec<SystemContentProviderMetadata>, SystemActivationError> {
        let entries =
            self.providers
                .read()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: None,
                    lock: "content_providers",
                })?;
        let providers: Vec<Arc<dyn SystemContentProvider>> = entries
            .iter()
            .map(|entry| Arc::clone(&entry.provider))
            .collect();
        drop(entries);
        Ok(providers.iter().map(|p| p.metadata()).collect())
    }

    #[cfg(test)]
    pub(crate) fn snapshot_for_test(
        &self,
    ) -> Result<Vec<Arc<dyn SystemContentProvider>>, SystemActivationError> {
        let entries =
            self.providers
                .read()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: None,
                    lock: "content_providers",
                })?;
        Ok(entries
            .iter()
            .map(|entry| Arc::clone(&entry.provider))
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
}
