//! Component listing purchase, error path, and overflow unit tests for `quasar_cosmetics_market`.

#![forbid(unsafe_code)]

use super::super::*;
use super::{call, make_char, make_sess, new_market_harness, wm_bytes, wm_sub, wm_varint};
use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK};
use eve_db::IskCents;
use eve_proto::quasar::{encode_varint_field, WireMessage};

#[test]
fn component_listings_get_all_purchase_isk_plex_failures_and_overflow_bounds() {
    let (_router, db, _skinr, state) = new_market_harness();
    let now = 1_700_000_000u64;
    let cid = 2_112_000_201i64;
    make_char(&db, cid, 98_000_001, None, 1_000_000_00, 50, 0);
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let router = &_router;

    let sess = make_sess(cid, 98_000_001, None, 0);
    let r_all = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_all.status_code, STATUS_OK);
    let wm_all = WireMessage::decode(&r_all.payload.unwrap().value).unwrap();
    let uuids: Vec<[u8; 16]> = wm_all
        .fields
        .iter()
        .filter(|f| f.field_number == 1)
        .map(|f| {
            let entry = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
            wm_bytes(&wm_sub(&entry, 1), 1).try_into().unwrap()
        })
        .collect();
    assert_eq!(uuids.len(), 4);

    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut bad_qty = Vec::new();
    put_uuid(1, &uuids[0], &mut bad_qty);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            bad_qty,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut unknown_req = Vec::new();
    put_uuid(1, &[0xFF; 16], &mut unknown_req);
    encode_varint_field(2, 1, &mut unknown_req);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            unknown_req,
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    let mut buy_isk = Vec::new();
    put_uuid(1, &uuids[0], &mut buy_isk);
    encode_varint_field(2, 2, &mut buy_isk);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            buy_isk,
            now,
        )
        .status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        let ch = s.characters.get(&cid).unwrap();
        assert_eq!(ch.balance, IskCents(1_000_000_00 - 500_000_00));
        assert_eq!(
            s.items
                .values()
                .filter(|it| it.owner_id == cid && it.quantity == 2)
                .count(),
            1
        );
    });

    let mut buy_plex = Vec::new();
    put_uuid(1, &uuids[2], &mut buy_plex);
    encode_varint_field(2, 1, &mut buy_plex);
    encode_varint_field(3, 1, &mut buy_plex);
    let r_buy_plex = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        buy_plex,
        now + 1,
    );
    assert_eq!(r_buy_plex.status_code, STATUS_OK);
    let wm_bp = WireMessage::decode(&r_buy_plex.payload.unwrap().value).unwrap();
    assert_eq!(wm_varint(&wm_bp, 2), 1);
    db.with_snapshot(|s| {
        assert_eq!(s.unified_plex_balance(cid), 40);
    });

    let mut fail_plex = Vec::new();
    put_uuid(1, &uuids[3], &mut fail_plex);
    encode_varint_field(2, 2, &mut fail_plex);
    let r_fail_plex = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        fail_plex,
        now + 2,
    );
    assert_eq!(r_fail_plex.status_code, STATUS_OK);
    let fail_tx: [u8; 16] = wm_bytes(
        &wm_sub(&WireMessage::decode(&r_fail_plex.payload.unwrap().value).unwrap(), 1),
        1,
    )
    .try_into()
    .unwrap();
    let saved_blob: CosmeticsMarketData = db.with_snapshot(|s| {
        serde_json::from_str(s.quasar_state_blobs.get(BLOB_KEY).unwrap()).unwrap()
    });
    assert_eq!(saved_blob.transactions.get(&hex(&fail_tx)).unwrap().status, 2);

    let expired_uuid = [0x11u8; 16];
    let overflow_isk_uuid = [0x12u8; 16];
    let overflow_plex_uuid = [0x13u8; 16];
    let overflow_qty_uuid = [0x14u8; 16];
    let mut mut_blob = saved_blob;
    mut_blob.component_listings.insert(
        hex(&expired_uuid),
        ComponentListingRecord {
            uuid: expired_uuid,
            sale_end_unix: now,
            price_isk: Some(1_000),
            price_plex_cents: None,
            component_item_type_id: 82_340,
            bundle_size: 1,
            discount_percent: 0,
        },
    );
    mut_blob.component_listings.insert(
        hex(&overflow_isk_uuid),
        ComponentListingRecord {
            uuid: overflow_isk_uuid,
            sale_end_unix: now + 3_600,
            price_isk: Some(MAX_LISTING_PRICE_UNITS + 1),
            price_plex_cents: None,
            component_item_type_id: 82_340,
            bundle_size: 1,
            discount_percent: 0,
        },
    );
    mut_blob.component_listings.insert(
        hex(&overflow_plex_uuid),
        ComponentListingRecord {
            uuid: overflow_plex_uuid,
            sale_end_unix: now + 3_600,
            price_isk: None,
            price_plex_cents: Some(MAX_LISTING_PRICE_UNITS),
            component_item_type_id: 82_340,
            bundle_size: 1,
            discount_percent: 0,
        },
    );
    mut_blob.component_listings.insert(
        hex(&overflow_qty_uuid),
        ComponentListingRecord {
            uuid: overflow_qty_uuid,
            sale_end_unix: now + 3_600,
            price_isk: Some(1_000),
            price_plex_cents: None,
            component_item_type_id: 82_340,
            bundle_size: u64::MAX / 2,
            discount_percent: 0,
        },
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert(BLOB_KEY.into(), serde_json::to_string(&mut_blob).unwrap());
    });
    state.data.write().unwrap().component_listings.clear();

    for (u, qty) in [
        (expired_uuid, 1u64),
        (overflow_isk_uuid, 1u64),
        (overflow_plex_uuid, 2u64),
        (overflow_qty_uuid, 3u64),
    ] {
        let mut req = Vec::new();
        put_uuid(1, &u, &mut req);
        encode_varint_field(2, qty, &mut req);
        assert_eq!(
            call(
                &router,
                &sess,
                "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
                req,
                now + 3,
            )
            .status_code,
            STATUS_BAD_REQUEST
        );
    }
}
