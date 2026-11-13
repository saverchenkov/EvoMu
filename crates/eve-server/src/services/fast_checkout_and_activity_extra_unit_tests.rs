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
use eve_proto::PyRep;
use std::collections::HashMap;

#[cfg(test)]
#[path = "activity_mgr_and_mining_details_unit_tests.rs"]
mod activity_mgr_and_mining_details_unit_tests;

#[test]
fn fast_checkout_enabled_config_overrides_and_testing_configuration() {
    let ctx = test_ctx();
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
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    assert!(!is_fast_checkout_enabled_for_user(&ctx, 88_888));
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = true;
    assert!(is_fast_checkout_enabled_for_user(&ctx, 88_888));

    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .seed_demo = true;
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

    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .seed_demo = false;
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
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Disabled user gets empty list and BuyOffer fails with FastCheckoutDisabled
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
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
        for ch in snap.characters.values_mut().filter(|c| c.user_id == 1) {
            ch.plex_balance = 0;
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
        for ch in snap.characters.values_mut().filter(|c| c.user_id == 1) {
            ch.plex_balance = 0;
        }
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
        snap.quasar_commerce.payment_tokens = vec![PaymentTokenRecord {
            id: 1,
            user_id: 1,
            token_str: "tok_qp_1".into(),
            is_quick_pay: true,
            consumed: false,
        }];
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
        let receipts: &[PaymentReceiptRecord] = &snap.quasar_commerce.payment_receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].offer_id, "2002");
        assert_eq!(receipts[0].amount_in_cents, 1999);
    });
}

#[test]
fn fast_checkout_activity_overflow_pop_keyval_buy_validation_and_free_mode() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // 1. SetActivitiesPopState with KeyVal & truncation at 100 entries
    let mut big_kv = Vec::new();
    for i in 0..115 {
        big_kv.push((format!("k_{i}"), PyRep::Integer(i)));
    }
    let set_kv = handle(
        &ctx,
        &sess,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::KeyVal(big_kv.into_iter().collect())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_kv, PyRep::Bool(true));
    let pop_res = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::Dict(entries) = pop_res else {
        panic!("expected Dict");
    };
    assert_eq!(entries.len(), 100);

    // 2. ActivitySeen via kwargs + idempotency + unknown ID rejection + badge status Pyset output
    let valid_ids = ctx.sde.load_new_activity_ids().unwrap_or_default();
    if let Some(&first_aid) = valid_ids.iter().next() {
        let mut akw = HashMap::new();
        akw.insert("activityID".to_string(), PyRep::Integer(first_aid));
        assert_eq!(
            handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[], &akw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        // Duplicate call keeps set length unchanged
        assert_eq!(
            handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(first_aid)], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        let PyRep::PySet(badges) = handle(&ctx, &sess, "activityMgr", "GetActivityBadgeStatuses", &[], &kw)
            .unwrap()
            .unwrap()
        else {
            panic!("expected PySet");
        };
        assert_eq!(badges.iter().filter(|v| *v == &PyRep::Integer(first_aid)).count(), 1);
    }

    // Overflow > 128 cap drains oldest
    if valid_ids.len() > 130 {
        for &aid in valid_ids.iter().take(132) {
            let _ = handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(aid)], &kw);
        }
        let PyRep::PySet(badges) = handle(&ctx, &sess, "activityMgr", "GetActivityBadgeStatuses", &[], &kw)
            .unwrap()
            .unwrap()
        else {
            panic!("expected PySet");
        };
        assert_eq!(badges.len(), 128);
    }

    // 3. BuyOffer negative/zero validation + free PLEX mode COMPLETED branch without password
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    set_user_fast_checkout_enabled(&ctx, 1, true);
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(-5)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "OfferNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1001), PyRep::Integer(-2), PyRep::String("demo-secret".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(999_999_999), PyRep::Integer(1), PyRep::String("demo-secret".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // Free mode buys offer without password and returns COMPLETED
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = true;
    ctx.db.with_mut_snapshot(|snap| {
        let catalog = StoreCatalog {
            categories: vec![],
            products: vec![StoreProduct {
                id: 401,
                name: "100 PLEX".into(),
                description: "Free test PLEX".into(),
                type_id: 44992,
                quantity: 100,
            }],
            offers: vec![StoreOffer {
                id: 3001,
                name: "Free 100 PLEX".into(),
                description: "Free".into(),
                category_id: 2,
                price_plex: 0,
                product_ids: vec![401],
                targeted: false,
                price_cents: Some(500),
                currency: Some("USD".into()),
            }],
        };
        save_store_catalog(snap, &catalog);
    });
    let _ = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let free_res = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(3001), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(free_res.dict_get("status").and_then(|v| v.as_str()), Some("COMPLETED"));
    assert_eq!(free_res.dict_get("plexAmount").and_then(|v| v.as_i64()), Some(200));
    assert_eq!(free_res.dict_get("itemsDelivered").and_then(|v| v.as_i64()), Some(0));
}

#[test]
fn fast_checkout_state_tutorial_activity_and_buy_offer_exact_mutants() {
    let ctx = test_ctx();
    let uid = 78_001_u64;
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // 1. FastCheckout state helpers: get / save / set_enabled
    set_user_fast_checkout_enabled(&ctx, uid, true);
    let mut st = get_user_fast_checkout_state(&ctx, uid);
    assert!(st.enabled);
    st.cached_session_token = Some("saved_tok_78001".into());
    save_user_fast_checkout_state(&ctx, uid, &st);
    assert_eq!(
        get_user_fast_checkout_state(&ctx, uid).cached_session_token.as_deref(),
        Some("saved_tok_78001")
    );
    set_user_fast_checkout_enabled(&ctx, uid, false);
    assert!(!get_user_fast_checkout_state(&ctx, uid).enabled);

    // 2. Tutorial & activityMgr RPC handlers with exact return & DB assertions
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "initialize_tutorial_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "skip_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "get_tutorial_completion_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "initialize_tutorial_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "reset_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "get_tutorial_completion_status", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    let first_aid = ctx
        .sde
        .load_new_activity_ids()
        .unwrap_or_default()
        .into_iter()
        .next()
        .expect("SDE should have at least one activity ID");
    assert_eq!(
        handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(first_aid)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let PyRep::PySet(badges) = handle(&ctx, &sess, "activityMgr", "GetActivityBadgeStatuses", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("expected PySet from GetActivityBadgeStatuses");
    };
    assert!(badges.contains(&PyRep::Integer(first_aid)));

    assert_eq!(
        handle(
            &ctx,
            &sess,
            "activityMgr",
            "SetActivitiesPopState",
            &[PyRep::Dict(vec![(PyRep::String("banner".into()), PyRep::Bool(true))])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let pop = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pop.dict_get("banner"), Some(&PyRep::Bool(true)));

    // 3. BuyOffer non-free mode: PLEX-priced non-PLEX item vs Fiat-priced PLEX pack
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    set_user_fast_checkout_enabled(&ctx, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        for ch in snap.characters.values_mut().filter(|c| c.user_id == 1) {
            ch.plex_balance = 0;
        }
        snap.account_plex_vault.insert(1, 100);
        snap.quasar_commerce.payment_tokens.clear();
        snap.quasar_commerce.payment_receipts.clear();
        let catalog = StoreCatalog {
            categories: vec![],
            products: vec![
                StoreProduct {
                    id: 501,
                    name: "Tritanium Crate".into(),
                    description: "Items".into(),
                    type_id: 34,
                    quantity: 2,
                },
                StoreProduct {
                    id: 502,
                    name: "100 PLEX".into(),
                    description: "PLEX".into(),
                    type_id: 44992,
                    quantity: 100,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 4001,
                    name: "Tritanium Offer".into(),
                    description: "PLEX priced".into(),
                    category_id: 2,
                    price_plex: 40,
                    product_ids: vec![501],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 4002,
                    name: "100 PLEX Offer".into(),
                    description: "Fiat priced".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![502],
                    targeted: false,
                    price_cents: Some(500),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &catalog);
    });

    // Offer 4001 (amount_in_cents == 0, total_cost_plex == 40, total_plex == 0):
    // succeeds without payment token, produces NO payment receipt, deducts 40 PLEX (100 -> 60)
    let plex_spent_res = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(4001),
            PyRep::Integer(1),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(plex_spent_res.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(plex_spent_res.dict_get("newPlexBalance").and_then(|v| v.as_i64()), Some(60));
    assert_eq!(plex_spent_res.dict_get("itemsDelivered").and_then(|v| v.as_i64()), Some(2));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.quasar_commerce.payment_receipts.is_empty());
        assert_eq!(snap.unified_plex_balance_for_user(1), 60);
    });

    // Offer 4002 (amount_in_cents == 500 > 0, total_cost_plex == 0, total_plex == 100):
    // without token -> PaymentTokenRequired; with token -> adds 100 PLEX (60 -> 160) and pushes receipt
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(4002),
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
        snap.quasar_commerce.payment_tokens.push(PaymentTokenRecord {
            id: 77,
            user_id: 1,
            token_str: "tok_qp_77".into(),
            is_quick_pay: true,
            consumed: false,
        });
    });
    let fiat_plex_res = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(4002),
            PyRep::Integer(1),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(fiat_plex_res.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(fiat_plex_res.dict_get("plexAmount").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(fiat_plex_res.dict_get("newPlexBalance").and_then(|v| v.as_i64()), Some(160));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.quasar_commerce.payment_receipts.len(), 1);
        assert_eq!(snap.quasar_commerce.payment_receipts[0].amount_in_cents, 500);
        assert_eq!(snap.unified_plex_balance_for_user(1), 160);
    });
}
