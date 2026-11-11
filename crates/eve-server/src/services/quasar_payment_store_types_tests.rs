//! Unit tests for `quasar_payment_store_types` extra vault/store handlers,
//! recurring payment schedule processing, and wire varint/ID helpers.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
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

fn call(router: &PublicGatewayRouter, sess: &SessionState, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: vec![2; 16],
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
fn plex_vault_purchase_request_handles_items_gifts_omega_and_errors() {
    let router = new_router();
    let uid1 = 31u64;
    let cid1 = 90_000_031i64;
    let uid2 = 32u64;
    let cid2 = 90_000_032i64;
    let cid_nostation = 90_000_033i64;

    router.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().expect("demo char").clone();
        let mut c1 = tmpl.clone();
        c1.character_id = cid1;
        c1.user_id = uid1;
        c1.station_id = Some(60_003_760);
        snap.characters.insert(cid1, c1);

        let mut c2 = tmpl.clone();
        c2.character_id = cid2;
        c2.user_id = uid2;
        c2.station_id = Some(60_008_494);
        snap.characters.insert(cid2, c2);

        let mut c3 = tmpl;
        c3.character_id = cid_nostation;
        c3.user_id = 33;
        c3.station_id = None;
        c3.structure_id = None;
        c3.home_station_id = 0;
        snap.characters.insert(cid_nostation, c3);

        snap.account_plex_vault.insert(uid1, 1_000);
        snap.account_plex_vault.insert(33, 1_000);

        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![
                    StoreCategory { id: 1, name: "Omega".into(), description: "Omega".into() },
                    StoreCategory { id: 2, name: "Items".into(), description: "Items".into() },
                ],
                products: vec![
                    StoreProduct { id: 201, name: "SKIN".into(), description: "SKIN".into(), type_id: 34599, quantity: 3 },
                ],
                offers: vec![
                    StoreOffer {
                        id: 2001,
                        name: "Omega 30d".into(),
                        description: "Omega".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: Vec::new(),
                        targeted: false,
                        price_cents: Some(1_999),
                        currency: Some("USD".into()),
                    },
                    StoreOffer {
                        id: 2002,
                        name: "SKIN Pack".into(),
                        description: "SKIN".into(),
                        category_id: 2,
                        price_plex: 120,
                        product_ids: vec![201],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                    StoreOffer {
                        id: 2003,
                        name: "BadNegative".into(),
                        description: "Bad".into(),
                        category_id: 2,
                        price_plex: -10,
                        product_ids: Vec::new(),
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                ],
            },
        );
    });

    let p1 = make_sess(Some(uid1), Some(cid1), service_roles::ROLE_PLAYER_DEFAULT);
    let p_nostation = make_sess(Some(33), Some(cid_nostation), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_760_000_000u64;

    let encode_offer_field = |id: &str, out: &mut Vec<u8>| {
        let mut inner = Vec::new();
        encode_string_field(1, id, &mut inner);
        encode_bytes_field(1, &inner, out);
    };

    // Missing offer ID -> 400; qty = 0 -> 400; unknown offer -> 404; negative price -> 400
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    let mut zero_qty = Vec::new();
    encode_offer_field("2002", &mut zero_qty);
    encode_varint_field(2, 0, &mut zero_qty);
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", zero_qty, now).status_code, STATUS_BAD_REQUEST);

    let mut unknown_off = Vec::new();
    encode_offer_field("9999", &mut unknown_off);
    encode_varint_field(2, 1, &mut unknown_off);
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", unknown_off, now).status_code, STATUS_NOT_FOUND);

    let mut neg_off = Vec::new();
    encode_offer_field("2003", &mut neg_off);
    encode_varint_field(2, 1, &mut neg_off);
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", neg_off, now).status_code, STATUS_BAD_REQUEST);

    // Unknown gift recipient -> 404; recipient with no station -> 400
    let mut bad_gift = Vec::new();
    encode_offer_field("2002", &mut bad_gift);
    encode_varint_field(2, 1, &mut bad_gift);
    let mut gift_char = Vec::new();
    encode_varint_field(1, 99_999_999, &mut gift_char);
    let mut gift_msg = Vec::new();
    encode_bytes_field(3, &gift_char, &mut gift_msg);
    encode_bytes_field(3, &gift_msg, &mut bad_gift);
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", bad_gift, now).status_code, STATUS_NOT_FOUND);

    let mut item_req = Vec::new();
    encode_offer_field("2002", &mut item_req);
    encode_varint_field(2, 2, &mut item_req);
    assert_eq!(call(&router, &p_nostation, "eve_public.plex.vault.api.PurchaseRequest", item_req.clone(), now).status_code, STATUS_BAD_REQUEST);

    // Valid item purchase (qty=2 -> 240 PLEX, 6 items delivered to cid1)
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", item_req, now).status_code, STATUS_OK);
    // Valid Omega purchase (500 PLEX -> activates Omega for uid1, leaves 260 PLEX)
    let mut omega_req = Vec::new();
    encode_offer_field("2001", &mut omega_req);
    encode_varint_field(2, 1, &mut omega_req);
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", omega_req.clone(), now).status_code, STATUS_OK);
    // Second Omega purchase exceeds remaining 260 PLEX -> 400 Insufficient PLEX balance
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", omega_req, now).status_code, STATUS_BAD_REQUEST);

    router.db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault[&uid1], 260);
        assert_eq!(s.characters[&cid1].clone_grade, 2);
        let qty: i32 = s.items.values().filter(|i| i.owner_id == cid1 && i.type_id == 34599).map(|i| i.quantity).sum();
        assert_eq!(qty, 6);
    });
}

#[test]
fn admin_store_creation_invoices_tag_lookup_and_credit_card_disabled() {
    let router = new_router();
    let player = make_sess(Some(10), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = make_sess(Some(10), Some(90_000_001), service_roles::ROLE_GM_DEFAULT);
    let now = 1_760_000_100u64;

    // 1. Admin CreateCategoryRequest, CreateProductRequest, CreateOfferRequest
    assert_eq!(call(&router, &player, "eve.store.admin.api.CreateCategoryRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateCategoryRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    let mut cat_attrs = Vec::new();
    encode_string_field(1, "Bundles", &mut cat_attrs);
    encode_varint_field(6, 1, &mut cat_attrs);
    let mut cat_req = Vec::new();
    encode_bytes_field(1, &cat_attrs, &mut cat_req);
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateCategoryRequest", cat_req, now).status_code, STATUS_OK);

    let mut prod_attrs = Vec::new();
    encode_string_field(1, "Starter Pack", &mut prod_attrs);
    let mut prod_req = Vec::new();
    encode_bytes_field(2, &prod_attrs, &mut prod_req);
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateProductRequest", prod_req, now).status_code, STATUS_OK);

    // Public tagged offer (repeated categories & products) + targeted offer (single category & product)
    let mut off_attrs = Vec::new();
    encode_string_field(1, "Flash Bundle", &mut off_attrs);
    let mut cp = Vec::new();
    encode_string_field(1, "USD", &mut cp);
    encode_varint_field(2, 999, &mut cp);
    encode_bytes_field(3, &cp, &mut off_attrs);
    let mut pp = Vec::new();
    encode_varint_field(1, 25_000, &mut pp);
    encode_bytes_field(5, &pp, &mut off_attrs);
    encode_string_field(16, "flash", &mut off_attrs);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, 1, &mut cid_msg);
    encode_bytes_field(17, &cid_msg, &mut off_attrs);
    encode_bytes_field(17, &cid_msg, &mut off_attrs);
    let mut op_msg = Vec::new();
    let mut pid_msg = Vec::new();
    encode_varint_field(1, 1, &mut pid_msg);
    encode_bytes_field(1, &pid_msg, &mut op_msg);
    encode_bytes_field(18, &op_msg, &mut off_attrs);
    encode_bytes_field(18, &op_msg, &mut off_attrs);
    let mut off_req = Vec::new();
    encode_bytes_field(1, &off_attrs, &mut off_req);
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateOfferRequest", off_req, now).status_code, STATUS_OK);

    let mut tgt_attrs = Vec::new();
    encode_string_field(1, "VIP Promo", &mut tgt_attrs);
    encode_varint_field(7, 1, &mut tgt_attrs);
    encode_bytes_field(17, &cid_msg, &mut tgt_attrs);
    encode_bytes_field(18, &op_msg, &mut tgt_attrs);
    let mut tgt_req = Vec::new();
    encode_bytes_field(1, &tgt_attrs, &mut tgt_req);
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateOfferRequest", tgt_req, now).status_code, STATUS_OK);

    // Direct register_user_payment_token persists token & publishes QuickPayAddedNotice
    let ctx = crate::router::ServerContext::with_sde_and_repository(shared_sde(), Arc::clone(&router.db));
    register_user_payment_token(
        &ctx,
        10,
        PaymentTokenRecord { id: 808, user_id: 10, token_str: "qp-808".into(), is_quick_pay: true, consumed: false },
    );
    router.db.with_snapshot(|s| {
        assert!(s
            .quasar_commerce
            .payment_tokens
            .iter()
            .any(|t| t.id == 808 && t.user_id == 10 && t.is_quick_pay));
    });

    // GetAllByTagRequest: empty tag -> 400; "FLASH" (case-insensitive) -> 200
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllByTagRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut tag_req = Vec::new();
    encode_string_field(2, "FLASH", &mut tag_req);
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllByTagRequest", tag_req, now).status_code, STATUS_OK);

    // 2. RecordInvoiceRequest
    assert_eq!(call(&router, &player, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut inv_id = Vec::new();
    encode_bytes_field(1, &[0xAB; 16], &mut inv_id);
    let mut inv_req = Vec::new();
    encode_bytes_field(1, &inv_id, &mut inv_req);
    assert_eq!(call(&router, &gm, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", inv_req, now).status_code, STATUS_OK);

    // 3. Assetholding operation GetRequest (owner vs other user vs admin)
    router.db.with_mut_snapshot(|snap| {
        record_plex_transaction(snap, 7001, 10, 100, 100, "Seed", now);
    });
    let mut op_id = Vec::new();
    encode_bytes_field(1, b"7001", &mut op_id);
    let mut op_req = Vec::new();
    encode_bytes_field(1, &op_id, &mut op_req);
    assert_eq!(call(&router, &player, "eve.plex.vault.assetholding.operation.api.GetRequest", op_req.clone(), now).status_code, STATUS_OK);

    let intruder = make_sess(Some(888), Some(90_000_888), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(call(&router, &intruder, "eve.plex.vault.assetholding.operation.api.GetRequest", op_req.clone(), now).status_code, STATUS_FORBIDDEN);
    let intruder_gm = make_sess(Some(888), Some(90_000_888), service_roles::ROLE_GM_DEFAULT);
    assert_eq!(call(&router, &intruder_gm, "eve.plex.vault.assetholding.operation.api.GetRequest", op_req, now).status_code, STATUS_OK);

    // 4. CreditCardRequest fails closed with 403
    assert_eq!(call(&router, &player, "eve.payment.purchase.api.CreditCardRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
}

#[test]
fn due_payment_schedules_renew_with_quick_pay_or_cancel_without_token() {
    let router = new_router();
    let now = 1_800_000_000u64;

    router.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: Vec::new(),
                products: Vec::new(),
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "Omega".into(),
                    description: "Omega".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: Vec::new(),
                    targeted: false,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                }],
            },
        );
        // User 1 has quick-pay token -> schedule 1 renews; User 2 has no token -> schedule 2 fails & cancels.
        upsert_user_payment_schedule_in_snapshot(snap, 1, 1, now - 10, "VEVC-1001", 30, now - 1000);
        upsert_user_payment_schedule_in_snapshot(snap, 2, 1, now - 5, "VEVC-1001", 30, now - 1000);
        // Schedule 3 is not due yet (next_payment_unix > now) -> untouched.
        upsert_user_payment_schedule_in_snapshot(snap, 3, 1, now + 500, "VEVC-1001", 30, now - 1000);
    });

    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        1,
        PaymentTokenRecord { id: 1, user_id: 1, token_str: "qp1".into(), is_quick_pay: true, consumed: false },
    );

    process_due_payment_schedules(&router.db, &router.pending_notices, now);

    router.db.with_snapshot(|snap| {
        let scheds = &snap.quasar_commerce.payment_schedules;
        assert_eq!(scheds.len(), 2);
        let s1 = scheds.iter().find(|s| s.user_id == 1).unwrap();
        assert_eq!(s1.next_payment_unix, now + 30 * 86_400);
        assert!(scheds.iter().all(|s| s.user_id != 2));
        let s3 = scheds.iter().find(|s| s.user_id == 3).unwrap();
        assert_eq!(s3.next_payment_unix, now + 500);

        let receipts = &snap.quasar_commerce.payment_receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].user_id, 1);
        assert_eq!(receipts[0].amount_in_cents, 1_999);
    });

    // cancel_user_payment_schedule returns true for user 1 and false for non-owner.
    let s1_id = router.db.with_snapshot(|snap| {
        snap.quasar_commerce
            .payment_schedules
            .iter()
            .find(|s| s.user_id == 1)
            .unwrap()
            .id
    });
    assert!(!cancel_user_payment_schedule(&router.db, &router.pending_notices, 999, s1_id));
    assert!(cancel_user_payment_schedule(&router.db, &router.pending_notices, 1, s1_id));

    // Verify encode_targeted_offer_list_response pagination helper directly.
    let offers = vec![
        StoreOffer { id: 1, name: "A".into(), description: "".into(), category_id: 1, price_plex: 10, product_ids: vec![], targeted: true, price_cents: None, currency: None },
        StoreOffer { id: 2, name: "B".into(), description: "".into(), category_id: 1, price_plex: 10, product_ids: vec![], targeted: true, price_cents: None, currency: None },
    ];
    let mut page = DynamicQuasarMessage::new("Page");
    page.set(1, DynamicValue::Uint32(1));
    page.set(2, DynamicValue::String("5".into())); // offset past end
    let mut req_msg = DynamicQuasarMessage::new("ListRequest");
    req_msg.set(1, DynamicValue::Message(Box::new(page)));
    assert!(encode_targeted_offer_list_response(&offers, 1, &req_msg).is_empty());

    // Default seeded catalog includes Omega Weekly 15,000 Free SP offer (1005) and tag.
    let db_seed = Arc::new(WorldRepository::with_demo_seed());
    let sde_seed = SdeRegistry::default();
    db_seed.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
        ensure_store_catalog_seeded(snap, &sde_seed);
    });
    db_seed.with_snapshot(|snap| {
        let cat = read_store_catalog(snap);
        assert!(
            cat.offers.iter().any(|o| o.id == 1005 && o.price_plex == 0),
            "default catalog must include 0-PLEX Omega Weekly offer 1005"
        );
        assert!(
            cat.products
                .iter()
                .any(|p| p.id == 105 && p.type_id == 0 && p.quantity == 15_000),
            "default catalog must include 15,000 SP product 105"
        );
        let tags = read_store_offer_tags(snap);
        assert!(
            tags.get(&1005)
                .is_some_and(|t| t.iter().any(|s| s == "omega-weekly-free-sp")),
            "offer 1005 must carry omega-weekly-free-sp tag"
        );
    });

    // Verify `ensure_store_catalog_seeded` backfills offer 1005 & product 105 into an existing custom catalog idempotently
    db_seed.with_mut_snapshot(|snap| {
        let mut cat = read_store_catalog(snap);
        cat.offers.retain(|o| o.id != 1005);
        cat.products.retain(|p| p.id != 105);
        cat.offers.push(StoreOffer {
            id: 9999,
            name: "Custom Offer".into(),
            description: "".into(),
            category_id: 1,
            price_plex: 42,
            product_ids: vec![101],
            targeted: false,
            price_cents: None,
            currency: None,
        });
        snap.quasar_commerce.nes_store_catalog = Some(cat);
        ensure_store_catalog_seeded(snap, &sde_seed);
        ensure_store_catalog_seeded(snap, &sde_seed);
    });
    db_seed.with_snapshot(|snap| {
        let cat = read_store_catalog(snap);
        assert!(cat.offers.iter().any(|o| o.id == 9999));
        assert_eq!(cat.offers.iter().filter(|o| o.id == 1005).count(), 1);
        assert_eq!(cat.products.iter().filter(|p| p.id == 105).count(), 1);
    });

    // Verify `PurchaseRequest` on offer 1005 (0 PLEX, product 105 has type_id == 0):
    // Rejected when `free_fast_checkout_plex == false`, succeeds when `true`, and never creates a `type_id == 0` inventory item.
    let uid_sp = 41u64;
    let cid_sp = 90_000_041i64;
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
        ensure_store_catalog_seeded(snap, &sde_seed);
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid_sp;
        ch.user_id = uid_sp;
        ch.clone_grade = 2;
        ch.station_id = Some(60_003_760);
        snap.characters.insert(cid_sp, ch);
        snap.account_plex_vault.insert(uid_sp, 100);
    });
    let sess_sp = make_sess(Some(uid_sp), Some(cid_sp), service_roles::ROLE_PLAYER_DEFAULT);
    let mut buy_1005 = Vec::new();
    let mut off_inner = Vec::new();
    encode_string_field(1, "1005", &mut off_inner);
    encode_bytes_field(1, &off_inner, &mut buy_1005);
    encode_varint_field(2, 1, &mut buy_1005);

    let t_claim = 1_750_000_000u64;
    router
        .free_fast_checkout_plex
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        call(
            &router,
            &sess_sp,
            "eve_public.plex.vault.api.PurchaseRequest",
            buy_1005.clone(),
            t_claim,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    router
        .free_fast_checkout_plex
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        call(
            &router,
            &sess_sp,
            "eve_public.plex.vault.api.PurchaseRequest",
            buy_1005,
            t_claim,
        )
        .status_code,
        STATUS_OK
    );
    router.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault[&uid_sp], 100);
        assert!(
            !snap.items.values().any(|i| i.owner_id == cid_sp && i.type_id == 0),
            "Free SP product (type_id == 0) must not create an inventory item"
        );
    });
}

#[cfg(test)]
#[path = "quasar_payment_store_types_extra_unit_tests.rs"]
mod quasar_payment_store_types_extra_unit_tests;
