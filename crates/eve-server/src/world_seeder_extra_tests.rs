//! Additional unit and mutation-killing tests for `world_seeder`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::{IskCents, WorldRepository};
use eve_sde::{BlueprintMaterial, SdeRegistry};
use std::collections::HashMap;

#[test]
fn gameplay_categories_bom_edge_cases_group_1027_and_t1_blueprints() {
    for &cat in SEEDED_GAMEPLAY_CATEGORIES {
        assert!(is_seeded_gameplay_category(cat));
    }
    for excluded in [0, 1, 3, 5, 30, 63, 91, 2100, 2118] {
        assert!(!is_seeded_gameplay_category(excluded));
    }

    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.values().min_by_key(|t| t.type_id).unwrap().clone();
    let base_bp = sde
        .blueprints
        .values()
        .min_by_key(|b| b.blueprint_type_id)
        .unwrap()
        .clone();

    // 1. Blueprint with empty materials (unit_cost == 0.0 -> not inserted)
    let mut empty_bp = base_bp.clone();
    empty_bp.blueprint_type_id = 890_100;
    empty_bp.product_type_id = 890_000;
    empty_bp.product_quantity = 0;
    empty_bp.materials.clear();
    sde.blueprints.insert(empty_bp.blueprint_type_id, empty_bp);

    // 2. Blueprint with product_quantity == 0 (.max(1)) and zero-base-price material (fallback 5.0)
    let mut zero_mat = base_type.clone();
    zero_mat.type_id = 890_001;
    zero_mat.base_price = 0.0;
    sde.types.insert(zero_mat.type_id, zero_mat);

    let mut prod_t1 = base_type.clone();
    prod_t1.type_id = 890_002;
    prod_t1.category_id = 6;
    prod_t1.group_id = 25;
    prod_t1.base_price = 10_000.0;
    prod_t1.published = true;
    prod_t1.market_group_id = Some(10);
    prod_t1.default_attributes = HashMap::from([(422, 1.0), (1692, 1.0)]);
    sde.types.insert(prod_t1.type_id, prod_t1);

    let mut bp_t1 = base_bp.clone();
    bp_t1.blueprint_type_id = 890_102;
    bp_t1.product_type_id = 890_002;
    bp_t1.product_quantity = 0; // tests .max(1)
    bp_t1.materials = vec![BlueprintMaterial {
        type_id: 890_001,
        quantity: 8,
    }];
    sde.blueprints.insert(bp_t1.blueprint_type_id, bp_t1);

    // T1 blueprint item (category 9, product TL == 1.0) -> included in baseline and seeded as NPC ask at base_price
    let mut bp_t1_item = base_type.clone();
    bp_t1_item.type_id = 890_102;
    bp_t1_item.category_id = 9;
    bp_t1_item.group_id = 9950;
    bp_t1_item.base_price = 77_000.0;
    bp_t1_item.published = true;
    bp_t1_item.market_group_id = Some(20);
    bp_t1_item.default_attributes.clear();
    sde.types.insert(bp_t1_item.type_id, bp_t1_item);

    // 3. Multiple items in group 1027 -> all included in select_seeded_market_types (not capped at 1)
    for tid in [890_010u32, 890_011, 890_012] {
        let mut t = base_type.clone();
        t.type_id = tid;
        t.category_id = 16;
        t.group_id = 1027;
        t.base_price = 1_500.0;
        t.published = true;
        t.market_group_id = Some(30);
        t.default_attributes.clear();
        sde.types.insert(tid, t);
    }

    // 4. Skillbook with base_price == 0.0 and BOM > 0.0, plus skillbook with base_price == 0.0 and BOM == 0.0
    let mut sb_bom = base_type.clone();
    sb_bom.type_id = 890_020;
    sb_bom.category_id = 16;
    sb_bom.group_id = 9951;
    sb_bom.base_price = 0.0;
    sb_bom.published = true;
    sb_bom.market_group_id = Some(40);
    sb_bom.default_attributes.clear();
    sde.types.insert(sb_bom.type_id, sb_bom);

    let mut bp_sb = base_bp;
    bp_sb.blueprint_type_id = 890_120;
    bp_sb.product_type_id = 890_020;
    bp_sb.product_quantity = 2;
    bp_sb.materials = vec![BlueprintMaterial {
        type_id: 890_001,
        quantity: 20, // 20 * 5.0 / 2 = 50.0
    }];
    sde.blueprints.insert(bp_sb.blueprint_type_id, bp_sb);

    let mut sb_fb = base_type;
    sb_fb.type_id = 890_021;
    sb_fb.category_id = 16;
    sb_fb.group_id = 9952;
    sb_fb.base_price = 0.0;
    sb_fb.published = true;
    sb_fb.market_group_id = Some(40);
    sb_fb.default_attributes.clear();
    sde.types.insert(sb_fb.type_id, sb_fb);

    let bom_map = build_bom_cost_map(&sde);
    assert!(!bom_map.contains_key(&890_000));
    assert!((bom_map[&890_002] - 40.0).abs() < 1e-9);
    assert!((compute_bom_cost(890_002, &sde) - 40.0).abs() < 1e-9);
    assert!((compute_bom_cost(890_020, &sde) - 50.0).abs() < 1e-9);

    let baseline = select_seeded_market_types(&sde);
    assert!(baseline.contains(&890_102));
    assert!(baseline.contains(&890_010));
    assert!(baseline.contains(&890_011));
    assert!(baseline.contains(&890_012));

    // Remove one trade hub station to cover missing station continue branch in seed_npc_skillbooks_and_faction_items
    sde.stations.remove(&60_004_588);

    let db = WorldRepository::new_empty();
    let count = seed_dual_anchor_markets(&db, &sde);
    assert!(count > 0);

    db.with_snapshot(|snap| {
        // T1 blueprint 890_102 has NPC ask at exact base_price (77_000.0) and no bid order
        let bp_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 890_102 && !o.is_bid)
            .expect("T1 blueprint ask order");
        assert!(bp_ask.is_npc);
        assert_eq!(bp_ask.price, IskCents::from_f64_isk(77_000.0));
        assert!(snap
            .market_orders
            .values()
            .all(|o| !(o.type_id == 890_102 && o.is_bid)));

        // Skillbook 890_020 uses BOM price (50.0), skillbook 890_021 uses fallback (100.0)
        let sb_bom_order = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 890_020)
            .expect("BOM skillbook order");
        assert_eq!(sb_bom_order.price, IskCents::from_f64_isk(50.0));

        let sb_fb_order = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 890_021)
            .expect("Fallback skillbook order");
        assert_eq!(
            sb_fb_order.price,
            IskCents::from_f64_isk(DEFAULT_FALLBACK_BASE_PRICE_ISK)
        );
    });
}

#[test]
fn market_catalog_4_sig_fig_rounding_plex_fallback_and_idempotence() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.values().min_by_key(|t| t.type_id).unwrap().clone();

    // 1. PLEX (44_992) with base_price == 0.0 -> uses 5_000_000.0 fallback and seeds both ask and bid
    let mut plex = base_type.clone();
    plex.type_id = 44_992;
    plex.category_id = 17; // Commodity (not NPC-seeded category 9/16)
    plex.group_id = 1875;
    plex.base_price = 0.0;
    plex.published = true;
    plex.market_group_id = Some(1922);
    plex.default_attributes.clear();
    sde.types.insert(44_992, plex);

    // 2. Category 9 blueprint with non-4-sig-fig base_price -> ask rounded to 4 sig figs (123_456.78 -> 123_500.0)
    let mut bp_round = base_type.clone();
    bp_round.type_id = 890_501;
    bp_round.category_id = 9;
    bp_round.group_id = 9950;
    bp_round.base_price = 123_456.78;
    bp_round.published = true;
    bp_round.market_group_id = Some(20);
    bp_round.default_attributes.clear();
    sde.types.insert(890_501, bp_round);

    // 3. Category 16 skillbook with tiny base_price (0.001) -> clamped to >= 0.01 ISK
    let mut sb_tiny = base_type;
    sb_tiny.type_id = 890_502;
    sb_tiny.category_id = 16;
    sb_tiny.group_id = 255;
    sb_tiny.base_price = 0.001;
    sb_tiny.published = true;
    sb_tiny.market_group_id = Some(150);
    sb_tiny.default_attributes.clear();
    sde.types.insert(890_502, sb_tiny);

    let db = WorldRepository::new_empty();
    let first_inserted = seed_full_market_catalog(&db, &sde);
    assert!(first_inserted > 0);
    // Second call skips all existing orders (idempotent)
    let second_inserted = seed_full_market_catalog(&db, &sde);
    assert_eq!(second_inserted, 0);

    db.with_snapshot(|snap| {
        let bp_order = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 890_501 && !o.is_bid)
            .expect("rounded blueprint ask order");
        assert_eq!(bp_order.price, IskCents::from_f64_isk(123_500.0));
        assert!(bp_order.is_npc);

        let tiny_order = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 890_502 && !o.is_bid)
            .expect("tiny skillbook ask order");
        assert_eq!(tiny_order.price, IskCents(1));

        // PLEX bid order should exist even though 44_992 is seeded via special rule
        let plex_bid = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 44_992 && o.is_bid)
            .expect("PLEX bid order");
        assert!(plex_bid.price.0 > 0);
    });
}
