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
    pub handler: RpcDispatchHandler,
}

/// Const constructor for concise service-module `RPCS` tables.
pub const fn rpc_entry(
    service: &'static str,
    method: &'static str,
    handler: RpcDispatchHandler,
) -> RpcEntry {
    RpcEntry {
        service,
        method,
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

/// Migrated handler modules included in the compiled RPC registry (Track 2A + Track 2B + Track 2C).
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
    CompiledRpcModule {
        file: "services/campaigns_and_milestones.rs",
        rpcs: crate::services::campaigns_and_milestones::RPCS,
    },
    CompiledRpcModule {
        file: "services/cosmetics_events.rs",
        rpcs: crate::services::cosmetics_events::RPCS,
    },
    CompiledRpcModule {
        file: "services/cosmetics_ui_ops.rs",
        rpcs: crate::services::cosmetics_ui_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/config_service.rs",
        rpcs: crate::services::config_service::RPCS,
    },
    CompiledRpcModule {
        file: "services/hypernet_raffle_ops.rs",
        rpcs: crate::services::hypernet_raffle_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/implant_plugging.rs",
        rpcs: crate::services::implant_plugging::RPCS,
    },
    CompiledRpcModule {
        file: "services/pve_incursion_sites.rs",
        rpcs: crate::services::pve_incursion_sites::RPCS,
    },
    CompiledRpcModule {
        file: "services/security_mgr.rs",
        rpcs: crate::services::security_mgr::RPCS,
    },
    CompiledRpcModule {
        file: "services/jump_clone_names.rs",
        rpcs: crate::services::jump_clone_names::RPCS,
    },
    CompiledRpcModule {
        file: "services/fw_enlistment.rs",
        rpcs: crate::services::fw_enlistment::RPCS,
    },
    CompiledRpcModule {
        file: "services/fw_gameplay_rpc.rs",
        rpcs: crate::services::fw_gameplay::fw_gameplay_rpc::RPCS,
    },
    CompiledRpcModule {
        file: "services/insurgency_services.rs",
        rpcs: crate::services::insurgency_services::RPCS,
    },
    CompiledRpcModule {
        file: "services/sov_warzone_dynamic.rs",
        rpcs: crate::services::sov_warzone_dynamic::RPCS,
    },
    CompiledRpcModule {
        file: "services/industry_facilities_bps.rs",
        rpcs: crate::services::industry_pi::industry_facilities_bps::RPCS,
    },
    CompiledRpcModule {
        file: "services/pi_colonies.rs",
        rpcs: crate::services::industry_pi::pi_colonies::RPCS,
    },
    CompiledRpcModule {
        file: "services/pi_orbital_registry.rs",
        rpcs: crate::services::industry_pi::pi_orbital_registry::RPCS,
    },
    CompiledRpcModule {
        file: "services/pi_orbitals_gm.rs",
        rpcs: crate::services::industry_pi::pi_orbitals_gm::RPCS,
    },
    CompiledRpcModule {
        file: "services/pi_orbitals.rs",
        rpcs: crate::services::industry_pi::pi_orbitals::RPCS,
    },
    CompiledRpcModule {
        file: "services/market_orders_escrow.rs",
        rpcs: crate::services::market_trading::market_orders_escrow::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_dogma_ops.rs",
        rpcs: crate::services::inventory_fitting::fitting_dogma_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_mgr_service.rs",
        rpcs: crate::services::inventory_fitting::fitting_mgr_service::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_saved_multifit.rs",
        rpcs: crate::services::inventory_fitting::fitting_saved_multifit::RPCS,
    },
    CompiledRpcModule {
        file: "services/combat_fighters.rs",
        rpcs: crate::services::combat_crimewatch::combat_fighters::RPCS,
    },
    CompiledRpcModule {
        file: "services/combat_npcs_drones.rs",
        rpcs: crate::services::combat_crimewatch::combat_npcs_drones::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_access_group_bookmarks.rs",
        rpcs: crate::services::fleet_social::fleet_social_access_group_bookmarks::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_finder_tactical.rs",
        rpcs: crate::services::fleet_social::fleet_social_finder_tactical::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_bookmarks.rs",
        rpcs: crate::services::fleet_social::fleet_social_bookmarks::RPCS,
    },
    CompiledRpcModule {
        file: "services/inventory_stacking_ops.rs",
        rpcs: crate::services::inventory_stacking::inventory_stacking_ops::RPCS,
    },
    // Track 2C: next disjoint batch of standalone dispatchers and leaf sub-modules.
    CompiledRpcModule {
        file: "services/character_skill_ops.rs",
        rpcs: crate::services::character_skill_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/space_entity_mining.rs",
        rpcs: crate::services::space_entity_mining::RPCS,
    },
    CompiledRpcModule {
        file: "services/contracts_service.rs",
        rpcs: crate::services::contracts_service::RPCS,
    },
    CompiledRpcModule {
        file: "services/map_service.rs",
        rpcs: crate::services::map_service::RPCS,
    },
    CompiledRpcModule {
        file: "services/corp_registry_ops.rs",
        rpcs: crate::services::corp_registry_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/shipcaster_ops.rs",
        rpcs: crate::services::shipcaster_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/abyssal_void_mgr.rs",
        rpcs: crate::services::abyssal_void_mgr::RPCS,
    },
    CompiledRpcModule {
        file: "services/market_contracts.rs",
        rpcs: crate::services::market_contracts::RPCS,
    },
    CompiledRpcModule {
        file: "services/wallet_serverops.rs",
        rpcs: crate::services::wallet_serverops::RPCS,
    },
    CompiledRpcModule {
        file: "services/notify_emitters_skills_trade_godma.rs",
        rpcs: crate::services::notify_emitters_skills_trade_godma::RPCS,
    },
    CompiledRpcModule {
        file: "services/structure_profiles_directory.rs",
        rpcs: crate::services::station_structure::structure_profiles_directory::RPCS,
    },
    CompiledRpcModule {
        file: "services/structure_profiles_rpc.rs",
        rpcs: crate::services::station_structure::structure_profiles_rpc::RPCS,
    },
    CompiledRpcModule {
        file: "services/structure_upwell_ops.rs",
        rpcs: crate::services::station_structure::structure_upwell_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/crimewatch_bounty.rs",
        rpcs: crate::services::combat_crimewatch::crimewatch_bounty::RPCS,
    },
    CompiledRpcModule {
        file: "services/combat_wrecks_killmails.rs",
        rpcs: crate::services::combat_crimewatch::combat_wrecks_killmails::RPCS,
    },
    CompiledRpcModule {
        file: "services/character_clones_implants.rs",
        rpcs: crate::services::character_skills::character_clones_implants::RPCS,
    },
    CompiledRpcModule {
        file: "services/character_skills_helpers.rs",
        rpcs: crate::services::character_skills::character_skills_helpers::RPCS,
    },
    CompiledRpcModule {
        file: "services/character_contacts.rs",
        rpcs: crate::services::character_skills::character_contacts::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_dogma_im.rs",
        rpcs: crate::services::inventory_fitting::fitting_dogma_im::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_ships_mastery.rs",
        rpcs: crate::services::inventory_fitting::fitting_ships_mastery::RPCS,
    },
    CompiledRpcModule {
        file: "services/fitting_containers_ships_ops.rs",
        rpcs: crate::services::inventory_fitting::fitting_containers_ships_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_fleet_ops.rs",
        rpcs: crate::services::fleet_social::fleet_social_fleet_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_fleets.rs",
        rpcs: crate::services::fleet_social::fleet_social_fleets::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_chat_auth_ops.rs",
        rpcs: crate::services::fleet_social::fleet_social_chat::fleet_social_chat_auth_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/fleet_social_mail.rs",
        rpcs: crate::services::fleet_social::fleet_social_mail::RPCS,
    },
    CompiledRpcModule {
        file: "services/mailing_lists_ops.rs",
        rpcs: crate::services::fleet_social::fleet_social_mail::mailing_lists_ops::RPCS,
    },
    CompiledRpcModule {
        file: "services/space_cyno_filaments.rs",
        rpcs: crate::services::space_destiny::space_cyno_filaments::RPCS,
    },
    CompiledRpcModule {
        file: "services/space_destiny.rs",
        rpcs: crate::services::space_destiny::RPCS,
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
