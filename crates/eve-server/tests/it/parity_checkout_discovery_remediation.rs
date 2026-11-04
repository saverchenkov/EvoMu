//! Parity closure integration tests for:
//! - `rpc:FastCheckoutService.BuyOffer`
//! - `rpc:FastCheckoutService.GetOffersForUser`
//! - `rpc:ProjectDiscovery.get_data_from_url`
//! - `rpc:ProjectDiscovery.get_maximum_bonus_samples`
//! - `rpc:ProjectDiscovery.get_new_task`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const SKIN_TYPE_ID: u32 = 40519;

async fn login_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(5);
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_fast_checkout_offers_and_buy_remediation() {
    // parity: rpc:FastCheckoutService.BuyOffer, rpc:FastCheckoutService.GetOffersForUser
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.BuyOffer",
        "rpc:FastCheckoutService.GetOffersForUser",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Unauthenticated call fails with Unauthorized
    let mut unauth = SessionState::new(88_001);
    let unauth_err = rpc(&ctx, &mut unauth, "FastCheckoutService", "GetOffersForUser", &[])
        .await
        .unwrap_err();
    assert_eq!(unauth_err.code, "Unauthorized");

    // 2. Authenticate pilot
    let mut sess = login_pilot(&ctx, 88_002, PILOT).await;
    let user_id = sess.user_id.expect("user_id on pilot session");

    // 3. Fast checkout initially disabled: GetOffersForUser returns empty list, BuyOffer fails
    let disabled_offers = rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[])
        .await
        .expect("GetOffersForUser disabled");
    assert_eq!(disabled_offers, PyRep::List(Vec::new()));

    let disabled_buy = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(disabled_buy.code, "FastCheckoutDisabled");

    // 4. Enable FastCheckout for user via slash command
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    let enable_cmd = rpc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/fastcheckout 1".into())],
    )
    .await
    .expect("/fastcheckout 1");
    assert_eq!(enable_cmd, PyRep::Bool(true));

    // 5. Seed authentic store catalog:
    //    - Offer 5001: 500 PLEX pack ($14.99 USD cash offer)
    //    - Offer 5002: Skin item pack priced in PLEX (150 PLEX)
    //    - Offer 5003: Expensive PLEX item pack (10,000 PLEX)
    ctx.db.with_mut_snapshot(|snap| {
        use eve_server::services::quasar_payment_store_types::{
            save_store_catalog, save_store_offer_tags, StoreCatalog, StoreCategory, StoreOffer,
            StoreProduct,
        };
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![
                    StoreCategory {
                        id: 1,
                        name: "PLEX".into(),
                        description: "PLEX Packages".into(),
                    },
                    StoreCategory {
                        id: 2,
                        name: "SKINs".into(),
                        description: "Ship SKINs".into(),
                    },
                ],
                products: vec![
                    StoreProduct {
                        id: 501,
                        name: "500 PLEX".into(),
                        description: "500 PLEX Pack".into(),
                        type_id: 44_992,
                        quantity: 500,
                    },
                    StoreProduct {
                        id: 502,
                        name: "Svipul Skin".into(),
                        description: "Exclusive Svipul Skin".into(),
                        type_id: SKIN_TYPE_ID,
                        quantity: 1,
                    },
                ],
                offers: vec![
                    StoreOffer {
                        id: 5001,
                        name: "500 PLEX".into(),
                        description: "500 PLEX Pack".into(),
                        category_id: 1,
                        price_plex: 0,
                        product_ids: vec![501],
                        targeted: false,
                        price_cents: Some(1_499),
                        currency: Some("USD".into()),
                    },
                    StoreOffer {
                        id: 5002,
                        name: "Svipul Skin Pack".into(),
                        description: "Svipul Skin Offer".into(),
                        category_id: 2,
                        price_plex: 150,
                        product_ids: vec![502],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                    StoreOffer {
                        id: 5003,
                        name: "Luxury Pack".into(),
                        description: "Luxury Pack 10k PLEX".into(),
                        category_id: 2,
                        price_plex: 10_000,
                        product_ids: vec![502],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                ],
            },
        );
        let mut tags = HashMap::new();
        tags.insert(5001, vec!["popular".to_string(), "bestseller".to_string()]);
        tags.insert(5002, vec!["skin".to_string()]);
        save_store_offer_tags(snap, &tags);
    });

    // 6. GetOffersForUser returns authentic structure matching client expectation dictionary
    let offers_rep = rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[])
        .await
        .expect("GetOffersForUser enabled");
    let PyRep::Dict(offers_dict) = offers_rep else {
        panic!("Expected Dict from GetOffersForUser");
    };
    let plex_offers = offers_dict
        .iter()
        .find_map(|(k, v)| (k == &PyRep::String("plex".into())).then_some(v))
        .expect("plex key in GetOffersForUser");
    let PyRep::List(offer_list) = plex_offers else {
        panic!("Expected List for plex key");
    };
    assert_eq!(offer_list.len(), 3);

    let o5001 = offer_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(|v| v.as_i64()) == Some(5001))
        .expect("offer 5001");
    assert_eq!(o5001.dict_get("baseQuantity").and_then(|v| v.as_i64()), Some(500));
    assert_eq!(o5001.dict_get("price").and_then(|v| v.as_f64()), Some(14.99));
    let tags_val = o5001.dict_get("tags").expect("tags");
    let PyRep::List(tags_list) = tags_val else { panic!("expected tags List"); };
    assert_eq!(tags_list.len(), 2);

    // 7. Negative BuyOffer validations:
    // 7a. Non-existent offer ID -> OfferNotFound
    let not_found_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(9999), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(not_found_err.code, "OfferNotFound");

    // 7b. Invalid non-positive quantity -> InvalidQuantity
    let invalid_qty_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::Integer(0), PyRep::String("demo-secret".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(invalid_qty_err.code, "InvalidQuantity");

    // 7c. Incorrect password -> InvalidPassword
    let bad_pass_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::Integer(1), PyRep::String("wrong-pwd".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_pass_err.code, "InvalidPassword");

    // 7d. Cash offer without payment token -> PaymentTokenRequired
    let no_token_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(no_token_err.code, "PaymentTokenRequired");

    // 7e. Insufficient PLEX balance -> UserError
    let no_balance_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5003), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(no_balance_err.code, "UserError");
    assert!(no_balance_err.message.contains("Insufficient"));

    // 8. Register quick-pay payment token
    eve_server::services::quasar_payment_store_types::add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        user_id,
        eve_server::services::quasar_payment_store_types::PaymentTokenRecord {
            id: 88,
            user_id,
            token_str: "tok_fc_88".into(),
            is_quick_pay: true,
            consumed: false,
        },
    );

    // 9. Authentic BuyOffer delivering PLEX (Offer 5001)
    let vault_before = ctx
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    let buy_plex_rep = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .expect("BuyOffer 5001 succeeds");
    let PyRep::Dict(buy_plex_dict) = buy_plex_rep else {
        panic!("expected Dict from BuyOffer");
    };
    assert_eq!(
        buy_plex_dict.iter().find_map(|(k, v)| (k == &PyRep::String("status".into())).then_some(v.as_str())),
        Some(Some("CONFIRMED"))
    );
    assert_eq!(
        buy_plex_dict.iter().find_map(|(k, v)| (k == &PyRep::String("plexAmount".into())).then_some(v.as_i64())),
        Some(Some(500))
    );

    let vault_after = ctx
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    assert_eq!(vault_after, vault_before + 500);

    // 10. Authentic BuyOffer with PLEX delivering items to hangar (Offer 5002)
    let hangar_skins_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == PILOT && i.type_id == SKIN_TYPE_ID && i.flag == InventoryFlag::Hangar)
            .map(|i| i.quantity)
            .sum::<i32>()
    });

    let buy_item_rep = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5002), PyRep::Integer(1), PyRep::String("demo-secret".into())],
    )
    .await
    .expect("BuyOffer 5002 succeeds");
    let PyRep::Dict(buy_item_dict) = buy_item_rep else {
        panic!("expected Dict from BuyOffer");
    };
    assert_eq!(
        buy_item_dict.iter().find_map(|(k, v)| (k == &PyRep::String("status".into())).then_some(v.as_str())),
        Some(Some("CONFIRMED"))
    );

    // Vault decreased by 150 PLEX
    let vault_final = ctx
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    assert_eq!(vault_final, vault_after - 150);

    // Item delivered to character's station hangar
    let hangar_skins_after = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == PILOT && i.type_id == SKIN_TYPE_ID && i.flag == InventoryFlag::Hangar)
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(hangar_skins_after, hangar_skins_before + 1);
}

#[tokio::test]
async fn test_project_discovery_authentic_tasks_and_bonus_samples() {
    // parity: rpc:ProjectDiscovery.get_data_from_url, rpc:ProjectDiscovery.get_maximum_bonus_samples, rpc:ProjectDiscovery.get_new_task
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_data_from_url",
        "rpc:ProjectDiscovery.get_maximum_bonus_samples",
        "rpc:ProjectDiscovery.get_new_task",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = login_pilot(&ctx, 89_001, PILOT).await;

    // 1. Initial bonus samples: max is 10, remaining starts at 10
    let max_bonus_init = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_maximum_bonus_samples", &[])
        .await
        .expect("get_maximum_bonus_samples initial");
    assert_eq!(max_bonus_init, PyRep::Integer(10));

    let rem_bonus_init = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
        .await
        .expect("get_remaining_bonus_samples initial");
    assert_eq!(rem_bonus_init, PyRep::Integer(10));

    // 2. Configure MMOS bonus sample pool via server helper
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_bonus_samples(snap, 20);
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 2_000.0, 500.0, 200.0, 50.0, 100,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 1, true);
    });

    let max_bonus_updated = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_maximum_bonus_samples", &[])
        .await
        .expect("get_maximum_bonus_samples updated");
    assert_eq!(max_bonus_updated, PyRep::Integer(20));

    // 3. Issue authentic task via get_new_task
    let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let PyRep::KeyVal(task_kv) = task_rep else {
        panic!("expected KeyVal from get_new_task");
    };
    let task_id = task_kv.get("task_id").and_then(|v| v.as_str()).expect("task_id");
    assert_eq!(task_id, "task_1");
    assert_eq!(task_kv.get("taskId").and_then(|v| v.as_str()), Some("task_1"));
    assert_eq!(task_kv.get("taskID").and_then(|v| v.as_str()), Some("task_1"));
    assert_eq!(task_kv.get("task_number").and_then(|v| v.as_i64()), Some(1));
    let task_url = task_kv.get("url").and_then(|v| v.as_str()).expect("task url");
    assert!(task_url.starts_with("https://projectdiscovery.eveonline.com/"));
    assert!(task_url.contains("/tasks/1"));

    // 4. get_data_from_url error conditions:
    // 4a. Empty URL -> InvalidTaskUrl
    let err_empty = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("".into())])
        .await
        .unwrap_err();
    assert_eq!(err_empty.code.to_ascii_lowercase(), "invalidtaskurl");

    // 4b. Foreign host -> InvalidTaskUrl
    let err_foreign = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://malicious.example.com/tasks/1".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_foreign.code.to_ascii_lowercase(), "invalidtaskurl");

    // 4c. Non-numeric task index -> InvalidTaskUrl
    let err_bad_num = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/abc".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_bad_num.code.to_ascii_lowercase(), "invalidtaskurl");

    // 4d. Unissued task number -> TaskNotFound
    let err_unissued = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/9999".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_unissued.code.to_ascii_lowercase(), "tasknotfound");

    // 5. Authentic task data fetch via get_data_from_url:
    let data_rep1 = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(task_url.into())],
    )
    .await
    .expect("get_data_from_url first fetch");
    let PyRep::String(json_str) = data_rep1 else {
        panic!("expected PyRep::String from get_data_from_url");
    };
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON payload");

    // Authentic top-level and MMOS payload fields
    assert_eq!(val["task_id"], "task_1");
    assert_eq!(val["task_number"], 1);
    assert_eq!(val["project_id"], "mcgill-polyseg");
    assert_eq!(val["samples"], 20);
    assert_eq!(val["fetch_count"], 1);
    assert_eq!(val["task_type"], "standard");
    assert!(val["experiment"]["cell_type"].as_str().is_some());
    assert!(val["assets"]["image_url"].as_str().unwrap().contains("sample_1.png"));
    assert!(val["clusters"].as_array().expect("clusters array").len() >= 2);

    // Second fetch increments fetch_count
    let data_rep2 = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(task_url.into())],
    )
    .await
    .expect("get_data_from_url second fetch");
    let val2: serde_json::Value = serde_json::from_str(data_rep2.as_str().unwrap()).unwrap();
    assert_eq!(val2["fetch_count"], 2);

    // 6. Submit classification consuming a bonus sample:
    let submit_payload = PyRep::Dict(vec![
        (PyRep::String("polygons".into()), PyRep::List(vec![])),
    ]);
    let submit_rep = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::Integer(1), submit_payload],
    )
    .await
    .expect("post_classification");
    let PyRep::KeyVal(res_kv) = submit_rep else {
        panic!("expected KeyVal from post_classification");
    };
    assert_eq!(res_kv.get("gotBonusXP"), Some(&PyRep::Bool(true)));
    assert_eq!(
        res_kv.get("bonusSamplesAfterClassification"),
        Some(&PyRep::Integer(19))
    );

    let rem_after_submit = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
        .await
        .expect("get_remaining_bonus_samples after submit");
    assert_eq!(rem_after_submit, PyRep::Integer(19));

    // 7. Daily bonus sample refill: advance virtual clock by 25 hours (>86,400s)
    ctx.advance_secs(90_000);
    let rem_refilled = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_remaining_bonus_samples", &[])
        .await
        .expect("get_remaining_bonus_samples after daily reset");
    assert_eq!(rem_refilled, PyRep::Integer(20));
}
