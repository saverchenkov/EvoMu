//! `eve-db` — Durable transactional persistence layer and domain repositories for `EvoMu` (`M1`–`M8`).
//!
//! Encapsulates all account, character, skill queue, clone, inventory, fitting, market, wallet,
//! contract, industry, planetary interaction, agent/mission, NPE, social, corporation, fleet,
//! CrimeWatch, Upwell structure, sovereignty, Faction Warfare, and dynamic system state behind
//! atomic repository operations with entity versioning, fixed-point ISK currency (`IskCents`),
//! per-character idempotency keys, and durable JSON snapshot save/load.

#![forbid(unsafe_code)]

pub mod accounts;
pub mod characters;
pub mod clock;
pub mod inventory;
pub mod ledger;
pub mod persistence;
#[cfg(any(test, feature = "demo-seed"))]
pub mod seed;
pub mod snapshot_ext;
pub mod sqlite_store;
pub mod types;

#[cfg(test)]
mod tests;

pub use accounts::service_roles;
pub use accounts::{base64url_decode_no_pad, base64url_encode_no_pad};
pub use clock::{ServerClock, VirtualServerClock};
pub use ledger::{JournalEntryInput, JournalMeta};
pub use persistence::WorldRepository;
pub use snapshot_ext::{CorpVoteBallotRecord, CorpVoteCaseRecord, StarbaseDetailSettings};
pub use types::*;
