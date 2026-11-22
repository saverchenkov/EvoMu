//! `pve_content_providers` — Lifecycle [`SystemContentProvider`] registrations for
//! special sites (incursions, Drifter hives, Triglavian minor victories, seasonal events)
//! and dynamic resource/mining sites (`#1542` Track 2a).

#![forbid(unsafe_code)]

use crate::rng::MechanicRng;
use crate::services::combat_crimewatch::CombatRuntimeState;
use crate::services::space_mining_harvest::PhasedAsteroidFieldState;
use crate::system_actor::{
    SystemActivationError, SystemContentMaintenanceContext, SystemContentProvider,
    SystemContentProviderMetadata, SystemContentSeedContext, SystemRegistry,
};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) const PROVIDER_SPECIAL_SITES: &str = "pve_special_sites";
pub(crate) const PROVIDER_RESOURCE_SITES: &str = "resource_sites";

struct SpecialSitesContentProvider {
    sde: Arc<SdeRegistry>,
    db: Arc<WorldRepository>,
    combat_ephemeral: Arc<RankedRwLock<CombatRuntimeState, RANK_LEAF>>,
}

impl SystemContentProvider for SpecialSitesContentProvider {
    fn name(&self) -> &'static str {
        PROVIDER_SPECIAL_SITES
    }

    fn metadata(&self) -> SystemContentProviderMetadata {
        SystemContentProviderMetadata {
            name: self.name(),
            has_tick: true,
            has_expire: true,
        }
    }

    fn seed(&self, ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
        let sde = ctx.sde.cloned().unwrap_or_else(|| Arc::clone(&self.sde));
        let db = ctx.db.cloned().unwrap_or_else(|| Arc::clone(&self.db));
        super::pve_special_sites::seed_special_sites_in_activation(
            ctx,
            &sde,
            &db,
            &*self.combat_ephemeral,
        );
        Ok(())
    }

    fn tick(&self, ctx: &SystemContentMaintenanceContext<'_>) -> Result<(), String> {
        let sde = ctx.sde.cloned().unwrap_or_else(|| Arc::clone(&self.sde));
        let db = ctx.db.cloned().unwrap_or_else(|| Arc::clone(&self.db));
        super::pve_special_sites_seasonal::tick_seasonal_event_sites_for_system_sync(
            sde.as_ref(),
            db.as_ref(),
            &*self.combat_ephemeral,
            ctx.handle,
            ctx.solar_system_id,
            ctx.now_unix_secs,
        );
        Ok(())
    }

    fn expire(&self, ctx: &SystemContentMaintenanceContext<'_>) -> Result<(), String> {
        let db = ctx.db.cloned().unwrap_or_else(|| Arc::clone(&self.db));
        super::pve_special_sites_seasonal::expire_seasonal_event_sites_for_system_sync(
            db.as_ref(),
            &*self.combat_ephemeral,
            Some(ctx.handle),
            ctx.solar_system_id,
            ctx.now_unix_secs,
        );
        Ok(())
    }
}

struct ResourceSitesContentProvider {
    sde: Arc<SdeRegistry>,
    db: Arc<WorldRepository>,
    combat_ephemeral: Arc<RankedRwLock<CombatRuntimeState, RANK_LEAF>>,
    mechanic_rng: Arc<MechanicRng>,
    phased_fields: Arc<RankedRwLock<HashMap<u32, PhasedAsteroidFieldState>, RANK_LEAF>>,
}

impl SystemContentProvider for ResourceSitesContentProvider {
    fn name(&self) -> &'static str {
        PROVIDER_RESOURCE_SITES
    }

    fn metadata(&self) -> SystemContentProviderMetadata {
        SystemContentProviderMetadata {
            name: self.name(),
            has_tick: true,
            has_expire: false,
        }
    }

    fn seed(&self, ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
        let sde = ctx.sde.cloned().unwrap_or_else(|| Arc::clone(&self.sde));
        let db = ctx.db.cloned().unwrap_or_else(|| Arc::clone(&self.db));
        super::mining_sites::seed_resource_sites_in_activation(
            ctx,
            sde.as_ref(),
            db.as_ref(),
            &*self.combat_ephemeral,
            &self.mechanic_rng,
            &*self.phased_fields,
        );
        Ok(())
    }

    fn tick(&self, ctx: &SystemContentMaintenanceContext<'_>) -> Result<(), String> {
        let sde = ctx.sde.cloned().unwrap_or_else(|| Arc::clone(&self.sde));
        let db = ctx.db.cloned().unwrap_or_else(|| Arc::clone(&self.db));
        super::mining_sites::tick_resource_sites_for_system_sync(
            sde.as_ref(),
            db.as_ref(),
            ctx.handle,
            ctx.solar_system_id,
            ctx.now_unix_secs,
        );
        Ok(())
    }
}

/// Registers Track-2a special-site and resource [`SystemContentProvider`] hooks on `registry`
/// in deterministic activation order (`#1542`). Safe to invoke idempotently.
pub(crate) fn register_default_pve_content_providers(
    registry: &SystemRegistry,
    sde: Arc<SdeRegistry>,
    db: Arc<WorldRepository>,
    combat_ephemeral: Arc<RankedRwLock<CombatRuntimeState, RANK_LEAF>>,
    mechanic_rng: Arc<MechanicRng>,
    phased_fields: Arc<RankedRwLock<HashMap<u32, PhasedAsteroidFieldState>, RANK_LEAF>>,
) -> Result<(), SystemActivationError> {
    let providers: [Arc<dyn SystemContentProvider>; 2] = [
        Arc::new(SpecialSitesContentProvider {
            sde: Arc::clone(&sde),
            db: Arc::clone(&db),
            combat_ephemeral: Arc::clone(&combat_ephemeral),
        }),
        Arc::new(ResourceSitesContentProvider {
            sde,
            db,
            combat_ephemeral,
            mechanic_rng,
            phased_fields,
        }),
    ];
    for provider in providers {
        let _ = registry.register_content_provider(provider)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "pve_content_providers_tests.rs"]
mod tests;
