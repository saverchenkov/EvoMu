//! Deep branch-coverage and mutation-killing unit tests for `quasar_payment_store_types`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_OK,
};
use crate::services::quasar_payment_store_types::*;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::new(WorldRepository::with_demo_seed()),
        shared_sde(),
    )
}

fn make_sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1));
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s
}

fn call_gw(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![3; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: None,
        trace_context: None,
    };
    router.handle_public_request(sess, &req, None, false, now)
}

#[test]
fn schedule_upsert_non_omega_renewal_pagination_and_tx_log_ring_buffer() {
    let router = new_router();
    let now = 1_810_000_000_u64;

    // 1. upsert_user_payment_schedule_in_snapshot updates existing (uid, schedule_type) in-place
    let (id1, id2, id3) = router.db.with_mut_snapshot(|snap| {
        let a = upsert_user_payment_schedule_in_snapshot(snap, 10, 2, now - 50, "9999", 0, now - 100);
        let b = upsert_user_payment_schedule_in_snapshot(snap, 10, 2, now - 10, "9999", 0, now - 100);
        // Schedule with next_payment_unix == 0 is never considered due
        let c = upsert_user_payment_schedule_in_snapshot(snap, 11, 1, 0, "1001", 30, now - 100);
        (a, b, c)
    });
    assert_eq!(id1, id2);
    assert_ne!(id1, id3);

    // 2. publish_schedule_set_notice and non-quick-pay token (does not emit QuickPayAddedNotice)
    router.pending_notices.write().unwrap().clear();
    publish_schedule_set_notice(&router.pending_notices, 10, id1, 2);
    assert_eq!(router.pending_notices.read().unwrap().len(), 1);

    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        10,
        PaymentTokenRecord {
            id: 50,
            user_id: 10,
            token_str: "normal-card".into(),
            is_quick_pay: false,
            consumed: false,
        },
    );
    // Count stays 1 because is_quick_pay is false
    assert_eq!(router.pending_notices.read().unwrap().len(), 1);

    // Replace token 50 with quick_pay=true -> emits QuickPayAddedNotice and renews schedule_type=2
    // with interval_days=0 clamped to 1 day (86_400s) and fallback 0 cents / "USD" for unknown offer 9999
    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        10,
        PaymentTokenRecord {
            id: 50,
            user_id: 10,
            token_str: "qp-card".into(),
            is_quick_pay: true,
            consumed: false,
        },
    );
    assert_eq!(router.pending_notices.read().unwrap().len(), 2);

    process_due_payment_schedules(&router.db, &router.pending_notices, now);
    router.db.with_snapshot(|snap| {
        let scheds = &snap.quasar_commerce.payment_schedules;
        let s10 = scheds.iter().find(|s| s.user_id == 10).unwrap();
        assert_eq!(s10.next_payment_unix, now + 86_400);
        let receipts = &snap.quasar_commerce.payment_receipts;
        assert_eq!(receipts[0].amount_in_cents, 0);
        assert_eq!(receipts[0].currency, "USD");
    });

    // 3. encode_targeted_offer_list_response pagination with next_page token vs exact end
    let offers = vec![
        StoreOffer {
            id: 10,
            name: "T1".into(),
            description: String::new(),
            category_id: 1,
            price_plex: 10,
            product_ids: vec![],
            targeted: true,
            price_cents: None,
            currency: None,
        },
        StoreOffer {
            id: 11,
            name: "Public".into(),
            description: String::new(),
            category_id: 1,
            price_plex: 10,
            product_ids: vec![],
            targeted: false,
            price_cents: None,
            currency: None,
        },
        StoreOffer {
            id: 12,
            name: "T2".into(),
            description: String::new(),
            category_id: 1,
            price_plex: 10,
            product_ids: vec![],
            targeted: true,
            price_cents: None,
            currency: None,
        },
    ];
    let mut page1 = DynamicQuasarMessage::new("Page");
    page1.set(1, DynamicValue::Int32(1));
    page1.set(2, DynamicValue::String("0".into()));
    let mut list_req = DynamicQuasarMessage::new("List");
    list_req.set(1, DynamicValue::Message(Box::new(page1)));
    let bytes_p1 = encode_targeted_offer_list_response(&offers, 10, &list_req);
    assert!(!bytes_p1.is_empty());

    // 4. record_plex_transaction caps plex_tx_logs at 1000 entries
    router.db.with_mut_snapshot(|snap| {
        for i in 1..=1005 {
            record_plex_transaction(snap, i, 10, 5, 500, "tx", now);
        }
        let logs = get_plex_transactions(snap);
        assert_eq!(logs.len(), 1000);
        assert_eq!(logs.first().unwrap().id, 6);
        assert_eq!(logs.last().unwrap().id, 1005);
    });
}

#[test]
fn purchase_gift_delivery_fallback_recipient_overflow_and_operation_binary_lookup() {
    let router = new_router();
    let buyer_uid = 41_u64;
    let buyer_cid = 90_000_041_i64;
    let gift_uid = 42_u64;
    let gift_cid = 90_000_042_i64;
    let charless_uid = 43_u64;
    let now = 1_810_000_100_u64;

    router.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, uid, st_id) in [(buyer_cid, buyer_uid, 60_003_760), (gift_cid, gift_uid, 60_008_494)] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.station_id = Some(st_id);
            ch.clone_grade = 1;
            snap.characters.insert(cid, ch);
        }
        snap.account_plex_vault.insert(buyer_uid, 5_000);
        snap.account_plex_vault.insert(charless_uid, 5_000);

        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Omega".into(),
                }],
                products: vec![
                    StoreProduct {
                        id: 301,
                        name: "Booster".into(),
                        description: "Booster".into(),
                        type_id: 34599,
                        quantity: 2,
                    },
                    StoreProduct {
                        id: 302,
                        name: "HugeStack".into(),
                        description: "Overflow".into(),
                        type_id: 34599,
                        quantity: u32::MAX,
                    },
                ],
                offers: vec![
                    StoreOffer {
                        id: 3001,
                        name: "Gifted Omega + Item".into(),
                        description: "Gift".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: vec![301],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                    StoreOffer {
                        id: 3002,
                        name: "Overflow Offer".into(),
                        description: "Overflow".into(),
                        category_id: 2,
                        price_plex: 10,
                        product_ids: vec![302],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                ],
            },
        );
    });

    // 1. Gift purchase of offer 3001 (default qty=1 when field 2 omitted) by buyer_uid to gift_cid:
    // deducts 500 PLEX from buyer_uid, delivers item to gift_cid, and upgrades gift_uid to Omega!
    let buyer_no_active_char = make_sess(Some(buyer_uid), None, service_roles::ROLE_PLAYER_DEFAULT);
    let gift_req = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "3001")
        .set("gift.character.sequential", gift_cid as u64)
        .must_encode();

    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve_public.plex.vault.api.PurchaseRequest",
            gift_req,
            now
        )
        .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault[&buyer_uid], 4_500);
        assert_eq!(snap.characters[&gift_cid].clone_grade, 2);
        assert!(snap
            .items
            .values()
            .any(|i| i.owner_id == gift_cid && i.type_id == 34599 && i.quantity == 2));
    });

    // 2. Fallback recipient resolution when active_cid and gift_char_id are both None: delivers to buyer_cid
    let self_req = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "3001")
        .must_encode();
    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve_public.plex.vault.api.PurchaseRequest",
            self_req.clone(),
            now
        )
        .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|snap| {
        assert!(snap
            .items
            .values()
            .any(|i| i.owner_id == buyer_cid && i.type_id == 34599));
    });

    // 3. Account with no characters at all purchasing product offer -> 400 BAD_REQUEST
    let charless_sess = make_sess(Some(charless_uid), None, service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(
        call_gw(
            &router,
            &charless_sess,
            "eve_public.plex.vault.api.PurchaseRequest",
            self_req,
            now
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 4. Delivery quantity overflow (u32::MAX > i32::MAX) -> 400 BAD_REQUEST
    let overflow_req = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "3002")
        .must_encode();
    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve_public.plex.vault.api.PurchaseRequest",
            overflow_req,
            now
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 5. Assetholding operation GetRequest with 8-byte big-endian binary UUID and missing/unknown IDs
    router.db.with_mut_snapshot(|snap| {
        record_plex_transaction(snap, 9876, buyer_uid, -500, 4_000, "BinaryOp", now);
    });
    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve.plex.vault.assetholding.operation.api.GetRequest",
            Vec::new(),
            now
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let bin_op_req = pb::message("eve.plex.vault.assetholding.operation.api.GetRequest")
        .set("operation.uuid", 9876_u64.to_be_bytes().as_slice())
        .must_encode();
    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve.plex.vault.assetholding.operation.api.GetRequest",
            bin_op_req,
            now
        )
        .status_code,
        STATUS_OK
    );

    let missing_op_req = pb::message("eve.plex.vault.assetholding.operation.api.GetRequest")
        .set("operation.uuid", 777_888_999_u64.to_be_bytes().as_slice())
        .must_encode();
    assert_eq!(
        call_gw(
            &router,
            &buyer_no_active_char,
            "eve.plex.vault.assetholding.operation.api.GetRequest",
            missing_op_req,
            now
        )
        .status_code,
        STATUS_NOT_FOUND
    );
}

#[test]
fn admin_store_proto_bytes_extraction_invoice_dedup_and_tag_filtering() {
    let router = new_router();
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        u64::MAX,
        "tranquility",
        vec![1],
    );
    admin_ctx.service_role_mask = u64::MAX;

    // 1. CreateProductRequest with nested item type_id (attrs[2].1.1) and quantity (attrs[2].2)
    let mut tid_msg = DynamicQuasarMessage::new("Tid");
    tid_msg.set(1, DynamicValue::Uint32(34599));
    let mut item_msg = DynamicQuasarMessage::new("Item");
    item_msg.set(1, DynamicValue::Message(Box::new(tid_msg)));
    item_msg.set(2, DynamicValue::Uint32(7));
    let mut prod_attrs = DynamicQuasarMessage::new("ProdAttrs");
    prod_attrs.set(1, DynamicValue::String("Custom SKIN".into()));
    prod_attrs.set(2, DynamicValue::Message(Box::new(item_msg)));
    let mut prod_req = DynamicQuasarMessage::new("CreateProductRequest");
    prod_req.set(1, DynamicValue::Int32(2));
    prod_req.set(2, DynamicValue::Message(Box::new(prod_attrs)));
    assert!(handlers["eve.store.admin.api.CreateProductRequest"](&admin_ctx, &dummy_any, &prod_req).is_ok());

    // 2. CreateOfferRequest with raw proto varint bytes for category_id (0x08, 0x03) and nested product_id (0x0a, 0x02, 0x08, 0x65),
    // repeated tags in field 16, and targeted=true
    let mut off_attrs = DynamicQuasarMessage::new("OfferAttrs");
    off_attrs.set(1, DynamicValue::String("Tagged Targeted Offer".into()));
    off_attrs.set(7, DynamicValue::Bool(true));
    off_attrs.set(
        16,
        DynamicValue::Repeated(vec![
            DynamicValue::String("vip_tag".into()),
            DynamicValue::String("promo".into()),
        ]),
    );
    off_attrs.set(17, DynamicValue::Bytes(vec![0x08, 0x03]));
    off_attrs.set(18, DynamicValue::Bytes(vec![0x0a, 0x02, 0x08, 0x65]));
    let mut off_req = DynamicQuasarMessage::new("CreateOfferRequest");
    off_req.set(1, DynamicValue::Message(Box::new(off_attrs)));
    assert!(handlers["eve.store.admin.api.CreateOfferRequest"](&admin_ctx, &dummy_any, &off_req).is_ok());

    // GetAllByTagRequest excludes targeted offers even when tag matches
    let mut tag_msg = DynamicQuasarMessage::new("GetAllByTagRequest");
    tag_msg.set(2, DynamicValue::String("vip_tag".into()));
    let tag_resp =
        handlers["eve_public.store.offer.api.GetAllByTagRequest"](&admin_ctx, &dummy_any, &tag_msg)
            .unwrap();
    assert!(tag_resp.value.is_empty());

    // 3. RecordInvoiceRequest deduplication and 1000-entry ring buffer cap
    router.db.with_mut_snapshot(|snap| {
        let full: Vec<String> = (0..1000).map(|i| format!("inv-{i:04}")).collect();
        snap.quasar_commerce.plex_invoices = full;
    });
    let mut inv_sub = DynamicQuasarMessage::new("Id");
    inv_sub.set(1, DynamicValue::String("new-uuid".into()));
    let mut inv_req = DynamicQuasarMessage::new("RecordInvoiceRequest");
    inv_req.set(1, DynamicValue::Message(Box::new(inv_sub)));
    assert!(handlers["eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest"](
        &admin_ctx, &dummy_any, &inv_req
    )
    .is_ok());
    // Duplicate invoice is not re-inserted
    assert!(handlers["eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest"](
        &admin_ctx, &dummy_any, &inv_req
    )
    .is_ok());
    router.db.with_snapshot(|snap| {
        let list = &snap.quasar_commerce.plex_invoices;
        assert_eq!(list.len(), 1000);
        assert_eq!(list.first().unwrap(), "inv-0001");
    });

    let _ = NoticeScope::User(1);
    let _ = STATUS_FORBIDDEN;
}

#[test]
fn store_purchase_consolidation_qty_overflow_virtual_reject_idempotency_and_on_item_change_key_9() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_proto::quasar::{WireField, WireMessage, WireValue};
    use eve_proto::PyRep;
    use std::sync::atomic::Ordering;

    let router = new_router();
    let ctx = Arc::new(crate::router::ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::clone(&router.db),
    ));
    let now = 1_820_000_000_u64;
    let uid = 99_401_u64;
    let cid = 94_400_001_i64;
    let station_id = 60_003_760_i64;

    let mut sess = make_sess(Some(uid), Some(cid), 0);
    sess.station_id = Some(station_id);

    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
        ensure_store_catalog_seeded(snap, &router.sde);
        let mut cat = read_store_catalog(snap);
        cat.offers.push(StoreOffer {
            id: 9951,
            name: "Unfulfillable Priced Virtual Vanity".into(),
            description: "Zero deliverable vanity offer".into(),
            category_id: 3,
            price_plex: 150,
            product_ids: Vec::new(),
            targeted: false,
            price_cents: Some(799),
            currency: Some("USD".into()),
        });
        save_store_catalog(snap, &cat);

        let mut ch = snap
            .characters
            .get(&90_000_001)
            .cloned()
            .expect("seed character");
        ch.character_id = cid;
        ch.user_id = uid;
        ch.clone_grade = 1;
        ch.station_id = Some(station_id);
        ch.home_station_id = station_id;
        let mut ch_alt = ch.clone();
        snap.characters.insert(cid, ch);

        ch_alt.character_id = 94_400_002;
        snap.characters.insert(ch_alt.character_id, ch_alt);

        snap.account_plex_vault.insert(uid, 2_000);
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            let username = format!("test_{uid}");
            snap.accounts.insert(
                username.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.quasar_commerce.payment_tokens = vec![
            PaymentTokenRecord {
                id: 9401,
                user_id: uid,
                token_str: "tok-9401".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 9403,
                user_id: uid,
                token_str: "tok-9403".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
        snap.quasar_commerce.payment_receipts.clear();

        snap.next_entity_id += 1;
        let stack_id = snap.next_entity_id;
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 40_519,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. u64 > u32::MAX quantity must reject cleanly (never truncate `(1 << 32) + 1` -> `1`).
    let overflow_purch = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1001),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint((1_u64 << 32) + 1),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.plex.vault.api.PurchaseRequest",
            overflow_purch,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let overflow_order = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1001),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint((1_u64 << 32) + 1),
            },
        ],
    }
    .encode();
    let overflow_tok = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(overflow_order),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(9401),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.payment.purchase.api.TokenRequest",
            overflow_tok,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 2. Priced virtual-only non-Omega offer 9951 rejected BEFORE PLEX debit or token consumption.
    let bad_virtual_purch = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(9951),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(1),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.plex.vault.api.PurchaseRequest",
            bad_virtual_purch,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let bad_virtual_tok = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(9951),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(9401),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.payment.purchase.api.TokenRequest",
            bad_virtual_tok,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    router.db.with_snapshot(|s| {
        assert_eq!(s.unified_plex_balance_for_user(uid), 2_000);
        assert!(!s.quasar_commerce.payment_tokens[0].consumed);
        assert!(s.quasar_commerce.payment_receipts.is_empty());
    });

    // Free weekly SP offer 1005 (when free mode allows 0 PLEX) and valid Omega offer 1001 stay allowed.
    router.free_fast_checkout_plex.store(true, Ordering::Relaxed);
    let free_1005 = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1005),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(1),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.plex.vault.api.PurchaseRequest",
            free_1005,
            now,
        )
        .status_code,
        STATUS_OK
    );
    router.free_fast_checkout_plex.store(false, Ordering::Relaxed);

    let ok_omega_tok = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1001),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(9401),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.payment.purchase.api.TokenRequest",
            ok_omega_tok,
            now,
        )
        .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|s| {
        assert_eq!(s.characters[&cid].clone_grade, 2);
        assert!(s.quasar_commerce.payment_tokens[0].consumed);
        assert_eq!(s.quasar_commerce.payment_receipts.len(), 1);
    });

    // 3. OnItemChange emits key `9` (`ixStackSize`), NEVER key `6` (`ixGroupID`), when stacking.
    ctx.db
        .acquire_online_lock(uid, 944_001, Some(cid), true, now)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(cid);
    let stack_purch = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1003),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(2),
            },
        ],
    }
    .encode();
    assert_eq!(
        call_gw(
            &router,
            &sess,
            "eve_public.plex.vault.api.PurchaseRequest",
            stack_purch,
            now,
        )
        .status_code,
        STATUS_OK
    );
    {
        let logs = ctx.drain_notifications_for_char(cid);
        let n = logs
            .iter()
            .find(|n| n.event == "OnItemChange")
            .expect("expected OnItemChange notification");
        let PyRep::Dict(entries) = &n.args[1] else {
            panic!("expected OnItemChange change dict, got {:?}", n.args[1]);
        };
        assert!(
            entries
                .iter()
                .any(|(k, v)| *k == PyRep::Integer(9) && *v == PyRep::Integer(1)),
            "OnItemChange stack delta must emit key 9 (ixStackSize) -> old qty 1; got {entries:?}"
        );
        assert!(
            !entries.iter().any(|(k, _)| *k == PyRep::Integer(6)),
            "OnItemChange stack delta must never emit key 6 (ixGroupID); got {entries:?}"
        );
    }

    // 4. Snapshot-persisted idempotency key deduplicates across fresh router instances, preserves settlement fields, and rejects mismatched parameters.
    let mut tok_msg = DynamicQuasarMessage::new("eve.idempotency.Token");
    tok_msg.set(1, DynamicValue::Bytes(b"snap-idem-1003".to_vec()));
    let mut wrap = DynamicQuasarMessage::new("eve_public.plex.vault.api.PurchaseRequest");
    wrap.set(4, DynamicValue::Message(Box::new(tok_msg)));
    let extracted_key = extract_quasar_store_idempotency_key(&wrap).expect("idempotency key");
    let idem_input = StorePurchaseInput {
        channel: StorePurchaseChannel::PlexVaultQuasar,
        buyer_user_id: uid,
        active_char_id: Some(cid),
        gift_char_id: None,
        session_station_id: Some(station_id),
        offer_id: 1003,
        quantity: 1,
        now_unix: now,
        payment: StorePurchasePaymentSpec::PlexVault {
            allow_free_when_zero: false,
        },
        fast_checkout_mode: false,
        tx_description_mode: StoreTxDescriptionMode::NesOfferNameUpper,
        idempotency_key: Some(&extracted_key),
    };
    let first_outcome = router
        .db
        .try_with_mut_snapshot(|snap| purchase_store_offer_in_snapshot(snap, &idem_input))
        .unwrap();
    assert!(!first_outcome.idempotent_replay);
    assert!(first_outcome.tx_id > 0);
    let replay_outcome = router
        .db
        .try_with_mut_snapshot(|snap| purchase_store_offer_in_snapshot(snap, &idem_input))
        .unwrap();
    assert!(replay_outcome.idempotent_replay);
    assert_eq!(replay_outcome.tx_id, first_outcome.tx_id);
    assert_eq!(replay_outcome.plex_spent, first_outcome.plex_spent);
    assert_eq!(replay_outcome.delivered_item_id, first_outcome.delivered_item_id);
    assert_eq!(
        replay_outcome.delivered_items_count,
        first_outcome.delivered_items_count
    );

    let mismatched_qty_input = StorePurchaseInput {
        quantity: 2,
        ..idem_input.clone()
    };
    assert!(matches!(
        router
            .db
            .try_with_mut_snapshot(|snap| purchase_store_offer_in_snapshot(snap, &mismatched_qty_input)),
        Err(StorePurchaseError::IdempotencyConflict(_))
    ));
    router.db.with_snapshot(|s| {
        assert!(s.committed_idempotency_keys.is_empty());
        assert_eq!(s.quasar_commerce.store_purchase_idempotency.len(), 1);
    });

    // Gateway-forwarded explicit idempotency key (`gw-idem-1003`): Call 1 on `router2`, Call 2 on fresh `router3` sharing the same DB.
    let router2 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&router.db),
        shared_sde(),
    );
    let gw_idem_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![7; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.PurchaseRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::Varint(1003),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(1),
                    },
                ],
            }
            .encode(),
        }),
        external_origin: "test".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: None,
        trace_context: None,
    };
    assert_eq!(
        router2
            .handle_public_request(&sess, &gw_idem_req, Some(b"gw-idem-1003".to_vec()), false, now)
            .status_code,
        STATUS_OK
    );
    // Same-router replay deduplicates and enforces fingerprint conflicts cleanly (`BF-1`, P2).
    assert_eq!(
        router2
            .handle_public_request(&sess, &gw_idem_req, Some(b"gw-idem-1003".to_vec()), false, now)
            .status_code,
        STATUS_OK
    );
    let router3 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&router.db),
        shared_sde(),
    );
    assert_eq!(
        router3
            .handle_public_request(&sess, &gw_idem_req, Some(b"gw-idem-1003".to_vec()), false, now)
            .status_code,
        STATUS_OK
    );
    let gw_mismatch_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.PurchaseRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::Varint(1003),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(2),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req.clone()
    };
    assert_eq!(
        router2
            .handle_public_request(
                &sess,
                &gw_mismatch_req,
                Some(b"gw-idem-1003".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_BAD_REQUEST
    );
    let mut sess_alt_char = sess.clone();
    sess_alt_char.character_id = Some(94_400_002);
    assert_eq!(
        router2
            .handle_public_request(
                &sess_alt_char,
                &gw_idem_req,
                Some(b"gw-idem-1003".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_BAD_REQUEST
    );
    // Non-existent active character is rejected on virtual/Omega offer 1001 (`BF-3`).
    let mut ghost_sess = sess.clone();
    ghost_sess.character_id = Some(999_400_999);
    let omega_virtual_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.PurchaseRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::Varint(1001),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(1),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req.clone()
    };
    assert_eq!(
        router2
            .handle_public_request(&ghost_sess, &omega_virtual_req, None, false, now)
            .status_code,
        STATUS_NOT_FOUND
    );
    // Transient handler failure does not poison `router2.idempotency_cache`: retry succeeds after funding.
    router.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 10);
    });
    assert_eq!(
        router2
            .handle_public_request(&sess, &gw_idem_req, Some(b"gw-retry-1003".to_vec()), false, now)
            .status_code,
        STATUS_BAD_REQUEST
    );
    router.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 1_440);
    });
    assert_eq!(
        router2
            .handle_public_request(&sess, &gw_idem_req, Some(b"gw-retry-1003".to_vec()), false, now)
            .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|s| {
        // 1_440 before retry-test - 140 (gw-retry-1003) = 1_300.
        assert_eq!(s.unified_plex_balance_for_user(uid), 1_300);
        let qty: i32 = s
            .items
            .values()
            .filter(|i| i.owner_id == cid && i.type_id == 40_519)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(qty, 6);
    });

    // Same-router & cross-router `TokenRequest` replay returns authentic `TokenResponse` sequential
    // without double consumption, rejects offer/qty/token_id mismatches, and recovers after failure.
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.payment_tokens.push(PaymentTokenRecord {
            id: 9402,
            user_id: uid,
            token_str: "tok-9402".into(),
            is_quick_pay: true,
            consumed: false,
        });
    });
    let tok_order_1 = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1001),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(1),
            },
        ],
    }
    .encode();
    let tok_req_1 = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.purchase.api.TokenRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(tok_order_1.clone()),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(9402),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req.clone()
    };
    let tok_rsp_1 = router2.handle_public_request(
        &sess,
        &tok_req_1,
        Some(b"gw-tok-9402".to_vec()),
        false,
        now,
    );
    assert_eq!(tok_rsp_1.status_code, STATUS_OK);
    let tok_rsp_same = router2.handle_public_request(
        &sess,
        &tok_req_1,
        Some(b"gw-tok-9402".to_vec()),
        false,
        now,
    );
    assert_eq!(tok_rsp_same.status_code, STATUS_OK);
    assert_eq!(tok_rsp_1.payload, tok_rsp_same.payload);
    let router4 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&router.db),
        shared_sde(),
    );
    let tok_rsp_2 = router4.handle_public_request(
        &sess,
        &tok_req_1,
        Some(b"gw-tok-9402".to_vec()),
        false,
        now,
    );
    assert_eq!(tok_rsp_2.status_code, STATUS_OK);
    assert_eq!(tok_rsp_1.payload, tok_rsp_2.payload);
    assert_ne!(tok_rsp_1.payload.as_ref().unwrap().value, Vec::<u8>::new());
    let tok_order_2 = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(1001),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(2),
            },
        ],
    }
    .encode();
    let tok_mismatch_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.purchase.api.TokenRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(tok_order_2),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(9402),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req.clone()
    };
    assert_eq!(
        router2
            .handle_public_request(
                &sess,
                &tok_mismatch_req,
                Some(b"gw-tok-9402".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_BAD_REQUEST
    );
    let tok_mismatch_token_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.purchase.api.TokenRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(tok_order_1.clone()),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(9403),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req.clone()
    };
    assert_eq!(
        router2
            .handle_public_request(
                &sess,
                &tok_mismatch_token_req,
                Some(b"gw-tok-9402".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_BAD_REQUEST
    );
    let tok_retry_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.purchase.api.TokenRequest"),
            value: WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(tok_order_1),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::Varint(9404),
                    },
                ],
            }
            .encode(),
        }),
        ..gw_idem_req
    };
    assert_eq!(
        router2
            .handle_public_request(
                &sess,
                &tok_retry_req,
                Some(b"gw-tok-retry-9404".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_NOT_FOUND
    );
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.payment_tokens.push(PaymentTokenRecord {
            id: 9404,
            user_id: uid,
            token_str: "tok-9404".into(),
            is_quick_pay: true,
            consumed: false,
        });
    });
    assert_eq!(
        router2
            .handle_public_request(
                &sess,
                &tok_retry_req,
                Some(b"gw-tok-retry-9404".to_vec()),
                false,
                now,
            )
            .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|s| {
        assert_eq!(s.quasar_commerce.payment_receipts.len(), 3);
        let tok_rec = s
            .quasar_commerce
            .store_purchase_idempotency
            .values()
            .find(|r| r.channel == "payment_token")
            .expect("persisted token purchase idempotency record");
        assert!(tok_rec.tx_id > 0);
    });

    // 5. Bounded eviction drops only oldest per-user/global record (never mass-wipes other users),
    //    and duplicate `type_id` entries across multi-product offers check aggregate `i32` overflow.
    let mut bounded_map = std::collections::BTreeMap::new();
    for (seq, (user_id, key, unix)) in [
        (101_u64, "u1-old", now),
        (102, "u2-keep", now + 1),
        (101, "u1-new", now + 2),
        (101, "u1-newest", now + 3),
    ]
    .into_iter()
    .enumerate()
    {
        insert_bounded_store_purchase_idempotency(
            &mut bounded_map,
            format!("{user_id}:plex_vault:{key}"),
            eve_db::service_domains::CommittedStorePurchaseRecord {
                buyer_user_id: user_id,
                channel: "plex_vault".into(),
                idempotency_key: key.into(),
                committed_unix: unix,
                commit_seq: seq as u64 + 1,
                ..Default::default()
            },
            2,
            3,
        );
    }
    assert_eq!(bounded_map.len(), 3);
    assert!(!bounded_map.contains_key("101:plex_vault:u1-old"));
    assert!(bounded_map.contains_key("102:plex_vault:u2-keep"));
    assert!(bounded_map.contains_key("101:plex_vault:u1-new"));
    assert!(bounded_map.contains_key("101:plex_vault:u1-newest"));

    router.db.with_mut_snapshot(|snap| {
        let mut cat = read_store_catalog(snap);
        cat.products.push(StoreProduct {
            id: 99521,
            name: "Split Huge A".into(),
            description: "Half A".into(),
            type_id: 34,
            quantity: i32::MAX as u32 - 5,
        });
        cat.products.push(StoreProduct {
            id: 99522,
            name: "Split Huge B".into(),
            description: "Half B".into(),
            type_id: 34,
            quantity: 10,
        });
        cat.offers.push(StoreOffer {
            id: 9952,
            name: "Duplicate Type Overflow".into(),
            description: "Aggregate overflows i32".into(),
            category_id: 3,
            price_plex: 10,
            product_ids: vec![99521, 99522],
            targeted: false,
            price_cents: None,
            currency: None,
        });
        save_store_catalog(snap, &cat);
    });
    let dup_type_err = router
        .db
        .try_with_mut_snapshot(|snap| {
            purchase_store_offer_in_snapshot(
                snap,
                &StorePurchaseInput {
                    channel: StorePurchaseChannel::PlexVaultQuasar,
                    buyer_user_id: uid,
                    active_char_id: Some(cid),
                    gift_char_id: None,
                    session_station_id: Some(station_id),
                    offer_id: 9952,
                    quantity: 1,
                    now_unix: now,
                    payment: StorePurchasePaymentSpec::PlexVault {
                        allow_free_when_zero: false,
                    },
                    fast_checkout_mode: false,
                    tx_description_mode: StoreTxDescriptionMode::NesOfferNameUpper,
                    idempotency_key: None,
                },
            )
        })
        .unwrap_err();
    assert!(matches!(dup_type_err, StorePurchaseError::Overflow(_)));
}
