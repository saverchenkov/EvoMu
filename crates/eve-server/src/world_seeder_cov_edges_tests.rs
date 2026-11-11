//! Edge-case unit tests for `world_seeder` filtering, PLEX/floor pricing, hub multipliers, and dedup.

#![forbid(unsafe_code)]

use super::*;
use eve_db::{IskCents, MarketOrderRecord, WorldRepository};
use eve_sde::{BlueprintMaterial, SdeRegistry};

fn sde_with_hubs_and_edges() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde
        .types
        .values()
        .min_by_key(|t| t.type_id)
        .expect("seed type")
        .clone();
    let base_bp = sde
        .blueprints
        .values()
        .min_by_key(|b| b.blueprint_type_id)
        .expect("seed bp")
        .clone();
    let tmpl_st = sde
        .stations
        .values()
        .min_by_key(|s| s.station_id)
        .expect("seed station")
        .clone();

    for (sid, rid, owner_corp, faction_id) in [
        (60_003_760i64, 10_000_002u32, 1_000_035i64, 500_001i64),
        (60_008_494i64, 10_000_043u32, 1_000_066i64, 500_003i64),
        (60_011_866i64, 10_000_032u32, 1_000_114i64, 500_004i64),
        (60_004_588i64, 10_000_030u32, 1_000_054i64, 500_002i64),
    ] {
        let st = sde.stations.entry(sid).or_insert_with(|| {
            let mut s = tmpl_st.clone();
            s.station_id = sid;
            s
        });
        st.region_id = rid;
        st.owner_corporation_id = owner_corp;
        st.owner_faction_id = Some(faction_id);
    }

    let mut make_t = |tid: u32, cat: u32, grp: u32, pubd: bool, mg: Option<u32>, bp: f64, meta: Option<f64>, tl: Option<f64>| {
        let mut t = base_type.clone();
        t.type_id = tid;
        t.category_id = cat;
        t.group_id = grp;
        t.published = pubd;
        t.market_group_id = mg;
        t.meta_group_id = meta.map(|m| m as u32);
        t.base_price = bp;
        t.name = format!("Type{tid}");
        t.default_attributes.clear();
        if let Some(ml) = meta {
            t.default_attributes.insert(1692, ml);
        }
        if let Some(tlev) = tl {
            t.default_attributes.insert(422, tlev);
        }
        sde.types.insert(tid, t);
    };

    // PLEX (44992) with base_price=0.0 -> floor 5_000_000.0 & gets bid even though not baseline
    make_t(44_992, 4, 1875, true, Some(1923), 0.0, None, None);
    // Unpublished skillbook (cat 16, published=false) & special unpublished skill 4050/56311
    make_t(998_101, 16, 255, false, Some(150), 10_000.0, None, None);
    make_t(998_102, 16, 4050, false, Some(150), 10_000.0, None, None);
    make_t(56_311, 16, 255, false, Some(150), 10_000.0, None, None);
    // Unpublished cat 9 blueprint included in full catalog via cat 9/16 rule; unpublished cat 6 ship excluded
    make_t(998_103, 9, 105, false, Some(200), 10_000.0, None, None);
    make_t(998_113, 6, 25, false, Some(61), 10_000.0, None, Some(1.0));
    // Market-group = None excluded from seeded
    make_t(998_104, 6, 25, true, None, 100_000.0, None, Some(1.0));
    // Tech level > 1.0 & meta group > 1.0 excluded from select_seeded_market_types
    make_t(998_105, 7, 40, true, Some(300), 100_000.0, Some(1.0), Some(2.0));
    make_t(998_106, 7, 40, true, Some(300), 100_000.0, Some(2.0), Some(1.0));
    // Cat 9 published blueprint missing sde.blueprints -> excluded
    make_t(998_107, 9, 105, true, Some(200), 10_000.0, None, None);
    // Cat 9 published blueprint pointing to missing product -> excluded
    make_t(998_108, 9, 105, true, Some(200), 10_000.0, None, None);
    let mut missing_prod_bp = base_bp.clone();
    missing_prod_bp.blueprint_type_id = 998_108;
    missing_prod_bp.product_type_id = 999_888_777;
    sde.blueprints.insert(998_108, missing_prod_bp);
    // Cheap non-baseline T1 ship -> ask >= 1.0 & no bid in full catalog
    make_t(998_109, 6, 25, true, Some(61), 0.1, Some(1.0), Some(1.0));
    // Material + blueprint module for hub multiplier comparison
    make_t(998_110, 4, 18, true, Some(18), 2.0, None, None);
    make_t(998_111, 7, 40, true, Some(300), 1000.0, Some(1.0), Some(1.0));
    let mut bp_mod = base_bp;
    bp_mod.blueprint_type_id = 998_112;
    bp_mod.product_type_id = 998_111;
    bp_mod.product_quantity = 1;
    bp_mod.materials = vec![BlueprintMaterial {
        type_id: 998_110,
        quantity: 100,
    }];
    sde.blueprints.insert(998_112, bp_mod);

    sde
}

#[test]
fn world_seeder_filters_plex_floor_hub_multipliers_and_dedup() {
    let sde = sde_with_hubs_and_edges();

    let seeded = select_seeded_market_types(&sde);
    assert!(!seeded.contains(&44_992)); // base_price=0.0 excluded from representative baseline
    assert!(seeded.contains(&998_110));
    assert!(seeded.contains(&998_111));
    assert!(!seeded.contains(&998_101));
    assert!(!seeded.contains(&998_104));
    assert!(!seeded.contains(&998_105));
    assert!(!seeded.contains(&998_106));
    assert!(!seeded.contains(&998_107));
    assert!(!seeded.contains(&998_108));

    let full = select_full_market_types(&sde);
    assert!(full.contains(&44_992));
    assert!(full.contains(&998_102));
    assert!(full.contains(&998_103));
    assert!(full.contains(&56_311));
    assert!(full.contains(&998_105));
    assert!(full.contains(&998_109));
    assert!(!full.contains(&998_113));

    let repo = WorldRepository::new_empty();
    repo.with_mut_snapshot(|snap| {
        snap.markets_seeded = false;
        snap.market_orders.insert(
            999_001,
            MarketOrderRecord {
                order_id: 999_001,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 998_110,
                is_bid: false,
                price: IskCents(200),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
    });

    let seeded_cnt = seed_full_market_catalog(&repo, &sde);
    assert!(seeded_cnt > 0);
    assert_eq!(seed_full_market_catalog(&repo, &sde), 0);

    repo.with_snapshot(|snap| {
        assert!(snap.markets_seeded);
        let jita_sells_mat: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.station_id == 60_003_760 && o.type_id == 998_110 && !o.is_bid)
            .collect();
        assert_eq!(jita_sells_mat.len(), 1);
        assert_eq!(jita_sells_mat[0].order_id, 999_001);

        let plex_orders: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.station_id == 60_003_760 && o.type_id == 44_992)
            .collect();
        assert_eq!(plex_orders.len(), 2);
        let plex_ask = plex_orders.iter().find(|o| !o.is_bid).unwrap();
        assert!(plex_ask.price.0 >= 500_000_000);

        let cheap_jita: Vec<_> = snap
            .market_orders
            .values()
            .filter(|o| o.station_id == 60_003_760 && o.type_id == 998_109)
            .collect();
        assert_eq!(cheap_jita.len(), 1);
        assert!(!cheap_jita[0].is_bid);
        assert!(cheap_jita[0].price.0 >= 100);

        let jita_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_003_760 && o.type_id == 998_111 && !o.is_bid)
            .unwrap()
            .price
            .0;
        let amarr_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_008_494 && o.type_id == 998_111 && !o.is_bid)
            .unwrap()
            .price
            .0;
        let rens_ask = snap
            .market_orders
            .values()
            .find(|o| o.station_id == 60_004_588 && o.type_id == 998_111 && !o.is_bid)
            .unwrap()
            .price
            .0;
        assert!(rens_ask > amarr_ask && amarr_ask > jita_ask);
    });
}
