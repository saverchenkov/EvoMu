#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use std::collections::HashMap;

#[test]
fn hypernet_raffle_helpers_and_rpc_mutations() {
    assert_eq!(raffle_note_id(10), -7_400_000_000_010);
    assert_eq!(raffle_note_id(-10), -7_400_000_000_010);

    assert!(compute_required_hypercores(10_000_000.0, 12).is_err());
    assert!(compute_required_hypercores(0.0, 8).is_err());
    assert!(compute_required_hypercores(-5.0, 8).is_err());
    assert!(compute_required_hypercores(f64::NAN, 8).is_err());
    assert_eq!(compute_required_hypercores(1_000_000.0, 8).unwrap(), 1);
    assert!(compute_required_hypercores(100_000_000.0, 512).unwrap() >= 20);

    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut sess, unknown_svc, "AmIBanned", &[], &kw).is_none());
    assert!(handle(&ctx, &mut sess, "raffleProxy", unknown_method, &[], &kw).is_none());

    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "raffleProxy", "AmIBanned", &[], &kw)
        .unwrap()
        .is_err());

    let banned = handle(&ctx, &mut sess, "raffleProxy", "AmIBanned", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(banned, PyRep::Bool(false));

    // Subscriptions
    assert!(handle(&ctx, &mut sess, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(-1)], &kw)
        .unwrap()
        .is_err());
    assert_eq!(
        handle(&ctx, &mut sess, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "raffleProxy", "UnsubscribeFromRaffle", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "raffleProxy", "SubscribeToTickets", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "raffleProxy", "UnSubscribeFromTickets", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // GetActiveHistoricPrices
    assert!(handle(&ctx, &mut sess, "raffleProxy", "GetActiveHistoricPrices", &[], &kw)
        .unwrap()
        .is_err());
    let prices = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(587)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(price_tup) = prices else {
        panic!("expected tuple");
    };
    assert_eq!(price_tup.len(), 2);

    // CreateRaffle, BuyTicket, BuyRandomTickets, AwardItem, GetRaffle, Grab
    let item_id = 999_600_001;
    let core_id = 999_600_002;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
        }
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            core_id,
            eve_db::InventoryItemRecord {
                item_id: core_id,
                type_id: HYPERCORE_TYPE_ID,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Invalid duration rejected
    assert!(handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
            PyRep::Integer(0)
        ],
        &kw,
    )
    .unwrap()
    .is_err());

    let created = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let raffle_id = created.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    assert!(raffle_id > 0);
    assert_eq!(created.dict_get("tickets_remaining"), Some(&PyRep::Integer(8)));

    let got = handle(&ctx, &mut sess, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(got.dict_get("raffle_id"), Some(&PyRep::Integer(raffle_id)));

    let grab = handle(&ctx, &mut sess, "raffleProxy", "Grab", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(!grab.as_list().unwrap().is_empty());

    // BuyTicket 0
    let after_buy1 = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(after_buy1.dict_get("tickets_remaining"), Some(&PyRep::Integer(7)));

    // Duplicate ticket 0 rejected
    assert!(handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .is_err());

    // BuyRandomTickets remaining 7
    let after_buy_all = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(7)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(after_buy_all.dict_get("tickets_remaining"), Some(&PyRep::Integer(0)));
    assert_eq!(after_buy_all.dict_get("is_finished"), Some(&PyRep::Bool(true)));
    assert_eq!(after_buy_all.dict_get("winner_id"), Some(&PyRep::Integer(90_000_001)));

    // AwardItem
    let awarded = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(raffle_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(awarded, PyRep::Integer(item_id));

    // Second AwardItem rejected
    assert!(handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(raffle_id)],
        &kw,
    )
    .unwrap()
    .is_err());

    let hist = handle(&ctx, &mut sess, "raffleProxy", "GetMyRaffleHistory", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(!hist.as_list().unwrap().is_empty());
}

#[tokio::test]
async fn create_raffle_rejects_invalid_and_non_finite_ticket_prices_and_counts() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let item_id = 88_100_021_i64;
    let core_id = 88_100_022_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
        }
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            core_id,
            eve_db::InventoryItemRecord {
                item_id: core_id,
                type_id: HYPERCORE_TYPE_ID,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = auth_session(1, char_id, corp_id, 30_000_142);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();

    // Omitted / invalid ticket_count & i64 -> u32 wrap bypass rejected
    for bad_args in [
        vec![PyRep::Integer(item_id)],
        vec![PyRep::Integer(item_id), PyRep::Integer(0), PyRep::Real(1_000.0)],
        vec![
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(u32::MAX) + 9),
            PyRep::Real(1_000.0),
        ],
        vec![
            PyRep::KeyVal(vec![
                ("itemID".into(), PyRep::Integer(item_id)),
                ("ticketCount".into(), PyRep::Integer(-8)),
                ("ticketPrice".into(), PyRep::Real(1_000.0)),
            ].into_iter().collect()),
        ],
    ] {
        let err = handle(&ctx, &mut sess, "raffleProxy", "CreateRaffle", &bad_args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidRaffleTicketCount", "unexpected error for {bad_args:?}: {err:?}");
    }

    // Omitted, zero, negative, sub-1 ISK, NaN, Infinity, and overflowing ticket_price rejected
    for bad_args in [
        vec![PyRep::Integer(item_id), PyRep::Integer(8)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(0.0)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(-100.0)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(0.5)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(f64::NAN)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(f64::INFINITY)],
        vec![PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Real(1e20)],
        vec![PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(item_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(0.0)),
        ])],
    ] {
        let err = handle(&ctx, &mut sess, "raffleProxy", "CreateRaffle", &bad_args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidTicketPrice", "unexpected error for {bad_args:?}: {err:?}");
    }

    // Item must still be untouched in hangar (not escrowed)
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_id].location_id, station_id);
        assert_eq!(snap.items[&item_id].owner_id, char_id);
    });

    // Valid integer ticket_price (>= 1 ISK) succeeds
    let ok = handle(
        &ctx,
        &mut sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(item_id), PyRep::Integer(8), PyRep::Integer(1_000)],
        &kw,
    )
    .unwrap()
    .expect("valid integer ticket_price must create raffle");
    assert_eq!(ok.dict_get("ticket_price").and_then(|v| v.as_f64()), Some(1_000.0));
}
