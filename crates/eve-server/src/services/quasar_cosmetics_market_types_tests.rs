//! Unit tests for `quasar_cosmetics_market_types`.

#![forbid(unsafe_code)]

use super::*;

#[test]
fn listing_target_is_branded_and_default() {
    assert_eq!(ListingTargetRecord::default(), ListingTargetRecord::Public);
    assert!(!ListingTargetRecord::Public.is_branded());
    assert!(ListingTargetRecord::Corporation(98_000_001).is_branded());
    assert!(ListingTargetRecord::Alliance(99_000_001).is_branded());
    assert!(ListingTargetRecord::Character(90_000_001).is_branded());
}

#[test]
fn constants_and_durations_match_paragon_hub_spec() {
    assert_eq!(PARAGON_CORP_ID, 1_000_419);
    assert_eq!(GROUP_SHIP_SKIN_DESIGN_ELEMENT, 4726);
    assert_eq!(BUYER_FEES_VERSION, "v24.01.1");
    assert_eq!(MAX_LISTING_PRICE_UNITS, 10_000_000_000_000);
    assert_eq!(MAX_LISTING_QUANTITY, 10_000);
    assert_eq!(
        LISTING_DURATIONS_SECS,
        [
            (86_400, 100_000),
            (259_200, 200_000),
            (604_800, 350_000),
            (1_209_600, 600_000),
            (2_592_000, 1_000_000),
            (7_776_000, 2_500_000),
        ]
    );
}

#[test]
fn record_tx_persists_status_and_serde_roundtrip() {
    let mut data = CosmeticsMarketData::default();
    let u1 = [0x11u8; 16];
    let u2 = [0x22u8; 16];

    data.record_tx(
        "tx1".to_string(),
        u1,
        1_760_000_100,
        true,
        Some(90_000_001),
        Some(90_000_002),
        CosmeticTransactionDetails::PurchasingSkin,
    );
    data.record_tx(
        "tx2".to_string(),
        u2,
        1_760_000_200,
        false,
        Some(90_000_003),
        None,
        CosmeticTransactionDetails::PurchasingComponent,
    );

    let r1 = &data.transactions["tx1"];
    assert_eq!(r1.uuid, u1);
    assert_eq!(r1.last_updated_unix, 1_760_000_100);
    assert_eq!(r1.status, 3);
    assert_eq!(r1.buyer_id, Some(90_000_001));
    assert_eq!(r1.seller_id, Some(90_000_002));
    assert_eq!(r1.details, CosmeticTransactionDetails::PurchasingSkin);

    let r2 = &data.transactions["tx2"];
    assert_eq!(r2.uuid, u2);
    assert_eq!(r2.last_updated_unix, 1_760_000_200);
    assert_eq!(r2.status, 2);
    assert_eq!(r2.buyer_id, Some(90_000_003));
    assert_eq!(r2.seller_id, None);
    assert_eq!(r2.details, CosmeticTransactionDetails::PurchasingComponent);

    data.component_listings.insert(
        "comp1".to_string(),
        ComponentListingRecord {
            uuid: [0x33; 16],
            sale_end_unix: 1_770_000_000,
            price_isk: Some(500_000),
            price_plex_cents: None,
            component_item_type_id: 75_000,
            bundle_size: 5,
            discount_percent: 10,
        },
    );
    data.skin_listings.insert(
        "skin1".to_string(),
        SkinListingRecord {
            uuid: [0x44; 16],
            seller_id: 90_000_001,
            created_unix: 1_760_000_000,
            expires_unix: 1_760_086_400,
            price_isk: None,
            price_plex_cents: Some(2_500),
            skin_hex: "skin_deadbeef".to_string(),
            quantity: 2,
            target: ListingTargetRecord::Corporation(98_000_001),
            branded: true,
        },
    );

    let json = serde_json::to_string(&data).expect("serialize CosmeticsMarketData");
    let decoded: CosmeticsMarketData = serde_json::from_str(&json).expect("deserialize CosmeticsMarketData");
    assert_eq!(decoded.transactions["tx1"], *r1);
    assert_eq!(decoded.transactions["tx2"], *r2);
    assert_eq!(decoded.component_listings["comp1"], data.component_listings["comp1"]);
    assert_eq!(decoded.skin_listings["skin1"], data.skin_listings["skin1"]);
}
