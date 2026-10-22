//! Contracts (`E-3`) lifecycle, ItemExchange, Courier, Auction, Visibility, Search, Fees/Limits,
//! Blueprints/Mutated Metadata, Plastic Wrap Break, Rejection, and GM Expiration integration tests.



use crate::common;
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM_TYPE_ID: TypeId = 34;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const CONTRACTING_SKILL: TypeId = 25235;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    ).await.expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

#[tokio::test]
async fn test_e3_contracts_lifecycle_item_exchange_courier_auction() {
    // parity: feat:contracts.item-exchange, feat:contracts.courier, feat:contracts.auction, plan:E-3
    eve_testkit::attest(&["feat:contracts.item-exchange", "feat:contracts.courier", "feat:contracts.auction"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".to_string(),
            AccountRecord {
                user_id: 3,
                username: "third".to_string(),
                password_token: WorldRepository::hash_password_token("third", "third-secret"),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut c3 = snap.characters[&90_000_002].clone();
        c3.character_id = 90_000_003;
        c3.user_id = 3;
        c3.name = "Third Pilot".to_string();
        c3.balance = IskCents::from_isk(50_000_000);
        c3.station_id = Some(JITA_STATION_ID);
        snap.characters.insert(90_000_003, c3);
    });
    let mut s3 = setup_pilot(&ctx, 303, "third", "third-secret", 90_000_003).await;

    // 1. ItemExchange Contract
    let c_exchange = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(50_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(100_000_003)]),
        ],
    ).await.expect("Create ItemExchange Contract");
    let PyRep::KeyVal(c_ex_kv) = c_exchange else { panic!("Expected KeyVal"); };
    let ex_cid = c_ex_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let item_locked = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_003).map(|i| i.flag == InventoryFlag::Locked && i.location_id == ex_cid)
    });
    assert_eq!(item_locked, Some(true));

    let search_rep = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "SearchContracts",
        &[PyRep::String("ItemExchange".into())],
    ).await.expect("SearchContracts");
    let PyRep::List(contracts_list) = search_rep else { panic!("Expected List"); };
    assert!(contracts_list.iter().any(|c| match c {
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()) == Some(ex_cid),
        _ => false,
    }));

    let my_contracts = rpc(&ctx, &mut s1, "contractProxy", "GetMyCurrentContractList", &[]).await
        .expect("GetMyCurrentContractList");
    let PyRep::List(my_list) = my_contracts else { panic!("Expected List"); };
    assert!(my_list.iter().any(|c| match c {
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()) == Some(ex_cid),
        _ => false,
    }));

    let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

    let accept_rep = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(ex_cid)],).await
    .expect("Accept ItemExchange Contract");
    let PyRep::KeyVal(accept_kv) = accept_rep else { panic!("Expected KeyVal"); };
    assert_eq!(accept_kv.get("status"), Some(&PyRep::String("Completed".into())));

    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s1_bal_after.0 - s1_bal_before.0, 5_000_000);
    assert_eq!(s2_bal_before.0 - s2_bal_after.0, 5_000_000);

    let item_transferred = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_003).map(|i| {
            i.owner_id == 90_000_002
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
        })
    });
    assert_eq!(item_transferred, Some(true));

    // 2. Courier Contract
    let courier_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let c_courier = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), // Courier
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(0.0),
            PyRep::Real(20_000.0),  // reward
            PyRep::Real(100_000.0), // collateral
            PyRep::String("".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(courier_item_id)]),
        ],
    ).await.expect("Create Courier Contract");
    let PyRep::KeyVal(c_courier_kv) = c_courier else { panic!("Expected KeyVal"); };
    let courier_cid = c_courier_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s2_bal_pre_courier = ctx.db.get_character(90_000_002).unwrap().balance;
    let accept_courier = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(courier_cid)],
    ).await.expect("Accept Courier Contract");
    let PyRep::KeyVal(ac_kv) = accept_courier else { panic!("Expected KeyVal"); };
    assert_eq!(ac_kv.get("status"), Some(&PyRep::String("InProgress".into())));

    let s2_bal_post_accept = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_pre_courier.0 - s2_bal_post_accept.0, 10_000_000);

    let complete_courier = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "CompleteContract",
        &[PyRep::Integer(courier_cid)],
    ).await.expect("Complete Courier Contract");
    let PyRep::KeyVal(cc_kv) = complete_courier else { panic!("Expected KeyVal"); };
    assert_eq!(cc_kv.get("status"), Some(&PyRep::String("Completed".into())));
    let payout = cc_kv.get("totalPayoutISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(payout, 120_000.0);

    let s2_bal_post_complete = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_post_complete.0 - s2_bal_post_accept.0, 12_000_000);

    // 3. Auction Contract
    let auction_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let c_auction = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2), // Auction
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(50_000.0),
            PyRep::String("".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(auction_item_id)]),
        ],
    ).await.expect("Create Auction Contract");
    let PyRep::KeyVal(c_auc_kv) = c_auction else { panic!("Expected KeyVal"); };
    let auc_cid = c_auc_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s2_bal_before_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    let bid_1 = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(15_000.0)],
    ).await.expect("PlaceBid 1");
    let PyRep::KeyVal(b1_kv) = bid_1 else { panic!("Expected KeyVal"); };
    assert_eq!(b1_kv.get("boughtOut"), Some(&PyRep::Bool(false)));
    let s2_bal_after_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_before_bid.0 - s2_bal_after_bid.0, 1_500_000);

    let s3_bal_before_bid = ctx.db.get_character(90_000_003).unwrap().balance;
    let bid_2 = rpc(
        &ctx,
        &mut s3,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(25_000.0)],
    ).await.expect("PlaceBid 2");
    let PyRep::KeyVal(b2_kv) = bid_2 else { panic!("Expected KeyVal"); };
    assert_eq!(b2_kv.get("boughtOut"), Some(&PyRep::Bool(false)));

    let s2_bal_after_outbid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_after_outbid, s2_bal_before_bid);

    let has_outbid_refund = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_002 && j.entry_type == "ContractAuctionBidRefund"
        })
    });
    assert!(has_outbid_refund);

    let bid_buyout = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(50_000.0)],
    ).await.expect("PlaceBid Buyout");
    let PyRep::KeyVal(bb_kv) = bid_buyout else { panic!("Expected KeyVal"); };
    assert_eq!(bb_kv.get("boughtOut"), Some(&PyRep::Bool(true)));
    assert_eq!(bb_kv.get("status"), Some(&PyRep::String("Completed".into())));

    let s3_bal_after_buyout = ctx.db.get_character(90_000_003).unwrap().balance;
    assert_eq!(s3_bal_after_buyout, s3_bal_before_bid);

    let item_in_s2_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.get(&auction_item_id).map(|i| {
            i.owner_id == 90_000_002
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
        })
    });
    assert_eq!(item_in_s2_hangar, Some(true));
}

#[tokio::test]
async fn test_contracts_item_exchange_and_request() {
    // parity: feat:contracts.item-exchange, feat:contracts.item-exchange-request, feat:removed.loan-contracts
    eve_testkit::attest(&["feat:contracts.item-exchange", "feat:contracts.item-exchange-request", "feat:removed.loan-contracts"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 304, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 305, "other", "other-secret", 90_000_002).await;

    // Verify invalid contract type (e.g., 4 = Loan) is rejected (`feat:removed.loan-contracts`)
    let invalid_type_err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(4), // Loan (unsupported)
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(0),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Invalid Loan".into()),
            PyRep::String("Should fail".into()),
        ],
    ).await.unwrap_err();
    assert_eq!(invalid_type_err.code, "InvalidContractType");

    let c_exchange = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(0),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(75_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Afterburner Sale".into()),
            PyRep::String("1MN Afterburner II in Jita".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(100_000_003), PyRep::Integer(1),])]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create ItemExchange Contract");

    let PyRep::KeyVal(cx_kv) = c_exchange else { panic!("Expected KeyVal"); };
    let exchange_cid = cx_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s1_bal_before_accept = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_before_accept = ctx.db.get_character(90_000_002).unwrap().balance;

    let accept_res = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(exchange_cid)],).await
    .expect("Accept ItemExchange Contract");
    let PyRep::KeyVal(acc_kv) = accept_res else { panic!("Expected KeyVal"); };
    assert_eq!(acc_kv.get("status"), Some(&PyRep::String("Completed".into())));

    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s1_bal_after.0 - s1_bal_before_accept.0, 7_500_000);
    assert_eq!(s2_bal_before_accept.0 - s2_bal_after.0, 7_500_000);

    let item_owner = ctx.db.with_snapshot(|snap| { snap.items.get(&100_000_003).map(|i| (i.owner_id, i.flag)) });
    assert_eq!(item_owner, Some((90_000_002, InventoryFlag::Hangar)));

    let requested_items = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(TRITANIUM_TYPE_ID as i64),
        PyRep::Integer(500),
    ])]);

    let c_req = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(90_000_002),
            PyRep::Integer(20160),
            PyRep::Integer(0),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(0.0),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::String("WTB Tritanium".into()),
            PyRep::String("Need 500 Tritanium".into()),
            PyRep::List(vec![]),
            PyRep::Integer(4),
            requested_items,
            PyRep::Bool(false),
        ],
    ).await.expect("Create ItemExchange with requested items");

    let PyRep::KeyVal(creq_kv) = c_req else { panic!("Expected KeyVal"); };
    let req_cid = creq_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let missing_req_err = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(req_cid)],).await
    .unwrap_err();
    assert_eq!(missing_req_err.code, "MissingRequestedContractItems");

    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let accept_req = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(req_cid)],).await
    .expect("Accept ItemExchange Request Contract");
    let PyRep::KeyVal(accreq_kv) = accept_req else { panic!("Expected KeyVal"); };
    assert_eq!(accreq_kv.get("status"), Some(&PyRep::String("Completed".into())));

    let s1_trit_qty = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            }).map(|i| i.quantity).sum::<i32>()
    });
    assert!(s1_trit_qty >= 500);
}

#[tokio::test]
async fn test_contracts_auction_and_courier() {
    // parity: feat:contracts.auction, feat:contracts.courier, feat:contracts.courier-failure, feat:contracts.expiry
    eve_testkit::attest(&["feat:contracts.auction", "feat:contracts.courier", "feat:contracts.courier-failure", "feat:contracts.expiry"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 306, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 307, "other", "other-secret", 90_000_002).await;

    let (auc_item_id, auc2_item_id, cour_item_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".to_string(),
            AccountRecord {
                user_id: 3,
                username: "third".to_string(),
                password_token: WorldRepository::hash_password_token("third", "third-secret"),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut c3 = snap.characters[&90_000_002].clone();
        c3.character_id = 90_000_003;
        c3.user_id = 3;
        c3.name = "Third Pilot".to_string();
        c3.balance = IskCents::from_isk(50_000_000);
        c3.station_id = Some(JITA_STATION_ID);
        snap.characters.insert(90_000_003, c3);

        snap.next_entity_id += 1;
        let auc_item_id = snap.next_entity_id;
        snap.items.insert(
            auc_item_id,
            InventoryItemRecord {
                item_id: auc_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let auc2_item_id = snap.next_entity_id;
        snap.items.insert(
            auc2_item_id,
            InventoryItemRecord {
                item_id: auc2_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let cour_item_id = snap.next_entity_id;
        snap.items.insert(
            cour_item_id,
            InventoryItemRecord {
                item_id: cour_item_id,
                type_id: 35,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (auc_item_id, auc2_item_id, cour_item_id)
    });
    let mut s3 = setup_pilot(&ctx, 308, "third", "third-secret", 90_000_003).await;

    let c_auction = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(0),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(500_000.0),
            PyRep::String("Rifter Auction".into()),
            PyRep::String("Hull auction".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(auc_item_id), PyRep::Integer(1),])]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create Auction Contract");

    let PyRep::KeyVal(auc_kv) = c_auction else { panic!("Expected KeyVal"); };
    let auc_cid = auc_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let s2_bal_before_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(auc_cid), PyRep::Real(150_000.0)],).await
    .expect("PlaceBid pilot 2");

    let s2_bal_after_bid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_before_bid.0 - s2_bal_after_bid.0, 15_000_000);

    let low_bid_err = rpc(
        &ctx,
        &mut s3,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(151_000.0)],
    ).await.unwrap_err();
    assert_eq!(low_bid_err.code, "BidTooLow");

    rpc(&ctx, &mut s3, "contractProxy", "PlaceBid", &[PyRep::Integer(auc_cid), PyRep::Real(200_000.0)],).await
    .expect("PlaceBid pilot 3 outbid");

    let s2_bal_after_outbid = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_after_outbid, s2_bal_before_bid);

    let my_bids = rpc(&ctx, &mut s3, "contractProxy", "GetMyBids", &[]).await.expect("GetMyBids");
    assert!(matches!(my_bids, PyRep::List(ref l) if !l.is_empty()));

    rpc(&ctx, &mut s3, "contractProxy", "PlaceBid", &[PyRep::Integer(auc_cid), PyRep::Real(500_000.0)],).await
    .expect("Buyout auction");

    let auc_status = ctx.db.with_snapshot(|snap| { snap.contracts.get(&auc_cid).map(|c| c.status.clone()) });
    assert_eq!(auc_status, Some("Completed".into()));

    let item_owner_auc = ctx.db.with_snapshot(|snap| { snap.items.get(&auc_item_id).map(|i| (i.owner_id, i.flag)) });
    assert_eq!(item_owner_auc, Some((90_000_003, InventoryFlag::Hangar)));

    let c_auction2 = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(0),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("No-Buyout Rifter Auction".into()),
            PyRep::String("Settles at expiry".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(auc2_item_id), PyRep::Integer(1),])]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create 2nd Auction Contract");
    let PyRep::KeyVal(auc2_kv) = c_auction2 else { panic!("Expected KeyVal"); };
    let auc2_cid = auc2_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(auc2_cid), PyRep::Real(120_000.0)],).await
    .expect("PlaceBid on 2nd auction");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&auc2_cid) { c.expiration_unix = 1; }
    });
    let settled_auc = rpc(&ctx, &mut s1, "contractProxy", "GetContract", &[PyRep::Integer(auc2_cid)],).await
    .expect("GetContract settles expired auction");
    let PyRep::KeyVal(sauc_kv) = settled_auc else { panic!("Expected KeyVal"); };
    assert_eq!(sauc_kv.get("status"), Some(&PyRep::String("Completed".into())));
    let item_owner_auc2 = ctx.db.with_snapshot(|snap| {
        snap.items.get(&auc2_item_id).map(|i| (i.owner_id, i.flag))
    });
    assert_eq!(item_owner_auc2, Some((90_000_002, InventoryFlag::Hangar)));

    let dest_station_id = 60_003_766_i64;
    let create_ts = ctx.now_unix() as i64;
    let c_courier = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(3),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(dest_station_id),
            PyRep::Real(0.0),
            PyRep::Real(50_000.0),
            PyRep::Real(200_000.0),
            PyRep::String("Haul Pyerite".into()),
            PyRep::String("Deliver to 60003766".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cour_item_id), PyRep::Integer(250),])]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create Courier Contract");

    let PyRep::KeyVal(cour_kv) = c_courier else { panic!("Expected KeyVal"); };
    let cour_cid = cour_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    let listing_exp = cour_kv.get("expirationUnix").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cour_kv.get("numDays"), Some(&PyRep::Integer(3)));
    assert_eq!(listing_exp, create_ts + 20_160 * 60);

    let s2_bal_before_courier = ctx.db.get_character(90_000_002).unwrap().balance;
    let s2_ship_id = s2.ship_id.unwrap();
    let accept_ts = ctx.now_unix() as i64;
    let accept_cour = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cour_cid)],).await
    .expect("Accept Courier Contract");
    let PyRep::KeyVal(acc_cour_kv) = accept_cour else { panic!("Expected KeyVal"); };
    let delivery_exp = acc_cour_kv.get("expirationUnix").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(delivery_exp, accept_ts + 3 * 86_400);
    let wrap_id = acc_cour_kv.get("courierPackageID").and_then(|v| v.as_i64()).unwrap();
    let wrap_item = ctx.db.get_item(wrap_id).unwrap();
    assert_eq!(wrap_item.type_id, 3468);
    assert_eq!(wrap_item.owner_id, 90_000_002);
    assert_eq!(wrap_item.location_id, s2_ship_id);
    assert_eq!(wrap_item.flag, InventoryFlag::Cargo);
    assert_eq!(ctx.db.get_item(cour_item_id).unwrap().location_id, wrap_id);

    let s2_bal_after_accept_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_before_courier.0 - s2_bal_after_accept_cour.0, 20_000_000);

    let wrong_st_err = rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(cour_cid)],).await
    .unwrap_err();
    assert_eq!(wrong_st_err.code, "CourierNotAtDestinationStation");

    s2.station_id = Some(dest_station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = Some(dest_station_id); }
        if let Some(sh) = snap.items.get_mut(&s2_ship_id) { sh.location_id = dest_station_id; }
        if let Some(wrap) = snap.items.get_mut(&wrap_id) { wrap.location_id = JITA_STATION_ID; }
    });
    let missing_pkg_err = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "CompleteContract",
        &[PyRep::Integer(cour_cid)],
    ).await.unwrap_err();
    assert_eq!(missing_pkg_err.code, "CourierPackageMissing");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(wrap) = snap.items.get_mut(&wrap_id) { wrap.location_id = s2_ship_id; }
    });
    rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(cour_cid)],).await
    .expect("Complete Courier Contract at destination station");

    let s2_bal_after_complete_cour = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_after_complete_cour.0 - s2_bal_before_courier.0, 5_000_000);
    assert!(ctx.db.get_item(wrap_id).is_none());
    let delivered_cour_item = ctx.db.with_snapshot(|snap| {
        snap.items.get(&cour_item_id).map(|i| (i.owner_id, i.location_id, i.flag, i.quantity))
    });
    assert_eq!(delivered_cour_item, Some((90_000_001, dest_station_id, InventoryFlag::Hangar, 250)));

    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(sh) = snap.items.get_mut(&s2_ship_id) { sh.location_id = JITA_STATION_ID; }
    });

    let empty_cour_err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(dest_station_id),
            PyRep::Real(0.0),
            PyRep::Real(50_000.0),
            PyRep::Real(300_000.0),
            PyRep::String("Empty Courier".into()),
            PyRep::String("Should fail".into()),
            PyRep::List(vec![]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.unwrap_err();
    assert_eq!(empty_cour_err.code, "CourierContractRequiresItems");

    let (fail_item_id, exp_item_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i2 = snap.next_entity_id;
        snap.items.insert(
            i2,
            InventoryItemRecord {
                item_id: i2,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2)
    });

    let c_fail_courier = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(dest_station_id),
            PyRep::Real(0.0),
            PyRep::Real(50_000.0),
            PyRep::Real(300_000.0),
            PyRep::String("Courier To Fail".into()),
            PyRep::String("Failure test".into()),
            PyRep::List(vec![PyRep::Integer(fail_item_id)]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create Courier Contract for Failure test");
    let PyRep::KeyVal(cfail_kv) = c_fail_courier else { panic!("Expected KeyVal"); };
    let fail_cid = cfail_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(fail_cid)],).await
    .expect("Accept Courier Contract to fail");

    let s1_bal_before_fail = ctx.db.get_character(90_000_001).unwrap().balance;
    let fail_res = rpc(&ctx, &mut s2, "contractProxy", "FailContract", &[PyRep::Integer(fail_cid)],).await
    .expect("FailContract");
    let PyRep::KeyVal(fres_kv) = fail_res else { panic!("Expected KeyVal"); };
    assert_eq!(fres_kv.get("status"), Some(&PyRep::String("Failed".into())));

    let s1_bal_after_fail = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_after_fail.0 - s1_bal_before_fail.0, 35_000_000);

    let c_exp_courier = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(20160),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(dest_station_id),
            PyRep::Real(0.0),
            PyRep::Real(25_000.0),
            PyRep::Real(100_000.0),
            PyRep::String("Overdue Courier".into()),
            PyRep::String("Deadline expiry test".into()),
            PyRep::List(vec![PyRep::Integer(exp_item_id)]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    ).await.expect("Create Courier Contract for deadline expiry");
    let PyRep::KeyVal(cexp_kv) = c_exp_courier else { panic!("Expected KeyVal"); };
    let exp_cour_cid = cexp_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(exp_cour_cid)],).await
    .expect("Accept overdue courier");
    let s1_bal_before_overdue = ctx.db.get_character(90_000_001).unwrap().balance;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&exp_cour_cid) { c.expiration_unix = 1; }
    });
    let overdue_err = rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "CompleteContract",
        &[PyRep::Integer(exp_cour_cid)],
    ).await.unwrap_err();
    assert_eq!(overdue_err.code, "ContractExpired");
    let s1_bal_after_overdue = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_after_overdue.0 - s1_bal_before_overdue.0, 12_500_000);
}

#[tokio::test]
async fn test_contracts_visibility_search_fees_limits_reject_blueprints_and_gm_expire() {
    // parity: feat:contracts.visibility, feat:contracts.search, feat:contracts.my-contracts, feat:contracts.fees-limits, feat:contracts.expiry, feat:contracts.reject, feat:admin.gm-contract-expire, feat:contracts.blueprint-singleton-preservation, feat:contracts.item-exchange-blueprints-metadata, feat:contracts.courier-plastic-wrap-break, rpc:contractProxy.GM_ExpireContract, rpc:contractProxy.CreateContract, rpc:contractProxy.GetMyCurrentContractList, rpc:contractProxy.SearchContracts
    eve_testkit::attest(&["feat:contracts.visibility", "feat:contracts.search", "feat:contracts.my-contracts", "feat:contracts.fees-limits", "feat:contracts.expiry", "feat:contracts.reject", "feat:admin.gm-contract-expire", "feat:contracts.blueprint-singleton-preservation", "feat:contracts.item-exchange-blueprints-metadata", "feat:contracts.courier-plastic-wrap-break", "rpc:contractProxy.GM_ExpireContract"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut s1 = setup_pilot(&ctx, 310, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 311, "other", "other-secret", 90_000_002).await;

    let (bpc_id, abyssal_id, priv_item_id, cour_wrap_item_id, gm_item_id) =
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert(
                "third".to_string(),
                AccountRecord {
                    user_id: 3,
                    username: "third".to_string(),
                    password_token: WorldRepository::hash_password_token("third", "third-secret"),
                    role: 1,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut c3 = snap.characters[&90_000_002].clone();
            c3.character_id = 90_000_003;
            c3.user_id = 3;
            c3.name = "Third Pilot".to_string();
            c3.corporation_id = 1_000_009; // different corporation
            c3.balance = IskCents::from_isk(50_000_000);
            c3.station_id = Some(JITA_STATION_ID);
            snap.characters.insert(90_000_003, c3);

            // Set Contracting skill (25235) to level 0 -> max_slots = 1
            snap.skills.entry(90_000_001).or_default().insert(
                CONTRACTING_SKILL,
                TrainedSkillRecord { type_id: CONTRACTING_SKILL, level: 0, skill_points: 0.0 },
            );

            // 1) BPC with ME=8, TE=14, runsRemaining=7
            snap.next_entity_id += 1;
            let bpc = snap.next_entity_id;
            let mut bp_attrs = HashMap::new();
            bp_attrs.insert(1112, 8.0); // ME
            bp_attrs.insert(1113, 14.0); // TE
            snap.items.insert(
                bpc,
                InventoryItemRecord {
                    item_id: bpc,
                    type_id: RIFTER_BP_TYPE_ID,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: bp_attrs,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(7),
                    ..Default::default()
                },
            );

            // 2) Mutated Abyssal module with source_mutaplasmid_id and dynamic_attributes
            snap.next_entity_id += 1;
            let aby = snap.next_entity_id;
            let mut aby_attrs = HashMap::new();
            aby_attrs.insert(37, 1.35); // speed factor roll
            aby_attrs.insert(3, 0.25); // damage (attr 3)
            snap.items.insert(
                aby,
                InventoryItemRecord {
                    item_id: aby,
                    type_id: 47_702,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: aby_attrs,
                    source_mutaplasmid_id: Some(47_740),
                    ..Default::default()
                },
            );

            // 3) Item for private contract rejection test
            snap.next_entity_id += 1;
            let priv_id = snap.next_entity_id;
            snap.items.insert(
                priv_id,
                InventoryItemRecord {
                    item_id: priv_id,
                    type_id: 35,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );

            // 4) Item for BreakPlasticWrap courier test
            snap.next_entity_id += 1;
            let cw_id = snap.next_entity_id;
            snap.items.insert(
                cw_id,
                InventoryItemRecord {
                    item_id: cw_id,
                    type_id: 36, // Mexallon
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 50,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );

            // 5) Item for GM_ExpireContract test
            snap.next_entity_id += 1;
            let gm_id = snap.next_entity_id;
            snap.items.insert(
                gm_id,
                InventoryItemRecord {
                    item_id: gm_id,
                    type_id: 37, // Isogen
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 80,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            (bpc, aby, priv_id, cw_id, gm_id)
        });
    let mut s3 = setup_pilot(&ctx, 312, "third", "third-secret", 90_000_003).await;

    // 1. Fees & Limits (`feat:contracts.fees-limits`)
    let s1_bal_before_create = ctx.db.get_character(90_000_001).unwrap().balance;
    let c_bp = ctx.dispatch_rpc(
        &mut s1,
        "contractProxy",
        None,
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(0), // Public -> charges brokerFeeISK + depositISK
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(200_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("BPC + Abyssal Pack".into()),
            PyRep::String("With metadata".into()),
            PyRep::List(vec![PyRep::Integer(bpc_id), PyRep::Integer(abyssal_id)]),
        ],
        &kw,
        None,
    ).await.expect("CreateContract with BPC and Abyssal module");
    let PyRep::KeyVal(cbp_kv) = c_bp else { panic!("Expected KeyVal"); };
    let bp_cid = cbp_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();
    let broker_fee = cbp_kv.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    let deposit_fee = cbp_kv.get("depositISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(cbp_kv.get("maxContractsLimit"), Some(&PyRep::Integer(1)));
    assert!(broker_fee >= 10_000.0 && deposit_fee >= 100_000.0);
    let s1_bal_after_create = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_create.0 - s1_bal_after_create.0, IskCents::from_f64_isk(broker_fee + deposit_fee).0);

    // Creating a 2nd contract while Contracting skill = 0 (limit = 1) must fail with ContractSlotLimitReached
    let limit_err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(90_000_002),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Exceed Limit".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(priv_item_id)]),
        ],
    ).await.unwrap_err();
    assert_eq!(limit_err.code, "ContractSlotLimitReached");

    // Train Contracting (25235) to level 4 -> maxContracts = 1 + 4*4 = 17
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            CONTRACTING_SKILL,
            TrainedSkillRecord { type_id: CONTRACTING_SKILL, level: 4, skill_points: 135_765.0 },
        );
    });

    // 2. Blueprint & Mutated Item Metadata (`feat:contracts.item-exchange-blueprints-metadata`, `feat:contracts.blueprint-singleton-preservation`)
    let bp_detail = rpc(&ctx, &mut s2, "contractProxy", "GetContract", &[PyRep::Integer(bp_cid)]).await
        .expect("GetContract BPC + Abyssal");
    let PyRep::KeyVal(bpd_kv) = bp_detail else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(items_meta)) = bpd_kv.get("items") else {
        panic!("Expected items metadata list in GetContract");
    };
    assert_eq!(items_meta.len(), 2);
    let PyRep::KeyVal(bpc_meta) = &items_meta[0] else { panic!("Expected KeyVal for BPC item metadata"); };
    assert_eq!(bpc_meta.get("copy"), Some(&PyRep::Bool(true)));
    assert_eq!(bpc_meta.get("isBlueprintCopy"), Some(&PyRep::Bool(true)));
    assert_eq!(bpc_meta.get("runsRemaining"), Some(&PyRep::Integer(7)));
    assert_eq!(bpc_meta.get("materialEfficiency"), Some(&PyRep::Integer(8)));
    assert_eq!(bpc_meta.get("timeEfficiency"), Some(&PyRep::Integer(14)));
    assert_eq!(bpc_meta.get("singleton"), Some(&PyRep::Bool(true)));

    let PyRep::KeyVal(aby_meta) = &items_meta[1] else { panic!("Expected KeyVal for Abyssal item metadata"); };
    assert_eq!(aby_meta.get("sourceMutaplasmidID"), Some(&PyRep::Integer(47_740)));
    assert_eq!(aby_meta.get("damage"), Some(&PyRep::Real(0.25)));

    // SearchContracts filtered by itemTypeID (`feat:contracts.search`)
    let mut search_kw = HashMap::new();
    search_kw.insert("itemTypeID".into(), PyRep::Integer(RIFTER_BP_TYPE_ID as i64));
    let filtered_search = ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "SearchContracts",
        &[],
        &search_kw,
        None,
    ).await.expect("SearchContracts by itemTypeID");
    let PyRep::List(fs_list) = filtered_search else { panic!("Expected List"); };
    assert!(fs_list.iter().any(|c| match c {
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()) == Some(bp_cid),
        _ => false,
    }));

    // Pilot 2 accepts BPC + Abyssal contract -> singleton, BPC runs/ME/TE, and mutaplasmid attributes preserved!
    rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(bp_cid)]).await
        .expect("Accept BPC + Abyssal contract");
    let transferred_bpc = ctx.db.get_item(bpc_id).unwrap();
    assert_eq!(transferred_bpc.owner_id, 90_000_002);
    assert!(transferred_bpc.singleton);
    assert!(transferred_bpc.is_blueprint_copy);
    assert_eq!(transferred_bpc.blueprint_runs_remaining, Some(7));
    assert_eq!(transferred_bpc.dynamic_attributes.get(&1112), Some(&8.0));
    assert_eq!(transferred_bpc.dynamic_attributes.get(&1113), Some(&14.0));

    let transferred_aby = ctx.db.get_item(abyssal_id).unwrap();
    assert_eq!(transferred_aby.owner_id, 90_000_002);
    assert!(transferred_aby.singleton);
    assert_eq!(transferred_aby.source_mutaplasmid_id, Some(47_740));
    assert_eq!(transferred_aby.dynamic_attributes.get(&37), Some(&1.35));

    // 3. Private Contract Visibility & RejectContract (`feat:contracts.visibility`, `feat:contracts.reject`)
    let c_priv = ctx.dispatch_rpc(
        &mut s1,
        "contractProxy",
        None,
        "CreateContract",
        &[
            PyRep::Integer(1),          // ItemExchange
            PyRep::Integer(1),          // Private
            PyRep::Integer(90_000_002), // Assigned to Pilot 2
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(25_000.0),
            PyRep::Real(5_000.0), // reward escrowed by issuer
            PyRep::Real(0.0),
            PyRep::String("Private to Pilot 2".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(priv_item_id)]),
            PyRep::Integer(4),
        ],
        &kw,
        None,
    ).await.expect("Create Private Contract");
    let PyRep::KeyVal(cpriv_kv) = c_priv else { panic!("Expected KeyVal"); };
    let priv_cid = cpriv_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 3 (unrelated pilot) cannot accept or reject Pilot 2's private contract
    let p3_accept_err = rpc(&ctx, &mut s3, "contractProxy", "AcceptContract", &[PyRep::Integer(priv_cid)]).await
        .unwrap_err();
    assert_eq!(p3_accept_err.code, "ContractAccessDenied");

    // Pilot 2 rejects the private contract via RejectContract -> status = Rejected, items unlocked, reward refunded to s1
    let s1_bal_before_reject = ctx.db.get_character(90_000_001).unwrap().balance;
    let reject_rep = rpc(&ctx, &mut s2, "contractProxy", "RejectContract", &[PyRep::Integer(priv_cid)]).await
        .expect("RejectContract");
    let PyRep::KeyVal(rej_kv) = reject_rep else { panic!("Expected KeyVal"); };
    assert_eq!(rej_kv.get("status"), Some(&PyRep::String("Rejected".into())));
    let s1_bal_after_reject = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_after_reject.0 - s1_bal_before_reject.0, 500_000); // 5,000 ISK reward refunded
    assert_eq!(ctx.db.get_item(priv_item_id).unwrap().flag, InventoryFlag::Hangar);

    // 4. BreakPlasticWrap on Courier Contract (`feat:contracts.courier-plastic-wrap-break`)
    let c_wrap = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), // Courier
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(0.0),
            PyRep::Real(15_000.0),  // reward
            PyRep::Real(120_000.0), // collateral
            PyRep::String("Break Wrap Test".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(cour_wrap_item_id)]),
        ],
    ).await.expect("Create Courier for BreakPlasticWrap");
    let PyRep::KeyVal(cw_kv) = c_wrap else { panic!("Expected KeyVal"); };
    let cw_cid = cw_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let acc_cw = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cw_cid)]).await
        .expect("Accept Courier for BreakPlasticWrap");
    let PyRep::KeyVal(acc_cw_kv) = acc_cw else { panic!("Expected KeyVal"); };
    let pkg_id = acc_cw_kv.get("courierPackageID").and_then(|v| v.as_i64()).unwrap();

    let s1_bal_before_break = ctx.db.get_character(90_000_001).unwrap().balance;
    let break_rep = rpc(&ctx, &mut s2, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(pkg_id)]).await
        .expect("BreakPlasticWrap");
    let PyRep::KeyVal(brk_kv) = break_rep else { panic!("Expected KeyVal"); };
    assert_eq!(brk_kv.get("status"), Some(&PyRep::String("Failed".into())));
    assert_eq!(brk_kv.get("extractedItemIDs"), Some(&PyRep::List(vec![PyRep::Integer(cour_wrap_item_id)])));
    // Issuer receives 120,000 collateral + 15,000 reward refund = 135,000 ISK
    let s1_bal_after_break = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_after_break.0 - s1_bal_before_break.0, 13_500_000);
    // Courier keeps the unwrapped item (`cour_wrap_item_id`)
    let unwrapped_item = ctx.db.get_item(cour_wrap_item_id).unwrap();
    assert_eq!(unwrapped_item.owner_id, 90_000_002);
    assert!(ctx.db.get_item(pkg_id).is_none());

    // 5. GM_ExpireContract (`rpc:contractProxy.GM_ExpireContract`, `feat:admin.gm-contract-expire`, `feat:contracts.expiry`)
    let c_gm = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(40_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("GM Expire Target".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(gm_item_id)]),
        ],
    ).await.expect("Create Contract for GM_ExpireContract");
    let PyRep::KeyVal(cgm_kv) = c_gm else { panic!("Expected KeyVal"); };
    let gm_cid = cgm_kv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Non-GM pilot s2 (`role == 1`) is rejected with InsufficientRole
    let non_gm_err = rpc(&ctx, &mut s2, "contractProxy", "GM_ExpireContract", &[PyRep::Integer(gm_cid)]).await
        .unwrap_err();
    assert_eq!(non_gm_err.code, "InsufficientRole");

    // Grant GM role (`ROLE_GML`) to s1 and call GM_ExpireContract
    s1.role |= eve_machonet::service_roles::ROLE_GML;
    let gm_exp_rep = rpc(&ctx, &mut s1, "contractProxy", "GM_ExpireContract", &[PyRep::Integer(gm_cid)]).await
        .expect("GM_ExpireContract by GM");
    let PyRep::KeyVal(gm_exp_kv) = gm_exp_rep else { panic!("Expected KeyVal"); };
    assert_eq!(gm_exp_kv.get("status"), Some(&PyRep::String("Expired".into())));
    assert_eq!(ctx.db.get_item(gm_item_id).unwrap().flag, InventoryFlag::Hangar);

    // 6. Query contract lists (`feat:contracts.my-contracts`, `feat:contracts.search`)
    let _ = rpc(&ctx, &mut s1, "contractProxy", "GetMyExpiredContractList", &[PyRep::Bool(false)]).await.unwrap();
    let _ = rpc(&ctx, &mut s1, "contractProxy", "GetContractListForOwner", &[PyRep::Integer(90_000_001)]).await.unwrap();
    let _ = rpc(&ctx, &mut s1, "contractProxy", "GetMyBids", &[]).await.unwrap();
    let _ = ctx.dispatch_rpc(&mut s1, "contractProxy", None, "GetMyCurrentContractList", &[], &kw, None).await.unwrap();
}
