//! Semantic unit tests split from `quasar_cosmetics_market_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::quasar_cosmetics_market_extra_unit_tests::*;

use super::quasar_cosmetics_market::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_PRECONDITION_FAILED,
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

#[test]
fn admin_revoke_purges_ship_thirdparty_skin_notices_and_hub_listings_and_rejects_stale_listing() {
    use crate::services::quasar_cosmetics_live::{register as register_live, ship_state, CosmeticsLiveState};
    use crate::services::quasar_cosmetics_skinr::register as register_skinr;
    use crate::services::quasar_cosmetics_skinr_seq::register as register_skinr_seq;

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let live = CosmeticsLiveState::load(Arc::clone(&db), Arc::clone(&sde));
    let market = CosmeticsMarketState::load_with_sink(
        Arc::clone(&db),
        Arc::clone(&sde),
        Arc::clone(&skinr),
        Arc::clone(&router.macho_notify_sink),
    );
    register_skinr(&router, Arc::clone(&skinr));
    register_skinr_seq(&router, Arc::clone(&skinr));
    register_live(&router, Arc::clone(&live), Arc::clone(&skinr));
    register_with_state(&router, Arc::clone(&market));

    let now = 1_700_000_000u64;
    let seller = 90_000_001i64;
    let buyer = 90_000_002i64;
    let corp_a = 98_000_701i64;
    let ship_id = 100_000_001u64;
    let admin = 2_112_000_799i64;

    make_char(&db, seller, corp_a, None, 10_000_000_00, 100, 100_000);
    make_char(&db, buyer, corp_a, None, 20_000_000_00, 100, 100_000);
    make_char(&db, admin, corp_a, None, 1_000_000_00, 0, 0);

    let seller_sess = make_sess(seller, corp_a, None, 0);
    let buyer_sess = make_sess(buyer, corp_a, None, 0);
    let admin_sess = make_sess(admin, corp_a, None, service_roles::ROLE_GM_DEFAULT);

    let skin_hex = "aabbccddeeff0011";
    let ship_tid = db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "RevokedDesign".into(),
                line_name: "Line".into(),
                creator_id: seller as u32,
                ship_type_id: ship_tid,
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
                activated: true,
                unactivated: 3,
                escrowed: 0,
                first_acquired_unix: now,
            },
        );
        sd.applied_thirdparty_skins.insert(ship_id, skin_hex.into());
    }
    skinr.save();

    // Seller creates a Hub listing for 1 unactivated copy
    let mut shex_msg = Vec::new();
    encode_string_field(1, skin_hex, &mut shex_msg);
    let mut lic_id = Vec::new();
    put_seq(1, seller as u64, &mut lic_id);
    encode_bytes_field(2, &shex_msg, &mut lic_id);
    let mut req_create = Vec::new();
    encode_isk_msg(1, 500_000, &mut req_create);
    encode_bytes_field(3, &lic_id, &mut req_create);
    put_seconds(4, 86_400, &mut req_create);
    encode_varint_field(5, 1, &mut req_create);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_create.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    // 1. Admin revokes the license -> must purge ship applied skin + hub listings
    let mut rev_req = Vec::new();
    encode_bytes_field(1, &lic_id, &mut rev_req);
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest",
            rev_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert!(
        !skinr
            .data
            .read()
            .unwrap()
            .applied_thirdparty_skins
            .contains_key(&ship_id),
        "applied_thirdparty_skins must be purged on revoke"
    );
    db.with_snapshot(|s| {
        let raw_state = ship_state(s, &sde, ship_id).expect("ship_state encodes no_skin for valid ship");
        let wm = eve_proto::quasar::WireMessage::decode(&raw_state).unwrap();
        let skin_bytes = wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let skin_wm = eve_proto::quasar::WireMessage::decode(skin_bytes).unwrap();
        assert!(!skin_wm.fields.iter().any(|f| f.field_number == 1 || f.field_number == 2));
        assert_eq!(
            skin_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
            Some(1),
            "ship_state must report no SKIN after revoke"
        );
    });
    assert!(
        market.data.read().unwrap().skin_listings.is_empty(),
        "Hub listings must be purged when seller license is revoked"
    );

    // 2. Re-seed a stale/desynced listing whose escrow is 0 (< requested quantity 1) and verify
    //    purchase rejects it, deletes the stale listing, and leaves ISK/licenses untouched!
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skin_licenses.insert(
            SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex),
            ThirdPartySkinLicense {
                character_id: seller as u32,
                skin_hex: skin_hex.into(),
                activated: false,
                unactivated: 1,
                escrowed: 0,
                first_acquired_unix: now,
            },
        );
    }
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
    let stale_uuid = market.data.read().unwrap().skin_listings.values().next().unwrap().uuid;
    {
        // Simulate external depletion/revocation of escrowed copies while listing survived
        let mut sd = skinr.data.write().unwrap();
        sd.skin_licenses
            .get_mut(&SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex))
            .unwrap()
            .escrowed = 0;
    }
    let seller_bal_before = db.with_snapshot(|s| s.characters[&seller].balance.0);
    let buyer_bal_before = db.with_snapshot(|s| s.characters[&buyer].balance.0);
    let mut buy_req = Vec::new();
    put_uuid(1, &stale_uuid, &mut buy_req);
    encode_varint_field(2, 1, &mut buy_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_req);
    let stale_rsp = call(
        &router,
        &buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
        now,
    );
    assert_eq!(stale_rsp.status_code, STATUS_PRECONDITION_FAILED);
    assert!(market.data.read().unwrap().skin_listings.is_empty());
    assert_eq!(db.with_snapshot(|s| s.characters[&seller].balance.0), seller_bal_before);
    assert_eq!(db.with_snapshot(|s| s.characters[&buyer].balance.0), buyer_bal_before);
    assert!(
        !skinr
            .data
            .read()
            .unwrap()
            .skin_licenses
            .contains_key(&SkinrCosmeticsState::skin_lic_key(buyer as u32, skin_hex))
    );
}
