//! Integration tests covering open RPC parity rows across market, contracts, insurance, and bounty services.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

#[tokio::test]
async fn test_market_proxy_plex_and_order_rpcs() {
    // parity: rpc:marketProxy.StartupCheck, rpc:marketProxy.GetSystemAsks, rpc:marketProxy.GetHistoryForManyTypeIDs, rpc:marketProxy.GetCorporationOrders, rpc:marketProxy.GetMarketOrderHistory, rpc:marketProxy.ModifyPlexCharOrder, rpc:marketProxy.PlacePlexSellOrder, rpc:marketProxy.GetPlexBest, rpc:marketProxy.GetPlexOrders, rpc:marketProxy.GetPlexHistory, rpc:marketProxy.GetPlexNewPriceHistory, rpc:marketProxy.GetPlexOldPriceHistory
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 101, "demo", "demo-secret", 90_000_001).await;
    let empty_kw = HashMap::new();

    // 1. StartupCheck
    let res = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "StartupCheck", &[], &empty_kw, None)
        .await
        .expect("StartupCheck");
    assert!(matches!(res, PyRep::KeyVal(_)));

    // 2. GetSystemAsks
    let asks = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetSystemAsks",
            &[PyRep::Integer(30_000_142)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetSystemAsks");
    assert!(matches!(asks, PyRep::ObjectEx { .. } | PyRep::Rowset { .. }));

    // 3. GetHistoryForManyTypeIDs
    let history = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![
                PyRep::Integer(34),
                PyRep::Integer(1230),
            ])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetHistoryForManyTypeIDs");
    assert!(matches!(history, PyRep::Dict(_)));

    // 4. GetCorporationOrders
    let corp_orders = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetCorporationOrders",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetCorporationOrders");
    assert!(matches!(corp_orders, PyRep::Rowset { .. }));

    // 5. GetMarketOrderHistory
    let order_hist = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetMarketOrderHistory",
            &[PyRep::Bool(false)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetMarketOrderHistory");
    assert!(matches!(order_hist, PyRep::Rowset { .. }));

    // 6. Give character 1000 PLEX in vault and 100M ISK, then PlacePlexSellOrder
    let initial_vault = ctx.db.with_mut_snapshot(|snap| {
        let _ = snap.adjust_plex_vault(90_000_001, 1000);
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents::from_isk(100_000_000);
        }
        snap.plex_vault_balance(90_000_001)
    });

    let place_res = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlacePlexSellOrder",
            &[
                PyRep::Real(4_500_000.0),
                PyRep::Integer(100),
                PyRep::Integer(90),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlacePlexSellOrder");
    let kv = kv_map(&place_res);
    let order_id = kv.get("orderID").and_then(|v| v.as_i64()).expect("orderID");
    assert!(order_id > 0);

    // Verify vault balance decreased by 100
    let remaining_vault = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(90_000_001));
    assert_eq!(remaining_vault, initial_vault - 100);

    // 7. GetPlexBest
    let best = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetPlexBest", &[], &empty_kw, None)
        .await
        .expect("GetPlexBest");
    let best_kv = kv_map(&best);
    assert!(best_kv.contains_key("bestAskPrice"));

    // 8. GetPlexOrders
    let orders = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetPlexOrders", &[], &empty_kw, None)
        .await
        .expect("GetPlexOrders");
    assert!(matches!(orders, PyRep::Tuple(_)));

    // 9. GetPlexHistory, GetPlexNewPriceHistory, GetPlexOldPriceHistory
    let h1 = ctx
        .dispatch_rpc(&mut s1, "marketProxy", None, "GetPlexHistory", &[], &empty_kw, None)
        .await
        .expect("GetPlexHistory");
    assert!(matches!(h1, PyRep::ObjectEx { .. } | PyRep::Rowset { .. }));

    let h2 = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetPlexNewPriceHistory",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetPlexNewPriceHistory");
    assert!(matches!(h2, PyRep::ObjectEx { .. } | PyRep::Rowset { .. }));

    let h3 = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "GetPlexOldPriceHistory",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetPlexOldPriceHistory");
    assert!(matches!(h3, PyRep::ObjectEx { .. } | PyRep::Rowset { .. }));

    // 10. Advance virtual clock by 305 seconds to bypass modification cooldown, then ModifyPlexCharOrder
    ctx.advance_secs(305);
    let mod_res = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "ModifyPlexCharOrder",
            &[PyRep::Integer(order_id), PyRep::Real(4_600_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("ModifyPlexCharOrder");
    let mod_kv = kv_map(&mod_res);
    assert_eq!(mod_kv.get("newPrice"), Some(&PyRep::Real(4_600_000.0)));

    // Assert order price in DB mutated
    let db_price = ctx.db.with_snapshot(|snap| {
        snap.market_orders.get(&order_id).map(|o| o.price.to_wire_f64())
    });
    assert_eq!(db_price, Some(4_600_000.0));
}

#[tokio::test]
async fn test_contract_proxy_and_insurance_rpcs() {
    // parity: rpc:contractProxy.GetContractListForOwner, rpc:contractProxy.GetMyBids, rpc:contractProxy.GetMyExpiredContractList, rpc:insuranceSvc.GetContracts, rpc:insuranceSvc.GetInsurancePrices
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;
    let empty_kw = HashMap::new();

    // Give both pilots ISK and items in station
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.station_id = Some(60_003_760);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.station_id = Some(60_003_760);
        }
        // Give pilot 1 a Rifter in station
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    s1.station_id = Some(60_003_760);
    s2.station_id = Some(60_003_760);

    // 1. insuranceSvc.GetInsurancePrices
    let prices = ctx
        .dispatch_rpc(
            &mut s1,
            "insuranceSvc",
            None,
            "GetInsurancePrices",
            &[PyRep::List(vec![PyRep::Integer(587)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetInsurancePrices");
    let PyRep::List(price_entries) = prices else {
        panic!("Expected list from GetInsurancePrices");
    };
    assert!(!price_entries.is_empty());

    // 2. Insure ship and call insuranceSvc.GetContracts
    let ship_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.owner_id == 90_000_001 && i.type_id == 587)
                .map(|i| i.item_id)
        })
        .expect("ship_id");
    s1.ship_id = Some(ship_id);

    let _insure = ctx
        .dispatch_rpc(
            &mut s1,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("InsureShip");

    let contracts = ctx
        .dispatch_rpc(&mut s1, "insuranceSvc", None, "GetContracts", &[], &empty_kw, None)
        .await
        .expect("GetContracts");
    let PyRep::List(insurance_contracts) = contracts else {
        panic!("Expected list from GetContracts");
    };
    assert_eq!(insurance_contracts.len(), 1);

    // 3. Create ItemExchange contract and call contractProxy.GetContractListForOwner
    let ex_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                ..Default::default()
            },
        );
        iid
    });

    let contract_res = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1),          // ItemExchange
                PyRep::Integer(0),          // Public
                PyRep::Integer(0),          // Assignee
                PyRep::Integer(1440),       // Expire minutes
                PyRep::Integer(1),          // Days to complete
                PyRep::Integer(60_003_760), // Start station
                PyRep::Integer(60_003_760), // End station
                PyRep::Real(100_000.0),     // Price
                PyRep::Real(0.0),           // Reward
                PyRep::Real(0.0),           // Collateral
                PyRep::String("ItemExchange Contract Test".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(ex_item_id)]),
                PyRep::List(vec![]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("CreateContract ItemExchange");
    let c_kv = kv_map(&contract_res);
    let contract_id = c_kv.get("contractID").and_then(|v| v.as_i64()).expect("contractID");

    let owner_list = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetContractListForOwner",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetContractListForOwner");
    let PyRep::List(c_list) = owner_list else {
        panic!("Expected list from GetContractListForOwner");
    };
    assert!(c_list
        .iter()
        .any(|c| kv_map(c).get("contractID") == Some(&PyRep::Integer(contract_id))));

    // 4. Create an Auction contract, bid with pilot 2, call contractProxy.GetMyBids
    let auc_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let iid = snap.next_entity_id;
        snap.items.insert(
            iid,
            eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        iid
    });

    let auc_res = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2),          // Auction
                PyRep::Integer(0),          // Public
                PyRep::Integer(0),          // Assignee
                PyRep::Integer(1440),       // Expire minutes
                PyRep::Integer(1),          // Days
                PyRep::Integer(60_003_760), // Start station
                PyRep::Integer(60_003_760), // End station
                PyRep::Real(50_000.0),      // Starting bid
                PyRep::Real(0.0),           // Reward
                PyRep::Real(1_000_000.0),   // Buyout
                PyRep::String("Auction Contract Test".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(auc_item_id)]),
                PyRep::List(vec![]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("CreateContract Auction");
    let auc_id = kv_map(&auc_res).get("contractID").and_then(|v| v.as_i64()).expect("auc_id");

    // Pilot 2 places bid
    let _bid = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(auc_id), PyRep::Real(60_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBid");

    // Check GetMyBids for pilot 2
    let my_bids = ctx
        .dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[], &empty_kw, None)
        .await
        .expect("GetMyBids");
    let PyRep::List(bids_list) = my_bids else {
        panic!("Expected list from GetMyBids");
    };
    assert!(bids_list
        .iter()
        .any(|c| kv_map(c).get("contractID") == Some(&PyRep::Integer(auc_id))));

    // 5. Expire the ItemExchange contract via GM_ExpireContract, then call contractProxy.GetMyExpiredContractList
    ctx.db.provision_gm_account("gm_contract", "gm-pass", 90_000_003, "GM Contract");
    let mut gm = SessionState::new(203);
    common::login_pilot(&ctx, &mut gm, "gm_contract", "gm-pass").await;
    ctx.dispatch_rpc(
        &mut gm,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &empty_kw,
        None,
    )
    .await
    .expect("SelectCharacterID GM");

    let _expire = ctx
        .dispatch_rpc(
            &mut gm,
            "contractProxy",
            None,
            "GM_ExpireContract",
            &[PyRep::Integer(contract_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("GM_ExpireContract");

    let expired_list = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetMyExpiredContractList",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetMyExpiredContractList");
    let PyRep::List(exp_list) = expired_list else {
        panic!("Expected list from GetMyExpiredContractList");
    };
    assert!(exp_list
        .iter()
        .any(|c| kv_map(c).get("contractID") == Some(&PyRep::Integer(contract_id))));
}
