//! Additional edge-case and branch-coverage tests for `world_seeder`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::{IskCents, MarketOrderRecord, WorldRepository};
use eve_sde::{BlueprintMaterial, SdeRegistry};
use std::collections::HashMap;

#[test]
fn full_market_types_unpublished_exceptions_and_seeded_type_filters() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.values().min_by_key(|t| t.type_id).unwrap().clone();
    let base_bp = sde
        .blueprints
        .values()
        .min_by_key(|b| b.blueprint_type_id)
        .unwrap()
        .clone();

    // 1. Unpublished category 9 (Blueprint) and 16 (Skill) -> included in select_full_market_types.
    for (tid, cat, grp) in [
        (980_001_u32, 9_u32, 9980_u32),
        (980_002, 16, 9981),
        // Unpublished special group IDs: 1978, 4050, 4176, 4743 -> included.
        (980_003, 999, 1978),
        (980_004, 999, 4050),
        (980_005, 999, 4176),
        (980_006, 999, 4743),
        // Unpublished non-gameplay category/group -> excluded.
        (980_007, 30, 9982),
        // Published cosmetic category (91 SKINs) -> excluded.
        (980_008, 91, 9983),
    ] {
        let mut t = base.clone();
        t.type_id = tid;
        t.category_id = cat;
        t.group_id = grp;
        t.published = tid == 980_008;
        t.market_group_id = Some(100);
        t.base_price = 1_000.0;
        t.default_attributes.clear();
        sde.types.insert(tid, t);
    }

    // Special type IDs (unpublished, non-gameplay category): 2899, 3302, 22_175, 22_177, 30_832, 44_992, 56_311.
    for &special_tid in &[2899_u32, 3302, 22_175, 22_177, 30_832, 44_992, 56_311] {
        let mut t = base.clone();
        t.type_id = special_tid;
        t.category_id = 5; // PLEX/NES Accessories (not in SEEDED_GAMEPLAY_CATEGORIES)
        t.group_id = 9989;
        t.published = false;
        t.base_price = 0.0;
        t.market_group_id = Some(1922);
        sde.types.insert(special_tid, t);
    }

    let full = select_full_market_types(&sde);
    for expected in [
        980_001_u32,
        980_002,
        980_003,
        980_004,
        980_005,
        980_006,
        2899,
        3302,
        22_175,
        22_177,
        30_832,
        44_992,
        56_311,
    ] {
        assert!(full.contains(&expected), "missing {expected} in full market types");
    }
    assert!(!full.contains(&980_007));
    assert!(!full.contains(&980_008));

    // 2. select_seeded_market_types filters:
    // - T2 item (attr 422 == 2.0) excluded
    // - Faction/meta item (attr 1692 > 1.0) excluded
    // - Blueprint (category 9) whose product has attr 422 >= 2.0 excluded
    // - Blueprint (category 9) NOT present in sde.blueprints included
    // - Missing market_group_id excluded
    for (tid, cat, grp, mg, attrs) in [
        (981_001_u32, 6, 9990, Some(10), HashMap::from([(422, 2.0)])),
        (981_002, 6, 9991, Some(10), HashMap::from([(1692, 4.0)])),
        (981_003, 6, 9992, None, HashMap::new()),
        (981_004, 9, 9993, Some(10), HashMap::new()), // BP for 981_001 (T2 product)
        (981_005, 9, 9994, Some(10), HashMap::new()), // BP not in sde.blueprints -> included!
    ] {
        let mut t = base.clone();
        t.type_id = tid;
        t.category_id = cat;
        t.group_id = grp;
        t.published = true;
        t.base_price = 5_000.0;
        t.market_group_id = mg;
        t.default_attributes = attrs;
        sde.types.insert(tid, t);
    }
    let mut t2_bp = base_bp.clone();
    t2_bp.blueprint_type_id = 981_004;
    t2_bp.product_type_id = 981_001;
    t2_bp.product_quantity = 1;
    sde.blueprints.insert(981_004, t2_bp);

    let seeded = select_seeded_market_types(&sde);
    assert!(!seeded.contains(&981_001));
    assert!(!seeded.contains(&981_002));
    assert!(!seeded.contains(&981_003));
    assert!(!seeded.contains(&981_004));
    assert!(seeded.contains(&981_005));

    // compute_bom_cost for unknown product returns 0.0, and missing material type_id falls back to 5.0
    assert_eq!(compute_bom_cost(989_999, &sde), 0.0);
    let mut missing_mat_bp = base_bp;
    missing_mat_bp.blueprint_type_id = 981_099;
    missing_mat_bp.product_type_id = 981_098;
    missing_mat_bp.product_quantity = 1;
    missing_mat_bp.materials = vec![BlueprintMaterial {
        type_id: 989_998, // absent from sde.types -> fallback 5.0
        quantity: 6,
    }];
    sde.blueprints.insert(981_099, missing_mat_bp);
    assert!((compute_bom_cost(981_098, &sde) - 30.0).abs() < 1e-9);
    let bom_map = build_bom_cost_map(&sde);
    assert!((bom_map[&981_098] - 30.0).abs() < 1e-9);
}

#[test]
fn seed_full_catalog_plex_t2_faction_and_preexisting_order_dedup() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.values().min_by_key(|t| t.type_id).unwrap().clone();

    // PLEX (44_992) with base_price == 0.0 -> seeded at 5_000_000.0 ISK fair price + both ask and bid in Jita.
    let mut plex = base.clone();
    plex.type_id = 44_992;
    plex.category_id = 5;
    plex.group_id = 1875;
    plex.published = true;
    plex.base_price = 0.0;
    plex.market_group_id = Some(1922);
    plex.default_attributes.clear();
    sde.types.insert(44_992, plex);

    // T2 module (meta_group_id == Some(2), attr 422 == 2.0): NOT in baseline, IS in full Jita catalog,
    // and `should_seed_bid` is true because `meta_group_id == Some(2)`.
    let mut t2_mod = base.clone();
    t2_mod.type_id = 982_001;
    t2_mod.category_id = 7;
    t2_mod.group_id = 9995;
    t2_mod.published = true;
    t2_mod.base_price = 200_000.0;
    t2_mod.market_group_id = Some(50);
    t2_mod.meta_group_id = Some(2);
    t2_mod.default_attributes = HashMap::from([(422, 2.0)]);
    sde.types.insert(982_001, t2_mod);

    // Regular T1 item in unique group with higher type_id than another item in same group -> NOT in baseline,
    // so in Jita full catalog it gets ask order ONLY (should_seed_bid == false).
    let mut t1_lo = base.clone();
    t1_lo.type_id = 982_010;
    t1_lo.category_id = 7;
    t1_lo.group_id = 9996;
    t1_lo.published = true;
    t1_lo.base_price = 1_000.0;
    t1_lo.market_group_id = Some(50);
    t1_lo.meta_group_id = None;
    t1_lo.default_attributes.clear();
    sde.types.insert(982_010, t1_lo);

    let mut t1_hi = base.clone();
    t1_hi.type_id = 982_011;
    t1_hi.category_id = 7;
    t1_hi.group_id = 9996;
    t1_hi.published = true;
    t1_hi.base_price = 2_000.0;
    t1_hi.market_group_id = Some(50);
    t1_hi.meta_group_id = None;
    t1_hi.default_attributes.clear();
    sde.types.insert(982_011, t1_hi);

    // Faction item (meta_group_id == Some(4), faction_id == Some(amarr_faction) -> Amarr 60_008_494)
    // and faction item with faction_id == None (must NOT be seeded at Amarr).
    let amarr_station = sde.stations.get(&60_008_494).expect("Amarr in SDE");
    let amarr_faction = amarr_station.owner_faction_id.expect("Amarr faction") as u32;
    let mut fac_item = base.clone();
    fac_item.type_id = 982_020;
    fac_item.category_id = 7;
    fac_item.group_id = 9997;
    fac_item.published = true;
    fac_item.base_price = 15_000_000.0;
    fac_item.market_group_id = Some(50);
    fac_item.meta_group_id = Some(4);
    fac_item.faction_id = Some(amarr_faction);
    fac_item.default_attributes = HashMap::from([(1692, 4.0)]);
    sde.types.insert(982_020, fac_item);

    let mut fac_no_id = base;
    fac_no_id.type_id = 982_021;
    fac_no_id.category_id = 7;
    fac_no_id.group_id = 9998;
    fac_no_id.published = true;
    fac_no_id.base_price = 15_000_000.0;
    fac_no_id.market_group_id = Some(50);
    fac_no_id.meta_group_id = Some(4);
    fac_no_id.faction_id = None;
    fac_no_id.default_attributes = HashMap::from([(1692, 4.0)]);
    sde.types.insert(982_021, fac_no_id);

    let db = WorldRepository::new_empty();
    // Pre-insert a Jita ask order for 982_011 owned by 1_000_035 to test existing-order deduplication.
    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            555_001,
            MarketOrderRecord {
                order_id: 555_001,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 982_011,
                is_bid: false,
                price: IskCents::from_isk(9_999),
                vol_entered: 42,
                vol_remaining: 42,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let inserted = seed_full_market_catalog(&db, &sde);
    assert!(inserted > 0);
    // Ephemeral DB with markets_seeded == true is idempotent on seed_dual_anchor_markets -> 0.
    assert_eq!(seed_dual_anchor_markets(&db, &sde), 0);

    db.with_snapshot(|snap| {
        // PLEX 44_992 has both ask and bid at Jita 4-4 (5_000_000 * 1.15 = 5_750_000 fair).
        let plex_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 44_992 && !o.is_bid)
            .expect("PLEX ask");
        assert!(plex_ask.price.0 > IskCents::from_isk(5_000_000).0);
        assert!(snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 44_992 && o.is_bid));

        // T2 item 982_001 has both ask and bid at Jita 4-4.
        assert!(snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 982_001 && !o.is_bid));
        assert!(snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 982_001 && o.is_bid));

        // Non-baseline T1 item 982_011 kept pre-existing ask (no duplicate) and got NO bid.
        let asks_982_011: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.station_id == 60_003_760 && o.type_id == 982_011 && !o.is_bid)
            .collect();
        assert_eq!(asks_982_011.len(), 1);
        assert_eq!(asks_982_011[0].order_id, 555_001);
        assert!(!snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 982_011 && o.is_bid));

        // Faction item 982_020 seeded at Amarr trade hub with vol_entered == 100.
        let fac_orders: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.type_id == 982_020 && o.station_id == 60_008_494)
            .collect();
        assert_eq!(fac_orders.len(), 1);
        assert_eq!(fac_orders[0].vol_entered, 100);
        assert!(fac_orders[0].is_npc);
        assert_eq!(
            sde.stations[&fac_orders[0].station_id].owner_faction_id,
            Some(i64::from(amarr_faction))
        );
        // Faction item without faction_id was not seeded at non-Jita trade hubs.
        assert!(!snap
            .market_orders
            .values()
            .any(|o| o.type_id == 982_021 && o.station_id == 60_008_494));
    });
}
