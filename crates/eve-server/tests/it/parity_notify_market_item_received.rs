//! Parity integration test for `notify:OnClientEvent_MarketItemReceived`.
//!
//! # Protocol Background
//! In the official EVE Online client (`eve/client/script/ui/services/marketsvc.pyj:MarketQuote.OnMarketItemReceived`),
//! the client receives the server-pushed notification `OnMarketItemReceived(itemID, typeID, flag, locationID)`
//! upon market purchase fulfillment or expired sell order item return.
//!
//! Upon receipt of `OnMarketItemReceived`, `MarketQuote` locally scatters the internal client UI event:
//! `sm.ScatterEvent("OnClientEvent_MarketItemReceived", itemID, typeID, flag, locationID)`.
//! This internal UI event is consumed by client components such as `NeocomInventoryBadging`
//! (`eve/client/script/ui/shared/inventory/neocomInventoryBadging.pyj`).
//!
//! The EVE server NEVER emits `OnClientEvent_MarketItemReceived` over MachoNet; it is strictly a
//! client-side internal event. This test verifies that:
//! 1. Buy order fulfillment emits the canonical server notification `OnMarketItemReceived` to the buyer.
//! 2. The wire notifications to both buyer and seller contain NO `OnClientEvent_MarketItemReceived`.

#![forbid(unsafe_code)]

use crate::notify_econ_char_emission::{call, events_from, seed_item, select, P1, P2, STATION_JITA};
use eve_db::{InventoryFlag, IskCents};
use eve_proto::PyRep;
use eve_server::router::ServerContext;

#[tokio::test]
async fn test_market_buy_fulfillment_emits_on_market_item_received_and_never_client_event() {
    // parity: notify:OnClientEvent_MarketItemReceived
    eve_testkit::attest(&["notify:OnClientEvent_MarketItemReceived"]);

    let ctx = ServerContext::new();
    let mut s_seller = select(&ctx, 7701, "demo", "demo-secret", P1).await;
    let mut s_buyer = select(&ctx, 7702, "other", "other-secret", P2).await;

    // Ensure adequate ISK balance for trading
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(100_000_000.0);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.balance = IskCents::from_f64_isk(100_000_000.0);
        }
    });

    // Drain login and setup notifications to ensure clean baseline
    for c in [P1, P2] {
        ctx.drain_notifications_for_char(c);
    }

    // Seed 100 units of Tritanium (typeID 34) in Jita 4-4 station hangar for seller
    let sell_item_id = 950_001i64;
    let tritanium_type_id = 34u32;
    seed_item(
        &ctx,
        sell_item_id,
        tritanium_type_id,
        P1,
        STATION_JITA,
        InventoryFlag::Hangar,
        100,
        false,
    );

    // 1. Seller lists 100 units of Tritanium @ 15.0 ISK each
    call(
        &ctx,
        &mut s_seller,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(tritanium_type_id as i64),
            PyRep::Real(15.0),
            PyRep::Integer(100),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;

    let seller_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        !events_from(&seller_notifs, "OnOwnOrdersChanged").is_empty(),
        "Seller must receive OnOwnOrdersChanged when sell order is created"
    );
    assert!(
        events_from(&seller_notifs, "OnClientEvent_MarketItemReceived").is_empty(),
        "Seller must never receive client-local OnClientEvent_MarketItemReceived"
    );

    // 2. Buyer places matching buy order for 100 units of Tritanium @ 15.0 ISK
    call(
        &ctx,
        &mut s_buyer,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(tritanium_type_id as i64),
            PyRep::Real(15.0),
            PyRep::Integer(100),
            PyRep::Integer(0), // range: 0 (station)
            PyRep::Integer(1), // duration: 1 day
        ],
    )
    .await;

    let buyer_notifs = ctx.drain_notifications_for_char(P2);
    let seller_trade_notifs = ctx.drain_notifications_for_char(P1);

    // 3. Verify canonical server notification OnMarketItemReceived is sent to buyer
    let market_received = events_from(&buyer_notifs, "OnMarketItemReceived");
    assert_eq!(
        market_received.len(),
        1,
        "Buyer must receive exactly 1 OnMarketItemReceived notification upon order fill"
    );

    let payload = &market_received[0];
    assert_eq!(
        payload.len(),
        4,
        "OnMarketItemReceived payload must contain [itemID, typeID, flagID, locationID]"
    );

    // Arg 0: itemID (positive integer)
    let delivered_item_id = payload[0]
        .as_i64()
        .expect("itemID must be integer");
    assert!(
        delivered_item_id > 0,
        "Delivered itemID must be a valid positive integer"
    );

    // Arg 1: typeID
    assert_eq!(
        payload[1],
        PyRep::Integer(tritanium_type_id as i64),
        "typeID must match purchased type"
    );

    // Arg 2: flag (Hangar = 4)
    assert_eq!(
        payload[2],
        PyRep::Integer(InventoryFlag::Hangar as i64),
        "flag must be InventoryFlag::Hangar"
    );

    // Arg 3: locationID (station)
    assert_eq!(
        payload[3],
        PyRep::Integer(STATION_JITA),
        "locationID must match delivery station"
    );

    // 4. Assert that OnClientEvent_MarketItemReceived is NEVER sent over MachoNet
    assert!(
        events_from(&buyer_notifs, "OnClientEvent_MarketItemReceived").is_empty(),
        "Server must NEVER send OnClientEvent_MarketItemReceived over the wire to buyer; it is client-local"
    );
    assert!(
        events_from(&seller_trade_notifs, "OnClientEvent_MarketItemReceived").is_empty(),
        "Server must NEVER send OnClientEvent_MarketItemReceived over the wire to seller"
    );
}
