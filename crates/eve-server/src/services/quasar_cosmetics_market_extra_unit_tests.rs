#![forbid(unsafe_code)]

use super::quasar_cosmetics_market::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_OK,
    STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_cosmetics_skinr::{
    SkinDesignRecord, SkinrCosmeticsData, SkinrCosmeticsState, ThirdPartySkinLicense,
    SKINR_BLOB_KEY,
};
use eve_db::{IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

pub(super) fn put_seconds(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

pub(super) fn put_uuid(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

pub(super) fn encode_isk_msg(field: u32, units: i64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, encode_zigzag64(units), &mut b);
    encode_varint_field(2, 0, &mut b);
    encode_bytes_field(field, &b, out);
}

pub(super) fn encode_plex_msg(field: u32, cents: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, cents, &mut b);
    encode_bytes_field(field, &b, out);
}

pub(super) fn hex_of(u: &[u8]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

pub(super) fn make_char(
    db: &WorldRepository,
    cid: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    isk_cents: i64,
    plex: i64,
    evermarks: i64,
) {
    db.with_mut_snapshot(|s| {
        if let Some(aid) = alliance_id {
            let tmpl_corp = s.corporations.values().next().expect("demo corp").clone();
            let corp = s.corporations.entry(corp_id).or_insert_with(|| {
                let mut c = tmpl_corp;
                c.corporation_id = corp_id;
                c
            });
            corp.alliance_id = Some(aid);
        }
        let mut ch = s.characters.values().next().expect("demo char").clone();
        ch.character_id = cid;
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.balance = IskCents(isk_cents);
        ch.plex_balance = plex;
        ch.home_station_id = 60_003_760;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.loyalty_points.clear();
        if evermarks > 0 {
            ch.loyalty_points.insert(PARAGON_CORP_ID as i64, evermarks);
        }
        s.characters.insert(cid, ch);
    });
}

pub(super) fn make_sess(cid: i64, corp_id: i64, alliance_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(cid as u64);
    s.user_id = Some(cid as u64);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s
}

pub(super) fn call(
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
        correlation_uuid: vec![7; 16],
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
fn test_load_register_seed_fallback_and_skin_tier_blob_fallback() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut empty_sde = SdeRegistry::default();
    empty_sde.skin_licenses.insert(
        40_001,
        eve_sde::SkinLicenseInfo {
            license_type_id: 40_001,
            skin_id: 1,
            duration: 30,
        },
    );
    empty_sde.skin_licenses.insert(
        40_002,
        eve_sde::SkinLicenseInfo {
            license_type_id: 40_002,
            skin_id: 2,
            duration: 30,
        },
    );
    let mut data = CosmeticsMarketData::default();
    assert!(seed_component_listings_if_empty(&mut data, &empty_sde));
    assert_eq!(data.component_listings.len(), 4);
    assert_eq!(data.next_seq, 10);
    // Calling a second time returns false when already populated
    assert!(!seed_component_listings_if_empty(&mut data, &empty_sde));

    let sde = shared_sde();
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let state = CosmeticsMarketState::load(Arc::clone(&db), Arc::clone(&sde), Arc::clone(&skinr));
    let seeded_len = state.data.read().unwrap().component_listings.len();
    assert!(seeded_len == 4 || seeded_len >= 1_000, "unexpected seeded component_listings len: {seeded_len}");

    // Test skin_tier_for_hex fallback to SKINR_BLOB_KEY in db when skinr.data in-memory doesn't have it
    let mut blob_data = SkinrCosmeticsData::default();
    blob_data.skins.insert(
        "deadbeef00112233".into(),
        SkinDesignRecord {
            skin_hex: "deadbeef00112233".into(),
            name: "BlobSkin".into(),
            line_name: "Line".into(),
            creator_id: 1001,
            ship_type_id: 587,
            pattern_blend_mode: 0,
            slots: Vec::new(),
            tier_level: 4,
            first_sequenced_unix: None,
        },
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            SKINR_BLOB_KEY.into(),
            serde_json::to_string(&blob_data).unwrap(),
        );
    });
    assert_eq!(state.skin_tier_for_hex("deadbeef00112233"), Some(4));
    assert_eq!(state.skin_tier_for_hex("missing_hex"), None);

    // Also verify public `register(&router, db, sde, skinr)` entry point
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register(&router, Arc::clone(&db), Arc::clone(&sde), Arc::clone(&skinr));
    let sess = make_sess(2_112_000_500, 1_000_009, None, 0);
    let resp = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
        Vec::new(),
        1_700_000_000,
    );
    assert_eq!(resp.status_code, STATUS_OK);
}

#[test]
fn test_skin_create_listing_error_and_broker_fee_rollback_branches() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let state = CosmeticsMarketState::load_with_sink(
        Arc::clone(&db),
        Arc::clone(&sde),
        Arc::clone(&skinr),
        Arc::clone(&router.macho_notify_sink),
    );
    register_with_state(&router, Arc::clone(&state));

    let now = 1_700_000_000u64;
    let seller = 2_112_000_601i64;
    let corp_a = 98_000_601i64;
    // Give seller 0 ISK initially so broker fee fails and triggers rollback
    make_char(&db, seller, corp_a, None, 0, 100, 50_000);
    let seller_sess = make_sess(seller, corp_a, None, 0);

    let skin_hex = "aabbccdd99887766";
    {
        let mut sd = skinr.data.write().unwrap();
        sd.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "RollbackSkin".into(),
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

    let mut shex_msg = Vec::new();
    encode_string_field(1, skin_hex, &mut shex_msg);
    let mut lic_ok = Vec::new();
    put_seq(1, seller as u64, &mut lic_ok);
    encode_bytes_field(2, &shex_msg, &mut lic_ok);

    // 1. Duration == 0 -> STATUS_BAD_REQUEST
    let mut req_zero_dur = Vec::new();
    encode_isk_msg(1, 100_000, &mut req_zero_dur);
    encode_bytes_field(3, &lic_ok, &mut req_zero_dur);
    put_seconds(4, 0, &mut req_zero_dur);
    encode_varint_field(5, 1, &mut req_zero_dur);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_zero_dur,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 2. Quantity == 0 -> STATUS_BAD_REQUEST
    let mut req_zero_qty = Vec::new();
    encode_isk_msg(1, 100_000, &mut req_zero_qty);
    encode_bytes_field(3, &lic_ok, &mut req_zero_qty);
    put_seconds(4, 86_400, &mut req_zero_qty);
    encode_varint_field(5, 0, &mut req_zero_qty);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_zero_qty,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 3. PLEX price > MAX_LISTING_PRICE_UNITS -> STATUS_BAD_REQUEST
    let mut req_over_plex = Vec::new();
    encode_plex_msg(2, MAX_LISTING_PRICE_UNITS + 1, &mut req_over_plex);
    encode_bytes_field(3, &lic_ok, &mut req_over_plex);
    put_seconds(4, 86_400, &mut req_over_plex);
    encode_varint_field(5, 1, &mut req_over_plex);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_over_plex,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 4. Insufficient unactivated licenses (request 10 when only 5 available) -> STATUS_PRECONDITION_FAILED
    let mut req_insuff_lic = Vec::new();
    encode_isk_msg(1, 100_000, &mut req_insuff_lic);
    encode_bytes_field(3, &lic_ok, &mut req_insuff_lic);
    put_seconds(4, 86_400, &mut req_insuff_lic);
    encode_varint_field(5, 10, &mut req_insuff_lic);
    assert_eq!(
        call(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            req_insuff_lic,
            now,
        )
        .status_code,
        STATUS_PRECONDITION_FAILED
    );

    // 5. Insufficient ISK for broker fee -> returns STATUS_OK with failed transaction notice and rolls back escrow
    let mut req_no_isk = Vec::new();
    encode_isk_msg(1, 100_000, &mut req_no_isk);
    encode_bytes_field(3, &lic_ok, &mut req_no_isk);
    put_seconds(4, 86_400, &mut req_no_isk);
    encode_varint_field(5, 2, &mut req_no_isk);
    let resp = call(
        &router,
        &seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        req_no_isk,
        now,
    );
    assert_eq!(resp.status_code, STATUS_OK);
    assert!(state.data.read().unwrap().skin_listings.is_empty());
    let lic_after = skinr
        .data
        .read()
        .unwrap()
        .skin_licenses
        .get(&SkinrCosmeticsState::skin_lic_key(seller as u32, skin_hex))
        .unwrap()
        .clone();
    assert_eq!(lic_after.unactivated, 5);
    assert_eq!(lic_after.escrowed, 0);
    assert_eq!(
        state
            .data
            .read()
            .unwrap()
            .transactions
            .values()
            .last()
            .unwrap()
            .status,
        2
    );
}
