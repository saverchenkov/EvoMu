//! `world_seeder` — Dual-Anchor Market Seeder & Universe Bootstrap Engine (`§3.9`, `M5`/`M8`).
//!
//! Seeds regional trade hubs (`Jita 4-4`, `Amarr VIII`, `Dodixie IX`, `Rens VI`) with:
//! - Tier 1: Authentic NPC infinite-duration orders (`skillbooks`, `blueprints`) at `basePrice`.
//! - Tier 2: Synthetic Living-World Order Books (`bids` at `-6%` and `asks` at `+6%` around
//!   Dual-Anchor Fair Value $P_{\text{fair}}(T) = \max(P_{\text{base}}(T), \text{BOM}(T) \cdot 1.15)$).

#![forbid(unsafe_code)]

use crate::formulas::compute_living_world_fair_price;
use eve_db::{IskCents, MarketOrderRecord, WorldRepository};
use eve_sde::{SdeRegistry, TypeId};

pub const TRADE_HUB_STATIONS: &[(i64, u32, f64)] = &[
    (60_003_760, 10_000_002, 1.00), // Jita 4-4 (The Forge)
    (60_008_494, 10_000_043, 1.03), // Amarr VIII (Domain)
    (60_011_866, 10_000_032, 1.05), // Dodixie IX (Sinq Laison)
    (60_004_588, 10_000_030, 1.06), // Rens VI (Heimatar)
];

/// Computes the Bill-of-Materials (BOM) mineral value for `product_type_id` if a blueprint exists in `sde`.
pub fn compute_bom_cost(product_type_id: TypeId, sde: &SdeRegistry) -> f64 {
    for bp in sde.blueprints.values() {
        if bp.product_type_id == product_type_id {
            let total_mat_cost: f64 = bp
                .materials
                .iter()
                .map(|m| {
                    let mat_price = sde
                        .types
                        .get(&m.type_id)
                        .map(|t| t.base_price)
                        .unwrap_or(5.0);
                    (m.quantity as f64) * mat_price
                })
                .sum();
            return total_mat_cost / (bp.product_quantity.max(1) as f64);
        }
    }
    0.0
}

/// Seeds Dual-Anchor market orders across all major trade hubs into `db`.
pub fn seed_dual_anchor_markets(db: &WorldRepository, sde: &SdeRegistry) -> usize {
    let mut inserted = 0usize;
    let mut type_ids: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| t.published && t.base_price > 0.0)
        .map(|t| t.type_id)
        .collect();
    type_ids.sort_unstable();

    db.with_mut_snapshot(|snap| {
        for &(station_id, region_id, hub_mult) in TRADE_HUB_STATIONS {
            for &type_id in &type_ids {
                let Some(inv_type) = sde.types.get(&type_id) else {
                    continue;
                };
                let bom = compute_bom_cost(type_id, sde);
                let fair_isk = compute_living_world_fair_price(inv_type.base_price, bom, hub_mult);

                // Tier 1: Skillbooks (category 16) and Blueprints (category 9) get NPC sell orders at basePrice
                let is_npc_seeded = inv_type.category_id == 16 || inv_type.category_id == 9;
                let ask_isk = if is_npc_seeded {
                    inv_type.base_price
                } else {
                    (fair_isk * 1.06).max(1.0)
                };
                let bid_isk = (fair_isk * 0.94).max(0.5);

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

                if !is_npc_seeded {
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
    });

    inserted
}
