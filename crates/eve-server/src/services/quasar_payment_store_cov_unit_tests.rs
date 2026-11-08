//! Comprehensive unit tests for `quasar_payment_store` covering raw varint/ID parsers, SDE catalog
//! hydration boundaries, DynamicValue integer/nested variants, and purchase/fulfillment edge cases.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED,
};
use eve_db::WorldRepository;
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::{SdeRegistry, SkinLicenseInfo};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router_and_db() -> (PublicGatewayRouter, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    (router, db)
}

fn auth(uid: Option<i32>, cid: Option<u32>, admin: bool, now: u64) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session_at(uid, cid, Some(1_000_009), 0, "tranquility", vec![1], now);
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
fn raw_varint_parser_id_extractor_and_sde_catalog_hydration_boundaries() {
    // Multi-byte varint (>= 128), nested wire-type 2 submessage, truncated wire-type 2, and unknown wire-type 5 break
    let mut sub = Vec::new();
    encode_varint_field(1, 300, &mut sub); // 2-byte varint
    let mut raw = Vec::new();
    encode_bytes_field(2, &sub, &mut raw);
    encode_varint_field(4, 20_000, &mut raw);
    raw.push((7 << 3) | 5); // wire_type 5 -> breaks loop cleanly
    raw.extend_from_slice(&[1, 2, 3, 4]);
    let parsed = parse_raw_varints(&raw);
    assert_eq!(parsed.get(&1), Some(&300));
    assert_eq!(parsed.get(&2), Some(&300));
    assert_eq!(parsed.get(&4), Some(&20_000));
    assert!(!parsed.contains_key(&7));

    // Truncated length-delimited field does not panic
    let trunc = vec![(1 << 3) | 2, 10, 1, 2];
    assert!(parse_raw_varints(&trunc).is_empty());

    // extract_u64_id: scalar & nested Uint64/Int64/Uint32/Int32 + id==0 raw fallback
    let empty_raw = Vec::new();
    for v in [
        DynamicValue::Uint64(41),
        DynamicValue::Int64(41),
        DynamicValue::Uint32(41),
        DynamicValue::Int32(41),
        sub_val(DynamicValue::Uint64(41)),
        sub_val(DynamicValue::Int64(41)),
        sub_val(DynamicValue::Uint32(41)),
        sub_val(DynamicValue::Int32(41)),
    ] {
        assert_eq!(extract_u64_id(&empty_raw, &msg_with(1, v)), Some(41));
    }
    let mut fallback_raw = Vec::new();
    encode_varint_field(1, 999, &mut fallback_raw);
    assert_eq!(extract_u64_id(&fallback_raw, &msg_with(1, DynamicValue::Uint64(0))), Some(999));
    assert_eq!(extract_u64_id(&empty_raw, &msg_with(1, DynamicValue::Bool(true))), None);

    // hydrate_store_catalog_from_sde boundary: `skin_licenses.len() == 20` is a no-op; `25` hydrates 28 offers!
    let mut sde_20 = (*shared_sde()).clone();
    sde_20.skin_licenses.clear();
    let valid_tids: Vec<u32> = sde_20.types.keys().copied().take(25).collect();
    for &tid in &valid_tids[..20] {
        sde_20.skin_licenses.insert(
            tid,
            SkinLicenseInfo {
                license_type_id: tid,
                skin_id: tid,
                duration: -1,
            },
        );
    }
    let db_20 = WorldRepository::new_empty();
    hydrate_store_catalog_from_sde(&db_20, &sde_20);
    assert!(db_20.with_snapshot(|s| s.quasar_commerce.nes_store_catalog.is_none()));

    let mut sde_25 = sde_20;
    for &tid in &valid_tids[20..25] {
        sde_25.skin_licenses.insert(
            tid,
            SkinLicenseInfo {
                license_type_id: tid,
                skin_id: tid,
                duration: -1,
            },
        );
    }
    hydrate_store_catalog_from_sde(&db_20, &sde_25);
    db_20.with_snapshot(|s| {
        let cat = read_store_catalog(s);
        assert_eq!(cat.categories.len(), 3);
        assert_eq!(cat.products.len(), 28);
        assert_eq!(cat.offers.len(), 28);
    });
    // Idempotency: second call does not overwrite custom catalog
    db_20.with_mut_snapshot(|s| {
        save_store_catalog(
            s,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 9,
                    name: "Custom".into(),
                    description: "Custom".into(),
                }],
                products: Vec::new(),
                offers: Vec::new(),
            },
        );
    });
    hydrate_store_catalog_from_sde(&db_20, &sde_25);
    assert_eq!(db_20.with_snapshot(|s| read_store_catalog(s).categories.len()), 1);
}

#[test]
fn vault_deposit_withdraw_variants_and_store_catalog_token_schedule_getters() {
    let (router, db) = setup_router_and_db();
    db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Sub".into(),
                }],
                products: vec![StoreProduct {
                    id: 101,
                    name: "Item".into(),
                    description: "Desc".into(),
                    type_id: 34,
                    quantity: 2,
                }],
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "Deal".into(),
                    description: "DealDesc".into(),
                    category_id: 1,
                    price_plex: 100,
                    product_ids: vec![101],
                    targeted: true,
                    price_cents: Some(999),
                    currency: None, // exercises `.unwrap_or("USD")`
                }],
            },
        );
        snap.quasar_commerce.payment_schedules = vec![PaymentScheduleRecord {
            id: 77,
            user_id: 1,
            schedule_type: 2,
            next_payment_unix: 2_000_000,
            offer_id: "VEVC-1001".into(),
            interval_days: 30,
            created_unix: 1_700_000_000,
        }];
        snap.quasar_commerce.payment_tokens = vec![PaymentTokenRecord {
            id: 55,
            user_id: 1,
            token_str: "4111-XXXX".into(),
            is_quick_pay: true,
            consumed: false,
        }];
    });

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let gm = auth(Some(1), Some(90_000_001), true, 1_700_000_000);

    // DepositRequest & WithdrawRequest: Uint64 in field 3 & field 2, empty sub-message defaults (100 / 50), negative (400), and overflow (400)
    assert!(handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Uint64(2_500))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(2, sub_val(DynamicValue::Uint64(1_500))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(3, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Empty")))),
    )
    .is_ok());
    assert_eq!(
        handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
            &gm,
            &any,
            &msg_with(2, sub_val(DynamicValue::Int64(-500))),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    assert!(handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Uint64(1_000))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(2, sub_val(DynamicValue::Uint64(1_000))),
    )
    .is_ok());
    assert!(handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
        &gm,
        &any,
        &msg_with(2, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Empty")))),
    )
    .is_ok());
    assert_eq!(
        handlers["eve.plex.vault.assetholding.operation.api.WithdrawRequest"](
            &gm,
            &any,
            &msg_with(3, sub_val(DynamicValue::Int64(0))),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // PLEX overflow on DepositRequest (`checked_add` returns None -> STATUS_BAD_REQUEST)
    db.with_mut_snapshot(|s| { s.account_plex_vault.insert(1, i64::MAX - 5); });
    let ov_err = handlers["eve.plex.vault.assetholding.operation.api.DepositRequest"](
        &gm,
        &any,
        &msg_with(3, sub_val(DynamicValue::Int64(1_000))),
    )
    .unwrap_err();
    assert_eq!(ov_err.0, STATUS_BAD_REQUEST);
    db.with_mut_snapshot(|s| { s.account_plex_vault.insert(1, 500); });

    // Store Category / Product / Offer / TargetAudience GetRequest scalar & nested variants
    for v in [DynamicValue::Uint32(1), DynamicValue::Int32(1), sub_val(DynamicValue::Int32(1))] {
        assert!(handlers["eve_public.store.category.api.GetRequest"](&gm, &any, &msg_with(1, v)).is_ok());
    }
    for v in [DynamicValue::Uint32(101), DynamicValue::Int32(101), sub_val(DynamicValue::Int32(101))] {
        assert!(handlers["eve_public.store.product.api.GetRequest"](&gm, &any, &msg_with(1, v)).is_ok());
    }
    for v in [
        DynamicValue::Uint32(1001),
        DynamicValue::Int32(1001),
        DynamicValue::String("1001".into()),
        sub_val(DynamicValue::Uint32(1001)),
    ] {
        assert!(handlers["eve_public.store.offer.api.GetRequest"](&gm, &any, &msg_with(1, v.clone())).is_ok());
        assert!(handlers["eve_public.store.targetaudience.offer.api.GetRequest"](&gm, &any, &msg_with(1, v)).is_ok());
    }
    // Nested field 2 -> Message(field 1 -> String("1001")) on targetaudience.offer.api.GetRequest
    let mut outer_ta = DynamicQuasarMessage::new("Outer");
    outer_ta.fields.insert(2, sub_val(DynamicValue::String("1001".into())));
    assert!(handlers["eve_public.store.targetaudience.offer.api.GetRequest"](
        &gm,
        &any,
        &msg_with(1, DynamicValue::Message(Box::new(outer_ta))),
    )
    .is_ok());

    // GetActiveByTypeRequest: default field 1 (1 -> 404) vs Enum(2) -> 200 OK
    assert_eq!(
        handlers["eve_public.payment.schedule.api.GetActiveByTypeRequest"](
            &gm,
            &any,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert!(handlers["eve_public.payment.schedule.api.GetActiveByTypeRequest"](
        &gm,
        &any,
        &msg_with(1, DynamicValue::Enum(2)),
    )
    .is_ok());

    // payment.token.api.GetRequest scalar & nested integer variants
    for v in [
        DynamicValue::Uint32(55),
        DynamicValue::Int32(55),
        DynamicValue::Uint64(55),
        DynamicValue::Int64(55),
        sub_val(DynamicValue::Uint64(55)),
        sub_val(DynamicValue::Int64(55)),
    ] {
        assert!(handlers["eve_public.payment.token.api.GetRequest"](&gm, &any, &msg_with(1, v)).is_ok());
    }

    // CostRequest with `"VEVC-1001"` prefix stripping + `currency: None` -> `"USD"`
    let cost_ok = handlers["eve_public.payment.purchase.api.CostRequest"](
        &gm,
        &any,
        &msg_with(1, DynamicValue::String("VEVC-1001".into())),
    )
    .unwrap();
    assert!(!cost_ok.value.is_empty());
    assert!(handlers["eve_public.payment.purchase.api.CostRequest"](
        &gm,
        &any,
        &msg_with(1, sub_val(DynamicValue::String("VEVC-1001".into()))),
    )
    .is_ok());
}

#[test]
fn fulfillment_and_token_purchase_station_fallbacks_stacking_and_error_guards() {
    let (router, db) = setup_router_and_db();
    let cid = 90_000_001_i64;
    let home_sta = 60_003_760_i64;

    db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.user_id = 1;
        ch.station_id = None;
        ch.structure_id = None;
        ch.home_station_id = home_sta;

        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 2,
                    name: "SKINs".into(),
                    description: "SKINs".into(),
                }],
                products: vec![
                    StoreProduct {
                        id: 201,
                        name: "TritPack".into(),
                        description: "Pack".into(),
                        type_id: 34,
                        quantity: 5,
                    },
                    StoreProduct {
                        id: 202,
                        name: "HugePack".into(),
                        description: "Overflow".into(),
                        type_id: 34,
                        quantity: u32::MAX,
                    },
                ],
                offers: vec![
                    StoreOffer {
                        id: 2001,
                        name: "ItemOffer".into(),
                        description: "Desc".into(),
                        category_id: 2,
                        price_plex: 50,
                        product_ids: vec![201],
                        targeted: false,
                        price_cents: Some(499),
                        currency: None,
                    },
                    StoreOffer {
                        id: 2002,
                        name: "NegPlex".into(),
                        description: "Desc".into(),
                        category_id: 2,
                        price_plex: -10,
                        product_ids: vec![201],
                        targeted: false,
                        price_cents: Some(499),
                        currency: Some("USD".into()),
                    },
                    StoreOffer {
                        id: 2003,
                        name: "NoCash".into(),
                        description: "Desc".into(),
                        category_id: 2,
                        price_plex: 50,
                        product_ids: vec![201],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                    StoreOffer {
                        id: 2004,
                        name: "OverflowOffer".into(),
                        description: "Desc".into(),
                        category_id: 2,
                        price_plex: 50,
                        product_ids: vec![202],
                        targeted: false,
                        price_cents: Some(499),
                        currency: Some("USD".into()),
                    },
                ],
            },
        );
        snap.quasar_commerce.payment_tokens = vec![
            PaymentTokenRecord {
                id: 1,
                user_id: 1,
                token_str: "T1".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 2,
                user_id: 1,
                token_str: "T2".into(),
                is_quick_pay: true,
                consumed: true, // already consumed while quick_pay == true!
            },
            PaymentTokenRecord {
                id: 3,
                user_id: 1,
                token_str: "T3".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 4,
                user_id: 99,
                token_str: "T4".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
    });

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let gm_no_char = auth(Some(1), None, true, 1_700_000_000);
    let gm_orphan_user = auth(Some(888), None, true, 1_700_000_000);

    // DeliverItemRequest: home_station_id fallback + stacking on second delivery + orphan user 404 + qty <= 0 (400)
    let mut deliv = DynamicQuasarMessage::new("Deliv");
    deliv.fields.insert(2, sub_val(DynamicValue::Uint32(34)));
    deliv.fields.insert(3, DynamicValue::Int32(10));
    assert!(handlers["eve.fulfillment.api.DeliverItemRequest"](&gm_no_char, &any, &deliv).is_ok());
    assert!(handlers["eve.fulfillment.api.DeliverItemRequest"](&gm_no_char, &any, &deliv).is_ok());

    assert_eq!(
        handlers["eve.fulfillment.api.DeliverItemRequest"](&gm_orphan_user, &any, &deliv)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    let mut bad_qty = DynamicQuasarMessage::new("Deliv");
    bad_qty.fields.insert(3, DynamicValue::Int32(0));
    assert_eq!(
        handlers["eve.fulfillment.api.DeliverItemRequest"](&gm_no_char, &any, &bad_qty)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // TokenRequest error guards: missing offer/token, negative PLEX, missing price_cents, consumed token, foreign user token, quantity overflow
    let user_no_char = auth(Some(1), None, false, 1_700_000_100);
    let mk_tok_req = |off: u32, tok: u32, qty: u32| {
        let mut m = DynamicQuasarMessage::new("TokReq");
        m.fields.insert(1, DynamicValue::Uint32(off));
        m.fields.insert(2, DynamicValue::Uint32(tok));
        m.fields.insert(3, DynamicValue::Uint32(qty));
        m
    };

    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](
            &user_no_char,
            &any,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&user_no_char, &any, &mk_tok_req(2002, 1, 1))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&user_no_char, &any, &mk_tok_req(2003, 1, 1))
            .unwrap_err()
            .0,
        STATUS_NOT_IMPLEMENTED
    );
    let consumed_err = handlers["eve_public.payment.purchase.api.TokenRequest"](
        &user_no_char,
        &any,
        &mk_tok_req(2001, 2, 1),
    )
    .unwrap_err();
    assert_eq!(consumed_err.0, STATUS_BAD_REQUEST);
    assert!(consumed_err.1.contains("already consumed"));

    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&user_no_char, &any, &mk_tok_req(2001, 4, 1))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&user_no_char, &any, &mk_tok_req(2004, 1, 2))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Valid item offer purchase via `active_char_id == None` (`user_chars.first()`) + `home_station_id` + hangar stacking!
    assert!(handlers["eve_public.payment.purchase.api.TokenRequest"](
        &user_no_char,
        &any,
        &mk_tok_req(2001, 1, 2),
    )
    .is_ok());

    // Wrong active_char_id (foreign char vs missing char)
    let wrong_char = auth(Some(99), Some(cid as u32), false, 1_700_000_200);
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&wrong_char, &any, &mk_tok_req(2001, 4, 1))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let ghost_char = auth(Some(1), Some(77_777_777), false, 1_700_000_200);
    assert_eq!(
        handlers["eve_public.payment.purchase.api.TokenRequest"](&ghost_char, &any, &mk_tok_req(2001, 3, 1))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
}
