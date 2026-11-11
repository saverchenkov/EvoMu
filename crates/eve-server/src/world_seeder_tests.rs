//! Unit tests for `world_seeder` BOM calculation, catalog filtering, and Dual-Anchor market seeding.

#![forbid(unsafe_code)]

use super::*;
use crate::formulas::compute_living_world_fair_price;
use eve_db::{IskCents, WorldRepository};
use eve_sde::{BlueprintMaterial, SdeRegistry};
use std::collections::HashMap;

fn build_synthetic_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.values().next().expect("seed has types").clone();
    let base_bp = sde
        .blueprints
        .values()
        .next()
        .expect("seed has blueprints")
        .clone();

    // Ensure all 4 TRADE_HUB_STATIONS exist with distinct owner factions.
    let hub_factions = [
        (60_003_760i64, 10_000_002u32, 1_000_035i64, 500_001i64),
        (60_008_494i64, 10_000_043u32, 1_000_066i64, 500_003i64),
        (60_011_866i64, 10_000_032u32, 1_000_114i64, 500_004i64),
        (60_004_588i64, 10_000_030u32, 1_000_054i64, 500_002i64),
    ];
    let template_station = sde.stations.values().next().expect("seed has stations").clone();
    for (sid, rid, owner_corp, faction_id) in hub_factions {
        let st = sde.stations.entry(sid).or_insert_with(|| {
            let mut s = template_station.clone();
            s.station_id = sid;
            s
        });
        st.region_id = rid;
        st.owner_corporation_id = owner_corp;
        st.owner_faction_id = Some(faction_id);
    }

    // 1. Raw mineral (cat 4, group 18 -> all group 18 types included in baseline)
    let mut mat_t = base_type.clone();
    mat_t.type_id = 880_000;
    mat_t.category_id = 4;
    mat_t.group_id = 18;
    mat_t.base_price = 5.0;
    mat_t.published = true;
    mat_t.market_group_id = Some(1857);
    mat_t.meta_group_id = Some(1);
    mat_t.default_attributes.clear();
    sde.types.insert(mat_t.type_id, mat_t);

    // 2. Intermediate component with base_price=0 (pass 1 BOM = 10 * 5.0 / 2 = 25.0)
    let mut comp_t = base_type.clone();
    comp_t.type_id = 880_001;
    comp_t.category_id = 4;
    comp_t.group_id = 334;
    comp_t.base_price = 0.0;
    comp_t.published = true;
    comp_t.market_group_id = Some(100);
    comp_t.meta_group_id = Some(1);
    comp_t.default_attributes.clear();
    sde.types.insert(comp_t.type_id, comp_t);

    let mut bp_comp = base_bp.clone();
    bp_comp.blueprint_type_id = 880_101;
    bp_comp.product_type_id = 880_001;
    bp_comp.product_quantity = 2;
    bp_comp.materials = vec![BlueprintMaterial {
        type_id: 880_000,
        quantity: 10,
    }];
    sde.blueprints.insert(bp_comp.blueprint_type_id, bp_comp);

    // 3. Tech II ship with base_price=0, meta_group_id=2 (pass 2 BOM = 4*25.0 + 3*5.0 = 115.0)
    let mut t2_ship = base_type.clone();
    t2_ship.type_id = 880_002;
    t2_ship.category_id = 6;
    t2_ship.group_id = 324;
    t2_ship.base_price = 0.0;
    t2_ship.published = true;
    t2_ship.market_group_id = Some(50);
    t2_ship.meta_group_id = Some(2);
    t2_ship.default_attributes = HashMap::from([(422, 2.0)]);
    sde.types.insert(t2_ship.type_id, t2_ship);

    let mut bp_t2 = base_bp.clone();
    bp_t2.blueprint_type_id = 880_102;
    bp_t2.product_type_id = 880_002;
    bp_t2.product_quantity = 1;
    bp_t2.materials = vec![
        BlueprintMaterial {
            type_id: 880_001,
            quantity: 4,
        },
        BlueprintMaterial {
            type_id: 889_999, // unknown material -> fallback 5.0
            quantity: 3,
        },
    ];
    sde.blueprints.insert(bp_t2.blueprint_type_id, bp_t2);

    // T2 blueprint item (excluded from select_seeded_market_types because product TL >= 2.0)
    let mut bp_t2_item = base_type.clone();
    bp_t2_item.type_id = 880_102;
    bp_t2_item.category_id = 9;
    bp_t2_item.group_id = 105;
    bp_t2_item.base_price = 10_000.0;
    bp_t2_item.published = true;
    bp_t2_item.market_group_id = Some(200);
    bp_t2_item.default_attributes.clear();
    sde.types.insert(bp_t2_item.type_id, bp_t2_item);

    // 4. Two T1 items in custom group 9901 -> baseline picks only lowest type_id (880_010)
    for (tid, price) in [(880_010u32, 1_000.0), (880_011u32, 2_000.0)] {
        let mut t = base_type.clone();
        t.type_id = tid;
        t.category_id = 7;
        t.group_id = 9901;
        t.base_price = price;
        t.published = true;
        t.market_group_id = Some(300);
        t.meta_group_id = Some(1);
        t.default_attributes.clear();
        sde.types.insert(tid, t);
    }

    // 5. Fallback zero-base-price item with no blueprint (uses DEFAULT_FALLBACK_BASE_PRICE_ISK = 100.0)
    let mut fallback_t = base_type.clone();
    fallback_t.type_id = 880_020;
    fallback_t.category_id = 65; // Structure
    fallback_t.group_id = 9902;
    fallback_t.base_price = 0.0;
    fallback_t.published = true;
    fallback_t.market_group_id = Some(400);
    fallback_t.default_attributes.clear();
    sde.types.insert(fallback_t.type_id, fallback_t);

    // 6. Skillbook (cat 16) + Faction module (meta_group_id = 4, faction_id = 500_001 Caldari)
    let mut skill_t = base_type.clone();
    skill_t.type_id = 880_030;
    skill_t.category_id = 16;
    skill_t.group_id = 9903;
    skill_t.base_price = 50_000.0;
    skill_t.published = true;
    skill_t.market_group_id = Some(500);
    skill_t.default_attributes.clear();
    sde.types.insert(skill_t.type_id, skill_t);

    let mut fac_t = base_type.clone();
    fac_t.type_id = 880_040;
    fac_t.category_id = 7;
    fac_t.group_id = 9904;
    fac_t.base_price = 250_000.0;
    fac_t.published = true;
    fac_t.market_group_id = Some(600);
    fac_t.meta_group_id = Some(4);
    fac_t.faction_id = Some(500_001);
    fac_t.default_attributes = HashMap::from([(1692, 4.0)]);
    sde.types.insert(fac_t.type_id, fac_t);

    // 7. Excluded cosmetic item (cat 91) and unpublished item
    let mut skin_t = base_type.clone();
    skin_t.type_id = 880_090;
    skin_t.category_id = 91;
    skin_t.group_id = 1950;
    skin_t.base_price = 500.0;
    skin_t.published = true;
    skin_t.market_group_id = Some(700);
    sde.types.insert(skin_t.type_id, skin_t);

    let mut unpub_t = base_type;
    unpub_t.type_id = 880_091;
    unpub_t.category_id = 4;
    unpub_t.group_id = 18;
    unpub_t.base_price = 500.0;
    unpub_t.published = false;
    unpub_t.market_group_id = Some(700);
    sde.types.insert(unpub_t.type_id, unpub_t);

    sde
}

#[test]
fn bom_cost_and_catalog_selection_cover_two_pass_and_filters() {
    let sde = build_synthetic_sde();
    let bom_map = build_bom_cost_map(&sde);

    assert!((bom_map[&880_001] - 25.0).abs() < 1e-6);
    assert!((bom_map[&880_002] - 115.0).abs() < 1e-6);
    assert!((compute_bom_cost(880_001, &sde) - 25.0).abs() < 1e-6);
    assert_eq!(compute_bom_cost(999_999, &sde), 0.0);

    let full = select_full_market_types(&sde);
    assert!(full.contains(&880_000));
    assert!(full.contains(&880_001));
    assert!(full.contains(&880_002));
    assert!(full.contains(&880_010));
    assert!(full.contains(&880_011));
    assert!(full.contains(&880_020));
    assert!(full.contains(&880_030));
    assert!(full.contains(&880_040));
    assert!(!full.contains(&880_090)); // category 91 excluded
    assert!(!full.contains(&880_091)); // unpublished excluded

    let baseline = select_seeded_market_types(&sde);
    assert!(baseline.contains(&880_000)); // group 18 included
    assert!(!baseline.contains(&880_001)); // base_price == 0.0 excluded
    assert!(!baseline.contains(&880_002)); // TL 2.0 excluded
    assert!(!baseline.contains(&880_102)); // T2 blueprint excluded
    assert!(baseline.contains(&880_010)); // lowest in group 9901
    assert!(!baseline.contains(&880_011)); // second in group 9901 capped at 1
    assert!(baseline.contains(&880_030)); // skillbook
    assert!(!baseline.contains(&880_040)); // meta_group 4 excluded from baseline
}

#[test]
fn seed_dual_anchor_and_full_catalog_create_tier1_and_tier2_orders_idempotently() {
    let sde = build_synthetic_sde();
    let db = WorldRepository::new_empty();

    let first_inserted = seed_dual_anchor_markets(&db, &sde);
    assert!(first_inserted > 0);
    // Ephemeral DB is idempotent after first seed.
    assert_eq!(seed_dual_anchor_markets(&db, &sde), 0);

    db.with_snapshot(|snap| {
        assert!(snap.markets_seeded);

        // Skillbook 880_030 gets NPC ask at exact base_price (50_000 ISK) and no bid order.
        let jita_skill_asks: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.station_id == 60_003_760 && o.type_id == 880_030 && !o.is_bid)
            .collect();
        assert_eq!(jita_skill_asks.len(), 1);
        assert!(jita_skill_asks[0].is_npc);
        assert_eq!(jita_skill_asks[0].price, IskCents::from_f64_isk(50_000.0));
        assert!(snap
            .market_orders
            .values()
            .all(|o| !(o.type_id == 880_030 && o.is_bid)));

        // T1 item 880_010 gets ask (+6%) and bid (-6%) in Jita (hub_mult = 1.00), rounded to 4 sig figs.
        let fair = compute_living_world_fair_price(1_000.0, 0.0, 1.00);
        let expected_ask = crate::services::market_contracts::round_to_4_sig_figs((fair * 1.06).max(1.0));
        let expected_bid = crate::services::market_contracts::round_to_4_sig_figs((fair * 0.94).max(0.5));
        let jita_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 880_010 && !o.is_bid)
            .expect("Jita T1 ask");
        assert!(!jita_ask.is_npc);
        assert_eq!(jita_ask.vol_entered, 1_000);
        assert_eq!(jita_ask.price, IskCents::from_f64_isk(expected_ask));

        let jita_bid = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 880_010 && o.is_bid)
            .expect("Jita T1 bid");
        assert_eq!(jita_bid.vol_entered, 500);
        assert_eq!(jita_bid.price, IskCents::from_f64_isk(expected_bid));
        assert_eq!(jita_bid.escrow, IskCents::from_f64_isk(expected_bid * 500.0));
        for order in snap.market_orders.values() {
            assert!(
                crate::services::market_contracts::is_valid_4_sig_fig_price(order.price.to_wire_f64()),
                "seeded order {} (type {}) price {} must conform to 4-sig-fig tick rule",
                order.order_id,
                order.type_id,
                order.price.to_wire_f64()
            );
        }

        // Faction item 880_040 (faction 500_001) is seeded at Jita (500_001) with vol=100, NOT at Amarr (500_003).
        let jita_fac = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 880_040)
            .expect("Jita faction order");
        assert!(jita_fac.is_npc);
        assert_eq!(jita_fac.vol_entered, 100);
        assert_eq!(jita_fac.owner_id, 1_000_035);
        assert!(snap
            .market_orders
            .values()
            .all(|o| !(o.station_id == 60_008_494 && o.type_id == 880_040)));
    });

    // Now seed full catalog: Jita gets T2 item (with bid because meta_group_id==2), BOM item, and fallback item.
    let full_inserted = seed_full_market_catalog(&db, &sde);
    assert!(full_inserted > 0);
    assert_eq!(seed_full_market_catalog(&db, &sde), 0);

    db.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs
                .get(MARKET_SEEDER_VERSION_KEY)
                .map(String::as_str),
            Some(MARKET_SEEDER_VERSION_V2)
        );
        // T2 item 880_002 (BOM 115.0) gets both ask and bid in Jita.
        assert!(snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 880_002 && !o.is_bid));
        assert!(snap
            .market_orders
            .values()
            .any(|o| o.station_id == 60_003_760 && o.type_id == 880_002 && o.is_bid));

        // Fallback zero-base-price item 880_020 uses DEFAULT_FALLBACK_BASE_PRICE_ISK (100.0).
        let fair_fb = compute_living_world_fair_price(DEFAULT_FALLBACK_BASE_PRICE_ISK, 0.0, 1.00);
        let ask_fb = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 880_020 && !o.is_bid)
            .expect("fallback item ask");
        assert_eq!(
            ask_fb.price,
            IskCents::from_f64_isk(crate::services::market_contracts::round_to_4_sig_figs((fair_fb * 1.06).max(1.0)))
        );
        for order in snap.market_orders.values() {
            assert!(
                crate::services::market_contracts::is_valid_4_sig_fig_price(order.price.to_wire_f64()),
                "seeded order {} (type {}) price {} must conform to 4-sig-fig tick rule",
                order.order_id,
                order.type_id,
                order.price.to_wire_f64()
            );
        }
        assert_eq!(crate::services::market_contracts::round_to_4_sig_figs(12_560.58), 12_560.0);
        assert!(crate::services::market_contracts::is_valid_4_sig_fig_price(
            crate::services::market_contracts::round_to_4_sig_figs(12_560.58)
        ));
    });
}

#[test]
fn persistent_sqlite_world_upgrades_from_v1_to_v2_full_catalog() {
    let sde = build_synthetic_sde();
    let tmp_dir = std::env::temp_dir().join(format!("evomu_seeder_test_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("world_seeder_v2.sqlite");

    let db = WorldRepository::open_sqlite(&db_path).expect("open sqlite world db");
    // Simulate a v1 persistent DB where markets_seeded=true but market_seeder_version is absent.
    db.with_mut_snapshot(|snap| {
        snap.markets_seeded = true;
    });

    let upgraded = seed_dual_anchor_markets(&db, &sde);
    assert!(upgraded > 0);
    // Second call sees v2_full_catalog and skips re-seeding.
    assert_eq!(seed_dual_anchor_markets(&db, &sde), 0);

    drop(db);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[cfg(test)]
#[path = "world_seeder_cov_edges_tests.rs"]
mod world_seeder_cov_edges_tests;
