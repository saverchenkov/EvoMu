//! Unit tests for `quasar_payment_store_types` extra vault/store handlers,
//! recurring payment schedule processing, and wire varint/ID helpers.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb::{self, enum_name};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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

    // Missing offer ID -> 400; qty = 0 -> 400; unknown offer -> 404; negative price -> 400
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    let zero_qty = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "2002")
        .set("quantity", 0u32)
        .must_encode();
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", zero_qty, now).status_code, STATUS_BAD_REQUEST);

    let unknown_off = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "9999")
        .set("quantity", 1u32)
        .must_encode();
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", unknown_off, now).status_code, STATUS_NOT_FOUND);

    let neg_off = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "2003")
        .set("quantity", 1u32)
        .must_encode();
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", neg_off, now).status_code, STATUS_BAD_REQUEST);

    // Unknown gift recipient -> 404; recipient with no station -> 400
    let bad_gift = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "2002")
        .set("quantity", 1u32)
        .set("gift.character.sequential", 99_999_999u64)
        .must_encode();
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", bad_gift, now).status_code, STATUS_NOT_FOUND);

    let item_req = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "2002")
        .set("quantity", 2u32)
        .must_encode();
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let _ = &p_nostation;
    assert_eq!(call(&router, &p_nostation, "eve_public.plex.vault.api.PurchaseRequest", item_req.clone(), now).status_code, STATUS_BAD_REQUEST);

    // Valid item purchase (qty=2 -> 240 PLEX, 6 items delivered to cid1)
    assert_eq!(call(&router, &p1, "eve_public.plex.vault.api.PurchaseRequest", item_req, now).status_code, STATUS_OK);
    // Valid Omega purchase (500 PLEX -> activates Omega for uid1, leaves 260 PLEX)
    let omega_req = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "2001")
        .set("quantity", 1u32)
        .must_encode();
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

    let cat_req = pb::message("eve.store.admin.api.CreateCategoryRequest")
        .set("attributes.name", "Bundles")
        .set("attributes.catalog", enum_name("CATALOG_NES"))
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateCategoryRequest", cat_req, now).status_code, STATUS_OK);

    let prod_req = pb::message("eve.store.admin.api.CreateProductRequest")
        .set("attributes.name", "Starter Pack")
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateProductRequest", prod_req, now).status_code, STATUS_OK);

    // Public tagged offer (repeated categories & products) + targeted offer (single category & product)
    let off_req = pb::message("eve.store.admin.api.CreateOfferRequest")
        .set("attributes.name", "Flash Bundle")
        .add_with("attributes.currency_prices", |cp| {
            cp.set("code", "USD").set("amount_in_cents", 999i64)
        })
        .set("attributes.plex_price.amount_in_cents", 25_000i64)
        .add("attributes.tags", "flash")
        .add_with("attributes.categories", |c| c.set("sequential", 1u32))
        .add_with("attributes.categories", |c| c.set("sequential", 1u32))
        .add_with("attributes.products", |p| p.set("product.sequential", 1u32))
        .add_with("attributes.products", |p| p.set("product.sequential", 1u32))
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.store.admin.api.CreateOfferRequest", off_req, now).status_code, STATUS_OK);

    let tgt_req = pb::message("eve.store.admin.api.CreateOfferRequest")
        .set("attributes.name", "VIP Promo")
        .set("attributes.targeted", true)
        .add_with("attributes.categories", |c| c.set("sequential", 1u32))
        .add_with("attributes.products", |p| p.set("product.sequential", 1u32))
        .must_encode();
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
    let tag_req = pb::message("eve_public.store.offer.api.GetAllByTagRequest")
        .set("tag", "FLASH")
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllByTagRequest", tag_req, now).status_code, STATUS_OK);

    // 2. RecordInvoiceRequest
    assert_eq!(call(&router, &player, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let inv_req = pb::message("eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest")
        .set("identifier.uuid", [0xABu8; 16])
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", inv_req, now).status_code, STATUS_OK);

    // 3. Assetholding operation GetRequest (owner vs other user vs admin)
    router.db.with_mut_snapshot(|snap| {
        record_plex_transaction(snap, 7001, 10, 100, 100, "Seed", now);
    });
    let op_req = pb::message("eve.plex.vault.assetholding.operation.api.GetRequest")
        .set("operation.uuid", b"7001".as_slice())
        .must_encode();
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
    let buy_1005 = pb::message("eve_public.plex.vault.api.PurchaseRequest")
        .set("offer.store_offer", "1005")
        .set("quantity", 1u32)
        .must_encode();

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

#[test]
fn payment_store_types_builders_decode_strictly_with_descriptors() {
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default()
        .expect("open default descriptor DB");

    let bal = encode_plex_balance_changed_notice(10, 42, -5, 95, "Spend", 1_760_000_000);
    let dec = reg
        .decode_dynamic(&bal.type_url, &bal.value)
        .expect("decode BalanceChangedNotice");
    assert_eq!(reg.encode_dynamic(&dec).unwrap(), bal.value);

    let router = new_router();
    let gm = make_sess(Some(10), Some(90_000_010), service_roles::ROLE_GM_DEFAULT);
    let player = make_sess(Some(10), Some(90_000_010), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_760_000_000;

    publish_schedule_set_notice(&router.pending_notices, 10, 5, 1);
    publish_quick_pay_added_notice(&router.pending_notices, 10, 88);
    {
        let pending = router.pending_notices.read().unwrap();
        for (_, notice) in pending.iter() {
            let d = reg
                .decode_dynamic(&notice.type_url, &notice.value)
                .unwrap_or_else(|e| panic!("decode {}: {e}", notice.type_url));
            assert_eq!(reg.encode_dynamic(&d).unwrap(), notice.value);
        }
    }

    // ListResponse (targeted offers)
    let offers = vec![
        StoreOffer {
            id: 1,
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
            id: 2,
            name: "T2".into(),
            description: String::new(),
            category_id: 1,
            price_plex: 20,
            product_ids: vec![],
            targeted: true,
            price_cents: None,
            currency: None,
        },
    ];
    let mut page = DynamicQuasarMessage::new("Page");
    page.set(1, DynamicValue::Uint32(1));
    page.set(2, DynamicValue::String("0".into()));
    let mut list_req = DynamicQuasarMessage::new("List");
    list_req.set(1, DynamicValue::Message(Box::new(page)));
    let list_bytes = encode_targeted_offer_list_response(&offers, 10, &list_req);
    let list_dec = reg
        .decode_dynamic("eve_public.store.targetaudience.offer.api.ListResponse", &list_bytes)
        .expect("decode ListResponse");
    assert_eq!(reg.encode_dynamic(&list_dec).unwrap(), list_bytes);

    // GetAllByTagResponse & assetholding operation GetResponse
    let tag_req = eve_proto::pb::message("eve_public.store.offer.api.GetAllByTagRequest")
        .set("tag", "FLASH")
        .must_encode();
    let tag_rsp = call(&router, &player, "eve_public.store.offer.api.GetAllByTagRequest", tag_req, now);
    assert_eq!(tag_rsp.status_code, STATUS_OK);
    let tag_any = tag_rsp.payload.unwrap();
    reg.decode_dynamic(&tag_any.type_url, &tag_any.value)
        .expect("decode GetAllByTagResponse");

    router.db.with_mut_snapshot(|snap| {
        record_plex_transaction(snap, 7701, 10, -25, 75, "TestOp", now);
    });
    let op_req = eve_proto::pb::message("eve.plex.vault.assetholding.operation.api.GetRequest")
        .set("operation.uuid", b"7701".as_slice())
        .must_encode();
    let op_rsp = call(&router, &gm, "eve.plex.vault.assetholding.operation.api.GetRequest", op_req, now);
    assert_eq!(op_rsp.status_code, STATUS_OK);
    let op_any = op_rsp.payload.unwrap();
    reg.decode_dynamic(&op_any.type_url, &op_any.value)
        .expect("decode assetholding operation GetResponse");
}

#[cfg(test)]
#[path = "quasar_payment_store_types_extra_unit_tests.rs"]
mod quasar_payment_store_types_extra_unit_tests;
