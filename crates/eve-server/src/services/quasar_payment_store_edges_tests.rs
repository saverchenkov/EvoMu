//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_payment_store`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_varint_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry, WireMessage,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup() -> (PublicGatewayRouter, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    (router, db)
}

fn auth(uid: Option<i32>, cid: Option<u32>, admin: bool, now: u64) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session_at(
        uid,
        cid,
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
        now,
    );
    if admin {
        ctx.service_role_mask = service_roles::ROLE_GMH;
    }
    ctx
}

fn msg_with(field: u32, val: DynamicValue) -> DynamicQuasarMessage {
    let mut m = DynamicQuasarMessage::new("Req");
    m.fields.insert(field, val);
    m
}

fn sub_val(val: DynamicValue) -> DynamicValue {
    let mut m = DynamicQuasarMessage::new("Sub");
    m.fields.insert(1, val);
    DynamicValue::Message(Box::new(m))
}

#[test]
fn unauthenticated_and_admin_guards_across_payment_store_endpoints() {
    let (router, _db) = setup();
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Empty");
    let anon = auth(None, None, false, 1_700_000_000);
    let zero_uid = auth(Some(0), None, true, 1_700_000_000);
    let player = auth(Some(1), Some(90_000_001), false, 1_700_000_000);

    // User-auth required -> 401
    for ep in [
        "eve_public.plex.vault.api.BalanceRequest",
        "eve_public.store.targetaudience.offer.api.ListRequest",
        "eve_public.payment.schedule.api.GetRequest",
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        "eve_public.payment.token.api.GetQuickPayRequest",
        "eve_public.payment.token.api.GetRequest",
        "eve_public.payment.token.api.DisableAllRequest",
        "eve_public.payment.purchase.api.TokenRequest",
        "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
        "eve_public.plex.vault.transaction.api.GetLogRequest",
        "eve_public.plex.vault.transaction.api.GetStatisticsRequest",
    ] {
        assert_eq!(handlers[ep](&anon, &any, &empty).unwrap_err().0, STATUS_UNAUTHORIZED, "{ep} anon");
        assert_eq!(handlers[ep](&zero_uid, &any, &empty).unwrap_err().0, STATUS_UNAUTHORIZED, "{ep} uid=0");
    }

    // Admin required -> 403 for player, 401 for admin with uid=0
    for ep in [
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        "eve.fulfillment.api.DeliverItemRequest",
    ] {
        assert_eq!(handlers[ep](&player, &any, &empty).unwrap_err().0, STATUS_FORBIDDEN, "{ep} player");
        assert_eq!(handlers[ep](&zero_uid, &any, &empty).unwrap_err().0, STATUS_UNAUTHORIZED, "{ep} uid=0");
    }

    // Code redeem always returns 501 NOT_IMPLEMENTED
    assert_eq!(
        handlers["eve_public.payment.code.api.RedeemRequest"](&player, &any, &empty)
            .unwrap_err()
            .0,
        STATUS_NOT_IMPLEMENTED
    );
}

#[test]
fn vault_sint64_insufficient_balance_and_transaction_stats_edges() {
    let (router, db) = setup();
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let now = 1_700_000_000u64;
    let gm = auth(Some(1), Some(90_000_001), true, now);
    let user2 = auth(Some(2), Some(90_000_002), false, now);

    // Empty tx stats for user 1 -> count = 0, earliest = 0
    let stats_empty = handlers["eve_public.plex.vault.transaction.api.GetStatisticsRequest"](
        &gm,
        &any,
        &DynamicQuasarMessage::new("E"),
    )
    .unwrap();
    let wm_empty = WireMessage::decode(&stats_empty.value).unwrap();
    let entry_b = wm_empty.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let entry_wm = WireMessage::decode(entry_b).unwrap();
    assert_eq!(entry_wm.fields.iter().find(|f| f.field_number == 5).and_then(|f| f.value.as_varint()), Some(0));

    // Deposit via Sint64 in field 3 (+200 PLEX = 20_000 cents) and field 2 (+100 PLEX = 10_000 cents)
    db.with_mut_snapshot(|s| {
        s.account_plex_vault.insert(1, 0);
    });
    assert!(handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Sint64(20_000))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(2, sub_val(DynamicValue::Sint64(10_000))),
    )
    .is_ok());

    // Withdraw via Sint64 in field 3 (-150 PLEX) and field 2 (-50 PLEX)
    assert!(handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Sint64(15_000))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(2, sub_val(DynamicValue::Sint64(5_000))),
    )
    .is_ok());

    // Negative Sint64 withdraw -> 400
    assert_eq!(
        handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
            &gm,
            &any,
            &msg_with(2, sub_val(DynamicValue::Sint64(-1_000))),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Withdraw exceeding remaining balance (100 PLEX left, try 500 PLEX) -> 400 Insufficient PLEX balance
    let insuf = handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Int64(50_000))),
    )
    .unwrap_err();
    assert_eq!(insuf.0, STATUS_BAD_REQUEST);
    assert!(insuf.1.contains("Insufficient PLEX balance"));

    // Transaction log ownership & missing id guards
    assert_eq!(
        handlers["eve_public.plex.vault.transaction.api.GetLogRequest"](
            &gm,
            &any,
            &DynamicQuasarMessage::new("E"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let tx_list = handlers["eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest"](
        &gm,
        &any,
        &DynamicQuasarMessage::new("E"),
    )
    .unwrap();
    let tx_wm = WireMessage::decode(&tx_list.value).unwrap();
    assert_eq!(tx_wm.fields.len(), 4);
    let first_tx_id = WireMessage::decode(tx_wm.fields[0].value.as_bytes().unwrap())
        .unwrap()
        .fields[0]
        .value
        .as_varint()
        .unwrap();
    // User 2 cannot read User 1's transaction log
    assert_eq!(
        handlers["eve_public.plex.vault.transaction.api.GetLogRequest"](
            &user2,
            &any,
            &msg_with(1, DynamicValue::Uint64(first_tx_id)),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    // GetStatisticsRequest with both positive incomes (300) and negative expenses (200)
    let stats_pop = handlers["eve_public.plex.vault.transaction.api.GetStatisticsRequest"](
        &gm,
        &any,
        &DynamicQuasarMessage::new("E"),
    )
    .unwrap();
    let wm_pop = WireMessage::decode(&stats_pop.value).unwrap();
    let entry_pop = WireMessage::decode(wm_pop.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(
        entry_pop.fields.iter().find(|f| f.field_number == 5).and_then(|f| f.value.as_varint()),
        Some(4)
    );
}

#[test]
fn store_getters_schedules_tokens_and_disable_all_scoping() {
    let (router, db) = setup();
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let now = 1_700_000_000u64;
    let u1 = auth(Some(1), Some(90_000_001), false, now);
    let u2 = auth(Some(2), Some(90_000_002), false, now);

    db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Cat1".into(),
                    description: "D".into(),
                }],
                products: vec![StoreProduct {
                    id: 10,
                    name: "Prod10".into(),
                    description: "D".into(),
                    type_id: 34,
                    quantity: 1,
                }],
                offers: vec![
                    StoreOffer {
                        id: 501,
                        name: "TaggedOffer".into(),
                        description: "Desc".into(),
                        category_id: 1,
                        price_plex: 100,
                        product_ids: vec![10],
                        targeted: false,
                        price_cents: Some(1_000),
                        currency: Some("EUR".into()),
                    },
                    StoreOffer {
                        id: 502,
                        name: "PlexOnlyOffer".into(),
                        description: "Desc".into(),
                        category_id: 1,
                        price_plex: 50,
                        product_ids: vec![10],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                ],
            },
        );
        let mut tags = HashMap::new();
        tags.insert(501, vec!["featured".to_string(), "sale".to_string()]);
        save_store_offer_tags(snap, &tags);

        snap.quasar_commerce.payment_schedules = serde_json::from_str(&String::from(serde_json::to_string(&vec![PaymentScheduleRecord {
                id: 90,
                user_id: 1,
                schedule_type: 1,
                next_payment_unix: now + 86_400,
                offer_id: "VEVC-501".into(),
                interval_days: 30,
                created_unix: now,
            }])
            .unwrap())).unwrap();
        snap.quasar_commerce.payment_tokens = serde_json::from_str(&String::from(serde_json::to_string(&vec![
                PaymentTokenRecord {
                    id: 11,
                    user_id: 1,
                    token_str: "U1-QP1".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
                PaymentTokenRecord {
                    id: 12,
                    user_id: 1,
                    token_str: "U1-QP2".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
                PaymentTokenRecord {
                    id: 13,
                    user_id: 2,
                    token_str: "U2-QP".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
            ])
            .unwrap())).unwrap();
    });

    // Category / Product / Offer / TargetAudience missing / not found guards
    for ep in [
        "eve_public.store.category.api.GetRequest",
        "eve_public.store.product.api.GetRequest",
        "eve_public.store.offer.api.GetRequest",
        "eve_public.store.targetaudience.offer.api.GetRequest",
    ] {
        assert_eq!(handlers[ep](&u1, &any, &msg_with(1, DynamicValue::Uint32(0))).unwrap_err().0, STATUS_BAD_REQUEST);
        assert_eq!(handlers[ep](&u1, &any, &msg_with(1, DynamicValue::Uint32(999_999))).unwrap_err().0, STATUS_NOT_FOUND);
    }

    // Store offer tags appear in GetResponse attributes
    let off_rsp = handlers["eve_public.store.offer.api.GetRequest"](&u1, &any, &msg_with(1, DynamicValue::Uint32(501))).unwrap();
    let off_wm = WireMessage::decode(&off_rsp.value).unwrap();
    let attr_wm = WireMessage::decode(off_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let tag_count = attr_wm.fields.iter().filter(|f| f.field_number == 2).count();
    assert_eq!(tag_count, 2);

    // CostRequest error guards: missing -> 400, not found -> 404, price_cents None -> 501
    assert_eq!(handlers["eve_public.payment.purchase.api.CostRequest"](&u1, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.payment.purchase.api.CostRequest"](&u1, &any, &msg_with(1, DynamicValue::Uint32(999_999))).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.payment.purchase.api.CostRequest"](&u1, &any, &msg_with(1, DynamicValue::Uint32(502))).unwrap_err().0, STATUS_NOT_IMPLEMENTED);

    // Schedule GetRequest & Token GetRequest cross-user isolation + missing ID
    assert_eq!(handlers["eve_public.payment.schedule.api.GetRequest"](&u1, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.payment.schedule.api.GetRequest"](&u2, &any, &msg_with(1, DynamicValue::Uint64(90))).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.payment.token.api.GetRequest"](&u1, &any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.payment.token.api.GetRequest"](&u2, &any, &msg_with(1, DynamicValue::Uint32(11))).unwrap_err().0, STATUS_NOT_FOUND);

    // DisableAllRequest for u1 disables tokens 11 & 12, emits 2 notices, leaves u2's token 13 untouched!
    router.pending_notices.write().unwrap().clear();
    assert!(handlers["eve_public.payment.token.api.DisableAllRequest"](&u1, &any, &DynamicQuasarMessage::new("E")).is_ok());
    assert_eq!(router.pending_notices.read().unwrap().len(), 2);
    // Second call for u1 emits 0 notices
    router.pending_notices.write().unwrap().clear();
    assert!(handlers["eve_public.payment.token.api.DisableAllRequest"](&u1, &any, &DynamicQuasarMessage::new("E")).is_ok());
    assert_eq!(router.pending_notices.read().unwrap().len(), 0);
    // u2 still has token 13 in quick-pay
    let u2_qp = handlers["eve_public.payment.token.api.GetQuickPayRequest"](&u2, &any, &DynamicQuasarMessage::new("E")).unwrap();
    assert_eq!(WireMessage::decode(&u2_qp.value).unwrap().fields.len(), 1);
}

#[test]
fn fulfillment_and_purchase_nested_order_omega_and_raw_fallback_edges() {
    let (router, db) = setup();
    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let now = 1_700_000_000u64;
    let cid_no_loc = 90_777_001i64;
    let cid_ok = 90_000_001i64;

    db.with_mut_snapshot(|snap| {
        let mut ch_bad = snap.characters[&cid_ok].clone();
        ch_bad.character_id = cid_no_loc;
        ch_bad.user_id = 55;
        ch_bad.station_id = None;
        ch_bad.structure_id = None;
        ch_bad.home_station_id = 0;
        snap.characters.insert(cid_no_loc, ch_bad);

        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![
                    StoreCategory { id: 1, name: "Omega".into(), description: "O".into() },
                    StoreCategory { id: 2, name: "Services".into(), description: "S".into() },
                ],
                products: vec![StoreProduct {
                    id: 101,
                    name: "Trit".into(),
                    description: "T".into(),
                    type_id: 34,
                    quantity: 2,
                }],
                offers: vec![
                    StoreOffer {
                        id: 1001,
                        name: "30 Days Omega".into(),
                        description: "Omega".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: vec![101],
                        targeted: false,
                        price_cents: Some(1999),
                        currency: Some("USD".into()),
                    },
                    StoreOffer {
                        id: 2001,
                        name: "Trit Offer".into(),
                        description: "Trit".into(),
                        category_id: 2,
                        price_plex: 50,
                        product_ids: vec![101],
                        targeted: false,
                        price_cents: Some(499),
                        currency: Some("USD".into()),
                    },
                ],
            },
        );
        snap.quasar_commerce.payment_tokens = serde_json::from_str(&String::from(serde_json::to_string(&vec![
                PaymentTokenRecord { id: 1, user_id: 1, token_str: "T1".into(), is_quick_pay: true, consumed: false },
                PaymentTokenRecord { id: 2, user_id: 1, token_str: "T2".into(), is_quick_pay: false, consumed: false },
                PaymentTokenRecord { id: 3, user_id: 1, token_str: "T3".into(), is_quick_pay: true, consumed: false },
                PaymentTokenRecord { id: 4, user_id: 55, token_str: "T4".into(), is_quick_pay: true, consumed: false },
            ])
            .unwrap())).unwrap();
    });

    // DeliverItemRequest: character with no valid station -> 400; default type_id/qty with explicit field 1 cid -> 200
    let gm = auth(Some(1), Some(cid_ok as u32), true, now);
    let deliv_no_sta = msg_with(1, sub_val(DynamicValue::Int64(cid_no_loc)));
    assert_eq!(
        handlers["eve.fulfillment.api.DeliverItemRequest"](&gm, &any, &deliv_no_sta)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let deliv_default = msg_with(1, DynamicValue::Uint64(cid_ok as u64));
    assert!(handlers["eve.fulfillment.api.DeliverItemRequest"](&gm, &any, &deliv_default).is_ok());

    // TokenRequest error guards: missing token id -> 400, non-quick-pay token -> 400, missing offer -> 404, missing token -> 404, no valid station -> 400
    let u1 = auth(Some(1), Some(cid_ok as u32), false, now);
    let u55 = auth(Some(55), Some(cid_no_loc as u32), false, now);
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&u1, &any, &msg_with(1, DynamicValue::Uint32(1001)))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut non_qp = msg_with(1, DynamicValue::Uint32(2001));
    non_qp.fields.insert(2, DynamicValue::Uint32(2));
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&u1, &any, &non_qp)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut missing_off = msg_with(1, DynamicValue::Uint32(999_999));
    missing_off.fields.insert(2, DynamicValue::Uint32(1));
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&u1, &any, &missing_off)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    let mut missing_tok = msg_with(1, DynamicValue::Uint32(1001));
    missing_tok.fields.insert(2, DynamicValue::Uint32(999_999));
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&u1, &any, &missing_tok)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    let mut no_sta_buy = msg_with(1, DynamicValue::Uint32(2001));
    no_sta_buy.fields.insert(2, DynamicValue::Uint32(4));
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&u55, &any, &no_sta_buy)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Nested order { offer { id: "VEVC-1001" }, qty: 2 } for Omega offer creates schedule + Omega notice + skips item delivery
    let mut offer_inner = DynamicQuasarMessage::new("OfferId");
    offer_inner.fields.insert(1, DynamicValue::String("VEVC-1001".into()));
    let mut order_msg = DynamicQuasarMessage::new("Order");
    order_msg.fields.insert(1, DynamicValue::Message(Box::new(offer_inner)));
    order_msg.fields.insert(2, DynamicValue::Uint32(2));
    let mut omega_req = DynamicQuasarMessage::new("TokenReq");
    omega_req.fields.insert(1, DynamicValue::Message(Box::new(order_msg)));
    omega_req.fields.insert(2, sub_val(DynamicValue::Uint32(1)));

    router.pending_notices.write().unwrap().clear();
    let omega_rsp = handlers["eve_public.payment.purchase.api.TokenRequest"](&u1, &any, &omega_req).unwrap();
    assert!(!omega_rsp.value.is_empty());
    // Omega ActivatedNotice + Schedule SetNotice pushed
    assert!(router.pending_notices.read().unwrap().len() >= 2);
    let sched_exists = db.with_snapshot(|s| !s.quasar_commerce.payment_schedules.is_empty());
    assert!(sched_exists);

    // Raw protobuf bytes fallback when dyn_msg is empty (field 1 = offer 2001, field 2 = token 3, field 3 = qty 2)
    let mut raw_bytes = Vec::new();
    encode_varint_field(1, 2001, &mut raw_bytes);
    encode_varint_field(2, 3, &mut raw_bytes);
    encode_varint_field(3, 2, &mut raw_bytes);
    let raw_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.purchase.api.TokenRequest"),
        value: raw_bytes,
    };
    let raw_rsp = handlers["eve_public.payment.purchase.api.TokenRequest"](
        &u1,
        &raw_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!raw_rsp.value.is_empty());
    let receipts_count = db.with_snapshot(|s| {
        s.quasar_commerce.payment_receipts.len()
    });
    assert_eq!(receipts_count, 2);
}
