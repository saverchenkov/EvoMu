//! Edge-case behavioral tests for `quasar_cosmetics_market`.

#![forbid(unsafe_code)]

use super::quasar_cosmetics_market_tests::{call, make_char, make_sess, new_market_harness, shared_sde};
use super::*;
use crate::quasar_gateway::{STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use crate::services::quasar_cosmetics_skinr::{
    SkinDesignRecord, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use eve_db::{IskCents, TrainedSkillRecord};
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, WireMessage,
};

fn encode_isk_units(field: u32, units: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, units, &mut sub);
    encode_bytes_field(field, &sub, out);
}

fn encode_uuid_msg(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    encode_bytes_field(field, &sub, out);
}

fn encode_seconds_msg(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, secs, &mut sub);
    encode_bytes_field(field, &sub, out);
}

fn encode_license_ref(field: u32, owner_id: u64, skin_hex: &str, out: &mut Vec<u8>) {
    let mut shex = Vec::new();
    encode_string_field(1, skin_hex, &mut shex);
    let mut char_sub = Vec::new();
    encode_varint_field(1, owner_id, &mut char_sub);
    let mut lic = Vec::new();
    encode_bytes_field(1, &char_sub, &mut lic);
    encode_bytes_field(2, &shex, &mut lic);
    encode_bytes_field(field, &lic, out);
}

#[test]
fn purge_revoked_skin_listings_clears_db_and_registered_live_state() {
    let (_router, db, _skinr, state) = new_market_harness();
    let uuid_target = [0x11; 16];
    let uuid_other_skin = [0x22; 16];
    let uuid_other_seller = [0x33; 16];

    {
        let mut guard = state.data.write().unwrap();
        for (uuid, seller_id, hex_str) in [
            (uuid_target, 9001u32, "revoked_hex"),
            (uuid_other_skin, 9001u32, "kept_hex"),
            (uuid_other_seller, 9002u32, "revoked_hex"),
        ] {
            guard.skin_listings.insert(
                hex(&uuid),
                SkinListingRecord {
                    uuid,
                    seller_id,
                    skin_hex: hex_str.into(),
                    price_isk: Some(1_000_000),
                    price_plex_cents: None,
                    quantity: 1,
                    target: ListingTargetRecord::Public,
                    created_unix: 1_700_000_000,
                    expires_unix: 1_700_086_400,
                    branded: false,
                },
            );
        }
        let encoded = serde_json::to_string(&*guard).unwrap();
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(BLOB_KEY.into(), encoded);
        });
    }

    // No match leaves all 3 intact
    purge_revoked_skin_listings(&db, 9999, "revoked_hex");
    assert_eq!(state.data.read().unwrap().skin_listings.len(), 3);

    // Matching (9001, "revoked_hex") removes from both DB blob and live state RwLock
    purge_revoked_skin_listings(&db, 9001, "revoked_hex");

    {
        let guard = state.data.read().unwrap();
        assert!(!guard.skin_listings.contains_key(&hex(&uuid_target)));
        assert!(guard.skin_listings.contains_key(&hex(&uuid_other_skin)));
        assert!(guard.skin_listings.contains_key(&hex(&uuid_other_seller)));
    }
    db.with_snapshot(|s| {
        let blob = &s.quasar_state_blobs[BLOB_KEY];
        let parsed: CosmeticsMarketData = serde_json::from_str(blob).unwrap();
        assert!(!parsed.skin_listings.contains_key(&hex(&uuid_target)));
        assert!(parsed.skin_listings.contains_key(&hex(&uuid_other_skin)));
    });
}

#[test]
fn valuation_broker_fees_skill_reductions_and_extract_skin_hex_edges() {
    // 0 valuation hits 100k public floor and 50k branded floor
    assert_eq!(broker_fee_for_valuation(0, 86_400, &ListingTargetRecord::Public), 100_000);
    assert_eq!(broker_fee_for_valuation(0, 86_400, &ListingTargetRecord::Character(1)), 50_000);

    // <= 86_400s tier -> 200 bps (2%) -> 2% of 50_000_000 = 1_000_000; branded 50% = 500_000
    assert_eq!(
        broker_fee_for_valuation(50_000_000, 12_345, &ListingTargetRecord::Public),
        1_000_000
    );
    assert_eq!(
        broker_fee_for_valuation(50_000_000, 12_345, &ListingTargetRecord::Corporation(55)),
        500_000
    );
    // 90d duration (7_776_000) -> 500 bps = 5% of 100_000_000 = 5_000_000
    assert_eq!(
        broker_fee_for_valuation(100_000_000, 7_776_000, &ListingTargetRecord::Public),
        5_000_000
    );

    // Skill reductions & Paragon Hub tax rates
    let (_router, db, _skinr, state) = new_market_harness();
    let cid = 90_000_001i64;
    db.with_mut_snapshot(|s| {
        let skills = s.skills.entry(cid).or_default();
        skills.insert(82665, TrainedSkillRecord { type_id: 82665, level: 5, skill_points: 256_000.0 });
        skills.insert(82664, TrainedSkillRecord { type_id: 82664, level: 5, skill_points: 256_000.0 });
    });
    let (tax_red, max_list) = paragon_hub_skill_reductions(&state, cid as u32);
    assert!(tax_red <= 15);
    assert!(max_list >= 5);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Character(1)), 30);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Corporation(1)), 10);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Alliance(1)), 10);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Public), 20u64.saturating_sub(tax_red));

    // extract_skin_hex validates owner seq == 0 or caller cid and decodes hex field 2
    let mut lic_ok = DynamicQuasarMessage::new("Lic");
    let mut shex_msg = DynamicQuasarMessage::new("SkinHex");
    shex_msg.set(1, DynamicValue::String("deadbeef".into()));
    lic_ok.set(1, DynamicValue::Uint64(0));
    lic_ok.set(2, DynamicValue::Message(Box::new(shex_msg)));
    assert_eq!(extract_skin_hex(Some(&lic_ok), 42).unwrap(), "deadbeef");

    let mut lic_wrong_owner = DynamicQuasarMessage::new("Lic");
    let mut owner_msg = DynamicQuasarMessage::new("CharId");
    owner_msg.set(1, DynamicValue::Uint64(999));
    lic_wrong_owner.set(1, DynamicValue::Message(Box::new(owner_msg)));
    assert_eq!(extract_skin_hex(Some(&lic_wrong_owner), 42).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(extract_skin_hex(None, 42).is_err());

    // seed_component_listings_if_empty returns false when already populated
    let mut non_empty = CosmeticsMarketData::default();
    non_empty.component_listings.insert(
        hex(&[1u8; 16]),
        ComponentListingRecord {
            uuid: [1u8; 16],
            sale_end_unix: 0,
            component_item_type_id: 82_400,
            bundle_size: 1,
            price_isk: Some(100),
            price_plex_cents: None,
            discount_percent: 0,
        },
    );
    assert!(!seed_component_listings_if_empty(&mut non_empty, &shared_sde()));
}

#[test]
fn skin_listing_fee_rollback_target_visibility_cancel_and_admin_edges() {
    let (router, db, skinr, state) = new_market_harness();
    let now = 1_700_000_000u64;
    let seller = 2_112_000_501i64;
    let same_corp_buyer = 2_112_000_502i64;
    let outsider = 2_112_000_503i64;
    let admin = 2_112_000_599i64;
    let corp_id = 98_000_501i64;
    let alliance_id = 99_000_501i64;

    // Seller initially has only 10_000 ISK (less than 50k branded / 100k public broker fee!)
    make_char(&db, seller, corp_id, Some(alliance_id), 10_000_00, 0, 0);
    make_char(&db, same_corp_buyer, corp_id, Some(alliance_id), 50_000_000_00, 100, 100_000);
    make_char(&db, outsider, 98_000_599, None, 50_000_000_00, 100, 100_000);
    make_char(&db, admin, 98_000_001, None, 1_000_000_00, 0, 0);

    let seller_sess = make_sess(seller, corp_id, Some(alliance_id), 0);
    let corp_buyer_sess = make_sess(same_corp_buyer, corp_id, Some(alliance_id), 0);
    let outsider_sess = make_sess(outsider, 98_000_599, None, 0);
    let admin_sess = make_sess(admin, 98_000_001, None, service_roles::ROLE_GM_DEFAULT);

    let skin_hex = "0011223344556677";
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "EdgeSkin".into(),
                line_name: "EdgeLine".into(),
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

    // Attempt to create listing with insufficient ISK for broker fee -> InitiateCreateListingResponse with rolled-back escrow/listing and failed tx
    let mut create_req = Vec::new();
    encode_isk_units(1, 500_000, &mut create_req);
    encode_license_ref(3, seller as u64, skin_hex, &mut create_req);
    encode_seconds_msg(4, 86_400, &mut create_req);
    encode_varint_field(5, 2, &mut create_req);
    let fail_create = call(
        &router,
        &seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_req.clone(),
        now,
    );
    assert_eq!(fail_create.status_code, STATUS_OK);
    {
        let sd = skinr.data.read().unwrap();
        let lic = &sd.skin_licenses[&SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex)];
        assert_eq!(lic.unactivated, 5);
        assert_eq!(lic.escrowed, 0);
    }
    {
        let data = state.data.read().unwrap();
        assert!(data.skin_listings.is_empty());
        assert!(data.transactions.values().any(|t| t.status == 2));
    }

    // Fund seller and create a corporation-targeted listing
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&seller).unwrap().balance = IskCents::from_isk(5_000_000);
    });
    let mut corp_sub = Vec::new();
    encode_varint_field(1, corp_id as u64, &mut corp_sub);
    let mut corp_target_msg = Vec::new();
    encode_bytes_field(2, &corp_sub, &mut corp_target_msg);
    let mut corp_create_req = create_req.clone();
    encode_bytes_field(6, &corp_target_msg, &mut corp_create_req);
    let ok_create = call(
        &router,
        &seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        corp_create_req,
        now,
    );
    assert_eq!(ok_create.status_code, STATUS_OK);
    let listing_uuid: [u8; 16] = {
        let guard = state.data.read().unwrap();
        guard.skin_listings.values().next().unwrap().uuid
    };

    // Outsider cannot Get targeted listing (STATUS_FORBIDDEN), while admin CAN bypass, and corp mate CAN view
    let mut get_req = Vec::new();
    encode_uuid_msg(1, &listing_uuid, &mut get_req);
    assert_eq!(
        call(
            &router,
            &outsider_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_req.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_req.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &corp_buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_req.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    // GetAllRequest visibility for outsider vs corp buyer
    let r_out_all = call(
        &router,
        &outsider_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
        now,
    );
    let wm_out = WireMessage::decode(&r_out_all.payload.unwrap().value).unwrap();
    assert_eq!(wm_out.fields.iter().filter(|f| f.field_number == 1).count(), 0);

    let r_corp_all = call(
        &router,
        &corp_buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
        now,
    );
    let wm_corp = WireMessage::decode(&r_corp_all.payload.unwrap().value).unwrap();
    assert_eq!(wm_corp.fields.iter().filter(|f| f.field_number == 1).count(), 1);

    // CancelListingRequest rejected for non-seller and missing UUID
    let mut cancel_req = Vec::new();
    encode_uuid_msg(1, &listing_uuid, &mut cancel_req);
    assert_eq!(
        call(
            &router,
            &outsider_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            cancel_req.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    let mut missing_cancel = Vec::new();
    encode_uuid_msg(1, &[0xFF; 16], &mut missing_cancel);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            missing_cancel,
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Admin SetExpirationRequest non-admin -> FORBIDDEN, missing UUID -> NOT_FOUND, eve. alias succeeds
    let mut exp_req = Vec::new();
    encode_uuid_msg(1, &listing_uuid, &mut exp_req);
    encode_seconds_msg(2, now + 3600, &mut exp_req);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            exp_req.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    let mut exp_missing = Vec::new();
    encode_uuid_msg(1, &[0xEE; 16], &mut exp_missing);
    encode_seconds_msg(2, now + 3600, &mut exp_missing);
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            exp_missing,
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    let admin_ok = call(
        &router,
        &admin_sess,
        "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
        exp_req,
        now,
    );
    assert_eq!(admin_ok.status_code, STATUS_OK);
    assert_eq!(
        state.data.read().unwrap().skin_listings[&hex(&listing_uuid)].expires_unix,
        now + 3600
    );

    // Seller cancels -> restores 2 escrowed licenses back to unactivated == 5
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            cancel_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    let sd = skinr.data.read().unwrap();
    let lic = &sd.skin_licenses[&SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex)];
    assert_eq!(lic.unactivated, 5);
    assert_eq!(lic.escrowed, 0);
}

#[test]
fn state_save_rehydrate_and_register_with_state_kill_mutants() {
    use eve_proto::quasar::QuasarDescriptorRegistry;

    let (_router, db, skinr, state) = new_market_harness();
    let custom_uuid = [0xAB; 16];
    let hex_key: String = custom_uuid.iter().map(|b| format!("{b:02x}")).collect();

    // 1. CosmeticsMarketState::save persists in-memory mutations back to DB BLOB_KEY
    {
        let mut guard = state.data.write().unwrap();
        guard.skin_listings.insert(
            hex_key.clone(),
            SkinListingRecord {
                uuid: custom_uuid,
                seller_id: 90_777,
                skin_hex: "deadbeef".into(),
                price_isk: Some(2_500_000),
                price_plex_cents: None,
                quantity: 3,
                target: ListingTargetRecord::Public,
                created_unix: 1_700_000_000,
                expires_unix: 1_700_086_400,
                branded: true,
            },
        );
    }
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.remove(BLOB_KEY);
    });
    state.save();
    db.with_snapshot(|s| {
        let blob = s
            .quasar_state_blobs
            .get(BLOB_KEY)
            .expect("CosmeticsMarketState::save must write BLOB_KEY");
        let parsed: CosmeticsMarketData = serde_json::from_str(blob).unwrap();
        assert!(parsed.skin_listings.contains_key(&hex_key));
    });

    // 2. CosmeticsMarketState::rehydrate_from_db reloads DB mutations into live `state.data`
    let uuid_2 = [0xCD; 16];
    let hex_key_2: String = uuid_2.iter().map(|b| format!("{b:02x}")).collect();
    db.with_mut_snapshot(|s| {
        let mut updated = CosmeticsMarketData::default();
        updated.skin_listings.insert(
            hex_key_2.clone(),
            SkinListingRecord {
                uuid: uuid_2,
                seller_id: 90_888,
                skin_hex: "cafebabe".into(),
                price_isk: None,
                price_plex_cents: Some(500),
                quantity: 1,
                target: ListingTargetRecord::Public,
                created_unix: 1_700_000_100,
                expires_unix: 1_700_086_500,
                branded: false,
            },
        );
        s.quasar_state_blobs
            .insert(BLOB_KEY.into(), serde_json::to_string(&updated).unwrap());
    });
    state.rehydrate_from_db();
    {
        let guard = state.data.read().unwrap();
        assert!(
            guard.skin_listings.contains_key(&hex_key_2),
            "rehydrate_from_db must load updated DB listings"
        );
        assert!(
            !guard.skin_listings.contains_key(&hex_key),
            "rehydrate_from_db must replace stale listings"
        );
    }

    // 3. register_with_state wires handlers onto a fresh PublicGatewayRouter
    let uuid_3 = [0xEF; 16];
    let hex_key_3: String = uuid_3.iter().map(|b| format!("{b:02x}")).collect();
    state.data.write().unwrap().skin_listings.insert(
        hex_key_3,
        SkinListingRecord {
            uuid: uuid_3,
            seller_id: 90_888,
            skin_hex: "deadc0de".into(),
            price_isk: None,
            price_plex_cents: Some(750),
            quantity: 1,
            target: ListingTargetRecord::Public,
            created_unix: 1_700_000_100,
            expires_unix: 1_700_086_500,
            branded: false,
        },
    );
    let mut get_3_id = Vec::new();
    encode_bytes_field(1, &uuid_3, &mut get_3_id);
    let mut get_3_req = Vec::new();
    encode_bytes_field(1, &get_3_id, &mut get_3_req);

    let fresh_router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        shared_sde(),
    );
    let sess = make_sess(90_888, 98_000_001, None, 0);
    assert_eq!(
        call(
            &fresh_router,
            &sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_3_req.clone(),
            1_700_000_200,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    register_with_state(&fresh_router, Arc::clone(&state));
    assert!(fresh_router
        .handlers
        .read()
        .unwrap()
        .contains_key("eve_public.cosmetic.market.api.GetBrokerFeesRequest"));
    assert_eq!(
        call(
            &fresh_router,
            &sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_3_req,
            1_700_000_200,
        )
        .status_code,
        STATUS_OK
    );
    let rsp = call(
        &fresh_router,
        &sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
        1_700_000_200,
    );
    assert_eq!(rsp.status_code, STATUS_OK);
    let _ = skinr;
}
