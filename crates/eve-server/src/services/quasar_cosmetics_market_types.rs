//! `quasar_cosmetics_market_types` — persistent state records and protocol constants
//! for the Paragon Hub Cosmetic Market (`quasar_cosmetics_market`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// Paragon corporation ID used for EverMark loyalty-point buyer fees.
pub const PARAGON_CORP_ID: u64 = 1_000_419;
/// SDE group ID for `Ship SKIN Design Element` (`sde_groups.group_id = 4726`).
pub(super) const GROUP_SHIP_SKIN_DESIGN_ELEMENT: u32 = 4726;
/// Current Paragon Hub buyer fee schedule version string.
pub const BUYER_FEES_VERSION: &str = "v24.01.1";
/// Maximum allowed unit or total price in ISK or PLEX cents (`10_000_000_000_000` = 10 trillion).
pub const MAX_LISTING_PRICE_UNITS: u64 = 10_000_000_000_000;
/// Maximum allowed listing or purchase quantity (`10_000`).
pub const MAX_LISTING_QUANTITY: u32 = 10_000;
/// Standard Paragon Hub listing durations in seconds (`tradingUtil.pyj`: 1, 3, 7, 14, 30, 90 days).
pub(super) const LISTING_DURATIONS_SECS: [(u64, u64); 6] = [
    (86_400, 100_000),
    (259_200, 200_000),
    (604_800, 350_000),
    (1_209_600, 600_000),
    (2_592_000, 1_000_000),
    (7_776_000, 2_500_000),
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListingTargetRecord {
    Public,
    Corporation(u64),
    Alliance(u64),
    Character(u64),
}

impl Default for ListingTargetRecord {
    fn default() -> Self {
        Self::Public
    }
}

impl ListingTargetRecord {
    pub fn is_branded(&self) -> bool {
        !matches!(self, Self::Public)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComponentListingRecord {
    pub uuid: [u8; 16],
    pub sale_end_unix: u64,
    pub price_isk: Option<u64>,
    pub price_plex_cents: Option<u64>,
    pub component_item_type_id: u32,
    pub bundle_size: u64,
    pub discount_percent: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkinListingRecord {
    pub uuid: [u8; 16],
    pub seller_id: u32,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub price_isk: Option<u64>,
    pub price_plex_cents: Option<u64>,
    pub skin_hex: String,
    pub quantity: u32,
    pub target: ListingTargetRecord,
    pub branded: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CosmeticsMarketData {
    pub component_listings: BTreeMap<String, ComponentListingRecord>,
    pub skin_listings: BTreeMap<String, SkinListingRecord>,
    pub next_seq: u64,
}
