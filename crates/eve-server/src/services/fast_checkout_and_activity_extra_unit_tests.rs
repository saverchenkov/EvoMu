//! Extra unit and mutation-killing tests for `fast_checkout_and_activity`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::fast_checkout_and_activity::*;
use crate::services::quasar_payment_store_types::{
    save_store_catalog, save_store_offer_tags, PaymentReceiptRecord, PaymentTokenRecord,
    StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
};
use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
    UpwellStructureRecord,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn fast_checkout_enabled_config_overrides_and_testing_configuration() {
    let mut ctx = test_ctx();
    let uid = 77_001u64;
    set_user_fast_checkout_enabled(&ctx, uid, false);
    assert!(!is_fast_checkout_enabled_for_user(&ctx, uid));

    for (cfg_val, expected) in [
        (PyRep::Bool(true), true),
        (PyRep::Bool(false), false),
        (PyRep::Integer(1), true),
        (PyRep::Integer(0), false),
        (PyRep::String("1".into()), true),
        (PyRep::String("TRUE".into()), true),
        (PyRep::String("false".into()), false),
        (PyRep::None, false),
    ] {
        ctx.global_config_values
            .write()
            .unwrap()
            .insert("fastCheckoutEnabled".into(), cfg_val);
        assert_eq!(is_fast_checkout_enabled_for_user(&ctx, uid), expected);
    }
    ctx.global_config_values
        .write()
        .unwrap()
        .remove("fastCheckoutEnabled");

    // Default user without saved state falls back to ctx.feature_flags.free_fast_checkout_plex
    ctx.feature_flags.free_fast_checkout_plex = false;
    assert!(!is_fast_checkout_enabled_for_user(&ctx, 88_888));
    ctx.feature_flags.free_fast_checkout_plex = true;
    assert!(is_fast_checkout_enabled_for_user(&ctx, 88_888));

    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();
    ctx.feature_flags.seed_demo = true;
    let PyRep::Tuple(t_demo) = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "GetTestingConfiguration",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected tuple");
    };
    assert_eq!(t_demo[0], PyRep::Bool(true));

    ctx.feature_flags.seed_demo = false;
    let PyRep::Tuple(t_prod) = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "GetTestingConfiguration",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected tuple");
    };
    assert_eq!(t_prod[0], PyRep::Bool(false));
}

#[test]
fn fast_checkout_custom_catalog_offers_and_paid_vs_plex_buy_offer_flows() {
    let mut ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Disabled user gets empty list and BuyOffer fails with FastCheckoutDisabled
    ctx.feature_flags.free_fast_checkout_plex = false;
    set_user_fast_checkout_enabled(&ctx, 1, false);
    let disabled_offers = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "GetOffersForUser",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(disabled_offers, PyRep::List(Vec::new()));
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1001)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FastCheckoutDisabled"
    );

    set_user_fast_checkout_enabled(&ctx, 1, true);

    // Seed custom NES catalog with a PLEX pack offer (id 2001, fiat) + Omega/Item offer (id 2002, fiat, category 1) + PLEX-priced offer (id 2003, price_plex 50)
    ctx.db.with_mut_snapshot(|snap| {
        let catalog = StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "Omega & PLEX".into(),
                description: "Store".into(),
            }],
            products: vec![
                StoreProduct {
                    id: 301,
                    name: "250 PLEX".into(),
                    description: "PLEX".into(),
                    type_id: 44992,
                    quantity: 250,
                },
                StoreProduct {
                    id: 302,
                    name: "30d Omega + Rifter".into(),
                    description: "Bundle".into(),
                    type_id: 587,
                    quantity: 2,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 2001,
                    name: "250 PLEX Pack".into(),
                    description: "Pack".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![301],
                    targeted: false,
                    price_cents: Some(999),
                    currency: Some("EUR".into()),
                },
                StoreOffer {
                    id: 2002,
                    name: "Omega Bundle".into(),
                    description: "Omega".into(),
                    category_id: 1,
                    price_plex: 0,
                    product_ids: vec![302],
                    targeted: false,
                    price_cents: Some(1999),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 2003,
                    name: "NES Ship Offer".into(),
                    description: "Ship".into(),
                    category_id: 2,
                    price_plex: 50,
                    product_ids: vec![302],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
            ],
        };
        save_store_catalog(snap, &catalog);
        let mut tags = HashMap::new();
        tags.insert(2001, vec!["featured".to_string()]);
        save_store_offer_tags(snap, &tags);
    });

    let _ = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "ClearFastCheckoutCacheForUser",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();

    // GetOffersForUser builds from custom catalog and caches; second call reads from cache
    let offers1 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "GetOffersForUser",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let offers2 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "GetOffersForUser",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(offers1, offers2);
    let plex_offers = offers1.dict_get("plex").and_then(|v| v.as_list()).unwrap();
    assert_eq!(plex_offers.len(), 3);

    // Paid mode (`free_fast_checkout_plex = false`): password verification, banned check, PLEX-priced offer, and quick-pay token
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.plex_balance = 20;
        }
        snap.account_plex_vault.insert(1, 20);
    });

    // Missing or wrong password fails InvalidPassword
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Dict(vec![(
                PyRep::String("offerId".into()),
                PyRep::Integer(2002),
            )])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(2002),
                PyRep::Integer(1),
                PyRep::String("wrong".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPassword"
    );

    // Banned account fails AccountBanned
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.get_mut("demo").unwrap().banned_until_unix = Some(u64::MAX);
    });
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(2002),
                PyRep::Integer(1),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccountBanned"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.get_mut("demo").unwrap().banned_until_unix = None;
    });

    // PLEX-priced offer (2003, 50 PLEX): fails UserError with 20 PLEX, succeeds with 100 PLEX
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(2003),
                PyRep::Integer(1),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserError"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().plex_balance = 100;
        snap.account_plex_vault.insert(1, 100);
    });
    let plex_buy = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(2003),
            PyRep::Integer(1),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        plex_buy.dict_get("status").and_then(|v| v.as_str()),
        Some("CONFIRMED")
    );
    assert_eq!(
        plex_buy.dict_get("newPlexBalance").and_then(|v| v.as_i64()),
        Some(50)
    );

    // Fiat offer (2002) without quick-pay token fails PaymentTokenRequired; with quick-pay token succeeds, activates Omega, and records receipt
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(2002),
                PyRep::Integer(1),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PaymentTokenRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = vec![PaymentTokenRecord {
            id: 1,
            user_id: 1,
            token_str: "tok_qp_1".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs.insert(
            "payment_tokens".into(),
            serde_json::to_string(&tokens).unwrap(),
        );
    });
    let mut buy_kw = HashMap::new();
    buy_kw.insert("offer_id".to_string(), PyRep::Integer(2002));
    buy_kw.insert("quantity".to_string(), PyRep::Integer(1));
    buy_kw.insert("password".to_string(), PyRep::String("demo-secret".into()));
    let fiat_buy = handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[], &buy_kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        fiat_buy.dict_get("status").and_then(|v| v.as_str()),
        Some("CONFIRMED")
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&90_000_001].clone_grade, 2);
        let receipts: Vec<PaymentReceiptRecord> = serde_json::from_str(
            snap.service_state_blobs.get("payment_receipts").unwrap(),
        )
        .unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].offer_id, "2002");
        assert_eq!(receipts[0].amount_in_cents, 1999);
    });
}

#[test]
fn mining_details_cycle_output_and_activity_mgr_edge_cases() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Direct helper compute_mining_materials_per_cycle with explicit moon_ores vs empty fallback
    let mats_explicit = compute_mining_materials_per_cycle(
        &ctx.sde,
        30_000_142,
        40_009_082,
        &[(45_490, 0.5)],
    );
    assert!(!mats_explicit.is_empty());

    let sid = 10_002_500_001i64;
    let fuel_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1136)
        .map(|t| t.type_id)
        .unwrap_or(4247);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35835,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Athanor Miner".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.auto_moon_miners.insert(
            sid,
            AutoMoonMinerRecord {
                structure_id: sid,
                fuel_units_remaining: 77,
                magmatic_gas_remaining: 100,
                efficiency: 0.4,
                cycle_duration_seconds: 3_600,
                last_cycle_unix: 1_700_000_000,
                ore_type_id: 45_490,
                hourly_yield: 500,
                status: "Active".into(),
            },
        );
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: sid,
                flag: InventoryFlag::SpecializedMineralHold,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // AccessDenied for outsider character without ACL access
    let outsider = auth_session(2, 90_000_999, 98_000_999, 30_000_142);
    assert_eq!(
        handle(
            &ctx,
            &outsider,
            "FastCheckoutService",
            "RequestMiningDetails",
            &[PyRep::Integer(sid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Owner corp member queries RequestMiningDetails via kwargs `structureID` -> uses auto_miner fuel (77)
    let mut m_kw = HashMap::new();
    m_kw.insert("structureID".to_string(), PyRep::Integer(sid));
    let details1 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[],
        &m_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        details1.dict_get("availableFuel").and_then(|v| v.as_i64()),
        Some(77)
    );
    assert!(
        details1
            .dict_get("remainingOutputBayCapacity")
            .and_then(|v| v.as_f64())
            .unwrap()
            > 0.0
    );

    // Add SpecializedFuelBay fuel block item + moon_extractions record -> takes precedence over auto_miner
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: fuel_tid,
                owner_id: 98_000_001,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 120,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.moon_extractions.insert(
            sid,
            MoonExtractionRecord {
                structure_id: sid,
                moon_id: 40_009_082,
                extraction_start_unix: 1_700_000_000,
                chunk_arrival_unix: 1_700_050_000,
                natural_decay_unix: 1_700_090_000,
                status: "Started".into(),
                auto_fracture: false,
                event_id: 9_001,
                moon_ores: vec![(45_490, 0.75)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
    });
    let details2 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        details2.dict_get("availableFuel").and_then(|v| v.as_i64()),
        Some(120)
    );
    assert_eq!(
        details2.dict_get("moonID").and_then(|v| v.as_i64()),
        Some(40_009_082)
    );

    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(sid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidStructureType"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&sid).unwrap();
        st.type_id = 81826;
        st.solar_system_id = 30_002_537;
    });
    let cycle_out = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(!cycle_out.as_list().unwrap().is_empty());

    // activityMgr: SetActivitiesPopState with KeyVal and >100 truncation
    let mut big_kv = Vec::new();
    for i in 0..105 {
        big_kv.push((format!("k_{i}"), PyRep::Integer(i)));
    }
    let _ = handle(
        &ctx,
        &sess,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::KeyVal(big_kv.into_iter().collect())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let pop_after = handle(
        &ctx,
        &sess,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(entries) = pop_after else {
        panic!("expected dict");
    };
    assert_eq!(entries.len(), 100);
}
