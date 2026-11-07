//! Semantic unit tests split from `contracts_deep_extra_unit_tests`.

use super::contracts_deep_extra_unit_tests::*;
use crate::services::contracts_service::{break_courier_plastic_wrap, handle};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn accept_two_way_item_exchange_filters_disallowed_items_and_consumes_valid_stack() {
    let ctx = make_test_context();
    let issuer_id = 911_001;
    let acceptor_id = 911_002;
    let corp_id = 1_000_009;
    seed_char(&ctx, issuer_id, corp_id, None, Some(JITA_STATION_ID), 911_501, 50_000_000);
    seed_char(&ctx, acceptor_id, corp_id, None, Some(JITA_STATION_ID), 911_502, 50_000_000);
    seed_item(&ctx, 911_100, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 500, false);

    let mut issuer_sess = make_authed_session(1, issuer_id, corp_id, None, Some(JITA_STATION_ID), Some(911_501));
    let mut acceptor_sess = make_authed_session(2, acceptor_id, corp_id, None, Some(JITA_STATION_ID), Some(911_502));
    let kw = HashMap::new();

    let created = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("ItemExchange".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(10_000.0)),
            ("itemList", PyRep::Integer(911_100)),
            (
                "requestedItems",
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(10)])]),
            ),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Seed ineligible stacks of type 35: damaged item, BPC, and non-empty container
    ctx.db.with_mut_snapshot(|snap| {
        let mut damaged = InventoryItemRecord {
            item_id: 911_201,
            type_id: 35,
            owner_id: acceptor_id,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 20,
            ..Default::default()
        };
        damaged.dynamic_attributes.insert(3, 1.5);
        snap.items.insert(911_201, damaged);

        snap.items.insert(
            911_202,
            InventoryItemRecord {
                item_id: 911_202,
                type_id: 35,
                owner_id: acceptor_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
                ..Default::default()
            },
        );
        snap.items.insert(
            911_203,
            InventoryItemRecord {
                item_id: 911_203,
                type_id: 35,
                owner_id: acceptor_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                ..Default::default()
            },
        );
        snap.items.insert(
            911_204,
            InventoryItemRecord {
                item_id: 911_204,
                type_id: 34,
                owner_id: acceptor_id,
                location_id: 911_203,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let err = handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "MissingRequestedContractItems");

    seed_item(&ctx, 911_210, 35, acceptor_id, JITA_STATION_ID, InventoryFlag::Hangar, 15, false);
    let ok = handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(ok.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&911_210).unwrap().quantity, 5);
        assert_eq!(snap.items.get(&911_100).unwrap().owner_id, acceptor_id);
        assert!(snap.items.values().any(|i| {
            i.owner_id == issuer_id
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 35
                && i.quantity == 10
        }));
    });
}

#[test]
fn courier_contract_accept_complete_and_break_plastic_wrap_lifecycle() {
    let ctx = make_test_context();
    let issuer_id = 912_001;
    let courier_id = 912_002;
    let corp_id = 1_000_009;
    let courier_ship_id = 912_502;
    seed_char(&ctx, issuer_id, corp_id, None, Some(JITA_STATION_ID), 912_501, 50_000_000);
    seed_char(&ctx, courier_id, corp_id, None, Some(JITA_STATION_ID), courier_ship_id, 50_000_000);
    seed_item(&ctx, courier_ship_id, 606, courier_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 912_101, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 200, false);

    let mut issuer_sess = make_authed_session(1, issuer_id, corp_id, None, Some(JITA_STATION_ID), Some(912_501));
    let mut courier_sess = make_authed_session(2, courier_id, corp_id, None, Some(JITA_STATION_ID), Some(courier_ship_id));
    let kw = HashMap::new();

    let created = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(DEST_STRUCT_ID)),
            ("reward", PyRep::Real(25_000.0)),
            ("collateral", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(912_101)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let acc = handle(&ctx, &mut courier_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    let pkg_id = acc.dict_get("courierPackageID").and_then(|v| v.as_i64()).unwrap();
    assert!(pkg_id > 0);

    // 1. Not at destination station -> CourierNotAtDestinationStation
    let err = handle(&ctx, &mut courier_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "CourierNotAtDestinationStation");

    // 2. Session at destination, ship still in Jita -> DeliveryShipNotAtDestination
    courier_sess.station_id = Some(DEST_STRUCT_ID);
    let err = handle(&ctx, &mut courier_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "DeliveryShipNotAtDestination");

    // 3. Ship at destination, package still in Jita -> CourierPackageMissing
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&courier_ship_id).unwrap().location_id = DEST_STRUCT_ID;
    });
    let err = handle(&ctx, &mut courier_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "CourierPackageMissing");

    // 4. Put Plastic Wrap into courier ship cargo and complete contract
    ctx.db.with_mut_snapshot(|snap| {
        let pkg = snap.items.get_mut(&pkg_id).unwrap();
        pkg.location_id = courier_ship_id;
        pkg.flag = InventoryFlag::Cargo;
    });
    let comp = handle(&ctx, &mut courier_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(comp.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&pkg_id));
        let item = snap.items.get(&912_101).unwrap();
        assert_eq!(item.owner_id, issuer_id);
        assert_eq!(item.location_id, DEST_STRUCT_ID);
        assert_eq!(item.flag, InventoryFlag::Hangar);
    });

    // 5. BreakPlasticWrap on a second courier contract
    seed_item(&ctx, 912_102, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 300, false);
    courier_sess.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&courier_ship_id).unwrap().location_id = JITA_STATION_ID;
    });
    let created2 = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(DEST_STRUCT_ID)),
            ("reward", PyRep::Real(15_000.0)),
            ("collateral", PyRep::Real(80_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(912_102)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid2 = created2.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();
    let acc2 = handle(&ctx, &mut courier_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid2)], &kw)
        .unwrap()
        .unwrap();
    let pkg_id2 = acc2.dict_get("courierPackageID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(break_courier_plastic_wrap(snap, courier_id, 999_999_999, 1000).unwrap_err().code, "ItemNotFound");
        assert_eq!(break_courier_plastic_wrap(snap, courier_id, courier_ship_id, 1000).unwrap_err().code, "InvalidPlasticWrap");
    });

    let issuer_bal_before = ctx.db.with_snapshot(|s| s.characters.get(&issuer_id).unwrap().balance);
    let broken = handle(&ctx, &mut courier_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(pkg_id2)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(broken.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts.get(&cid2).unwrap().status, "Failed");
        let item = snap.items.get(&912_102).unwrap();
        assert_eq!(item.owner_id, courier_id);
        assert_eq!(item.location_id, JITA_STATION_ID);
        assert_eq!(item.flag, InventoryFlag::Hangar);
        let issuer_bal_after = snap.characters.get(&issuer_id).unwrap().balance;
        assert!(issuer_bal_after.0 >= issuer_bal_before.0 + IskCents::from_isk(95_000).0);
    });
}

#[test]
fn auction_bidding_outbid_refund_buyout_and_get_my_bids_login_info() {
    let ctx = make_test_context();
    let issuer_id = 913_001;
    let bidder1_id = 913_002;
    let bidder2_id = 913_003;
    let corp_id = 1_000_009;
    seed_char(&ctx, issuer_id, corp_id, None, Some(JITA_STATION_ID), 913_501, 50_000_000);
    seed_char(&ctx, bidder1_id, corp_id, None, Some(JITA_STATION_ID), 913_502, 50_000_000);
    seed_char(&ctx, bidder2_id, corp_id, None, Some(JITA_STATION_ID), 913_503, 50_000_000);
    seed_item(&ctx, 913_100, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 1_000, false);

    let mut issuer_sess = make_authed_session(1, issuer_id, corp_id, None, Some(JITA_STATION_ID), Some(913_501));
    let mut b1_sess = make_authed_session(2, bidder1_id, corp_id, None, Some(JITA_STATION_ID), Some(913_502));
    let mut b2_sess = make_authed_session(3, bidder2_id, corp_id, None, Some(JITA_STATION_ID), Some(913_503));
    let kw = HashMap::new();

    let created = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Auction".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(913_100)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let self_bid = handle(&ctx, &mut issuer_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(cid), PyRep::Real(150_000.0)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(self_bid.code, "CannotBidOnOwnContract");

    let zero_bid = handle(&ctx, &mut b1_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(cid), PyRep::Real(0.0)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(zero_bid.code, "InvalidBidAmount");

    let b1_bal_before = ctx.db.with_snapshot(|s| s.characters.get(&bidder1_id).unwrap().balance);
    let _ = handle(&ctx, &mut b1_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(cid), PyRep::Real(100_000.0)], &kw)
        .unwrap()
        .unwrap();
    let b1_bal_mid = ctx.db.with_snapshot(|s| s.characters.get(&bidder1_id).unwrap().balance);
    assert_eq!(b1_bal_before.0 - b1_bal_mid.0, IskCents::from_isk(100_000).0);

    let my_bids = handle(&ctx, &mut b1_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(my_bids, PyRep::List(ref l) if !l.is_empty()));

    let buyout_res = handle(&ctx, &mut b2_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(cid), PyRep::Real(500_000.0)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(buyout_res.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&bidder1_id).unwrap().balance, b1_bal_before);
        let item = snap.items.get(&913_100).unwrap();
        assert_eq!(item.owner_id, bidder2_id);
        assert_eq!(item.flag, InventoryFlag::Hangar);
    });

    let login_info = handle(&ctx, &mut issuer_sess, "contractProxy", "GetLoginInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(login_info.dict_get("needsAttention").is_some());
}
