//! Semantic unit tests split from `quasar_cosmetics_market_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::quasar_cosmetics_market_extra_unit_tests::*;

use super::quasar_cosmetics_market::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK,
};
use crate::router::NotificationTarget;
use crate::services::quasar_cosmetics_skinr::{
    SkinDesignRecord, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use eve_db::{IskCents, WorldRepository};
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, QuasarDescriptorRegistry,
};
use eve_proto::PyRep;
use std::sync::{Arc, Mutex};

#[test]
fn test_skin_purchase_rollback_branches_account_sink_and_admin_expiration_errors() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let captured_notifies: Arc<Mutex<Vec<(NotificationTarget, String)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let cap_clone = Arc::clone(&captured_notifies);
    *router.macho_notify_sink.write().unwrap() = Some(Arc::new(
        move |target, method, _id_type, _args: Vec<PyRep>| {
            cap_clone.lock().unwrap().push((target, method.to_string()));
        },
    ));

    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let state = CosmeticsMarketState::load_with_sink(
        Arc::clone(&db),
        Arc::clone(&sde),
        Arc::clone(&skinr),
        Arc::clone(&router.macho_notify_sink),
    );
    register_with_state(&router, Arc::clone(&state));

    let now = 1_700_000_000u64;
    let seller = 2_112_000_701i64;
    let buyer = 2_112_000_702i64;
    let admin = 2_112_000_799i64;
    let corp_a = 98_000_701i64;

    make_char(&db, seller, corp_a, None, 10_000_000_00, 100, 100_000);
    // Buyer has EverMarks (100_000) and PLEX (0) and ISK (0) initially
    make_char(&db, buyer, corp_a, None, 0, 0, 100_000);
    make_char(&db, admin, corp_a, None, 1_000_000_00, 0, 0);

    let seller_sess = make_sess(seller, corp_a, None, 0);
    let buyer_sess = make_sess(buyer, corp_a, None, 0);
    let admin_sess = make_sess(admin, corp_a, None, service_roles::ROLE_GM_DEFAULT);

    let skin_hex = "11223344aabbccdd";
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "PurchaseSkin".into(),
                line_name: "Line".into(),
                creator_id: seller as u32,
                ship_type_id: 587,
                pattern_blend_mode: 0,
                slots: Vec::new(),
                tier_level: 1,
                first_sequenced_unix: None,
            },
        );
        sd.skin_licenses.insert(
            SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex),
            ThirdPartySkinLicense {
                character_id: seller as u32,
                skin_hex: skin_hex.into(),
                activated: false,
                unactivated: 5,
                escrowed: 0,
                first_acquired_unix: now,
            },
        );
    }

    // Create a character-targeted listing for `buyer` (30% tax rate)
    let mut shex_msg = Vec::new();
    encode_string_field(1, skin_hex, &mut shex_msg);
    let mut lic_ok = Vec::new();
    put_seq(1, seller as u64, &mut lic_ok);
    encode_bytes_field(2, &shex_msg, &mut lic_ok);
    let mut target_char = Vec::new();
    put_seq(4, buyer as u64, &mut target_char);

    let mut req_create = Vec::new();
    encode_isk_msg(1, 1_000_000, &mut req_create);
    encode_bytes_field(3, &lic_ok, &mut req_create);
    put_seconds(4, 86_400, &mut req_create);
    encode_varint_field(5, 1, &mut req_create);
    encode_bytes_field(6, &target_char, &mut req_create);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_create,
            now,
        )
        .status_code,
        STATUS_OK
    );
    // Broker fee notification was emitted via macho_notify_sink
    assert!(!captured_notifies.lock().unwrap().is_empty());

    let (listing_uuid, listed_isk) = {
        let sd = state.data.read().unwrap();
        let l = sd.skin_listings.values().next().unwrap();
        (l.uuid, l.price_isk.unwrap())
    };

    // Purchase error branches:
    // 1. Missing UUID -> STATUS_BAD_REQUEST
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 2. Quantity == 0 -> STATUS_BAD_REQUEST
    let mut req_q0 = Vec::new();
    put_uuid(1, &listing_uuid, &mut req_q0);
    encode_varint_field(2, 0, &mut req_q0);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            req_q0,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 3. Quantity > listing.quantity (2 > 1) -> STATUS_BAD_REQUEST
    let mut req_q2 = Vec::new();
    put_uuid(1, &listing_uuid, &mut req_q2);
    encode_varint_field(2, 2, &mut req_q2);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            req_q2,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 4. Buyer has 0 ISK -> settlement fails, listing is re-inserted into `skin_listings` (rollback when full qty purchased)
    let mut req_buy = Vec::new();
    put_uuid(1, &listing_uuid, &mut req_buy);
    encode_varint_field(2, 1, &mut req_buy);
    encode_string_field(3, BUYER_FEES_VERSION, &mut req_buy);
    let resp_fail = call(
        &router,
        &buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        req_buy.clone(),
        now,
    );
    assert_eq!(resp_fail.status_code, STATUS_OK);
    // Listing was restored with quantity 1
    assert_eq!(
        state
            .data
            .read()
            .unwrap()
            .skin_listings
            .get(&hex_of(&listing_uuid))
            .map(|l| l.quantity),
        Some(1)
    );

    // 5. Give buyer 10_000_000 ISK and purchase -> 30% Character-targeted tax applied (seller gets +700_000 ISK)
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&buyer).unwrap().balance = IskCents(10_000_000_00);
    });
    let seller_bal_before = db.with_snapshot(|s| s.characters.get(&seller).unwrap().balance.0);
    let resp_ok = call(
        &router,
        &buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        req_buy,
        now,
    );
    assert_eq!(resp_ok.status_code, STATUS_OK);
    let seller_bal_after = db.with_snapshot(|s| s.characters.get(&seller).unwrap().balance.0);
    assert_eq!(seller_bal_after - seller_bal_before, listed_isk as i64 * 70);

    // Admin SetExpiration error branches: missing UUID, missing timestamp, unknown listing
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut admin_no_ts = Vec::new();
    put_uuid(1, &listing_uuid, &mut admin_no_ts);
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            admin_no_ts,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut admin_not_found = Vec::new();
    put_uuid(1, &listing_uuid, &mut admin_not_found);
    put_seconds(2, now + 3600, &mut admin_not_found);
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            admin_not_found,
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
}
