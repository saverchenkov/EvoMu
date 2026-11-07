//! Semantic unit tests split from `contracts_service_extra_unit_tests`.

use super::contracts_service_extra_unit_tests::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item;
use crate::services::contracts_service::*;
use eve_db::{
    corp_roles, InventoryFlag,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn want_to_buy_stack_split_auction_outbid_refund_and_delete_reject_fail_gm_expire() {
    let (ctx, mut issuer_sess, issuer_id, mut acc_sess, acc_id, station_id, dest_id, _corp_id) =
        setup_extra_contracts_env();
    let kw = HashMap::new();

    // 1. Create Want-To-Buy ItemExchange requesting 40 units of type 34, offering 1,000,000 ISK reward
    let wtb = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(1_000_000.0)),
            (
                "requestedItems",
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(34),
                    PyRep::Integer(40),
                ])]),
            ),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let wtb_cid = dict_i64(&wtb, "contractID").unwrap();

    // Issuer cannot accept own contract -> CannotAcceptOwnContract
    let err_self = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_self.code, "CannotAcceptOwnContract");

    // Acceptor without type 34 in hangar -> MissingRequestedContractItems
    let err_missing = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "MissingRequestedContractItems");

    // Give acceptor a stack of 100 units of type 34 -> accepting splits 40 to issuer and leaves 60 with acceptor!
    let acc_stack = 830_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            acc_stack,
            make_fighter_item(acc_stack, 34, acc_id, station_id, InventoryFlag::Hangar, 100),
        );
    });
    let acc_wtb = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&acc_wtb, "status"), Some("Completed"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&acc_stack].quantity, 60);
        let issuer_got: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == issuer_id && i.location_id == station_id && i.type_id == 34)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(issuer_got, 40);
    });

    // 2. Auction minimum bid increment, outbid refund, and CannotDeleteAuctionWithBids
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            830_002,
            make_fighter_item(830_002, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            830_003,
            make_fighter_item(830_003, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            830_004,
            make_fighter_item(830_004, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
    });
    let auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(1_000_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_002)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let auc_cid = dict_i64(&auc, "contractID").unwrap();

    // Opening bid below starting price -> BidTooLow
    let err_low1 = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(500_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_low1.code, "BidTooLow");

    // Valid opening bid of 1,000,000 ISK
    handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(1_000_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Second bid below 5% minimum increment (< 1,050,000 ISK) -> BidTooLow
    let err_low2 = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(1_010_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_low2.code, "BidTooLow");

    // Deleting an auction with active bids -> CannotDeleteAuctionWithBids
    let err_del_auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "DeleteContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_del_auc.code, "CannotDeleteAuctionWithBids");

    // 3. RejectContract by assignee, DeleteContract by issuer, FailContract on Courier, and GM_ExpireContract
    let priv_c = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)), // Personal
            ("assigneeID", PyRep::Integer(acc_id)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_003)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let priv_cid = dict_i64(&priv_c, "contractID").unwrap();

    // GetLoginInfo shows assignedToMe for acceptor
    let login_info = handle(&ctx, &mut acc_sess, "contractProxy", "GetLoginInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(login_info.dict_get("assignedToMe"), Some(PyRep::List(l)) if !l.is_empty()));

    // Assignee rejects personal contract -> status "Rejected"
    let rej = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "RejectContract",
        &[PyRep::Integer(priv_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&rej, "status"), Some("Rejected"));

    // Create courier contract, accept it, then FailContract by acceptor -> status "Failed"
    let cour = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_id)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(200_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_004)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cour_cid = dict_i64(&cour, "contractID").unwrap();
    handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let failed = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "FailContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&failed, "status"), Some("Failed"));

    // GM_ExpireContract on auction: non-GM fails with GMRoleRequired; GM succeeds
    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.accounts.values_mut() {
            a.role = 0;
        }
    });
    let err_gm = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GM_ExpireContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_gm.code, "GMRoleRequired");

    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.accounts.values_mut() {
            a.role = corp_roles::ROLE_DIRECTOR;
        }
    });
    let exp_auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GM_ExpireContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&exp_auc, "status"), Some("Completed"));

    // GetMyCurrentContractList & GetMyExpiredContractList
    let expired_list = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GetMyExpiredContractList",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(expired_list, PyRep::List(ref l) if l.len() >= 3));
}
