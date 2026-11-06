//! Deep branch-coverage and mutation-killing unit tests for `quasar_payment_store_types`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_OK,
};
use crate::services::quasar_payment_store_types::*;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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
        let scheds: Vec<PaymentScheduleRecord> =
            serde_json::from_str(&snap.service_state_blobs["payment_schedules"]).unwrap();
        let s10 = scheds.iter().find(|s| s.user_id == 10).unwrap();
        assert_eq!(s10.next_payment_unix, now + 86_400);
        let receipts: Vec<PaymentReceiptRecord> =
            serde_json::from_str(&snap.service_state_blobs["payment_receipts"]).unwrap();
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
    let mut gift_cm = Vec::new();
    encode_varint_field(1, gift_cid as u64, &mut gift_cm);
    let mut gift_gm = Vec::new();
    encode_bytes_field(3, &gift_cm, &mut gift_gm);
    let mut gift_req = Vec::new();
    encode_varint_field(1, 3001, &mut gift_req);
    encode_bytes_field(3, &gift_gm, &mut gift_req);

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
    let mut self_req = Vec::new();
    encode_varint_field(1, 3001, &mut self_req);
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
    let mut overflow_req = Vec::new();
    encode_varint_field(1, 3002, &mut overflow_req);
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
    let mut bin_op_inner = Vec::new();
    encode_bytes_field(1, &9876_u64.to_be_bytes(), &mut bin_op_inner);
    let mut bin_op_req = Vec::new();
    encode_bytes_field(1, &bin_op_inner, &mut bin_op_req);
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

    let mut missing_op_req = Vec::new();
    encode_varint_field(1, 777_888_999, &mut missing_op_req);
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
        snap.service_state_blobs
            .insert("plex_invoices".into(), serde_json::to_string(&full).unwrap());
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
        let list: Vec<String> =
            serde_json::from_str(&snap.service_state_blobs["plex_invoices"]).unwrap();
        assert_eq!(list.len(), 1000);
        assert_eq!(list.first().unwrap(), "inv-0001");
    });

    let _ = NoticeScope::User(1);
    let _ = STATUS_FORBIDDEN;
}
