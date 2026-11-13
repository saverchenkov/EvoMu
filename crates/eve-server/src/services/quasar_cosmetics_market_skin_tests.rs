//! SKIN listing creation, cancellation, visibility, purchase settlement, escrow rollback,
//! and admin expiration unit tests for `quasar_cosmetics_market`.

#![forbid(unsafe_code)]

use super::super::*;
use super::{call, make_char, make_sess, new_market_harness, wm_bytes, wm_sub, wm_varint};
use crate::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_cosmetics_skinr::{
    SequencingJobRecord, SkinDesignRecord, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use eve_db::IskCents;
use eve_machonet::service_roles;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleDuration, GoogleTimestamp,
    WireMessage,
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

    let req_foreign_owner = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_isk.units", 500_000i64)
        .set("price_isk.nanos", 0i32)
        .set("skin.character.sequential", 999_999u32)
        .set("skin.skin.hex", skin_hex)
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 1u32)
        .must_encode();
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

    let req_over_isk = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_isk.units", (MAX_LISTING_PRICE_UNITS + 1) as i64)
        .set("price_isk.nanos", 0i32)
        .set("skin.character.sequential", seller as u32)
        .set("skin.skin.hex", skin_hex)
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 1u32)
        .must_encode();
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

    let req_pub_create = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_isk.units", 1_000_000i64)
        .set("price_isk.nanos", 0i32)
        .set("skin.character.sequential", seller as u32)
        .set("skin.skin.hex", skin_hex)
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 3u32)
        .must_encode();
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

    let req_corp_create = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_plex.total_in_cents", 2_500i64)
        .set("skin.character.sequential", seller as u32)
        .set("skin.skin.hex", skin_hex)
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 2u32)
        .set("target.corporation.sequential", corp_a as u32)
        .must_encode();
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
        assert_eq!(s.characters.get(&seller).unwrap().balance, IskCents(5_000_000_00 - 2_600_000_00));
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

    let get_corp_req = pb::message("eve_public.cosmetic.market.skin.listing.api.GetRequest")
        .set("listing.uuid", corp_uuid.to_vec())
        .must_encode();
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

    let buy_self = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("quantity", 1u32)
        .must_encode();
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

    let buy_bad_ver = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("quantity", 1u32)
        .set("buyer_fees_version", "v0.0.0")
        .must_encode();
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

    let buy_one = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("quantity", 1u32)
        .set("buyer_fees_version", BUYER_FEES_VERSION)
        .must_encode();
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
        assert_eq!(sl.balance, IskCents(5_000_000_00 - 2_600_000_00 + 800_000_00));
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

    let req_ally_plex = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_plex.total_in_cents", 2_500i64)
        .set("skin.character.sequential", seller as u32)
        .set("skin.skin.hex", skin_hex)
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 1u32)
        .set("target.alliance.sequential", ally_a as i32)
        .must_encode();
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

    let buy_ally = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", ally_uuid.to_vec())
        .set("quantity", 1u32)
        .must_encode();
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
        assert_eq!(s.unified_plex_balance(buyer_ok), 475);
        assert_eq!(s.unified_plex_balance(seller), 43);
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
    let buy_no_lp = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("quantity", 2u32)
        .must_encode();
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

    let set_exp = pb::message("eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("expires", GoogleTimestamp { seconds: (now + 10) as i64, nanos: 0 })
        .must_encode();
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

    let buy_expired = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", pub_uuid.to_vec())
        .set("quantity", 1u32)
        .must_encode();
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

#[test]
fn issue_545_initiate_create_listing_settles_matured_sequencing_job() {
    let (router, db, skinr, _state) = new_market_harness();
    let now = 1_700_000_000u64;
    let seller = 2_112_100_991i64;
    let corp = 98_000_991i64;
    make_char(&db, seller, corp, None, 50_000_000_00, 0, 0);

    let skin_hex = "aabbccddeeff00112233445566778899".to_string();
    let job_uuid = [0x42u8; 16];
    let job_key: String = job_uuid.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.clone(),
            SkinDesignRecord {
                skin_hex: skin_hex.clone(),
                name: "MaturedUnpolledSkin".into(),
                line_name: "Line".into(),
                creator_id: seller as u32,
                ship_type_id: 587,
                pattern_blend_mode: 0,
                slots: Vec::new(),
                tier_level: 1,
                first_sequenced_unix: None,
            },
        );
        sd.sequencing_jobs.insert(
            job_key.clone(),
            SequencingJobRecord {
                job_uuid,
                sequencer_id: seller as u32,
                skin_hex: skin_hex.clone(),
                quantity: 2,
                initiated_unix: now - 3_600,
                planned_duration_secs: 1_800,
                completed_unix: None,
                failed_reason: None,
            },
        );
        // Explicitly verify no license was pre-granted before Paragon Hub listing request
        assert!(!sd.skin_licenses.contains_key(&SkinrCosmeticsState::skin_lic_key(seller as u32, &skin_hex)));
    }

    let seller_sess = make_sess(seller, corp, None, 0);
    let req = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_isk.units", 1_000_000i64)
        .set("price_isk.nanos", 0i32)
        .set("skin.character.sequential", seller as u32)
        .set("skin.skin.hex", skin_hex.as_str())
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 1u32)
        .must_encode();

    let rsp = call(
        &router,
        &seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        req,
        now,
    );
    assert_eq!(rsp.status_code, STATUS_OK);

    let sd = skinr.data.read().unwrap();
    let job = &sd.sequencing_jobs[&job_key];
    assert_eq!(job.completed_unix, Some(now - 1_800));
    let lic = &sd.skin_licenses[&SkinrCosmeticsState::skin_lic_key(seller as u32, &skin_hex)];
    assert_eq!(lic.unactivated, 1);
    assert_eq!(lic.escrowed, 1);
}
