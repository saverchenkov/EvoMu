//! Semantic unit tests split from `market_contracts_tests`.

#![forbid(unsafe_code)]

use super::*;

use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn market_contracts_startup_check_queries_and_history() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    let startup = handle(&ctx, &mut sess, "marketProxy", "StartupCheck", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(startup.dict_get("status").and_then(PyRep::as_str), Some("ok"));
    assert_eq!(
        startup.dict_get("sleeperBuyOrdersSeeded").and_then(PyRep::as_bool),
        Some(true)
    );
    let npc_active = startup
        .dict_get("npcOrdersActive")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    assert!(npc_active >= (SLEEPER_BLUE_LOOT_ORDERS.len() as i64));

    let sys_asks = handle(&ctx, &mut sess, "marketProxy", "GetSystemAsks", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(sys_asks.is_cindexed_rowset());

    // GetHistoryForManyTypeIDs with List and with PySet
    let hist_list = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(35)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hist_list.row_count(), None);
    assert!(matches!(hist_list, PyRep::Dict(ref entries) if entries.len() == 2));

    let hist_set = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::PySet(vec![PyRep::Integer(34)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::Dict(entries) = hist_set {
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].1, PyRep::Tuple(ref t) if t.len() == 2));
    } else {
        panic!("expected Dict from GetHistoryForManyTypeIDs");
    }

    let bad_hist = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::Integer(34)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_hist.code, "InvalidArgument");

    // GetCorporationOrders: own corp succeeds, foreign corp rejected unless GM
    let corp_orders = handle(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(corp_orders.row_count(), Some(0));

    let foreign_err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetCorporationOrders",
        &[PyRep::Integer(corp_id + 999)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(foreign_err.code, "AccessDenied");

    sess.role |= service_roles::ROLE_GML;
    let gm_ok = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetCorporationOrders",
        &[PyRep::Integer(corp_id + 999)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm_ok.row_count(), Some(0));
}

#[test]
fn modify_char_order_validation_cooldown_and_price_changes() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    // Seed personal buy order placed 600s ago (past 300s cooldown)
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        s.market_orders.insert(
            710_001,
            MarketOrderRecord {
                order_id: 710_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(10_000),
            },
        );
        save_market_order_meta(
            s,
            710_001,
            MarketOrderMeta {
                range: -1,
                duration: 90,
                issue_unix: now.saturating_sub(600),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });

    // Invalid tick size (5 sig figs) rejected
    let tick_err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(710_001), PyRep::Real(12345.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(tick_err.code, "InvalidPriceTickSize");

    // ModifyPlexCharOrder on non-PLEX order rejected
    let plex_err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyPlexCharOrder",
        &[PyRep::Integer(710_001), PyRep::Real(12.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(plex_err.code, "InvalidPlexOrder");

    // Price increase from 10 ISK -> 15 ISK (qty 1000 => +5000 ISK escrow + relist fee)
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let mod_ok = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(710_001), PyRep::Real(15.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mod_ok.dict_get("orderID").and_then(PyRep::as_i64), Some(710_001));

    ctx.db.with_snapshot(|s| {
        let ord = &s.market_orders[&710_001];
        assert_eq!(ord.price, IskCents::from_isk(15));
        assert_eq!(ord.escrow, IskCents::from_isk(15_000));
        assert!(s.characters[&char_id].balance < bal_before);
    });

    // Immediate second modify within 300s cooldown fails
    let cd_err = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(710_001), PyRep::Real(16.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(cd_err.code, "OrderModificationCooldown");
}

#[test]
fn plex_orders_history_and_contract_auction_finish_lifecycle() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let bidder_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(500_000_000);
        }
        let cur_plex = s.plex_vault_balance(char_id);
        let _ = s.adjust_plex_vault(char_id, 50 - cur_plex);
    });

    // PlacePlexSellOrder consumes from PLEX vault
    let sell_res = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlacePlexSellOrder",
        &[PyRep::Real(4_500_000.0), PyRep::Integer(10), PyRep::Integer(90)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let plex_oid = sell_res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    assert!(plex_oid > 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 40);

    let best = handle(&ctx, &mut sess, "marketProxy", "GetPlexBest", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(best.dict_get("bestAskPrice").and_then(PyRep::as_f64), Some(4_500_000.0));
    assert_eq!(best.dict_get("totalAskVolume").and_then(PyRep::as_i64), Some(10));

    let plex_orders = handle(&ctx, &mut sess, "marketProxy", "GetPlexOrders", &[], &kw)
        .unwrap()
        .unwrap();
    if let PyRep::Tuple(parts) = plex_orders {
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].crowset_rows().map(|r| r.len()), Some(1));
    } else {
        panic!("expected Tuple from GetPlexOrders");
    }

    let plex_hist = handle(&ctx, &mut sess, "marketProxy", "GetPlexHistory", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(plex_hist.is_crowset());

    // Account & Vault balance RPCs
    let cash = handle(&ctx, &mut sess, "account", "GetCashBalance", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(cash.as_f64().unwrap() > 0.0);
    let vault = handle(&ctx, &mut sess, "vaultManager", "get_account_balance", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(vault.as_i64(), Some(40));

    // FinishAuction with winning bidder & deposit refund + GetMyContractEscrow + GetNumItemsInContainers
    let (auction_id, auction_item_id, container_id) = ctx.db.with_mut_snapshot(|s| {
        let mut bidder = s.characters[&char_id].clone();
        bidder.character_id = bidder_id;
        bidder.user_id = 2;
        s.characters.insert(bidder_id, bidder);

        s.next_entity_id += 1;
        let item_id = s.next_entity_id;
        s.items.insert(item_id, InventoryItemRecord {
            item_id, type_id: 587, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Locked, quantity: 1, singleton: true, version: 1, ..Default::default()
        });

        s.next_entity_id += 1;
        let cont_id = s.next_entity_id;
        s.items.insert(cont_id, InventoryItemRecord {
            item_id: cont_id, type_id: 23, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        s.next_entity_id += 1;
        let inside_id = s.next_entity_id;
        s.items.insert(inside_id, InventoryItemRecord {
            item_id: inside_id, type_id: 34, owner_id: char_id, location_id: cont_id,
            flag: InventoryFlag::Hangar, quantity: 250, singleton: false, version: 1, ..Default::default()
        });

        s.next_entity_id += 1;
        let cid = s.next_entity_id;
        s.contracts.insert(cid, ContractRecord {
            contract_id: cid, issuer_id: char_id, acceptor_id: None,
            contract_type: "Auction".to_string(), status: "Outstanding".to_string(),
            start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID,
            price: IskCents::from_isk(1_000_000), reward: IskCents(0), collateral: IskCents(0),
            item_ids: vec![item_id], expiration_unix: now.saturating_sub(10),
            issuer_corp_id: corp_id, assignee_id: 0, availability: "Public".to_string(),
            buyout: IskCents::from_isk(10_000_000), current_bid: IskCents::from_isk(2_500_000),
            current_bidder_id: Some(bidder_id), ..Default::default()
        });
        s.fleet_and_social_ops.contract_deposits.insert(cid, 1_000_000);
        (cid, item_id, cont_id)
    });

    let page_info = handle(&ctx, &mut sess, "contractProxy", "CollectMyPageInfo", &[], &kw).unwrap().unwrap();
    assert_eq!(page_info.dict_get("numOutstandingContractsNonCorp").and_then(PyRep::as_i64), Some(1));

    let cont_counts = handle(&ctx, &mut sess, "contractProxy", "GetNumItemsInContainers", &[PyRep::List(vec![PyRep::Integer(container_id)])], &kw).unwrap().unwrap();
    let PyRep::Dict(pairs) = cont_counts else { panic!("expected Dict from GetNumItemsInContainers"); };
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0.as_i64(), Some(container_id));
    assert_eq!(pairs[0].1.as_i64(), Some(1));

    let in_cont = handle(&ctx, &mut sess, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(container_id)], &kw).unwrap().unwrap();
    assert_eq!(in_cont.row_count(), Some(1));

    let issuer_bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let finished = handle(&ctx, &mut sess, "contractProxy", "FinishAuction", &[PyRep::Integer(auction_id)], &kw).unwrap().unwrap();
    assert_eq!(finished, PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.contracts[&auction_id].status, "Finished");
        assert_eq!(s.contracts[&auction_id].acceptor_id, Some(bidder_id));
        assert_eq!(s.items[&auction_item_id].owner_id, bidder_id);
        assert_eq!(s.items[&auction_item_id].flag, InventoryFlag::Hangar);
        assert_eq!(s.characters[&char_id].balance, IskCents(issuer_bal_before.0 + 2_500_000_00 + 1_000_000));
    });

    let my_escrow = handle(&ctx, &mut sess, "contractProxy", "GetMyContractEscrow", &[], &kw).unwrap().unwrap();
    assert_eq!(my_escrow.dict_get("iskEscrow").and_then(PyRep::as_f64), Some(0.0));
    let order_hist = handle(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)], &kw).unwrap().unwrap();
    assert_eq!(order_hist.row_count(), Some(0));
}

#[test]
fn collect_my_page_info_computes_counts_and_outstanding_contracts_dynamically() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let peer_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let peer_corp_id = 98_000_777_i64;
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // 1. Fresh character with no contracts must return empty outstandingContracts (no hardcoded station dummy) and 0 counts.
    let empty_info = handle(&ctx, &mut sess, "contractProxy", "CollectMyPageInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        empty_info.dict_get("numOutstandingContractsNonCorp").and_then(PyRep::as_i64),
        Some(0)
    );
    assert_eq!(
        empty_info.dict_get("numOutstandingContractsForCorp").and_then(PyRep::as_i64),
        Some(0)
    );
    assert_eq!(
        empty_info.dict_get("outstandingContracts"),
        Some(&PyRep::List(Vec::new()))
    );

    // 2. Seed diverse personal/corp contracts across Outstanding, InProgress, Rejected/Expired attention, and Auction bids.
    ctx.db.with_mut_snapshot(|s| {
        s.contracts.insert(
            701,
            ContractRecord {
                contract_id: 701,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                assignee_id: peer_id,
                contract_type: "Courier".to_string(),
                status: "Outstanding".to_string(),
                for_corp: false,
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                ..Default::default()
            },
        );
        s.contracts.insert(
            702,
            ContractRecord {
                contract_id: 702,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                acceptor_id: Some(peer_id),
                assignee_id: 0,
                contract_type: "Courier".to_string(),
                status: "InProgress".to_string(),
                for_corp: true,
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                ..Default::default()
            },
        );
        s.contracts.insert(
            703,
            ContractRecord {
                contract_id: 703,
                issuer_id: peer_id,
                issuer_corp_id: peer_corp_id,
                assignee_id: char_id,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                for_corp: false,
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                ..Default::default()
            },
        );
        s.contracts.insert(
            704,
            ContractRecord {
                contract_id: 704,
                issuer_id: peer_id,
                issuer_corp_id: peer_corp_id,
                assignee_id: 0,
                contract_type: "Auction".to_string(),
                status: "Outstanding".to_string(),
                for_corp: false,
                current_bidder_id: Some(char_id),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                ..Default::default()
            },
        );
    });

    let info = handle(&ctx, &mut sess, "contractProxy", "CollectMyPageInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        info.dict_get("numOutstandingContractsNonCorp").and_then(PyRep::as_i64),
        Some(1)
    );
    assert_eq!(
        info.dict_get("numOutstandingContractsForCorp").and_then(PyRep::as_i64),
        Some(0)
    );
    assert_eq!(
        info.dict_get("numInProgress").and_then(PyRep::as_i64),
        Some(0)
    );
    assert_eq!(
        info.dict_get("numInProgressCorp").and_then(PyRep::as_i64),
        Some(1)
    );
    assert_eq!(
        info.dict_get("numRequiresAttention").and_then(PyRep::as_i64),
        Some(1)
    );
    assert_eq!(
        info.dict_get("numBiddingOn").and_then(PyRep::as_i64),
        Some(1)
    );
    let Some(PyRep::List(entries)) = info.dict_get("outstandingContracts") else {
        panic!("expected outstandingContracts List");
    };
    let expected = PyRep::Tuple(vec![
        PyRep::Integer(peer_id),
        PyRep::Integer(peer_corp_id),
        PyRep::Integer(char_id),
        PyRep::Integer(1),
    ]);
    assert!(
        entries.contains(&expected),
        "expected assigned ItemExchange tuple {expected:?} in {entries:?}"
    );
    assert!(
        !entries.iter().any(|e| format!("{e:?}").contains("60003760")),
        "hardcoded Jita dummy must not appear in {entries:?}"
    );
}
