//! `world_seeder` — Dual-Anchor Market Seeder & Universe Bootstrap Engine (`§3.9`, `M5`/`M8`).
//!
//! Seeds regional trade hubs (`Jita 4-4`, `Amarr VIII`, `Dodixie IX`, `Rens VI`) with:
//! - Tier 1: Authentic NPC infinite-duration orders (`skillbooks`, `blueprints`) at `basePrice`.
//! - Tier 2: Synthetic Living-World Order Books (`bids` at `-6%` and `asks` at `+6%` around
//!   Dual-Anchor Fair Value $P_{\text{fair}}(T) = \max(P_{\text{base}}(T), \text{BOM}(T) \cdot 1.15)$).

#![forbid(unsafe_code)]

use crate::formulas::compute_living_world_fair_price;
use eve_db::{IskCents, MarketOrderRecord, WorldRepository};
use eve_sde::{GroupId, SdeRegistry, TypeId};

pub const TRADE_HUB_STATIONS: &[(i64, u32, f64)] = &[
    (60_003_760, 10_000_002, 1.00), // Jita 4-4 (The Forge)
    (60_008_494, 10_000_043, 1.03), // Amarr VIII (Domain)
    (60_011_866, 10_000_032, 1.05), // Dodixie IX (Sinq Laison)
    (60_004_588, 10_000_030, 1.06), // Rens VI (Heimatar)
];

const MARKET_SEEDER_VERSION_KEY: &str = "market_seeder_version";
const MARKET_SEEDER_VERSION_V2: &str = "v2_full_catalog";
const DEFAULT_FALLBACK_BASE_PRICE_ISK: f64 = 100.0;

/// Gameplay SDE categories eligible for NPC/Dual-Anchor market seeding (matching EveMu's
/// `seed_station_market.sql` categories `4..46` plus V24.01 `2` Cargo/Gas, `65` Structure,
/// `66` Structure Module, `87` Fighter, and `2143` Colony Resources; excludes `5` PLEX/NES
/// Accessories and pure cosmetic categories `30` Apparel, `63` Special Edition, `91` SKINs,
/// `2100` Expert Systems, `2118` Personalization).
pub const SEEDED_GAMEPLAY_CATEGORIES: &[u32] = &[
    2, 4, 6, 7, 8, 9, 16, 17, 18, 20, 22, 23, 24, 25, 32, 34, 35, 39, 40, 41, 42, 43, 46, 65, 66,
    87, 2143,
];

#[inline]
fn is_seeded_gameplay_category(category_id: u32) -> bool {
    SEEDED_GAMEPLAY_CATEGORIES.contains(&category_id)
}

/// Precomputes the Bill-of-Materials (BOM) unit cost map for all blueprints in `sde` in $O(|B|)$.
/// Runs two passes so Tech II / advanced blueprints whose inputs are manufactured components
/// with `base_price == 0.0` inherit their component BOM costs from pass 1.
pub fn build_bom_cost_map(sde: &SdeRegistry) -> std::collections::HashMap<TypeId, f64> {
    let mut bom_map = std::collections::HashMap::with_capacity(sde.blueprints.len());
    for _pass in 0..2 {
        for bp in sde.blueprints.values() {
            let total_mat_cost: f64 = bp
                .materials
                .iter()
                .map(|mat| {
                    let sde_price = sde
                        .types
                        .get(&mat.type_id)
                        .map(|t| t.base_price)
                        .unwrap_or(0.0);
                    let mat_price = if sde_price > 0.0 {
                        sde_price
                    } else {
                        bom_map.get(&mat.type_id).copied().unwrap_or(5.0)
                    };
                    (mat.quantity as f64) * mat_price
                })
                .sum();
            let unit_cost = total_mat_cost / (bp.product_quantity.max(1) as f64);
            if unit_cost > 0.0 {
                bom_map.insert(bp.product_type_id, unit_cost);
            }
        }
    }
    bom_map
}

/// Computes the Bill-of-Materials (BOM) mineral value for `product_type_id` if a blueprint exists in `sde`.
pub fn compute_bom_cost(product_type_id: TypeId, sde: &SdeRegistry) -> f64 {
    for bp in sde.blueprints.values() {
        if bp.product_type_id == product_type_id {
            let total_mat_cost: f64 = bp
                .materials
                .iter()
                .map(|mat| {
                    let mat_price = sde
                        .types
                        .get(&mat.type_id)
                        .map(|t| if t.base_price > 0.0 { t.base_price } else { 5.0 })
                        .unwrap_or(5.0);
                    (mat.quantity as f64) * mat_price
                })
                .sum();
            return total_mat_cost / (bp.product_quantity.max(1) as f64);
        }
    }
    0.0
}

/// Returns all published, market-grouped gameplay types in `sde` (matching EveMu's
/// `seed_station_market.sql` + V24.01 Upwell/Fighter categories: ships, modules, rigs, ammo/charges,
/// blueprints, skillbooks, drones, fighters, implants, boosters, materials, ores, planetary items,
/// structures, subsystems, deployables, starbases, relics, decryptors).
pub fn select_full_market_types(sde: &SdeRegistry) -> Vec<TypeId> {
    let mut type_ids: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            t.published && t.market_group_id.is_some() && is_seeded_gameplay_category(t.category_id)
        })
        .map(|t| t.type_id)
        .collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    type_ids
}

/// Derives the representative per-group baseline market catalog dynamically from [`SdeRegistry`].
pub fn select_seeded_market_types(sde: &SdeRegistry) -> Vec<TypeId> {
    let mut by_group: std::collections::BTreeMap<GroupId, Vec<TypeId>> =
        std::collections::BTreeMap::new();
    for t in sde.types.values() {
        if !t.published
            || t.base_price <= 0.0
            || t.market_group_id.is_none()
            || !is_seeded_gameplay_category(t.category_id)
        {
            continue;
        }
        let tech_level = t.default_attributes.get(&422).copied().unwrap_or(1.0);
        let meta_group = t.default_attributes.get(&1692).copied().unwrap_or(1.0);
        if tech_level > 1.0 || meta_group > 1.0 {
            continue;
        }
        if t.category_id == 9 {
            if let Some(bp) = sde.blueprints.get(&t.type_id) {
                let prod_tl = sde
                    .types
                    .get(&bp.product_type_id)
                    .and_then(|pt| pt.default_attributes.get(&422).copied())
                    .unwrap_or(1.0);
                if prod_tl >= 2.0 {
                    continue;
                }
            }
        }
        by_group.entry(t.group_id).or_default().push(t.type_id);
    }

    let mut type_ids = Vec::new();
    for (group_id, mut tids) in by_group {
        tids.sort_unstable();
        let cap = if group_id == 18 || group_id == 1027 {
            tids.len()
        } else {
            1
        };
        type_ids.extend(tids.into_iter().take(cap));
    }
    type_ids.sort_unstable();
    type_ids.dedup();
    type_ids
}

/// Seeds Dual-Anchor market orders across all major trade hubs into `db` idempotently once
/// per world (`snap.markets_seeded`).
///
/// On persistent world databases (`!db.is_ephemeral_in_memory()`), Jita 4-4 (`60_003_760`) is
/// seeded with the full gameplay market catalog ([`select_full_market_types`]) and existing world
/// databases created prior to `v2_full_catalog` are automatically topped up on server init.
pub fn seed_dual_anchor_markets(db: &WorldRepository, sde: &SdeRegistry) -> usize {
    let is_persistent = !db.is_ephemeral_in_memory();
    let already_seeded = db.with_snapshot(|snap| {
        if !snap.markets_seeded {
            false
        } else if is_persistent {
            snap.service_state_blobs
                .get(MARKET_SEEDER_VERSION_KEY)
                .map(|s| s.as_str())
                == Some(MARKET_SEEDER_VERSION_V2)
        } else {
            true
        }
    });
    if already_seeded {
        return 0;
    }
    seed_market_catalog_internal(db, sde, is_persistent)
}

/// Explicitly seeds the full gameplay market catalog ([`select_full_market_types`]) into `db`
/// regardless of whether `db` is backed by an ephemeral in-memory test store or persistent SQLite.
pub fn seed_full_market_catalog(db: &WorldRepository, sde: &SdeRegistry) -> usize {
    seed_market_catalog_internal(db, sde, true)
}

fn seed_market_catalog_internal(
    db: &WorldRepository,
    sde: &SdeRegistry,
    full_jita_catalog: bool,
) -> usize {
    let baseline_type_ids = select_seeded_market_types(sde);
    let baseline_set: std::collections::HashSet<TypeId> =
        baseline_type_ids.iter().copied().collect();
    let full_type_ids = if full_jita_catalog {
        select_full_market_types(sde)
    } else {
        Vec::new()
    };
    let bom_map = build_bom_cost_map(sde);

    db.try_with_mut_snapshot::<_, usize, String>(|snap| {
        snap.markets_seeded = true;
        if full_jita_catalog {
            snap.service_state_blobs.insert(
                MARKET_SEEDER_VERSION_KEY.to_string(),
                MARKET_SEEDER_VERSION_V2.to_string(),
            );
        }
        let mut inserted = 0usize;
        let mut existing: std::collections::HashSet<(i64, i64, TypeId, bool)> = snap
            .market_orders
            .values()
            .map(|o| (o.owner_id, o.station_id, o.type_id, o.is_bid))
            .collect();
        for &(station_id, region_id, hub_mult) in TRADE_HUB_STATIONS {
            let station_types = if full_jita_catalog && station_id == 60_003_760 {
                &full_type_ids
            } else {
                &baseline_type_ids
            };
            for &type_id in station_types {
                let Some(inv_type) = sde.types.get(&type_id) else {
                    continue;
                };
                let bom = bom_map.get(&type_id).copied().unwrap_or(0.0);
                let effective_base_price = if inv_type.base_price > 0.0 {
                    inv_type.base_price
                } else if bom > 0.0 {
                    bom
                } else {
                    DEFAULT_FALLBACK_BASE_PRICE_ISK
                };
                let fair_isk =
                    compute_living_world_fair_price(effective_base_price, bom, hub_mult);

                // Tier 1: Skillbooks (category 16) and Blueprints (category 9) get NPC sell orders at basePrice
                let is_npc_seeded = inv_type.category_id == 16 || inv_type.category_id == 9;
                let ask_isk = if is_npc_seeded {
                    effective_base_price
                } else {
                    (fair_isk * 1.06).max(1.0)
                };
                let bid_isk = (fair_isk * 0.94).max(0.5);

                if existing.insert((1_000_035, station_id, type_id, false)) {
                    snap.next_entity_id += 1;
                    let ask_id = snap.next_entity_id;
                    snap.market_orders.insert(
                        ask_id,
                        MarketOrderRecord {
                            order_id: ask_id,
                            owner_id: 1_000_035,
                            station_id,
                            region_id,
                            type_id,
                            is_bid: false,
                            price: IskCents::from_f64_isk(ask_isk),
                            vol_entered: 1_000,
                            vol_remaining: 1_000,
                            min_volume: 1,
                            is_npc: is_npc_seeded,
                            escrow: IskCents(0),
                        },
                    );
                    inserted += 1;
                }

                let should_seed_bid = !is_npc_seeded
                    && (baseline_set.contains(&type_id) || inv_type.meta_group_id == Some(2));
                if should_seed_bid && existing.insert((1_000_125, station_id, type_id, true)) {
                    snap.next_entity_id += 1;
                    let bid_id = snap.next_entity_id;
                    let escrow = IskCents::from_f64_isk(bid_isk * 500.0);
                    snap.market_orders.insert(
                        bid_id,
                        MarketOrderRecord {
                            order_id: bid_id,
                            owner_id: 1_000_125,
                            station_id,
                            region_id,
                            type_id,
                            is_bid: true,
                            price: IskCents::from_f64_isk(bid_isk),
                            vol_entered: 500,
                            vol_remaining: 500,
                            min_volume: 1,
                            is_npc: false,
                            escrow,
                        },
                    );
                    inserted += 1;
                }
            }
        }
        inserted += seed_npc_skillbooks_and_faction_items(snap, sde, &bom_map, &mut existing);
        Ok(inserted)
    })
    .unwrap_or(0)
}

/// Tier 1b: every published, market-listed skillbook (SDE categoryID 16) is sold by the hub station's
/// owner corporation at basePrice; faction items (SDE metaGroupID 4 with a factionID) are sold at hub
/// stations whose owner faction matches the item's faction.
///
/// DERIVED: the SDE has no NPC market-seeding table, so which NPC corp sells which faction item, its
/// price (SDE basePrice) and its finite 100-unit stock are a seeding rule, not imported data.
/// Skillbooks are infinite NPC stock (the matcher replenishes them); faction stock depletes per fill.
fn seed_npc_skillbooks_and_faction_items(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &SdeRegistry,
    bom_map: &std::collections::HashMap<TypeId, f64>,
    existing: &mut std::collections::HashSet<(i64, i64, TypeId, bool)>,
) -> usize {
    let mut inserted = 0;
    let mut types: Vec<&eve_sde::InvType> = sde
        .types
        .values()
        .filter(|t| t.published && t.market_group_id.is_some())
        .filter(|t| t.category_id == 16 || (t.meta_group_id == Some(4) && t.faction_id.is_some()))
        .collect();
    types.sort_by_key(|t| t.type_id);
    let mut asks: std::collections::HashSet<(i64, TypeId)> =
        existing.iter().filter(|e| !e.3).map(|e| (e.1, e.2)).collect();
    for &(station_id, region_id, _) in TRADE_HUB_STATIONS {
        let Some(station) = sde.stations.get(&station_id) else {
            continue;
        };
        let owner = station.owner_corporation_id;
        for t in &types {
            let is_faction_item = t.category_id != 16;
            if is_faction_item && station.owner_faction_id != t.faction_id.map(i64::from) {
                continue;
            }
            if !asks.insert((station_id, t.type_id)) {
                continue;
            }
            existing.insert((owner, station_id, t.type_id, false));
            let vol = if is_faction_item { 100 } else { 1_000 };
            let price_isk = if t.base_price > 0.0 {
                t.base_price
            } else {
                bom_map
                    .get(&t.type_id)
                    .copied()
                    .filter(|&b| b > 0.0)
                    .unwrap_or(DEFAULT_FALLBACK_BASE_PRICE_ISK)
            };
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.market_orders.insert(
                id,
                MarketOrderRecord {
                    order_id: id,
                    owner_id: owner,
                    station_id,
                    region_id,
                    type_id: t.type_id,
                    is_bid: false,
                    price: IskCents::from_f64_isk(price_isk),
                    vol_entered: vol,
                    vol_remaining: vol,
                    min_volume: 1,
                    is_npc: true,
                    escrow: IskCents(0),
                },
            );
            inserted += 1;
        }
    }
    inserted
}
