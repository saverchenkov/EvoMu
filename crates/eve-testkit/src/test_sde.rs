//! Shared SDE handle amortization for test harnesses (`#1583` Track 14).
//!
//! Eliminates repeated 92 MB `data/evomu_sde.sqlite` reloads across test fixtures without keeping
//! any mutable global cache in `eve-sde` production code.

#![forbid(unsafe_code)]

use eve_sde::{SdeCache, SdeError, SdeRegistry};
use std::path::Path;
use std::sync::{Arc, OnceLock};

fn shared_test_sde_cache() -> &'static SdeCache {
    static TEST_SDE_CACHE: OnceLock<SdeCache> = OnceLock::new();
    TEST_SDE_CACHE.get_or_init(SdeCache::new)
}

/// Process-wide test-only SDE handle cache backed by [`eve_sde::SdeCache`].
pub struct TestSde;

impl TestSde {
    /// Opens and shares the canonical runtime SDE database (`SdeRegistry::default_db_path()`).
    pub fn canonical_shared() -> Arc<SdeRegistry> {
        let _env_guard = eve_db::lock_test_env_read();
        shared_test_sde_cache().canonical_shared()
    }

    /// Clones a canonical runtime SDE registry using the shared test cache.
    pub fn with_canonical_seed() -> SdeRegistry {
        (*Self::canonical_shared()).clone()
    }

    /// Opens an explicit SQLite SDE path through the shared test cache.
    pub fn open_shared(path: impl AsRef<Path>) -> Result<Arc<SdeRegistry>, SdeError> {
        shared_test_sde_cache().open_shared(path)
    }

    /// Clones an explicit SQLite SDE path through the shared test cache.
    pub fn open(path: impl AsRef<Path>) -> Result<SdeRegistry, SdeError> {
        shared_test_sde_cache().open(path)
    }
}
