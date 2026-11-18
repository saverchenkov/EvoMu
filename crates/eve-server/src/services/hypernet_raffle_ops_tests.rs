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
    assert!(compute_required_hypercores(0.0, 8).is_err());
    assert!(compute_required_hypercores(f64::NAN, 8).is_err());
    assert!(compute_required_hypercores(f64::INFINITY, 8).is_err());
    assert!(compute_required_hypercores(10_000_000.0, 7).is_err());
    assert_eq!(compute_required_hypercores(1_000_000.0, 8).unwrap(), 1);
    assert_eq!(compute_required_hypercores(6_999_999.0, 8).unwrap(), 1);
    assert_eq!(compute_required_hypercores(7_000_000.0, 8).unwrap(), 1);
    assert_eq!(compute_required_hypercores(13_999_999.0, 8).unwrap(), 1);
    assert_eq!(compute_required_hypercores(14_000_000.0, 8).unwrap(), 2);
    assert_eq!(compute_required_hypercores(21_000_000.0, 16).unwrap(), 3);
    for &sz in &ALLOWED_RAFFLE_SIZES {
        assert_eq!(compute_required_hypercores(100_000_000.0, sz).unwrap(), 14);
    }
    assert!(validate_raffle_total_price(999.0).is_err());
    assert!(validate_raffle_total_price(1_000.0).is_ok());
    assert!(validate_raffle_total_price(10_000_000_000_001.0).is_err());
    // Dynamic PLEX market price scaling: 7_000_000 ISK PLEX -> 700k token_price -> 14M = 1 core vs 2 cores at 3.5M default
    assert_eq!(
        compute_required_hypercores_for_plex_price(14_000_000.0, 8, 7_000_000.0).unwrap(),
        1
    );

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
    let PyRep::Integer(raffle_id) = created else {
        panic!("CreateRaffle must return integer raffle_id, got {created:?}");
    };
    assert!(raffle_id > 0);

    let got = handle(&ctx, &mut sess, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(got.dict_get("raffle_id"), Some(&PyRep::Integer(raffle_id)));
    assert_eq!(got.dict_get("tickets_remaining"), Some(&PyRep::Integer(8)));

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
    let PyRep::Tuple(hist_parts) = hist else {
        panic!("expected 2-tuple from GetMyRaffleHistory");
    };
    assert_eq!(hist_parts.len(), 2);
    let rows = hist_parts[0].as_list().expect("history list at [0]");
    assert!(!rows.is_empty());
    assert!(rows[0].dict_get("running_id").and_then(PyRep::as_i64).is_some());
    assert!(hist_parts[1].as_i64().unwrap_or(0) > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    let ok_rid = ok.as_i64().expect("CreateRaffle must return integer raffle_id");
    let got_ok = handle(&ctx, &mut sess, "raffleProxy", "GetRaffle", &[PyRep::Integer(ok_rid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(got_ok.dict_get("ticket_price").and_then(|v| v.as_f64()), Some(1_000.0));

    // V24 RaffleTypeValidation (list 133): PLEX (44992) and HyperCores (52568) rejected;
    // sub-MIN_TOTAL_PRICE (8 * 100 = 800 < 1000) rejected; non-singleton Blueprint rejected.
    let plex_item_id = 999_600_101_i64;
    let unseq_bp_id = 999_600_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_item_id,
            eve_db::InventoryItemRecord {
                item_id: plex_item_id,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            unseq_bp_id,
            eve_db::InventoryItemRecord {
                item_id: unseq_bp_id,
                type_id: 681, // Rifter Blueprint (category 9)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(plex_item_id), PyRep::Integer(8), PyRep::Real(1_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidRaffleItem"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(core_id), PyRep::Integer(8), PyRep::Real(1_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidRaffleItem"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(unseq_bp_id), PyRep::Integer(8), PyRep::Real(1_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidRaffleItem"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(unseq_bp_id), PyRep::Integer(8), PyRep::Real(100.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTicketPrice"
    );
}

#[test]
fn hypernet_v24_raffle_creation_data_and_created_participated_counts() {
    let ctx = test_ctx();
    let creator = 90_000_001_i64;
    let buyer = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let mut creator_sess = auth_session(1, creator, 98_000_001, 30_000_142);
    creator_sess.station_id = Some(station_id);
    let mut buyer_sess = auth_session(2, buyer, 98_000_001, 30_000_142);
    buyer_sess.station_id = Some(station_id);

    let (ship_running, ship_undelivered, ship_delivered, ship_expired, core_id) =
        (999_700_001_i64, 999_700_002_i64, 999_700_003_i64, 999_700_004_i64, 999_700_010_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &[creator, buyer] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = Some(station_id);
                ch.structure_id = None;
                ch.balance = IskCents::from_isk(500_000_000);
            }
        }
        for sid in [ship_running, ship_undelivered, ship_delivered, ship_expired] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: creator,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            core_id,
            eve_db::InventoryItemRecord {
                item_id: core_id,
                type_id: HYPERCORE_TYPE_ID,
                owner_id: creator,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let kw = HashMap::new();

    // CreateRaffle accepts PyRep::Object (RaffleCreationData) and returns integer raffle_id
    let obj_req = PyRep::Object {
        type_name: "raffles.common.raffle_data.RaffleCreationData".into(),
        args: Box::new(PyRep::keyval([
            ("item_id", PyRep::Integer(ship_running)),
            ("ticket_count", PyRep::Integer(8)),
            ("ticket_price", PyRep::Real(100_000.0)),
            ("duration_secs", PyRep::Integer(3_600)),
        ])),
    };
    let rid_running = handle(&ctx, &mut creator_sess, "raffleProxy", "CreateRaffle", &[obj_req], &kw)
        .unwrap()
        .expect("Object RaffleCreationData must succeed")
        .as_i64()
        .expect("CreateRaffle must return integer raffle_id");
    assert!(rid_running > 0);

    // CreateRaffle accepts PyRep::ObjectEx (RaffleCreationData) and returns integer raffle_id
    let mk_objex = |item_id: i64, dur_secs: i64| PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Token(
            "raffles.common.raffle_data.RaffleCreationData".into(),
        )])),
        list: vec![],
        dict: vec![
            (PyRep::String("item_id".into()), PyRep::Integer(item_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(100_000.0)),
            (PyRep::String("duration_secs".into()), PyRep::Integer(dur_secs)),
        ],
    };
    let rid_undelivered = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[mk_objex(ship_undelivered, 3_600)],
        &kw,
    )
    .unwrap()
    .expect("ObjectEx undelivered")
    .as_i64()
    .unwrap();
    let rid_delivered = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[mk_objex(ship_delivered, 3_600)],
        &kw,
    )
    .unwrap()
    .expect("ObjectEx delivered")
    .as_i64()
    .unwrap();
    let rid_expired = handle(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[mk_objex(ship_expired, 60)],
        &kw,
    )
    .unwrap()
    .expect("ObjectEx expired")
    .as_i64()
    .unwrap();

    // Buyer buys 1 ticket on running, all 8 on undelivered, all 8 + AwardItem on delivered
    handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(rid_running), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(rid_undelivered), PyRep::Integer(8)],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(rid_delivered), PyRep::Integer(8)],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(rid_delivered)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Expire rid_expired (duration = 60s; running/undelivered/delivered had 3600s)
    ctx.advance_secs(120);
    assert_eq!(sweep_expired_raffles(&ctx), 1);
    let _ = rid_expired;

    // Creator GetCreatedParticipated -> 6-key dict of Integer counts
    let creator_stats = handle(&ctx, &mut creator_sess, "raffleProxy", "GetCreatedParticipated", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(creator_stats, PyRep::Dict(_)));
    assert_eq!(creator_stats.dict_get("created_running"), Some(&PyRep::Integer(1)));
    assert_eq!(creator_stats.dict_get("finished_delivered"), Some(&PyRep::Integer(1)));
    assert_eq!(creator_stats.dict_get("finished_expired"), Some(&PyRep::Integer(1)));
    assert_eq!(creator_stats.dict_get("finished_undelivered"), Some(&PyRep::Integer(1)));
    assert_eq!(creator_stats.dict_get("raffles_participated"), Some(&PyRep::Integer(0)));
    assert_eq!(creator_stats.dict_get("raffles_won"), Some(&PyRep::Integer(0)));

    // Buyer GetCreatedParticipated -> 3 participated, 2 won (undelivered + delivered)
    let buyer_stats = handle(&ctx, &mut buyer_sess, "raffleProxy", "GetCreatedParticipated", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(buyer_stats.dict_get("created_running"), Some(&PyRep::Integer(0)));
    assert_eq!(buyer_stats.dict_get("raffles_participated"), Some(&PyRep::Integer(3)));
    assert_eq!(buyer_stats.dict_get("raffles_won"), Some(&PyRep::Integer(2)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_raffle_consumes_split_hypercore_stacks_in_station_hangar() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let item_id = 88_100_921_i64;
    let core_a = 88_100_922_i64;
    let core_b = 88_100_923_i64;
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
        // Two split HyperCore piles (2 + 2 = 4 < required 5 for 40M total value at 3.5M PLEX) -> rejected cleanly.
        for (id, qty) in [(core_a, 2), (core_b, 2)] {
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: HYPERCORE_TYPE_ID,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = auth_session(1, char_id, corp_id, 30_000_142);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();
    let args = [
        PyRep::Integer(item_id),
        PyRep::Integer(8),
        PyRep::Real(5_000_000.0), // 40,000,000 ISK total -> requires floor((40M / 350k) * 0.05) = 5 HyperCores
    ];
    let err = handle(&ctx, &mut sess, "raffleProxy", "CreateRaffle", &args, &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "InsufficientHyperCores");

    // Top up second pile to 4 (2 + 4 = 6 >= 5) -> consumes across both stacks, leaving 1 HyperCore.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&core_b).unwrap().quantity = 4;
    });
    let created = handle(&ctx, &mut sess, "raffleProxy", "CreateRaffle", &args, &kw)
        .unwrap()
        .unwrap();
    let PyRep::Integer(raffle_id) = created else {
        panic!("CreateRaffle must return integer raffle_id, got {created:?}");
    };
    assert!(raffle_id > 0);
    let remaining_cores: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == char_id
                    && i.location_id == station_id
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == HYPERCORE_TYPE_ID
            })
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(remaining_cores, 1);
}

#[cfg(test)]
#[path = "hypernet_raffle_ops_extra_unit_tests.rs"]
mod extra_unit_tests;
