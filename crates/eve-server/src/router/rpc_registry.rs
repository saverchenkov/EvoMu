//! Compiled RPC registry replacing legacy file-scan dispatch discovery incrementally.
//!
//! Each migrated service module exposes `pub const RPCS: &[RpcEntry]` alongside its
//! handlers. [`COMPILED_RPC_MODULES`] lists every migrated module and [`RpcRegistry`]
//! merges those slices into an O(1) lookup table owned by [`ServerContext`], panicking
//! loudly at startup if two entries claim the same `(service, method)` pair.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::context::{BoundObjectEntry, ServerContext};

/// Caller privilege class required to invoke an RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcCallerClass {
    /// Pre-authentication RPC callable before a session logs into an account.
    Unauthenticated,
    /// Authenticated account session (with or without a selected character).
    Account,
    /// Session with an active selected character.
    Character,
    /// Session holding GM/QA administrative roles.
    Gm,
}

/// Scene location requirement for invoking an RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcSceneClass {
    /// Must be docked in an NPC station or Upwell structure.
    Docked,
    /// Must be undocked in space.
    InSpace,
    /// Callable regardless of docked vs. in-space state.
    Either,
}

/// Coarse side-effect classification for negative-matrix and authorization audits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcAccessKind {
    /// Side-effect-free query or catalog lookup.
    Read,
    /// State-mutating command or telemetry write.
    Write,
}

/// Heap-allocated future returned by async RPC dispatch entries.
pub type RpcBoxFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<PyRep, BridgeRpcError>>> + Send + 'a>>;

/// Function signature variants supported by compiled RPC handlers.
#[derive(Clone, Copy)]
pub enum RpcDispatchHandler {
    /// Synchronous handler taking `(ctx, session, service, method, args, kwargs)`.
    Sync(
        fn(
            &ServerContext,
            &mut SessionState,
            &str,
            &str,
            &[PyRep],
            &HashMap<String, PyRep>,
        ) -> Option<Result<PyRep, BridgeRpcError>>,
    ),
    /// Synchronous handler taking an immutable session borrow.
    SyncReadOnlySession(
        fn(
            &ServerContext,
            &SessionState,
            &str,
            &str,
            &[PyRep],
            &HashMap<String, PyRep>,
        ) -> Option<Result<PyRep, BridgeRpcError>>,
    ),
    /// Synchronous handler that does not accept keyword arguments.
    SyncNoKwargs(
        fn(
            &ServerContext,
            &mut SessionState,
            &str,
            &str,
            &[PyRep],
        ) -> Option<Result<PyRep, BridgeRpcError>>,
    ),
    /// Synchronous handler scoped to a single service name (`service` parameter omitted).
    SyncNoService(
        fn(
            &ServerContext,
            &SessionState,
            &str,
            &[PyRep],
            &HashMap<String, PyRep>,
        ) -> Option<Result<PyRep, BridgeRpcError>>,
    ),
    /// Synchronous handler receiving `caller_trace` metadata.
    SyncCallerTrace(
        fn(
            &ServerContext,
            &mut SessionState,
            &str,
            &str,
            &[PyRep],
            &HashMap<String, PyRep>,
            Option<&str>,
        ) -> Option<Result<PyRep, BridgeRpcError>>,
    ),
    /// Asynchronous handler returning a boxed future.
    Async(
        for<'a> fn(
            &'a ServerContext,
            &'a mut SessionState,
            &'a str,
            &'a str,
            &'a [PyRep],
            &'a HashMap<String, PyRep>,
        ) -> RpcBoxFuture<'a>,
    ),
    /// Asynchronous handler receiving `caller_trace` metadata.
    AsyncCallerTrace(
        for<'a> fn(
            &'a ServerContext,
            &'a mut SessionState,
            &'a str,
            &'a str,
            &'a [PyRep],
            &'a HashMap<String, PyRep>,
            Option<&'a str>,
        ) -> RpcBoxFuture<'a>,
    ),
    /// Asynchronous bound-object handler receiving `Option<&BoundObjectEntry>`.
    BoundAsync(
        for<'a> fn(
            &'a ServerContext,
            &'a mut SessionState,
            Option<&'a BoundObjectEntry>,
            &'a str,
            &'a str,
            &'a [PyRep],
            &'a HashMap<String, PyRep>,
        ) -> RpcBoxFuture<'a>,
    ),
}

impl fmt::Debug for RpcDispatchHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Sync(_) => "Sync",
            Self::SyncReadOnlySession(_) => "SyncReadOnlySession",
            Self::SyncNoKwargs(_) => "SyncNoKwargs",
            Self::SyncNoService(_) => "SyncNoService",
            Self::SyncCallerTrace(_) => "SyncCallerTrace",
            Self::Async(_) => "Async",
            Self::AsyncCallerTrace(_) => "AsyncCallerTrace",
            Self::BoundAsync(_) => "BoundAsync",
        };
        f.write_str(label)
    }
}

/// Static declaration of a single compiled RPC endpoint.
#[derive(Debug, Clone, Copy)]
pub struct RpcEntry {
    pub service: &'static str,
    pub method: &'static str,
    pub who: RpcCallerClass,
    pub scene: RpcSceneClass,
    pub access: RpcAccessKind,
    pub handler: RpcDispatchHandler,
}

/// Const constructor for concise service-module `RPCS` tables.
pub const fn rpc_entry(
    service: &'static str,
    method: &'static str,
    who: RpcCallerClass,
    scene: RpcSceneClass,
    access: RpcAccessKind,
    handler: RpcDispatchHandler,
) -> RpcEntry {
    RpcEntry {
        service,
        method,
        who,
        scene,
        access,
        handler,
    }
}

/// Pairing between a migrated service `.rs` file path (relative to `crates/eve-server/src/`)
/// and its compiled `RPCS` slice.
#[derive(Debug, Clone, Copy)]
pub struct CompiledRpcModule {
    pub file: &'static str,
    pub rpcs: &'static [RpcEntry],
}

/// Migrated handler modules included in Track 2A's compiled RPC registry.
pub const COMPILED_RPC_MODULES: &[CompiledRpcModule] = &[
    CompiledRpcModule {
        file: "services/character_energy.rs",
        rpcs: crate::services::character_energy::RPCS,
    },
    CompiledRpcModule {
        file: "services/character_expert_systems.rs",
        rpcs: crate::services::character_expert_systems::RPCS,
    },
    CompiledRpcModule {
        file: "services/character_certificates.rs",
        rpcs: crate::services::character_certificates::RPCS,
    },
    CompiledRpcModule {
        file: "services/lookup_svc.rs",
        rpcs: crate::services::lookup_svc::RPCS,
    },
    CompiledRpcModule {
        file: "services/ingamereport_ops.rs",
        rpcs: crate::services::ingamereport_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/fast_checkout_and_activity.rs",
        rpcs: crate::services::fast_checkout_and_activity::RPCS,
    },
    CompiledRpcModule {
        file: "services/lp_store.rs",
        rpcs: crate::services::lp_store::RPCS,
    },
    CompiledRpcModule {
        file: "services/operations_manager.rs",
        rpcs: crate::services::operations_manager::RPCS,
    },
    CompiledRpcModule {
        file: "services/pvp_filament_mgr.rs",
        rpcs: crate::services::pvp_filament_mgr::RPCS,
    },
    CompiledRpcModule {
        file: "services/project_discovery.rs",
        rpcs: crate::services::project_discovery::RPCS,
    },
    CompiledRpcModule {
        file: "services/season_manager.rs",
        rpcs: crate::services::season_manager::RPCS,
    },
    CompiledRpcModule {
        file: "services/tourney_service.rs",
        rpcs: crate::services::tourney_service::RPCS,
    },
    CompiledRpcModule {
        file: "services/tourney_arena.rs",
        rpcs: crate::services::tourney_service::tourney_arena::RPCS,
    },
    CompiledRpcModule {
        file: "services/mining_advanced.rs",
        rpcs: crate::services::mining_advanced::RPCS,
    },
    CompiledRpcModule {
        file: "services/mining_compression.rs",
        rpcs: crate::services::mining_advanced::mining_compression::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_module_cycles.rs",
        rpcs: crate::services::inventory_fitting::fitting_module_cycles::RPCS,
    },
];

/// Returns the `.rs` paths (relative to `crates/eve-server/src/`) already migrated to `RPCS`.
pub fn compiled_rpc_module_files() -> Vec<&'static str> {
    COMPILED_RPC_MODULES.iter().map(|m| m.file).collect()
}

/// Returns all `(service, method)` pairs registered in the compiled RPC registry.
///
/// Validates uniqueness via [`RpcRegistry::default_compiled`].
pub fn compiled_rpc_pairs() -> Vec<(&'static str, &'static str)> {
    RpcRegistry::default_compiled()
        .all_entries()
        .iter()
        .map(|entry| (entry.service, entry.method))
        .collect()
}

/// Compiled O(1) lookup table of migrated RPC entries owned by [`ServerContext`].
#[derive(Debug, Clone)]
pub struct RpcRegistry {
    entries: Vec<RpcEntry>,
    by_service_method: HashMap<&'static str, HashMap<&'static str, usize>>,
}

impl Default for RpcRegistry {
    fn default() -> Self {
        Self::default_compiled()
    }
}

impl RpcRegistry {
    /// Builds the default production registry from [`COMPILED_RPC_MODULES`].
    pub fn default_compiled() -> Self {
        let mut reg = Self::try_from_modules(COMPILED_RPC_MODULES)
            .unwrap_or_else(|err| panic!("compiled RPC registry validation failed: {err}"));
        reg.entries.shrink_to_fit();
        reg
    }

    /// Builds a registry from arbitrary `&'static [RpcEntry]` slices, panicking on duplicates.
    pub fn from_slices(slices: &[&'static [RpcEntry]]) -> Self {
        Self::try_from_slices(slices)
            .unwrap_or_else(|err| panic!("compiled RPC registry validation failed: {err}"))
    }

    /// Builds a registry from [`CompiledRpcModule`] descriptors, rejecting duplicate `(service, method)`.
    pub fn try_from_modules(modules: &[CompiledRpcModule]) -> Result<Self, String> {
        let slices: Vec<&'static [RpcEntry]> = modules.iter().map(|m| m.rpcs).collect();
        Self::try_from_slices(&slices)
    }

    /// Builds a registry from `&'static [RpcEntry]` slices, returning `Err` on duplicate `(service, method)`.
    pub fn try_from_slices(slices: &[&'static [RpcEntry]]) -> Result<Self, String> {
        let total_len: usize = slices.iter().map(|s| s.len()).sum();
        let mut entries = Vec::with_capacity(total_len);
        let mut by_service_method: HashMap<&'static str, HashMap<&'static str, usize>> =
            HashMap::new();

        for slice in slices {
            for &entry in *slice {
                if entry.service.is_empty() || entry.method.is_empty() {
                    return Err(format!(
                        "RPC entry has empty service/method: ({:?}, {:?})",
                        entry.service, entry.method
                    ));
                }
                let methods = by_service_method.entry(entry.service).or_default();
                if methods.contains_key(entry.method) {
                    return Err(format!(
                        "duplicate compiled RPC registration: {}.{}",
                        entry.service, entry.method
                    ));
                }
                let idx = entries.len();
                entries.push(entry);
                methods.insert(entry.method, idx);
            }
        }

        Ok(Self {
            entries,
            by_service_method,
        })
    }

    /// Performs an O(1) lookup for `(service, method)`.
    #[inline]
    pub fn lookup(&self, service: &str, method: &str) -> Option<&RpcEntry> {
        let idx = self.by_service_method.get(service)?.get(method)?;
        self.entries.get(*idx)
    }

    /// Returns all registered entries in deterministic registration order.
    #[inline]
    pub fn all_entries(&self) -> &[RpcEntry] {
        &self.entries
    }
}

#[cfg(test)]
#[path = "rpc_registry_tests.rs"]
mod rpc_registry_tests;
