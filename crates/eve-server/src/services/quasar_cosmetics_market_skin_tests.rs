//! SKIN listing creation, cancellation, visibility, purchase settlement, escrow rollback,
//! and admin expiration unit tests for `quasar_cosmetics_market`.

#![forbid(unsafe_code)]

use super::super::*;
use super::{call, encode_isk_msg, make_char, make_sess, new_market_harness, wm_bytes, wm_sub, wm_varint};
use crate::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_cosmetics_skinr::{
    SkinDesignRecord, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use eve_db::IskCents;
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, WireMessage,
};

#[test]
fn skin_listing_lifecycle_create_cancel_visibility_purchase_and_admin_expiration() {
    let (router, db, skinr, state) = new_market_harness();
    let now = 1_700_000_000u64;
    let seller = 2_112_000_301i64;
    let buyer_ok = 2_112_000_302i64;
    let buyer_outsider = 2_112_000_303i64;
    let admin_cid = 2_112_000_399i64;
    let corp_a = 98_000_401i64;
    let corp_b = 98_000_402i64;
    let ally_a = 99_000_401i64;

    make_char(&db, seller, corp_a, Some(ally_a), 5_000_000_00, 20, 0);
    make_char(&db, buyer_ok, corp_b, Some(ally_a), 50_000_000_00, 500, 100_000);
    make_char(&db, buyer_outsider, 98_000_499, None, 50_000_000_00, 500, 100_000);
    make_char(&db, admin_cid, 98_000_001, None, 1_000_000_00, 0, 0);

    let seller_sess = make_sess(seller, corp_a, Some(ally_a), 0);
    let buyer_sess = make_sess(buyer_ok, corp_b, Some(ally_a), 0);
    let outsider_sess = make_sess(buyer_outsider, 98_000_499, None, 0);
    let admin_sess = make_sess(admin_cid, 98_000_001, None, service_roles::ROLE_GM_DEFAULT);

    let skin_hex = "aabbccdd11223344";
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "TestSkin".into(),
                line_name: "TestLine".into(),
                creator_id: seller as u32,
                ship_type_id: 587,
                pattern_blend_mode: 0,
                slots: Vec::new(),
                tier_level: 2,
                first_sequenced_unix: None,
            },
        );
        sd.skin_licenses.insert(
            SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex),
            ThirdPartySkinLicense {
                character_id: seller as u32,
                skin_hex: skin_hex.into(),
                activated: false,
                unactivated: 10,
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
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let mut req_foreign_owner = Vec::new();
    encode_isk_msg(1, 500_000, &mut req_foreign_owner);
    let mut bad_lic = Vec::new();
    put_seq(1, 999_999, &mut bad_lic);
    let mut shex_msg = Vec::new();
    encode_string_field(1, skin_hex, &mut shex_msg);
    encode_bytes_field(2, &shex_msg, &mut bad_lic);
    encode_bytes_field(3, &bad_lic, &mut req_foreign_owner);
    put_seconds(4, 86_400, &mut req_foreign_owner);
    encode_varint_field(5, 1, &mut req_foreign_owner);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_foreign_owner,
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    let mut lic_ok = Vec::new();
    put_seq(1, seller as u64, &mut lic_ok);
    encode_bytes_field(2, &shex_msg, &mut lic_ok);

    let mut req_over_isk = Vec::new();
    encode_isk_msg(1, MAX_LISTING_PRICE_UNITS + 1, &mut req_over_isk);
    encode_bytes_field(3, &lic_ok, &mut req_over_isk);
    put_seconds(4, 86_400, &mut req_over_isk);
    encode_varint_field(5, 1, &mut req_over_isk);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_over_isk,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let mut req_pub_create = Vec::new();
    encode_isk_msg(1, 1_000_000, &mut req_pub_create);
    encode_bytes_field(3, &lic_ok, &mut req_pub_create);
    put_seconds(4, 86_400, &mut req_pub_create);
    encode_varint_field(5, 3, &mut req_pub_create);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_pub_create,
            now,
        )
        .status_code,
        STATUS_OK
    );

    let mut req_corp_create = Vec::new();
    encode_isk_msg(2, 2_500, &mut req_corp_create);
    encode_bytes_field(3, &lic_ok, &mut req_corp_create);
    put_seconds(4, 86_400, &mut req_corp_create);
    encode_varint_field(5, 2, &mut req_corp_create);
    let mut target_corp = Vec::new();
    put_seq(2, corp_a as u64, &mut target_corp);
    encode_bytes_field(6, &target_corp, &mut req_corp_create);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_corp_create,
            now + 1,
        )
        .status_code,
        STATUS_OK
    );

    {
        let sd = skinr.data.read().unwrap();
        let lic = sd
            .skin_licenses
            .get(&SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex))
            .unwrap();
        assert_eq!(lic.unactivated, 5);
        assert_eq!(lic.escrowed, 5);
    }
    db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&seller).unwrap().balance, IskCents(5_000_000_00 - 150_000_00));
    });

    let r_owned = call(
        &router,
        &seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
        now + 2,
    );
    let wm_owned = WireMessage::decode(&r_owned.payload.unwrap().value).unwrap();
    let owned_entries: Vec<([u8; 16], bool)> = wm_owned
        .fields
        .iter()
        .filter(|f| f.field_number == 1)
        .map(|f| {
            let entry = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
            let u: [u8; 16] = wm_bytes(&wm_sub(&entry, 1), 1).try_into().unwrap();
            let attrs = wm_sub(&entry, 2);
            let branded = wm_varint(&attrs, 10) != 0;
            (u, branded)
        })
        .collect();
    assert_eq!(owned_entries.len(), 2);
    let pub_uuid = owned_entries.iter().find(|x| !x.1).unwrap().0;
    let corp_uuid = owned_entries.iter().find(|x| x.1).unwrap().0;

    let mut get_corp_req = Vec::new();
    put_uuid(1, &corp_uuid, &mut get_corp_req);
    assert_eq!(
        call(
            &router,
            &outsider_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_corp_req.clone(),
            now + 2,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_corp_req.clone(),
            now + 2,
        )
        .status_code,
        STATUS_OK
    );

    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            get_corp_req.clone(),
            now + 3,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            get_corp_req.clone(),
            now + 3,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            get_corp_req,
            now + 3,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    let mut buy_self = Vec::new();
    put_uuid(1, &pub_uuid, &mut buy_self);
    encode_varint_field(2, 1, &mut buy_self);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_self,
            now + 4,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let mut buy_bad_ver = Vec::new();
    put_uuid(1, &pub_uuid, &mut buy_bad_ver);
    encode_varint_field(2, 1, &mut buy_bad_ver);
    encode_string_field(3, "v0.0.0", &mut buy_bad_ver);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_bad_ver,
            now + 4,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let mut buy_one = Vec::new();
    put_uuid(1, &pub_uuid, &mut buy_one);
    encode_varint_field(2, 1, &mut buy_one);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_one);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_one,
            now + 5,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(state.data.read().unwrap().skin_listings.get(&hex(&pub_uuid)).unwrap().quantity, 2);
    db.with_snapshot(|s| {
        let b = s.characters.get(&buyer_ok).unwrap();
        assert_eq!(b.balance, IskCents(50_000_000_00 - 1_000_000_00));
        assert_eq!(b.loyalty_points.get(&(PARAGON_CORP_ID as i64)).copied(), Some(98_000));
        let sl = s.characters.get(&seller).unwrap();
        assert_eq!(sl.balance, IskCents(5_000_000_00 - 150_000_00 + 800_000_00));
    });
    assert_eq!(
        skinr
            .data
            .read()
            .unwrap()
            .skin_licenses
            .get(&SkinrCosmeticsState::skin_lic_key(buyer_ok as u32, skin_hex))
            .unwrap()
            .unactivated,
        1
    );

    let mut req_ally_plex = Vec::new();
    encode_isk_msg(2, 2_500, &mut req_ally_plex);
    encode_bytes_field(3, &lic_ok, &mut req_ally_plex);
    put_seconds(4, 86_400, &mut req_ally_plex);
    encode_varint_field(5, 1, &mut req_ally_plex);
    let mut target_ally = Vec::new();
    put_seq(3, ally_a as u64, &mut target_ally);
    encode_bytes_field(6, &target_ally, &mut req_ally_plex);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_ally_plex,
            now + 6,
        )
        .status_code,
        STATUS_OK
    );
    let ally_uuid = state
        .data
        .read()
        .unwrap()
        .skin_listings
        .values()
        .find(|l| matches!(l.target, ListingTargetRecord::Alliance(_)))
        .unwrap()
        .uuid;

    let mut buy_ally = Vec::new();
    put_uuid(1, &ally_uuid, &mut buy_ally);
    encode_varint_field(2, 1, &mut buy_ally);
    assert_eq!(
        call(
            &router,
            &outsider_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_ally.clone(),
            now + 7,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_ally,
            now + 7,
        )
        .status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&buyer_ok).unwrap().plex_balance, 475);
        assert_eq!(s.characters.get(&seller).unwrap().plex_balance, 43);
        assert_eq!(
            s.characters
                .get(&buyer_ok)
                .unwrap()
                .loyalty_points
                .get(&(PARAGON_CORP_ID as i64))
                .copied(),
            Some(98_000 - 5_000)
        );
    });

    // Query active skin listings via GetAllRequest and GetAllOwnedRequest while pub_uuid is active.
    assert_eq!(
        call(&router, &buyer_sess, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), now + 7).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &seller_sess, "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest", Vec::new(), now + 7).status_code,
        STATUS_OK
    );

    db.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&buyer_ok)
            .unwrap()
            .loyalty_points
            .insert(PARAGON_CORP_ID as i64, 100);
    });
    let mut buy_no_lp = Vec::new();
    put_uuid(1, &pub_uuid, &mut buy_no_lp);
    encode_varint_field(2, 2, &mut buy_no_lp);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_no_lp,
            now + 7,
        )
        .status_code,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(state.data.read().unwrap().skin_listings.get(&hex(&pub_uuid)).unwrap().quantity, 2);

    let mut set_exp = Vec::new();
    put_uuid(1, &pub_uuid, &mut set_exp);
    put_seconds(2, now + 10, &mut set_exp);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            set_exp.clone(),
            now + 8,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            set_exp,
            now + 8,
        )
        .status_code,
        STATUS_OK
    );

    let mut admin_dyn = DynamicQuasarMessage::new("eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest");
    let mut u_msg = DynamicQuasarMessage::new("Uuid");
    u_msg.set(1, DynamicValue::Bytes(pub_uuid.to_vec()));
    admin_dyn.set(1, DynamicValue::Message(Box::new(u_msg)));
    let mut ts_msg = DynamicQuasarMessage::new("Duration");
    ts_msg.set(1, DynamicValue::Uint64(now + 9));
    admin_dyn.set(2, DynamicValue::Message(Box::new(ts_msg)));
    let mut admin_ctx = AuthoritativeContext::for_session_at(
        Some(admin_cid as i32),
        Some(admin_cid as u32),
        Some(98_000_001),
        service_roles::ROLE_GM_DEFAULT,
        "default",
        Vec::new(),
        now + 9,
    );
    admin_ctx.service_role_mask = service_roles::ROLE_GM_DEFAULT;
    assert!(skin_admin_set_expiration_eve(&state, &admin_ctx, &admin_dyn).is_ok());

    let mut buy_expired = Vec::new();
    put_uuid(1, &pub_uuid, &mut buy_expired);
    encode_varint_field(2, 1, &mut buy_expired);
    assert_eq!(
        call(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_expired,
            now + 10,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
}
