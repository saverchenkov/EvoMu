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
    let PyRep::Dict(price_entries) = prices else {
        panic!("Expected Dict from GetInsurancePrices, got {prices:?}");
    };
    assert!(price_entries.iter().any(|(k, v)| k.as_i64() == Some(587) && v.as_f64().is_some_and(|p| p > 0.0)));

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

    // 4. Create two Auction contracts (one won by Pilot 2, one where Pilot 2 is outbid), test GetMyBids(isAccepted, ownerID)
    let (auc_item_id, auc2_item_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(i1, eve_db::InventoryItemRecord {
            item_id: i1, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 5_000, singleton: false, ..Default::default()
        });
        snap.next_entity_id += 1;
        let i2 = snap.next_entity_id;
        snap.items.insert(i2, eve_db::InventoryItemRecord {
            item_id: i2, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 5_000, singleton: false, ..Default::default()
        });
        (i1, i2)
    });

    let make_auc = |iid: i64, title: &str| vec![
        PyRep::Integer(2), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(60_003_760), PyRep::Integer(60_003_760), PyRep::Real(50_000.0), PyRep::Real(0.0),
        PyRep::Real(1_000_000.0), PyRep::String(title.into()), PyRep::String("".into()),
        PyRep::List(vec![PyRep::Integer(iid)]), PyRep::List(vec![]),
    ];
    let auc_res = ctx.dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &make_auc(auc_item_id, "Auction Won"), &empty_kw, None).await.expect("CreateContract Auction 1");
    let auc_id = kv_map(&auc_res).get("contractID").and_then(|v| v.as_i64()).expect("auc_id");
    let auc2_res = ctx.dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &make_auc(auc2_item_id, "Auction Outbid"), &empty_kw, None).await.expect("CreateContract Auction 2");
    let auc2_id = kv_map(&auc2_res).get("contractID").and_then(|v| v.as_i64()).expect("auc2_id");

    // Pilot 2 bids on both auctions
    ctx.dispatch_rpc(&mut s2, "contractProxy", None, "PlaceBid", &[PyRep::Integer(auc_id), PyRep::Real(60_000.0)], &empty_kw, None).await.expect("PlaceBid 1");
    ctx.dispatch_rpc(&mut s2, "contractProxy", None, "PlaceBid", &[PyRep::Integer(auc2_id), PyRep::Real(60_000.0)], &empty_kw, None).await.expect("PlaceBid 2");

    // Active bids query (isAccepted = false) includes both active auctions; won/finished query (isAccepted = true) is empty
    let PyRep::List(active_bids) = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[PyRep::Bool(false)], &empty_kw, None).await.expect("GetMyBids active") else { panic!("Expected list"); };
    assert!(active_bids.iter().any(|c| kv_map(c).get("contractID") == Some(&PyRep::Integer(auc_id))));
    assert!(active_bids.iter().any(|c| kv_map(c).get("contractID") == Some(&PyRep::Integer(auc2_id))));
    let PyRep::List(won_before) = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[PyRep::Bool(true)], &empty_kw, None).await.expect("GetMyBids won before") else { panic!("Expected list"); };
    assert!(won_before.is_empty(), "GetMyBids(isAccepted=true) must exclude still-active auctions");

    // Corporation bid query requires ROLE_CONTRACT_MANAGER or Director
    let corp2_id = s2.corporation_id.unwrap_or(1_000_009);
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp2_id).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp2_id;
            __corp.name = "Pilot2 Corp".into();
            __corp.ticker = "P2C".into();
            __corp.ceo_id = 90_000_001;
            __corp });
        corp.members.insert(90_000_002);
        corp.member_roles.insert(90_000_002, 0);
    });
    s2.corp_role = 0;
    let corp_denied = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[PyRep::Bool(false), PyRep::Integer(corp2_id)], &empty_kw, None).await;
    assert_eq!(corp_denied.unwrap_err().code, "CorpRoleDenied");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp2_id) {
            corp.member_roles.insert(90_000_002, eve_db::corp_roles::ROLE_CONTRACT_MANAGER);
        }
    });
    s2.corp_role = eve_db::corp_roles::ROLE_CONTRACT_MANAGER;
    let corp_ok = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[PyRep::Bool(false), PyRep::Integer(corp2_id)], &empty_kw, None).await;
    assert!(corp_ok.is_ok());

    // Outbid Pilot 2 on auc2_id and finish both auctions, then verify GetMyBids(true) only returns won auc_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.contracts.get_mut(&auc_id) {
            c1.status = "Finished".into();
            c1.acceptor_id = Some(90_000_002);
        }
        if let Some(c2) = snap.contracts.get_mut(&auc2_id) {
            c2.current_bidder_id = Some(90_000_003);
            c2.acceptor_id = Some(90_000_003);
            c2.status = "Finished".into();
        }
    });
    let PyRep::List(won_after) = ctx.dispatch_rpc(&mut s2, "contractProxy", None, "GetMyBids", &[PyRep::Bool(true)], &empty_kw, None).await.expect("GetMyBids won after") else { panic!("Expected list"); };
    assert_eq!(won_after.len(), 1);
    assert_eq!(kv_map(&won_after[0]).get("contractID"), Some(&PyRep::Integer(auc_id)));

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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_market_contracts_bounty_ext_b {
    use super::*;

// Part 2 of integration tests covering bountyProxy and kill rights.

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
        PyRep::Tuple(t) => t.get(1).map(kv_map).unwrap_or_default(),
        PyRep::List(l) => l.first().map(kv_map).unwrap_or_default(),
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
async fn test_bounty_proxy_and_kill_rights_rpcs() {
    // parity: rpc:bountyProxy.AddToBounty, rpc:bountyProxy.CancelSellKillRight, rpc:bountyProxy.GMClearBountyCache, rpc:bountyProxy.GMReimburseBounties, rpc:bountyProxy.GetBounties, rpc:bountyProxy.GetBountiesAndKillRights, rpc:bountyProxy.GetKillRightsOnCharacters, rpc:bountyProxy.GetMyBounties, rpc:bountyProxy.GetMyKillRights, rpc:bountyProxy.GetTopAllianceBounties
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let _s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;
    let empty_kw = HashMap::new();

    // Give pilots ISK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
    });

    let bal_before = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);

    // 1. AddToBounty: pilot 1 places 500,000 ISK bounty on pilot 2
    let add_res = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "AddToBounty",
            &[PyRep::Integer(90_000_002), PyRep::Real(500_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("AddToBounty");
    let add_kv = kv_map(&add_res);
    assert_eq!(add_kv.get("targetID"), Some(&PyRep::Integer(90_000_002)));
    assert_eq!(add_kv.get("addedISK"), Some(&PyRep::Real(500_000.0)));

    let bal_after = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    assert_eq!(bal_before - bal_after, 50_000_000);

    // 2. GetBounties (respects owner filter)
    let bounties = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetBounties", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &empty_kw, None)
        .await
        .expect("GetBounties with filter");
    let PyRep::List(b_list) = bounties else {
        panic!("Expected list from GetBounties");
    };
    assert!(b_list
        .iter()
        .any(|b| kv_map(b).get("targetID") == Some(&PyRep::Integer(90_000_002))));

    // 3. GetMyBounties
    let my_bounties = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyBounties", &[], &empty_kw, None)
        .await
        .expect("GetMyBounties");
    let PyRep::List(mb_list) = my_bounties else {
        panic!("Expected list from GetMyBounties");
    };
    assert!(mb_list
        .iter()
        .any(|b| kv_map(b).get("targetID") == Some(&PyRep::Integer(90_000_002))));

    // 4. GetTopAllianceBounties
    let top_all = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopAllianceBounties",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopAllianceBounties");
    assert!(matches!(top_all, PyRep::List(_)));

    // 5. Create a kill right from pilot 1 against pilot 2, sell it, verify, then CancelSellKillRight
    let kr_id = 77_001;
    eve_server::services::combat_crimewatch::create_kill_right(
        &ctx,
        kr_id,
        90_000_001,
        90_000_002,
        1_000_000.0,
    );

    // GetMyKillRights: returns expiryTime and restrictedTo
    let my_kr = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyKillRights", &[], &empty_kw, None)
        .await
        .expect("GetMyKillRights");
    let PyRep::List(kr_list) = my_kr else {
        panic!("Expected list from GetMyKillRights");
    };
    let found_kr = kr_list
        .iter()
        .map(kv_map)
        .find(|k| k.get("killRightID") == Some(&PyRep::Integer(kr_id)))
        .expect("Found kill right in GetMyKillRights");
    assert!(found_kr.contains_key("expiryTime"));
    assert!(found_kr.contains_key("restrictedTo"));

    // GetKillRightsOnCharacters
    let kr_on_char = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetKillRightsOnCharacters",
            &[PyRep::List(vec![PyRep::Integer(90_000_002)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetKillRightsOnCharacters");
    let PyRep::List(krc_list) = kr_on_char else {
        panic!("Expected list from GetKillRightsOnCharacters");
    };
    assert!(krc_list
        .iter()
        .any(|k| kv_map(k).get("killRightID") == Some(&PyRep::Integer(kr_id))));

    // CancelSellKillRight
    let cancel_kr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "CancelSellKillRight",
            &[PyRep::Integer(kr_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("CancelSellKillRight");
    let cancel_kv = kv_map(&cancel_kr);
    assert_eq!(cancel_kv.get("forSale"), Some(&PyRep::Bool(false)));

    let db_kr_fee = ctx
        .db
        .with_snapshot(|snap| snap.kill_rights.get(&kr_id).map(|k| k.activation_fee.0));
    assert_eq!(db_kr_fee, Some(0));

    // 6. GetBountiesAndKillRights: returns ([(ownerID, bounty)], [killRight rows])
    let bakr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetBountiesAndKillRights",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("GetBountiesAndKillRights");
    let PyRep::Tuple(bakr_tuple) = bakr else {
        panic!("Expected tuple from GetBountiesAndKillRights");
    };
    assert_eq!(bakr_tuple.len(), 2);
    let PyRep::List(b_entries) = &bakr_tuple[0] else { panic!("expected bounties list") };
    assert!(!b_entries.is_empty());
    let PyRep::List(kr_entries) = &bakr_tuple[1] else { panic!("expected kill rights list") };
    assert!(!kr_entries.is_empty());

    // 7. GMReimburseBounties
    ctx.db.provision_gm_account("gm_bounty", "gm-pass", 90_000_003, "GM Bounty");
    let mut gm = SessionState::new(303);
    common::login_pilot(&ctx, &mut gm, "gm_bounty", "gm-pass").await;
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

    let p1_bal_before_refund = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    let reimb = ctx
        .dispatch_rpc(
            &mut gm,
            "bountyProxy",
            None,
            "GMReimburseBounties",
            &[PyRep::Integer(90_000_002)],
            &empty_kw,
            None,
        )
        .await
        .expect("GMReimburseBounties");
    let reimb_kv = kv_map(&reimb);
    assert_eq!(reimb_kv.get("amountISK"), Some(&PyRep::Real(500_000.0)));

    let p1_bal_after_refund = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    assert_eq!(p1_bal_after_refund - p1_bal_before_refund, 50_000_000);

    // Verify wallet journal has bounty_reimbursement entry
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.wallet_journal
                .iter()
                .any(|e| e.character_id == 90_000_001 && e.entry_type == "bounty_reimbursement"),
            "Must have bounty_reimbursement in wallet journal"
        );
    });

    // 8. GMClearBountyCache: GM receives None, non-GM is rejected
    let clear = ctx
        .dispatch_rpc(
            &mut gm,
            "bountyProxy",
            None,
            "GMClearBountyCache",
            &[PyRep::Integer(0)],
            &empty_kw,
            None,
        )
        .await
        .expect("GMClearBountyCache");
    assert_eq!(clear, PyRep::None);

    let non_gm_clear = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GMClearBountyCache",
            &[PyRep::Integer(0)],
            &empty_kw,
            None,
        )
        .await;
    assert_eq!(non_gm_clear.unwrap_err().code, "CustomError");
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod market_price_history_ext {
    use super::*;

// Integration tests for Market Price History remediation:
// Verifies that `marketProxy.GetOldPriceHistory`, `GetNewPriceHistory`, and
// `GetHistoryForManyTypeIDs` query authentic trade aggregates from executed
// market orders rather than fabricating static synthetic numbers, and that
// un-traded types return an empty rowset.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_REGION_ID: u32 = 10_000_002;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM: TypeId = 34;
const UNTRADED_TYPE: TypeId = 99_999;

const SELLER_CHAR_ID: i64 = 90_000_001;
const BUYER_CHAR_ID: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess.station_id = Some(JITA_STATION_ID);
    sess.solar_system_id = Some(JITA_SYSTEM_ID);
    sess.region_id = Some(JITA_REGION_ID);
    sess
}

fn seed_hangar_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, quantity: i32) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag: InventoryFlag::Hangar,
            quantity,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

/// Comprehensive test for market price history remediation:
/// Verifies authentic OHLC, volume, and order-count calculation on executed trades,
/// and empty rowsets for un-traded types.
#[tokio::test]
async fn test_market_price_history_authentic_aggregates_and_empty_untraded() {
    // parity: feat:market.price-history, rpc:marketProxy.GetHistoryForManyTypeIDs, rpc:marketProxy.GetNewPriceHistory, rpc:marketProxy.GetOldPriceHistory
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1201, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1202, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with 50,000 Tritanium and buyer with 1,000,000 ISK; clear seeded NPC sell order
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.remove(&700_000_001);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 50_000)
    });

    let empty_kw = HashMap::new();

    // 1. Initial State: Un-traded types must return an empty CRowset (0 rows), NOT fabricated rows
    let old_hist_before = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory before trades");
    assert!(old_hist_before.is_crowset(), "GetOldPriceHistory must return valid CRowset");
    let rows_before = old_hist_before.crowset_rows().expect("crowset rows");
    assert_eq!(rows_before.len(), 0, "Un-traded item must return empty price history rowset");

    let new_hist_before = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory before trades");
    assert!(new_hist_before.is_crowset(), "GetNewPriceHistory must return valid CRowset");
    let rows_new_before = new_hist_before.crowset_rows().expect("crowset rows");
    assert_eq!(rows_new_before.len(), 0, "Un-traded item must return empty price history rowset");

    let many_hist_before = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetHistoryForManyTypeIDs before trades");
    let PyRep::Dict(entries_before) = many_hist_before else {
        panic!("Expected Dict from GetHistoryForManyTypeIDs");
    };
    assert_eq!(entries_before.len(), 2);
    for (_, crowset) in &entries_before {
        assert!(crowset.is_crowset());
        assert_eq!(crowset.crowset_rows().map(|r| r.len()), Some(0));
    }

    // 2. Execute Trade 1: Seller places sell for 400 units @ 4.00 ISK; Buyer buys 400 units @ 4.00 ISK
    let sell_order_arg = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
        (PyRep::String("price".into()), PyRep::Real(4.00)),
        (PyRep::String("quantity".into()), PyRep::Integer(400)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None)
        .await
        .expect("PlaceMultiSellOrder 1");

    ctx.dispatch_rpc(
        &mut buyer_sess,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM as i64),
            PyRep::Real(4.00),
            PyRep::Integer(400),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceBuyOrder 1");

    // 3. Execute Trade 2 on the same day: Seller places sell for 500 units @ 6.00 ISK; Buyer buys 200 units @ 6.00 ISK
    let seller_item_id_2 = ctx.db.with_mut_snapshot(|snap| {
        seed_hangar_item(snap, TRITANIUM, SELLER_CHAR_ID, JITA_STATION_ID, 500)
    });
    let sell_order_arg_2 = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(seller_item_id_2)),
        (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
        (PyRep::String("price".into()), PyRep::Real(6.00)),
        (PyRep::String("quantity".into()), PyRep::Integer(500)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(&mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder", &[sell_order_arg_2, PyRep::Bool(false)], &empty_kw, None)
        .await
        .expect("PlaceMultiSellOrder 2");

    ctx.dispatch_rpc(
        &mut buyer_sess,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM as i64),
            PyRep::Real(6.00),
            PyRep::Integer(200),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceBuyOrder 2");

    // 4. Verify persisted snapshot daily summary
    ctx.db.with_snapshot(|snap| {
        let hist = snap.market_history.get(&(JITA_REGION_ID, TRITANIUM)).expect("market_history entry for Tritanium");
        assert_eq!(hist.len(), 1, "Expected exactly 1 daily record for today");
        let day = &hist[0];
        assert_eq!(day.volume, 600, "Volume must equal sum of fills (400 + 200)");
        assert_eq!(day.order_count, 2, "Order count must equal number of executed trades");
        assert!((day.lowest - 4.00).abs() < 1e-4, "Lowest price must be 4.00");
        assert!((day.highest - 6.00).abs() < 1e-4, "Highest price must be 6.00");
        let expected_avg = (400.0 * 4.00 + 200.0 * 6.00) / 600.0; // 2800 / 600 = 4.66666...
        assert!((day.average - expected_avg).abs() < 1e-4, "Average price must be weighted average (4.6667)");
        assert!((day.open_isk() - 4.00).abs() < 1e-4, "Open price must be first trade price (4.00)");
        assert!((day.close_isk() - 6.00).abs() < 1e-4, "Close price must be latest trade price (6.00)");
    });

    // 5. Verify marketProxy.GetOldPriceHistory returns authentic daily OHLC aggregate
    let old_hist_after = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after trades");
    assert!(old_hist_after.is_crowset());
    let rows_after = old_hist_after.crowset_rows().expect("crowset rows");
    assert_eq!(rows_after.len(), 1, "Expected exactly 1 daily aggregate row");
    let row = &rows_after[0];
    let low_price = row.dbrow_get("lowPrice").and_then(|v| v.as_f64()).expect("lowPrice");
    let high_price = row.dbrow_get("highPrice").and_then(|v| v.as_f64()).expect("highPrice");
    let avg_price = row.dbrow_get("avgPrice").and_then(|v| v.as_f64()).expect("avgPrice");
    let vol = row.dbrow_get("volume").and_then(|v| v.as_i64()).expect("volume");
    let orders = row.dbrow_get("orders").and_then(|v| v.as_i64()).expect("orders");

    assert!((low_price - 4.00).abs() < 1e-4, "lowPrice must match executed low");
    assert!((high_price - 6.00).abs() < 1e-4, "highPrice must match executed high");
    let expected_avg = 2800.0 / 600.0;
    assert!((avg_price - expected_avg).abs() < 1e-4, "avgPrice must match volume-weighted average");
    assert_eq!(vol, 600, "volume must match executed units");
    assert_eq!(orders, 2, "orders must match executed order count");

    // 6. Verify marketProxy.GetNewPriceHistory returns identical authentic aggregate
    let new_hist_after = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(TRITANIUM as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory after trades");
    assert!(new_hist_after.is_crowset());
    let new_rows = new_hist_after.crowset_rows().expect("crowset rows");
    assert_eq!(new_rows.len(), 1);
    let new_row = &new_rows[0];
    assert!((new_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
    assert!((new_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
    assert!((new_row.dbrow_get("avgPrice").unwrap().as_f64().unwrap() - expected_avg).abs() < 1e-4);
    assert_eq!(new_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
    assert_eq!(new_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    // 7. Verify marketProxy.GetHistoryForManyTypeIDs
    let many_hist_after = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![PyRep::Integer(TRITANIUM as i64), PyRep::Integer(UNTRADED_TYPE as i64)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetHistoryForManyTypeIDs after trades");
    let PyRep::Dict(entries_after) = many_hist_after else {
        panic!("Expected Dict from GetHistoryForManyTypeIDs");
    };
    assert_eq!(entries_after.len(), 2);

    let trit_crowset = entries_after
        .iter()
        .find(|(k, _)| k.as_i64() == Some(TRITANIUM as i64))
        .map(|(_, v)| v)
        .expect("Tritanium entry in GetHistoryForManyTypeIDs");
    let trit_rows = trit_crowset.crowset_rows().expect("trit rows");
    assert_eq!(trit_rows.len(), 1);
    let trit_row = &trit_rows[0];
    assert!((trit_row.dbrow_get("lowPrice").unwrap().as_f64().unwrap() - 4.00).abs() < 1e-4);
    assert!((trit_row.dbrow_get("highPrice").unwrap().as_f64().unwrap() - 6.00).abs() < 1e-4);
    assert_eq!(trit_row.dbrow_get("volume").unwrap().as_i64().unwrap(), 600);
    assert_eq!(trit_row.dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    let untraded_crowset = entries_after
        .iter()
        .find(|(k, _)| k.as_i64() == Some(UNTRADED_TYPE as i64))
        .map(|(_, v)| v)
        .expect("Untraded entry in GetHistoryForManyTypeIDs");
    let untraded_rows = untraded_crowset.crowset_rows().expect("untraded rows");
    assert_eq!(untraded_rows.len(), 0, "Untraded type must return empty rowset");
}

const PYERITE: TypeId = 35;

#[tokio::test]
async fn test_market_price_history_buymultiple_and_modifyorder() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1301, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1302, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with Pyerite and buyer with ISK; clear any existing Pyerite market orders
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != PYERITE);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, PYERITE, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
    });

    let empty_kw = HashMap::new();

    // 1. Seller places a resting sell order: 1,000 Pyerite @ 10.00 ISK
    let sell_order_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(seller_item_id)),
        ("typeID", PyRep::Integer(PYERITE as i64)),
        ("price", PyRep::Real(10.00)),
        ("quantity", PyRep::Integer(1_000)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("duration", PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(
        &mut seller_sess,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_order_arg, PyRep::Bool(false)],
        &empty_kw,
        None,
    )
    .await
    .expect("PlaceMultiSellOrder for Pyerite");

    // 2. Buyer executes BuyMultipleItems for 300 units @ max 12.00 ISK
    let buy_items = PyRep::List(vec![PyRep::keyval([
        ("typeID", PyRep::Integer(PYERITE as i64)),
        ("quantity", PyRep::Integer(300)),
        ("maxPrice", PyRep::Real(12.00)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("orderRange", PyRep::Integer(-1)),
    ])]);
    let buy_mult_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[buy_items, PyRep::Bool(false)],
            &empty_kw,
            None,
        )
        .await
        .expect("BuyMultipleItems for Pyerite");
    let PyRep::KeyVal(bm_kv) = buy_mult_res else { panic!("Expected KeyVal"); };
    assert_eq!(bm_kv.get("purchasedCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(bm_kv.get("totalQuantity").and_then(|v| v.as_i64()), Some(300));

    // 3. Verify price history and market transactions after BuyMultipleItems
    let old_hist_bm = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after BuyMultipleItems");
    let rows_bm = old_hist_bm.crowset_rows().expect("crowset rows");
    assert_eq!(rows_bm.len(), 1, "Expected 1 row after BuyMultipleItems");
    assert_eq!(rows_bm[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 300);
    assert_eq!(rows_bm[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 1);
    assert!((rows_bm[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

    ctx.db.with_snapshot(|snap| {
        let has_bm_tx = snap.market_transactions.values().any(|t| t.type_id == PYERITE && t.quantity == 300 && t.price.0 == 1_000);
        assert!(has_bm_tx, "Market transactions must record BuyMultipleItems fill");
    });

    // 4. Buyer places a non-matching buy order for 500 Pyerite @ 8.00 ISK (below 10.00 ISK ask)
    let buy_order_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(PYERITE as i64),
                PyRep::Real(8.00),
                PyRep::Integer(500),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder for Pyerite @ 8.00 ISK");
    let PyRep::KeyVal(bo_kv) = buy_order_res else { panic!("Expected KeyVal"); };
    let buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    // Backdate order issue_unix by 301s to bypass modification cooldown
    let now_unix = ctx.now_unix();
    let meta_nid = -(4_000_000_000_000_i64.saturating_add(buy_oid.abs()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
        }
    });

    // 5. Buyer modifies the order to 10.00 ISK, crossing the resting ask of 10.00 ISK
    let mod_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "ModifyCharOrder",
            &[PyRep::Integer(buy_oid), PyRep::Real(10.00)],
            &empty_kw,
            None,
        )
        .await
        .expect("ModifyCharOrder to 10.00 ISK");
    let PyRep::KeyVal(mo_kv) = mod_res else { panic!("Expected KeyVal"); };
    assert_eq!(mo_kv.get("orderID").and_then(|v| v.as_i64()), Some(buy_oid));

    // 6. Verify combined price history in GetOldPriceHistory and GetNewPriceHistory:
    // Volume was 300, now + 500 = 800; Orders was 1, now + 1 = 2; Average = 10.00 ISK
    let old_hist_mod = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after ModifyCharOrder");
    let rows_mod = old_hist_mod.crowset_rows().expect("crowset rows");
    assert_eq!(rows_mod.len(), 1, "Expected 1 aggregate row");
    assert_eq!(rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800, "Volume must reflect BuyMultipleItems + ModifyCharOrder fills");
    assert_eq!(rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2, "Orders count must be 2");
    assert!((rows_mod[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 10.00).abs() < 1e-4);

    let new_hist_mod = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(PYERITE as i64)], &empty_kw, None)
        .await
        .expect("GetNewPriceHistory after ModifyCharOrder");
    let new_rows_mod = new_hist_mod.crowset_rows().expect("crowset rows");
    assert_eq!(new_rows_mod.len(), 1);
    assert_eq!(new_rows_mod[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 800);
    assert_eq!(new_rows_mod[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 2);

    ctx.db.with_snapshot(|snap| {
        let has_mod_tx = snap.market_transactions.values().any(|t| t.type_id == PYERITE && t.quantity == 500 && t.price.0 == 1_000);
        assert!(has_mod_tx, "Market transactions must record ModifyCharOrder crossed fill");
    });
}

const MEXALLON: TypeId = 36;

#[tokio::test]
async fn test_market_modify_sell_order_crossing() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1401, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1402, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with Mexallon and buyer with ISK; clear existing Mexallon market orders
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != MEXALLON);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, MEXALLON, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
    });

    let empty_kw = HashMap::new();

    // 1. Buyer places resting buy order for 400 Mexallon @ 50.00 ISK
    let buy_res = ctx
        .dispatch_rpc(
            &mut buyer_sess, "marketProxy", None, "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID), PyRep::Integer(MEXALLON as i64),
                PyRep::Real(50.00), PyRep::Integer(400), PyRep::Integer(-1),
                PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
            ],
            &empty_kw, None,
        )
        .await
        .expect("PlaceBuyOrder 400 Mexallon @ 50.00");
    let PyRep::KeyVal(bo_kv) = buy_res else { panic!("Expected KeyVal"); };
    let _buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // 2. Seller places sell order for 400 Mexallon @ 60.00 ISK (does not match 50.00 ISK bid)
    let sell_order_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(seller_item_id)),
        ("typeID", PyRep::Integer(MEXALLON as i64)),
        ("price", PyRep::Real(60.00)),
        ("quantity", PyRep::Integer(400)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("duration", PyRep::Integer(90)),
    ])]);
    let sell_res = ctx
        .dispatch_rpc(
            &mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder",
            &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None,
        )
        .await
        .expect("PlaceMultiSellOrder @ 60.00");
    let PyRep::KeyVal(so_kv) = sell_res else { panic!("Expected KeyVal"); };
    let PyRep::List(oids) = so_kv.get("orderIDs").unwrap() else { panic!("Expected orderIDs list"); };
    let sell_oid = oids[0].as_i64().unwrap();

    // 3. Backdate sell order by 301s to bypass modification cooldown
    let now_unix = ctx.now_unix();
    let meta_nid = -(4_000_000_000_000_i64.saturating_add(sell_oid.abs()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
        }
    });

    // 4. Seller modifies sell order price downward to 45.00 ISK (<= 50.00 ISK resting bid)
    // Symmetric sell order crossing: matches resting buy order @ 50.00 ISK
    let mod_sell_res = ctx
        .dispatch_rpc(
            &mut seller_sess, "marketProxy", None, "ModifyCharOrder",
            &[PyRep::Integer(sell_oid), PyRep::Real(45.00)], &empty_kw, None,
        )
        .await
        .expect("ModifyCharOrder to 45.00 ISK");
    let PyRep::KeyVal(mso_kv) = mod_sell_res else { panic!("Expected KeyVal"); };
    assert_eq!(mso_kv.get("orderID").and_then(|v| v.as_i64()), Some(sell_oid));
    assert_eq!(mso_kv.get("escrowISK").and_then(|v| v.as_f64()), Some(0.0));

    // Verify price history recorded fill @ 50.00 ISK for 400 units
    let hist = ctx
        .dispatch_rpc(&mut buyer_sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(MEXALLON as i64)], &empty_kw, None)
        .await
        .expect("GetOldPriceHistory after sell order crossing");
    let rows = hist.crowset_rows().expect("crowset rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dbrow_get("volume").unwrap().as_i64().unwrap(), 400);
    assert_eq!(rows[0].dbrow_get("orders").unwrap().as_i64().unwrap(), 1);
    assert!((rows[0].dbrow_get("avgPrice").unwrap().as_f64().unwrap() - 50.00).abs() < 1e-4);

    ctx.db.with_snapshot(|snap| {
        let has_buyer_items = snap.items.values().any(|i| i.owner_id == BUYER_CHAR_ID && i.type_id == MEXALLON && i.quantity == 400);
        assert!(has_buyer_items, "Buyer must receive 400 Mexallon items in hangar");
    });
}

const ISOGEN: TypeId = 37;

#[tokio::test]
async fn test_market_modify_order_margin_shortfall() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1501, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1502, "other", "other-secret", BUYER_CHAR_ID).await;

    // Seed seller with Isogen, buyer with Margin Trading skill (16597 = lvl 5, escrow_fraction = 0.75^5)
    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != ISOGEN);
        snap.skills.entry(BUYER_CHAR_ID).or_default().insert(16597, eve_db::TrainedSkillRecord {
            type_id: 16597, level: 5, skill_points: 1_280_000.0,
        });
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(1_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, ISOGEN, SELLER_CHAR_ID, JITA_STATION_ID, 10_000)
    });

    let empty_kw = HashMap::new();

    // 1. Seller places ask for 100 Isogen @ 100.00 ISK
    let sell_order_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(seller_item_id)),
        ("typeID", PyRep::Integer(ISOGEN as i64)),
        ("price", PyRep::Real(100.00)),
        ("quantity", PyRep::Integer(100)),
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("duration", PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(
        &mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder",
        &[sell_order_arg, PyRep::Bool(false)], &empty_kw, None,
    ).await.expect("PlaceMultiSellOrder Isogen @ 100.00");

    // 2. Buyer places buy order for 100 Isogen @ 10.00 ISK
    let buy_res = ctx
        .dispatch_rpc(
            &mut buyer_sess, "marketProxy", None, "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID), PyRep::Integer(ISOGEN as i64),
                PyRep::Real(10.00), PyRep::Integer(100), PyRep::Integer(-1),
                PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
            ],
            &empty_kw, None,
        )
        .await
        .expect("PlaceBuyOrder Isogen @ 10.00");
    let PyRep::KeyVal(bo_kv) = buy_res else { panic!("Expected KeyVal"); };
    let buy_oid = bo_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    // Drain buyer balance so buyer cannot cover shortfall
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents(0);
        }
        let now_unix = ctx.now_unix();
        let meta_nid = -(4_000_000_000_000_i64.saturating_add(buy_oid.abs()));
        if let Some(note) = snap.owner_notes.get_mut(&meta_nid) {
            note.note = format!("{{\"range\":-1,\"duration\":90,\"issue_unix\":{}}}", now_unix.saturating_sub(400));
        }
    });

    // 3. Buyer attempts to modify order to 100.00 ISK (crossing the 100.00 ISK ask).
    // Buyer has 0 ISK in wallet, so modification must fail with InsufficientFunds!
    let err = ctx
        .dispatch_rpc(
            &mut buyer_sess, "marketProxy", None, "ModifyCharOrder",
            &[PyRep::Integer(buy_oid), PyRep::Real(100.00)], &empty_kw, None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");
}

#[tokio::test]
async fn test_issue_631_buy_order_escrow_deducts_wallet_and_emits_on_account_change() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut buyer_sess = setup_pilot(&ctx, 1631, "other", "other-secret", BUYER_CHAR_ID).await;
    let start_isk = 500_000_000_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != 483);
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(start_isk);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
    });
    let _ = ctx.drain_notifications_for_char(BUYER_CHAR_ID);

    let empty_kw = HashMap::new();
    let initial_bal = ctx
        .dispatch_rpc(&mut buyer_sess, "account", None, "GetCashBalance", &[], &empty_kw, None)
        .await
        .expect("initial GetCashBalance")
        .as_f64()
        .unwrap();
    assert_eq!(initial_bal, start_isk as f64);

    // Client MarketQuote.BuyStuff passes 9 args (last is expectedBrokersFee=100.0)
    let buy_res = ctx
        .dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(483), // Miner I
                PyRep::Real(1.00),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
                PyRep::Real(100.0),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder Miner I @ 1.00");
    let broker_fee = buy_res.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    let escrow_isk = buy_res.dict_get("escrowISK").and_then(|v| v.as_f64()).unwrap();
    let expected_bal = (start_isk as f64) - broker_fee - escrow_isk;
    assert_eq!(broker_fee, 100.0);
    assert_eq!(escrow_isk, 1.0);

    let after_bal = ctx
        .dispatch_rpc(&mut buyer_sess, "account", None, "GetCashBalance", &[], &empty_kw, None)
        .await
        .expect("after GetCashBalance")
        .as_f64()
        .unwrap();
    assert_eq!(after_bal, expected_bal, "GetCashBalance must reflect broker fee + escrow deduction");

    let notifs = ctx.drain_notifications_for_char(BUYER_CHAR_ID);
    let acct_notif = notifs
        .iter()
        .find(|n| n.event == "OnAccountChange" && n.args.first() == Some(&PyRep::String("cash".into())))
        .expect("PlaceBuyOrder must emit OnAccountChange for buyer wallet");
    assert_eq!(acct_notif.args.get(1), Some(&PyRep::Integer(BUYER_CHAR_ID)));
    assert_eq!(acct_notif.args.get(2), Some(&PyRep::Real(expected_bal)));
}

#[tokio::test]
async fn test_remote_immediate_buy_and_notifications_e2e() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut seller_sess = setup_pilot(&ctx, 1511, "demo", "demo-secret", SELLER_CHAR_ID).await;
    let mut buyer_sess = setup_pilot(&ctx, 1512, "other", "other-secret", BUYER_CHAR_ID).await;
    let remote_station_id = 60_003_757_i64;
    let empty_kw = HashMap::new();

    let seller_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.type_id != ISOGEN);
        if let Some(ch) = snap.characters.get_mut(&SELLER_CHAR_ID) {
            ch.balance = IskCents::from_isk(500_000);
            ch.station_id = Some(remote_station_id);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&BUYER_CHAR_ID) {
            ch.balance = IskCents::from_isk(500_000);
            ch.station_id = Some(JITA_STATION_ID);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        seed_hangar_item(snap, ISOGEN, SELLER_CHAR_ID, remote_station_id, 50)
    });

    // Seller places remote ask at 60003757 (duration=90) while docked there
    let sell_arg = PyRep::List(vec![PyRep::keyval([
        ("itemID", PyRep::Integer(seller_item_id)),
        ("typeID", PyRep::Integer(ISOGEN as i64)),
        ("price", PyRep::Real(120.0)),
        ("quantity", PyRep::Integer(20)),
        ("stationID", PyRep::Integer(remote_station_id)),
        ("duration", PyRep::Integer(90)),
    ])]);
    ctx.dispatch_rpc(
        &mut seller_sess, "marketProxy", None, "PlaceMultiSellOrder",
        &[sell_arg, PyRep::Bool(false)], &empty_kw, None,
    )
    .await
    .expect("PlaceMultiSellOrder remote");

    let _ = ctx.drain_notifications_for_char(BUYER_CHAR_ID);
    let _ = ctx.drain_notifications_for_char(SELLER_CHAR_ID);

    // Buyer docked in Jita clicks Buy Now (duration=0) at remote station 60003757 (#659 + #660)
    let res = ctx
        .dispatch_rpc(
            &mut buyer_sess, "marketProxy", None, "PlaceBuyOrder",
            &[
                PyRep::Integer(remote_station_id), PyRep::Integer(ISOGEN as i64),
                PyRep::Real(120.0), PyRep::Integer(5), PyRep::Integer(-1),
                PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false), PyRep::None,
            ],
            &empty_kw, None,
        )
        .await
        .expect("remote immediate PlaceBuyOrder");
    let kv = kv_map(&res);
    assert_eq!(kv.get("filledImmediately").and_then(PyRep::as_bool), Some(true));

    let buyer_notifs = ctx.drain_notifications_for_char(BUYER_CHAR_ID);
    assert!(buyer_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(buyer_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(ctx
        .drain_notifications_for_char(SELLER_CHAR_ID)
        .iter()
        .any(|n| n.event == "OnAccountChange"));
}

}

#[tokio::test]
async fn issue_935_and_969_v24_contract_create_list_and_search_wire_flow() {
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let station_id = 60_003_760_i64;
    let system_id = 30_000_142_u32;
    let mut s1 = setup_pilot(&ctx, 901, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 902, "other", "other-secret", 90_000_002).await;
    s1.station_id = Some(station_id);
    s1.solar_system_id = Some(system_id);
    s2.station_id = Some(station_id);
    s2.solar_system_id = Some(system_id);

    let (ex_item_id, auc_item_id) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents::from_isk(10_000_000);
            ch.station_id = Some(station_id);
            ch.solar_system_id = system_id;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = IskCents::from_isk(10_000_000);
            ch.station_id = Some(station_id);
            ch.solar_system_id = system_id;
        }
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: station_id,
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
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2)
    });

    let empty_kw = HashMap::new();
    let make_contract_struct = |ctype: i64, price: f64, collateral: f64, item_id: i64, title: &str| {
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::String("ContractStruct".into()),
                PyRep::Tuple(vec![]),
                PyRep::Dict(vec![
                    (PyRep::String("type".into()), PyRep::Integer(ctype)),
                    (PyRep::String("isPrivate".into()), PyRep::Integer(0)),
                    (PyRep::String("minutesExpire".into()), PyRep::Integer(1440)),
                    (PyRep::String("startStationID".into()), PyRep::Integer(station_id)),
                    (PyRep::String("price".into()), PyRep::Real(price)),
                    (PyRep::String("collateral".into()), PyRep::Real(collateral)),
                    (PyRep::String("title".into()), PyRep::String(title.into())),
                    (
                        PyRep::String("itemList".into()),
                        PyRep::List(vec![PyRep::keyval([("itemID", PyRep::Integer(item_id))])]),
                    ),
                ]),
            ])),
            list: vec![],
            dict: vec![],
        }
    };

    // #935: CreateContract over machoNet:26000 must return [cid] sequence for V24 ShowCreatedContracts.
    let ex_reply = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[make_contract_struct(1, 1_000.0, 0.0, ex_item_id, "V24 Exchange")],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CreateContract exchange");
    let PyRep::List(ex_ids) = ex_reply else {
        panic!("Expected list of contractIDs from CreateContract over machoNet:26000");
    };
    assert_eq!(ex_ids.len(), 1);
    let PyRep::Integer(ex_cid) = ex_ids[0] else {
        panic!("Expected integer contractID");
    };

    let auc_reply = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[make_contract_struct(2, 5_000.0, 25_000.0, auc_item_id, "V24 Auction")],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CreateContract auction");
    let PyRep::List(auc_ids) = auc_reply else {
        panic!("Expected list of contractIDs for auction");
    };
    let PyRep::Integer(auc_cid) = auc_ids[0] else {
        panic!("Expected integer auction contractID");
    };

    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(6_000.0)],
        &empty_kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("PlaceBid");

    // #969: GetContractListForOwner returns { contracts, items } with filetime dateIssued & items.itemTypeID.
    let owner_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetContractListForOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(0), PyRep::None, PyRep::None],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetContractListForOwner V24");
    let Some(PyRep::List(owner_contracts)) = owner_rep.dict_get("contracts") else {
        panic!("Expected contracts list on GetContractListForOwner");
    };
    assert!(owner_contracts.len() >= 2);
    for c in owner_contracts {
        assert!(matches!(c.dict_get("dateIssued"), Some(PyRep::Integer(v)) if *v > 100_000_000_000_000));
        assert!(matches!(c.dict_get("dateExpired"), Some(PyRep::Integer(v)) if *v > 100_000_000_000_000));
        assert_eq!(c.dict_get("status"), Some(&PyRep::Integer(0)));
        assert_eq!(c.dict_get("acceptorID"), Some(&PyRep::Integer(0)));
    }
    let Some(PyRep::Dict(owner_items)) = owner_rep.dict_get("items") else {
        panic!("Expected items dict on GetContractListForOwner");
    };
    let Some((_, PyRep::List(ex_items))) = owner_items.iter().find(|(k, _)| *k == PyRep::Integer(ex_cid)) else {
        panic!("Missing items entry for exchange contract");
    };
    assert_eq!(ex_items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(34)));

    // #969: GetMyCurrentContractList & GetMyBids return normalized { contracts, items }.
    let current_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetMyCurrentContractList",
            &[PyRep::Bool(false), PyRep::Bool(false)],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetMyCurrentContractList V24");
    assert!(matches!(current_rep.dict_get("contracts"), Some(PyRep::List(l)) if l.len() >= 2));

    let bids_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "GetMyBids",
            &[PyRep::Bool(false)],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetMyBids V24");
    let Some(PyRep::List(bid_contracts)) = bids_rep.dict_get("contracts") else {
        panic!("Expected contracts list on GetMyBids");
    };
    let auc_entry = bid_contracts
        .iter()
        .find(|c| c.dict_get("contractID") == Some(&PyRep::Integer(auc_cid)))
        .expect("auc_cid in GetMyBids");
    let Some(PyRep::List(auc_bids)) = auc_entry.dict_get("bids") else {
        panic!("Expected bids list on auc_entry");
    };
    assert_eq!(auc_bids[0].dict_get("amount"), Some(&PyRep::Real(6_000.0)));

    // #969: SearchContracts returns normalized { contracts: [{ contract, items, bids }], numFound, ... }.
    let search_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "SearchContracts",
            &[],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("SearchContracts V24");
    let Some(PyRep::List(search_entries)) = search_rep.dict_get("contracts") else {
        panic!("Expected contracts list on SearchContracts");
    };
    let ex_search = search_entries
        .iter()
        .find(|e| {
            e.dict_get("contract")
                .and_then(|c| c.dict_get("contractID"))
                == Some(&PyRep::Integer(ex_cid))
        })
        .expect("ex_cid in SearchContracts");
    let ex_c = ex_search.dict_get("contract").expect("contract inner");
    assert!(matches!(ex_c.dict_get("dateIssued"), Some(PyRep::Integer(v)) if *v > 100_000_000_000_000));
    assert_eq!(ex_c.dict_get("type"), Some(&PyRep::Integer(1)));
    let Some(PyRep::List(ex_search_items)) = ex_search.dict_get("items") else {
        panic!("Expected items list on SearchContracts entry");
    };
    assert_eq!(ex_search_items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(34)));

    // Accept ex_cid so it moves to Finished, then verify GetContractListForOwner(filtStatus=4) & GetMyExpiredContractList.
    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "AcceptContract",
        &[PyRep::Integer(ex_cid), PyRep::Bool(false)],
        &empty_kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("AcceptContract");

    let finished_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetContractListForOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(4), PyRep::Integer(1), PyRep::Bool(false)],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetContractListForOwner finished");
    let Some(PyRep::List(finished_list)) = finished_rep.dict_get("contracts") else {
        panic!("Expected contracts list for finished filter");
    };
    assert!(finished_list.iter().any(|c| c.dict_get("contractID") == Some(&PyRep::Integer(ex_cid))));

    let expired_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "GetMyExpiredContractList",
            &[PyRep::Bool(false)],
            &empty_kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetMyExpiredContractList V24");
    let Some(PyRep::List(expired_list)) = expired_rep.dict_get("contracts") else {
        panic!("Expected contracts list on GetMyExpiredContractList");
    };
    assert!(expired_list.iter().any(|c| c.dict_get("contractID") == Some(&PyRep::Integer(ex_cid))));
}
