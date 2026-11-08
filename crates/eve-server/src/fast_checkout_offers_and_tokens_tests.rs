//! Semantic unit tests split from `fast_checkout_plex_tests`.

#![forbid(unsafe_code)]

use super::fast_checkout_plex_tests::*;
use crate::services::fast_checkout_and_activity::is_fast_checkout_enabled_for_user;
use crate::services::quasar_payment_store_types::{PaymentReceiptRecord, PaymentTokenRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn fast_checkout_default_paid_vs_free_one_click_behavior() {
    // 1) Default free_fast_checkout_plex = false preserves password & payment token gates
    let ctx_paid = test_ctx(false, 0);
    let uid_paid = ctx_paid.db.provision_player_account("paid_user", "secret123");
    let cid_paid = ctx_paid
        .db
        .create_character(uid_paid, "Paid Pilot", 1, 1, 11, 17, &shared_sde())
        .expect("create char")
        .character_id;
    assert!(!is_fast_checkout_enabled_for_user(&ctx_paid, uid_paid));

    ctx_paid
        .global_config_values
        .write()
        .expect("lock")
        .insert("fastCheckoutEnabled".into(), PyRep::Bool(true));
    assert!(is_fast_checkout_enabled_for_user(&ctx_paid, uid_paid));

    let mut sess_paid = SessionState::new(0);
    sess_paid.user_id = Some(uid_paid);
    sess_paid.character_id = Some(cid_paid);

    let paid_offers = ctx_paid
        .dispatch_rpc(
            &mut sess_paid,
            "FastCheckoutService",
            None,
            "GetOffersForUser",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("ok");
    let PyRep::List(paid_list) = dict_get(&paid_offers, "offers").expect("offers list") else {
        panic!("expected offers list");
    };
    let first_paid_price = match dict_get(&paid_list[0], "price") {
        Some(PyRep::Real(v)) => *v,
        _ => panic!("expected float price"),
    };
    assert!(first_paid_price > 0.0);

    let err_no_pw = ctx_paid
        .dispatch_rpc(
            &mut sess_paid,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("missing password must fail when free_fast_checkout_plex=false");
    assert_eq!(err_no_pw.code, "InvalidPassword");

    let err_no_tok = ctx_paid
        .dispatch_rpc(
            &mut sess_paid,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("secret123".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("missing payment token must fail when free_fast_checkout_plex=false");
    assert_eq!(err_no_tok.code, "PaymentTokenRequired");

    // Seed quick-pay token and confirm paid purchase works
    ctx_paid.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.payment_tokens = vec![PaymentTokenRecord {
            id: 1,
            user_id: uid_paid,
            token_str: "tok_4242".into(),
            is_quick_pay: true,
            consumed: false,
        }];
    });
    let paid_res = ctx_paid
        .dispatch_rpc(
            &mut sess_paid,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("secret123".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("paid purchase ok");
    assert_eq!(dict_get(&paid_res, "status").and_then(|r| r.as_str()), Some("CONFIRMED"));

    // 2) free_fast_checkout_plex = true enables $0.00 offers & passwordless 1-click checkout
    let ctx_free = test_ctx(true, 50_000);
    let uid_free = ctx_free.db.provision_player_account("free_user", "secret123");
    let cid_free = ctx_free
        .db
        .create_character(uid_free, "Free Pilot", 1, 1, 11, 17, &shared_sde())
        .expect("create char")
        .character_id;
    ctx_free.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(cid_free, 50_000);
    });
    assert!(is_fast_checkout_enabled_for_user(&ctx_free, uid_free));

    // Explicit globalConfig fastCheckoutEnabled=false overrides default
    ctx_free
        .global_config_values
        .write()
        .expect("lock")
        .insert("fastCheckoutEnabled".into(), PyRep::Bool(false));
    assert!(!is_fast_checkout_enabled_for_user(&ctx_free, uid_free));
    ctx_free
        .global_config_values
        .write()
        .expect("lock")
        .remove("fastCheckoutEnabled");

    let mut sess_free = SessionState::new(0);
    sess_free.user_id = Some(uid_free);
    sess_free.character_id = Some(cid_free);
    sess_free.address = "127.0.0.1:26000".into();

    // Even after NES catalog is seeded, all 4 default PLEX packs are present with price == 0.0
    let _ = ctx_free
        .dispatch_rpc(&mut sess_free, "storeManager", None, "get_offers", &[], &HashMap::new(), None)
        .await;
    let free_offers = ctx_free
        .dispatch_rpc(
            &mut sess_free,
            "FastCheckoutService",
            None,
            "GetOffersForUser",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("ok");
    let PyRep::List(free_list) = dict_get(&free_offers, "offers").expect("offers") else {
        panic!("expected list");
    };
    let plex_packs: Vec<i64> = free_list
        .iter()
        .filter_map(|o| {
            let amt = dict_get(o, "plexAmount").and_then(|v| v.as_i64()).unwrap_or(0);
            if amt > 0 {
                assert_eq!(dict_get(o, "price"), Some(&PyRep::Real(0.0)));
                Some(amt)
            } else {
                None
            }
        })
        .collect();
    assert!(plex_packs.contains(&100));
    assert!(plex_packs.contains(&250));
    assert!(plex_packs.contains(&500));
    assert!(plex_packs.contains(&1100));

    // Buy 2x 500 PLEX pack with empty/no password and no payment token
    let free_buy = ctx_free
        .dispatch_rpc(
            &mut sess_free,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(3), PyRep::Integer(2), PyRep::String(String::new())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("free buy ok");
    assert_eq!(dict_get(&free_buy, "status").and_then(|r| r.as_str()), Some("COMPLETED"));
    assert_eq!(dict_get(&free_buy, "Message").and_then(|r| r.as_str()), Some("OK"));
    assert_eq!(dict_get(&free_buy, "plexAmount").and_then(|r| r.as_i64()), Some(1000));
    assert_eq!(dict_get(&free_buy, "newPlexBalance").and_then(|r| r.as_i64()), Some(51_000));

    // Wrong non-empty password still rejected
    let wrong_pw = ctx_free
        .dispatch_rpc(
            &mut sess_free,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("wrong".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("wrong non-empty password must error");
    assert_eq!(wrong_pw.code, "InvalidPassword");

    // Verify no paid USD receipt was recorded for free purchase
    let free_receipts: Vec<PaymentReceiptRecord> = ctx_free
        .db
        .with_snapshot(|snap| snap.quasar_commerce.payment_receipts.clone());
    assert!(free_receipts.is_empty());

    // Spend PLEX in NES storeManager and verify non-zero PLEX prices & vault deduction
    let can_buy = ctx_free
        .dispatch_rpc(
            &mut sess_free,
            "storeManager",
            None,
            "can_purchase_offer",
            &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("ok");
    assert_eq!(can_buy, PyRep::Bool(true));

    let expected_cost = ctx_free.db.with_snapshot(|s| {
        crate::services::quasar_payment_store_types::read_store_catalog(s)
            .offers
            .into_iter()
            .find(|o| o.id == 1002)
            .map(|o| o.price_plex)
            .unwrap_or(110)
    });
    assert!(expected_cost > 0);
    let nes_buy = ctx_free
        .dispatch_rpc(
            &mut sess_free,
            "storeManager",
            None,
            "buy_offer",
            &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("nes buy ok");
    assert_eq!(ctx_free.db.with_snapshot(|s| s.plex_vault_balance(cid_free)), 51_000 - expected_cost);
    assert_eq!(nes_buy.dict_get("plexRemaining").and_then(|v| v.as_i64()), Some(51_000 - expected_cost));
}

#[tokio::test]
async fn fast_checkout_consumes_tokens_blocks_negative_vault_and_rejects_unpriced_offers() {
    let ctx = test_ctx(false, 0);
    let uid = ctx.db.provision_player_account("checkout_sec_pilot", "sec_pass");
    let cid = ctx
        .db
        .create_character(uid, "Checkout Sec", 1, 1, 11, 17, &shared_sde())
        .expect("create char")
        .character_id;

    let mut sess = SessionState::new(910);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.address = "127.0.0.1:26000".into();

    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 600);
        crate::services::quasar_payment_store::seed_default_store_catalog_if_empty(snap, &ctx.sde);
        let mut cat = crate::services::quasar_payment_store_types::read_store_catalog(snap);
        cat.offers.push(crate::services::quasar_payment_store_types::StoreOffer {
            id: 9901,
            name: "Unpriced Zero Offer".into(),
            description: "Zero price".into(),
            category_id: 2,
            price_plex: 0,
            product_ids: vec![],
            targeted: false,
            price_cents: None,
            currency: None,
        });
        snap.quasar_commerce.nes_store_catalog = Some(cat);
        snap.quasar_commerce.payment_tokens = vec![PaymentTokenRecord {
            id: 77_001,
            user_id: uid,
            token_str: "tok_quick_4242".into(),
            is_quick_pay: true,
            consumed: false,
        }];
    });

    // Enable FastCheckout for user
    crate::services::fast_checkout_and_activity::set_user_fast_checkout_enabled(&ctx, uid, true);

    // 1. PLEX offer more expensive than vault (2 * 500 = 1000 > 600) fails atomically without negative balance
    let over_res = ctx
        .dispatch_rpc(
            &mut sess,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1001), PyRep::Integer(2), PyRep::String("sec_pass".into())],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(over_res.is_err());
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(cid)), 600);

    // 2. Dual-priced offer (1002: price_plex > 0 and price_cents = Some(499)) settles in PLEX without USD double-charge
    let dual_res = ctx
        .dispatch_rpc(
            &mut sess,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1002), PyRep::Integer(1), PyRep::String("sec_pass".into())],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(dual_res.is_ok(), "dual_res failed: {dual_res:?}");
    let receipts_after_dual: Vec<PaymentReceiptRecord> =
        ctx.db.with_snapshot(|s| s.quasar_commerce.payment_receipts.clone());
    assert!(receipts_after_dual.is_empty());

    // 3. USD offer consumes quick-pay token once; replay with same token fails
    let usd_first = ctx
        .dispatch_rpc(
            &mut sess,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("sec_pass".into())],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(usd_first.is_ok());
    let usd_replay = ctx
        .dispatch_rpc(
            &mut sess,
            "FastCheckoutService",
            None,
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("sec_pass".into())],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(usd_replay.is_err());

    // 4. Unpriced offer 9901 rejected across storeManager and Quasar PurchaseRequest when !free_fast_checkout_plex
    assert!(ctx
        .dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(9901)], &HashMap::new(), None)
        .await
        .is_err());

    let mut q_val = Vec::new();
    eve_proto::quasar::encode_varint_field(1, 9901, &mut q_val);
    let q_req = eve_proto::quasar::EvePublicRequest {
        issued: Some(eve_proto::quasar::GoogleTimestamp { seconds: 1_700_000_010, nanos: 0 }),
        correlation_uuid: vec![2; 16],
        payload: Some(eve_proto::quasar::GoogleAny {
            type_url: format!("{}eve_public.plex.vault.api.PurchaseRequest", eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX),
            value: q_val,
        }),
        external_origin: "test".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: None,
        trace_context: None,
    };
    let q_resp = ctx.quasar_gateway.handle_public_request(&sess, &q_req, None, false, 1_700_000_010);
    assert_eq!(q_resp.status_code, crate::quasar_gateway::STATUS_BAD_REQUEST);

    // 5. Expiring Omega clears AccountServicesStore.subscription_expiry and user_entitlement_expiries
    ctx.db.with_mut_snapshot(|snap| {
        let mut acct = crate::services::account_services::load_store(snap);
        acct.subscription_expiry.insert(uid, 9_999_999_999);
        if let Ok(js) = serde_json::to_string(&acct) {
            snap.item_custom_names.insert(crate::services::account_services::ACCOUNT_SERVICES_STORE_KEY, js);
        }
        crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(snap, uid, 30 * 86_400, 1_700_000_000);
    });
    crate::services::quasar_auth_user::expire_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, uid);
    ctx.db.with_snapshot(|snap| {
        let acct = crate::services::account_services::load_store(snap);
        assert_eq!(acct.subscription_expiry.get(&uid).copied(), Some(0));
        assert_eq!(snap.characters.get(&cid).map(|c| c.clone_grade), Some(1));
    });
}
