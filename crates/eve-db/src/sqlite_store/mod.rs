//! `sqlite_store` — Durable SQLite schema migrations, row-level change tracking, and transactional persistence (`D-3`).

pub mod auth_store;
pub mod industry_market_pve_store;
pub mod corp_crimewatch_store;
pub mod social_store;
pub mod notes_lp_sequence_store;
pub mod fingerprints;
pub mod ledger_store;
pub mod load;
pub mod rollback;
pub mod schema;
pub mod service_domains_store;
pub mod sync;
pub mod sync_ext;

pub use auth_store::*;
pub use fingerprints::*;
pub use load::*;
pub use rollback::*;
pub use schema::*;
pub use sync::*;

#[cfg(test)]
mod sqlite_store_extra_unit_tests;
#[cfg(test)]
mod social_store_extra_unit_tests;
#[cfg(test)]
mod industry_market_pve_store_extra_unit_tests;
#[cfg(test)]
mod notes_lp_ledger_sync_extra_unit_tests;
#[cfg(test)]
mod r6_persist_regression_tests;
#[cfg(test)]
mod evedb_sqlite_cov_tests;
#[cfg(test)]
mod evedb_sqlite_load_errors_and_schema_tests;
#[cfg(test)]
mod evedb_sqlite_fingerprints_and_sync_tests;
#[cfg(test)]
mod notes_lp_sequence_store_cov_unit_tests;
#[cfg(test)]
mod sync_ext_cov_unit_tests;
#[cfg(test)]
mod ledger_store_cov_unit_tests;
#[cfg(test)]
mod social_store_r8b_unit_tests;
#[cfg(test)]
mod service_domains_store_tests;
#[cfg(test)]
mod auth_store_cov_unit_tests;
#[cfg(test)]
mod load_rollback_social_cov_unit_tests;

