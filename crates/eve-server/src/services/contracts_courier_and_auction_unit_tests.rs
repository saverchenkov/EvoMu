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

#[test]
fn courier_corp_acceptor_structure_destination_location_metadata_and_expiry() {
    use eve_db::{corp_roles, CorporationRecord, VirtualServerClock};
    use std::sync::Arc;

    let ctx = make_test_context();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_010_000)));
    let issuer_id = 914_001_i64;
    let hauler_id = 914_002_i64;
    let issuer_corp = 98_014_001_i64;
    let hauler_corp = 98_014_002_i64;
    let hauler_ship = 914_502_i64;
    let pkg_item = 914_100_i64;
    let mut muta_bpc_attrs = HashMap::new();
    muta_bpc_attrs.insert(1112u16, 9.0);
    muta_bpc_attrs.insert(1113u16, 18.0);
    muta_bpc_attrs.insert(3u16, 0.25);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ceo) in [(issuer_corp, issuer_id), (hauler_corp, hauler_id + 99)] {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = cid;
            corp.name = format!("Corp {cid}");
            corp.ceo_id = ceo;
            corp.ensure_wallet_divisions();
            for div in 1..=7u8 {
                corp.set_division_balance(div, IskCents::from_isk(100_000_000));
            }
            snap.corporations.insert(cid, corp);
        }
        if let Some(c) = snap.corporations.get_mut(&hauler_corp) {
            c.members.insert(hauler_id);
        }
    });
    seed_char(&ctx, issuer_id, issuer_corp, None, Some(JITA_STATION_ID), 914_501, 50_000_000);
    seed_char(&ctx, hauler_id, hauler_corp, None, Some(JITA_STATION_ID), hauler_ship, 50_000_000);
    seed_item(&ctx, hauler_ship, 600, hauler_corp, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pkg_item,
            InventoryItemRecord {
                item_id: pkg_item,
                type_id: 24_690,
                owner_id: issuer_corp,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(7),
                source_mutaplasmid_id: Some(49_726),
                dynamic_attributes: muta_bpc_attrs,
                version: 1,
                ..Default::default()
            },
        );
        for cid in [issuer_id, hauler_id] {
            snap.accounts.insert(
                format!("acct_{cid}"),
                eve_db::AccountRecord {
                    user_id: cid as u64,
                    username: format!("acct_{cid}"),
                    password_token: "pw".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(issuer_id as u64, 1, Some(issuer_id), false, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(hauler_id as u64, 2, Some(hauler_id), false, ctx.now_unix())
        .unwrap();

    let mut issuer_sess = make_authed_session(1, issuer_id, issuer_corp, None, Some(JITA_STATION_ID), Some(914_501));
    let mut hauler_sess = make_authed_session(2, hauler_id, hauler_corp, None, Some(JITA_STATION_ID), Some(hauler_ship));
    hauler_sess.corp_account_key = 1002; // wallet division 3 (index 2)
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
            ("reward", PyRep::Real(250_000.0)),
            ("collateral", PyRep::Real(1_500_000.0)),
            ("numDays", PyRep::Integer(2)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(pkg_item)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // GetContract does NOT emit OnShowContract, whereas GetContractInfo DOES, and includes W2 location & detailed item metadata
    let _ = ctx.drain_notifications_for_char(hauler_id);
    let info_silent = handle(&ctx, &mut hauler_sess, "contractProxy", "GetContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert!(!ctx.drain_notifications_for_char(hauler_id).iter().any(|n| n.event == "OnShowContract"));
    assert_eq!(info_silent.dict_get("startSolarSystemID").and_then(|v| v.as_i64()), Some(JITA_SYSTEM_ID as i64));
    assert_eq!(info_silent.dict_get("endSolarSystemID").and_then(|v| v.as_i64()), Some(JITA_SYSTEM_ID as i64));
    assert_eq!(info_silent.dict_get("startRegionID").and_then(|v| v.as_i64()), Some(THE_FORGE_REGION_ID as i64));
    assert_eq!(info_silent.dict_get("endRegionID").and_then(|v| v.as_i64()), Some(THE_FORGE_REGION_ID as i64));
    assert_eq!(info_silent.dict_get("forCorp").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(info_silent.dict_get("isBlueprintCopy").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(info_silent.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(info_silent.dict_get("sourceMutaplasmidID").and_then(|v| v.as_i64()), Some(49_726));

    let _ = handle(&ctx, &mut hauler_sess, "contractProxy", "GetContractInfo", &[PyRep::Integer(cid)], &kw) // rpc-lint: allow test fixture
        .unwrap()
        .unwrap();
    assert!(ctx
        .drain_notifications_for_char(hauler_id)
        .iter()
        .any(|n| n.event == "OnShowContract" && n.args == vec![PyRep::Integer(cid)]));

    // Corp accept RBAC: needs both contract role AND division-3 wallet take role
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&hauler_corp).unwrap().member_roles.insert(hauler_id, corp_roles::ROLE_CONTRACT_MANAGER);
    });
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&hauler_corp)
            .unwrap()
            .member_roles
            .insert(hauler_id, corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]);
    });

    let div3_before = ctx.db.with_snapshot(|s| s.corporations[&hauler_corp].division_balance(3));
    let accepted = handle(&ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(accepted.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    let wrap_id = accepted.dict_get("courierPackageID").and_then(|v| v.as_i64()).unwrap();
    let div3_after_acc = ctx.db.with_snapshot(|s| s.corporations[&hauler_corp].division_balance(3));
    assert_eq!(div3_before.0 - div3_after_acc.0, IskCents::from_isk(1_500_000).0);

    // Wrong station / missing ship failure before moving to DEST_STRUCT_ID
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CourierNotAtDestinationStation"
    );

    // Move hauler + corp ship + wrap to DEST_STRUCT_ID
    hauler_sess.station_id = None;
    hauler_sess.structure_id = Some(DEST_STRUCT_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&wrap_id).unwrap().location_id = DEST_STRUCT_ID;
    });
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DeliveryShipNotAtDestination"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&hauler_ship).unwrap().location_id = DEST_STRUCT_ID;
    });
    let div1_before = ctx.db.with_snapshot(|s| s.corporations[&hauler_corp].division_balance(1));
    let _ = ctx.drain_notifications_for_char(issuer_id);
    let done = handle(&ctx, &mut hauler_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(done.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));
    assert_eq!(done.dict_get("totalPayoutISK").and_then(|v| v.as_f64()), Some(1_750_000.0));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wrap_id));
        let it = snap.items.get(&pkg_item).unwrap();
        assert_eq!(it.owner_id, issuer_corp);
        assert_eq!(it.location_id, DEST_STRUCT_ID);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(
            snap.corporations[&hauler_corp].division_balance(1).0 - div1_before.0,
            IskCents::from_isk(1_750_000).0
        );
    });
    assert!(ctx.drain_notifications_for_char(issuer_id).iter().any(|n| n.event == "OnContractCompleted"));

    // Late InProgress courier past deadline -> ContractExpired + forfeits collateral to issuer
    seed_item(&ctx, 914_101, 34, issuer_corp, JITA_STATION_ID, InventoryFlag::CorpDeliveries, 20, false);
    hauler_sess.station_id = Some(JITA_STATION_ID);
    hauler_sess.structure_id = None;
    let c2 = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(DEST_STRUCT_ID)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(800_000.0)),
            ("numDays", PyRep::Integer(1)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(914_101)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    handle(&ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(c2), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    let issuer_div1_before = ctx.db.with_snapshot(|s| s.corporations[&issuer_corp].division_balance(1));
    ctx.advance_secs(86_400 + 1);
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(c2)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractExpired"
    );
    let issuer_div1_after = ctx.db.with_snapshot(|s| s.corporations[&issuer_corp].division_balance(1));
    assert!(issuer_div1_after.0 >= issuer_div1_before.0 + IskCents::from_isk(800_000).0);
}

#[test]
fn break_plastic_wrap_cargo_extraction_corp_manager_and_auth_boundaries() {
    use eve_db::{corp_roles, CorporationRecord};

    let ctx = make_test_context();
    let (issuer_id, hauler_id, outsider_id) = (915_001, 915_002, 915_003);
    let hauler_corp = 98_015_001_i64;
    let hauler_ship = 915_502_i64;
    let wrapped_ship = 915_100_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = hauler_corp;
        corp.name = "HaulCorp".into();
        corp.ceo_id = issuer_id;
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        corp.members.insert(hauler_id);
        corp.member_roles.insert(hauler_id, corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(hauler_corp, corp);
    });
    seed_char(&ctx, issuer_id, 1_000_009, None, Some(JITA_STATION_ID), 915_501, 50_000_000);
    seed_char(&ctx, hauler_id, hauler_corp, None, Some(JITA_STATION_ID), hauler_ship, 50_000_000);
    seed_char(&ctx, outsider_id, 1_000_009, None, Some(JITA_STATION_ID), 915_503, 50_000_000);
    seed_item(&ctx, wrapped_ship, 600, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, hauler_ship, 600, hauler_corp, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);

    let mut issuer_sess = make_authed_session(1, issuer_id, 1_000_009, None, Some(JITA_STATION_ID), Some(915_501));
    let mut hauler_sess = make_authed_session(2, hauler_id, hauler_corp, None, Some(JITA_STATION_ID), Some(hauler_ship));
    let mut outsider_sess = make_authed_session(3, outsider_id, 1_000_009, None, Some(JITA_STATION_ID), Some(915_503));
    let kw = HashMap::new();

    let cid = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("endStationID", PyRep::Integer(DEST_STRUCT_ID)),
            ("reward", PyRep::Real(50_000.0)),
            ("collateral", PyRep::Real(600_000.0)),
            ("itemList", PyRep::Integer(wrapped_ship)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let accepted = handle(&ctx, &mut hauler_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    let wrap_id = accepted.dict_get("courierPackageID").and_then(|v| v.as_i64()).unwrap();

    // Non-positive arg, non-wrap / outsider, and wrong location errors
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut outsider_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlasticWrap"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&wrap_id).unwrap().location_id = hauler_ship;
        snap.characters.get_mut(&hauler_id).unwrap().active_ship_id = 999_999; // wrong active ship while docked elsewhere checks CustomInfo
        snap.characters.get_mut(&hauler_id).unwrap().station_id = Some(60_003_493);
    });
    assert_eq!(
        handle(&ctx, &mut hauler_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CustomInfo"
    );

    // Restore hauler in hauler_ship -> breaking wrap inside ship extracts into InventoryFlag::Cargo & strips wrap!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&hauler_id).unwrap().active_ship_id = hauler_ship;
        snap.characters.get_mut(&hauler_id).unwrap().station_id = Some(JITA_STATION_ID);
    });
    let broken = handle(&ctx, &mut hauler_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(broken.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wrap_id));
        let item = snap.items.get(&wrapped_ship).unwrap();
        assert_eq!(item.owner_id, hauler_corp);
        assert_eq!(item.location_id, hauler_ship);
        assert_eq!(item.flag, InventoryFlag::Cargo);
        assert_eq!(snap.contracts[&cid].status, "Failed");
    });
}

#[test]
fn auction_corp_bidding_division_refund_self_outbid_buyout_and_get_my_bids_filters() {
    use eve_db::{corp_roles, CorporationRecord};

    let ctx = make_test_context();
    let (issuer_id, corp_bidder_id, p2_id, outsider_id) = (916_001, 916_002, 916_003, 916_004);
    let bidder_corp = 98_016_002_i64;
    let issuer_corp = 98_016_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ceo) in [(issuer_corp, issuer_id), (bidder_corp, 916_999)] {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = cid;
            corp.name = format!("AucCorp {cid}");
            corp.ceo_id = ceo;
            corp.ensure_wallet_divisions();
            for div in 1..=7u8 {
                corp.set_division_balance(div, IskCents::from_isk(10_000_000));
            }
            snap.corporations.insert(cid, corp);
        }
        if let Some(c) = snap.corporations.get_mut(&bidder_corp) {
            c.members.insert(corp_bidder_id);
        }
    });
    seed_char(&ctx, issuer_id, issuer_corp, None, Some(JITA_STATION_ID), 916_501, 50_000_000);
    seed_char(&ctx, corp_bidder_id, bidder_corp, None, Some(JITA_STATION_ID), 916_502, 50_000_000);
    seed_char(&ctx, p2_id, 1_000_009, None, Some(JITA_STATION_ID), 916_503, 50_000_000);
    seed_char(&ctx, outsider_id, 1_000_009, None, Some(JITA_STATION_ID), 916_504, 50_000_000);
    seed_item(&ctx, 916_100, 34, issuer_corp, JITA_STATION_ID, InventoryFlag::CorpDeliveries, 100, false);
    seed_item(&ctx, 916_101, 34, issuer_id, JITA_STATION_ID, InventoryFlag::Hangar, 10, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("acct_{corp_bidder_id}"),
            eve_db::AccountRecord {
                user_id: corp_bidder_id as u64,
                username: format!("acct_{corp_bidder_id}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(corp_bidder_id as u64, 2, Some(corp_bidder_id), false, ctx.now_unix())
        .unwrap();

    let mut issuer_sess = make_authed_session(1, issuer_id, issuer_corp, None, Some(JITA_STATION_ID), Some(916_501));
    let mut corp_bid_sess = make_authed_session(2, corp_bidder_id, bidder_corp, None, Some(JITA_STATION_ID), Some(916_502));
    corp_bid_sess.corp_account_key = 1003; // division 4 (index 3)
    let mut p2_sess = make_authed_session(3, p2_id, 1_000_009, None, Some(JITA_STATION_ID), Some(916_503));
    let mut out_sess = make_authed_session(4, outsider_id, 1_000_009, None, Some(JITA_STATION_ID), Some(916_504));
    let kw = HashMap::new();

    // Private auction to bidder_corp checks ContractAccessDenied vs corp visibility + Bid increments
    let priv_cid = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Auction".into())),
            ("availability", PyRep::String("Private".into())),
            ("assigneeID", PyRep::Integer(bidder_corp)),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(916_100)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Outsider cannot bid or view private auction
    assert_eq!(
        handle(&ctx, &mut out_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(150_000.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractAccessDenied"
    );
    // Corp bid RBAC: missing contract role, then missing div-4 wallet role, then GetMyBids without role fails CorpRoleDenied
    assert_eq!(
        handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(100_000.0), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    let mut for_corp_kw = HashMap::new();
    for_corp_kw.insert("forCorp".into(), PyRep::Bool(true));
    assert_eq!(
        handle(&ctx, &mut corp_bid_sess, "contractProxy", "GetMyBids", &[], &for_corp_kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&bidder_corp).unwrap().member_roles.insert(corp_bidder_id, corp_roles::ROLE_CONTRACT_MANAGER);
    });
    assert_eq!(
        handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(100_000.0), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&bidder_corp)
            .unwrap()
            .member_roles
            .insert(corp_bidder_id, corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[3]);
    });

    // Public auction for cross-bidder outbid & increment min boundary
    let pub_cid = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("type", PyRep::String("Auction".into())),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(JITA_STATION_ID)),
            ("price", PyRep::Real(100.0)), // tests min increment floor = max(5%, 100 ISK = 10_000 cents)
            ("collateral", PyRep::Real(500_000.0)),
            ("itemList", PyRep::Integer(916_101)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Issuer bidding for issuer_corp on own forCorp auction rejected; self-outbid delta & min increment floor checked
    assert_eq!(
        handle(&ctx, &mut issuer_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(100_000.0), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CannotBidOnOwnContract"
    );

    // Tiny auction minimum increment floor (100 ISK = 10_000 cents)
    handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(pub_cid), PyRep::Real(100.0), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle(&ctx, &mut p2_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(pub_cid), PyRep::Real(199.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "BidTooLow"
    );

    // Corp places 200_000 on priv_cid from div 4; lower div-4 bal to 60_000 -> self-outbid to 250_000 succeeds (60k + 200k >= 250k), 270_000 fails
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&bidder_corp).unwrap().set_division_balance(4, IskCents::from_isk(260_000));
    });
    handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(200_000.0), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&bidder_corp].division_balance(4)), IskCents::from_isk(60_000));
    // Below 5% (209_999.0 < 210_000.0) fails BidTooLow
    assert_eq!(
        handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(209_999.0), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "BidTooLow"
    );
    // 270_000 exceeds 60k + 200k -> InsufficientFunds
    assert_eq!(
        handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(270_000.0), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    // Self-outbid to 250_000 succeeds and does NOT emit OnContractOutbid to self
    let _ = ctx.drain_notifications_for_char(corp_bidder_id);
    handle(&ctx, &mut corp_bid_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(priv_cid), PyRep::Real(250_000.0), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    assert!(!ctx.drain_notifications_for_char(corp_bidder_id).iter().any(|n| n.event == "OnContractOutbid"));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&bidder_corp].division_balance(4)), IskCents::from_isk(10_000));

    // P2 outbids corp on pub_cid -> corp div 4 refunded exact 100.0 ISK!
    let div4_before_refund = ctx.db.with_snapshot(|s| s.corporations[&bidder_corp].division_balance(4));
    let p2_bal_before = ctx.db.with_snapshot(|s| s.characters[&p2_id].balance);
    // Overpay buyout (600_000 >= 500_000 buyout) -> caps charge at exact 500_000.0 buyout!
    let bo = handle(&ctx, &mut p2_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(pub_cid), PyRep::Real(600_000.0)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(bo.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));
    assert_eq!(bo.dict_get("currentBidISK").and_then(|v| v.as_f64()), Some(500_000.0));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&bidder_corp].division_balance(4)),
        IskCents(div4_before_refund.0 + IskCents::from_isk(100).0)
    );
    assert_eq!(
        p2_bal_before.0 - ctx.db.with_snapshot(|s| s.characters[&p2_id].balance).0,
        IskCents::from_isk(500_000).0
    );

    // GetMyBids filters for corp: isAccepted=false -> [priv_cid], isAccepted=true -> [], None -> [priv_cid, pub_cid]
    let outstanding = handle(&ctx, &mut corp_bid_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)], &for_corp_kw)
        .unwrap()
        .unwrap();
    let accepted_only = handle(&ctx, &mut corp_bid_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(true)], &for_corp_kw)
        .unwrap()
        .unwrap();
    let all_corp_bids = handle(&ctx, &mut corp_bid_sess, "contractProxy", "GetMyBids", &[], &for_corp_kw)
        .unwrap()
        .unwrap();
    assert!(matches!(outstanding, PyRep::List(ref l) if l.len() == 1));
    assert!(matches!(accepted_only, PyRep::List(ref l) if l.is_empty()));
    assert!(matches!(all_corp_bids, PyRep::List(ref l) if l.len() == 2));

    // contract_dismissed_notifs removes entry from GetMyBids and GetLoginInfo
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "contract_dismissed_notifs".into(),
            serde_json::to_string(&vec![format!("{bidder_corp}:{priv_cid}")]).unwrap(),
        );
    });
    let after_dismiss = handle(&ctx, &mut corp_bid_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)], &for_corp_kw)
        .unwrap()
        .unwrap();
    assert!(matches!(after_dismiss, PyRep::List(ref l) if l.is_empty()));
}
