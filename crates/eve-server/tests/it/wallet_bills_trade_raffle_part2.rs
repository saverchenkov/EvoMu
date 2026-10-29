//! Bills, Aurum removal, direct station trade window, HyperNet Relay, POS removal, and bill/contract
//! division isolation integration tests (part 2 of `wallet_serverops_complete`).

use crate::common;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::wallet_serverops::{
    compute_required_hypercores, create_bill, process_due_automatic_bills,
    run_daily_downtime_and_jobs, CorpBillRecord, HYPERCORE_TYPE_ID,
};
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM: TypeId = 34;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn balance(ctx: &ServerContext, char_id: i64) -> IskCents {
    ctx.db.get_character(char_id).unwrap().balance
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    let PyRep::KeyVal(kv) = rep else { panic!("expected KeyVal, got {rep:?}") };
    kv.get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key}"))
}

#[tokio::test]
async fn test_corporation_and_personal_bills_and_autopay() {
    // parity: feat:wallet.bills
    eve_testkit::attest(&["feat:wallet.bills"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 881, "demo", "demo-secret", P1).await;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    let landlord_corp = 1_000_035_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.master_wallet = IskCents::from_isk(10_000_000);
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(1_000_000);
        }
    });

    let p_bill = create_bill(&ctx, 1, P1, landlord_corp, 2_500.0, now + 3600, 0.0, 0, 0);
    let c_bill = create_bill(
        &ctx,
        2,
        corp_id,
        landlord_corp,
        10_000.0,
        now + 3600,
        0.0,
        JITA_STATION_ID,
        0,
    );

    let bills_rep = rpc(&ctx, &mut s1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
    let PyRep::Rowset { rows, .. } = bills_rep else { panic!("expected Rowset"); };
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill))));
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill))));

    let p1_before = balance(&ctx, P1);
    let pay_p = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[PyRep::Integer(p_bill)]).await.expect("CharPayBill");
    assert_eq!(pay_p, PyRep::Bool(true));
    assert_eq!(p1_before.0 - balance(&ctx, P1).0, 250_000);

    let div3_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    let pay_c = rpc(&ctx, &mut s1, "billMgr", "PayCorporationBill", &[PyRep::Integer(c_bill), PyRep::Integer(1002)]).await.expect("PayCorporationBill");
    assert_eq!(pay_c, PyRep::Bool(true));
    let div3_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    assert_eq!(div3_before.0 - div3_after.0, 1_000_000);

    let auto_dict = PyRep::Dict(vec![
        (PyRep::Integer(2), PyRep::Bool(true)),
        (PyRep::String("divisionID".into()), PyRep::Integer(1004)),
    ]);
    let set_ap = rpc(&ctx, &mut s1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(corp_id), auto_dict)])]).await.expect("SendAutomaticPaySettings");
    assert_eq!(set_ap, PyRep::Bool(true));

    let get_ap = rpc(&ctx, &mut s1, "billMgr", "GetAutomaticPaySettings", &[]).await.expect("GetAutomaticPaySettings");
    let PyRep::Dict(ap_entries) = get_ap else { panic!("expected Dict"); };
    assert!(!ap_entries.is_empty());

    let due_bill = create_bill(&ctx, 2, corp_id, landlord_corp, 5_000.0, now - 10, 0.0, 0, 0);
    let paid_auto = process_due_automatic_bills(&ctx);
    assert!(paid_auto >= 1);
    let is_due_paid = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| serde_json::from_str::<CorpBillRecord>(&n.note).ok())
            .find(|b| b.bill_id == due_bill)
            .map(|b| b.paid)
    });
    assert_eq!(is_due_paid, Some(true));
}

#[tokio::test]
async fn test_new_eden_store_and_aurum_removal() {
    // parity: feat:removed.aurum-currency
    // DATA GAP: feat:wallet.nes-store (NES store catalog is live Quasar backend data not in V24 client FSD)
    eve_testkit::attest(&["feat:removed.aurum-currency"]);
    let ctx = ServerContext::new();
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut s1 = setup_pilot(&ctx, 882, "demo", "demo-secret", P1).await;

    let cats = rpc(&ctx, &mut s1, "storeManager", "get_categories", &[]).await.expect("get_categories");
    let PyRep::List(cat_list) = cats else { panic!("expected List"); };
    assert!(!cat_list.is_empty());

    let offers = rpc(&ctx, &mut s1, "storeManager", "get_offers", &[]).await.expect("get_offers");
    let PyRep::List(offer_list) = offers else { panic!("expected List"); };
    assert!(!offer_list.is_empty());

    let can_p = rpc(&ctx, &mut s1, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)]).await.expect("can_purchase_offer");
    assert_eq!(can_p, PyRep::Bool(true));

    let aur_err = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    ctx.db.with_mut_snapshot(|s| {
        s.adjust_plex_vault(P1, 500).unwrap();
    });
    let vault_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    let buy_res = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.expect("buy_offer");
    let PyRep::KeyVal(buy_kv) = buy_res else { panic!("expected KeyVal"); };
    assert_eq!(buy_kv.get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(buy_kv.get("offerID"), Some(&PyRep::Integer(1002)));
    let vault_after = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    assert_eq!(vault_before - vault_after, 110, "Offer 1002 costs 110 PLEX");

    let has_skin = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(has_skin, "Purchased SKIN delivered to station hangar");
}

#[tokio::test]
async fn test_station_direct_trade_window() {
    // parity: feat:wallet.trade-window
    eve_testkit::attest(&["feat:wallet.trade-window"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 883, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 884, "other", "other-secret", P2).await;

    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    let p1_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });
    let p1_singleton = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: 587,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before = balance(&ctx, P1);
    let p2_bal_before = balance(&ctx, P2);

    let init_rep = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade");
    let trade_id = kv_i64(&init_rep, "tradeContainerID");

    let overflow_qty = (1i64 << 32) - 1000;
    let err_overflow = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(overflow_qty)]).await.unwrap_err();
    assert_eq!(err_overflow.code, "InvalidQuantity");

    let err_negative = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(-100)]).await.unwrap_err();
    assert_eq!(err_negative.code, "InvalidQuantity");

    let err_singleton = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_singleton), PyRep::Integer(2)]).await.unwrap_err();
    assert_eq!(err_singleton.code, "CannotSplitSingleton");

    let off_item1 = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(60)]).await.expect("AddItem 60");
    assert_eq!(kv_i64(&off_item1, "tradeContainerID"), trade_id);

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(30)]).await.expect("AddItem 30");

    let err_exceed = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(20)]).await.unwrap_err();
    assert_eq!(err_exceed.code, "InsufficientQuantity");

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(10)]).await.expect("AddItem 10");

    let off_money = rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(50_000.0)]).await.expect("OfferMoney");
    assert_eq!(kv_i64(&off_money, "tradeContainerID"), trade_id);

    let acc1 = rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s1");
    let PyRep::KeyVal(kv1) = acc1 else { panic!("expected KeyVal"); };
    assert_eq!(kv1.get("completed"), Some(&PyRep::Bool(false)));

    let acc2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s2");
    let PyRep::KeyVal(kv2) = acc2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2.get("completed"), Some(&PyRep::Bool(true)));

    let p2_has_trit = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_item).map(|i| i.owner_id == P2 && i.quantity == 100 && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p2_has_trit, "Item ownership transferred to P2 in hangar");
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before.0, IskCents::from_isk(50_000).0);
    assert_eq!(p2_bal_before.0 - balance(&ctx, P2).0, IskCents::from_isk(50_000).0);

    let p1_partial_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before_partial = balance(&ctx, P1);
    let p2_bal_before_partial = balance(&ctx, P2);
    let total_isk_before = p1_bal_before_partial.0 + p2_bal_before_partial.0;

    let init_rep2 = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade 2");
    let trade_id2 = kv_i64(&init_rep2, "tradeContainerID");

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id2), PyRep::Integer(p1_partial_item), PyRep::Integer(30)]).await.expect("AddItem partial");
    rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id2), PyRep::Real(15_000.0)]).await.expect("OfferMoney partial");
    rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s1");
    let acc2_2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s2");
    let PyRep::KeyVal(kv2_2) = acc2_2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2_2.get("completed"), Some(&PyRep::Bool(true)));

    let p1_retains_70 = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_partial_item).map(|i| i.owner_id == P1 && i.quantity == 70 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p1_retains_70, "Seller A retains exactly 70 units of the item in hangar");

    let p2_received_30 = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P2 && i.type_id == TRITANIUM && i.quantity == 30 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(p2_received_30, "Buyer B receives 30 units of the item in hangar");

    let total_isk_after = balance(&ctx, P1).0 + balance(&ctx, P2).0;
    assert_eq!(total_isk_before, total_isk_after, "Total ISK is strictly conserved");
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before_partial.0, IskCents::from_isk(15_000).0);
    assert_eq!(p2_bal_before_partial.0 - balance(&ctx, P2).0, IskCents::from_isk(15_000).0);

    let total_item_units: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| (i.item_id == p1_partial_item || (i.owner_id == P2 && i.quantity == 30)) && i.type_id == TRITANIUM).map(|i| i.quantity).sum()
    });
    assert_eq!(total_item_units, 100, "Item units strictly conserved (70 retained + 30 transferred = 100)");
}

#[tokio::test]
async fn test_hypernet_relay_and_hypercore_scaling_and_expiry() {
    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry
    eve_testkit::attest(&["feat:wallet.hypernet-relay", "feat:wallet.hypernet-hypercore-sizing-expiry"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 885, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 886, "other", "other-secret", P2).await;
    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    assert!(compute_required_hypercores(10_000_000.0, 10).is_err());
    let cores_8 = compute_required_hypercores(50_000_000.0, 8).unwrap();
    let cores_512 = compute_required_hypercores(50_000_000.0, 512).unwrap();
    assert!(cores_512 >= cores_8, "More nodes require more HyperCores");

    let (raffle_item, _) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let it_id = s.next_entity_id;
        s.items.insert(it_id, InventoryItemRecord {
            item_id: it_id,
            type_id: 587,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.next_entity_id += 1;
        let hc_id = s.next_entity_id;
        s.items.insert(hc_id, InventoryItemRecord {
            item_id: hc_id,
            type_id: HYPERCORE_TYPE_ID,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        (it_id, hc_id)
    });

    let c_raf = rpc(&ctx, &mut s1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(raffle_item)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(16)),
        (PyRep::String("ticketPrice".into()), PyRep::Real(10_000.0)),
    ])]).await.expect("CreateRaffle");
    let raffle_id = kv_i64(&c_raf, "raffle_id");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), -52_568);

    let p2_before = balance(&ctx, P2);
    let buy_t = rpc(&ctx, &mut s2, "raffleProxy", "BuyTicket", &[PyRep::Integer(raffle_id), PyRep::Integer(0)]).await.expect("BuyTicket");
    assert_eq!(kv_i64(&buy_t, "raffle_id"), raffle_id);
    assert_eq!(kv_i64(&buy_t, "tickets_remaining"), 15);
    assert_eq!(p2_before.0 - balance(&ctx, P2).0, IskCents::from_isk(10_000).0);

    ctx.advance_virtual_clock(72 * 3_600 + 1);
    let raf_info = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.expect("GetRaffle sweeps expired");
    let PyRep::KeyVal(r_kv) = raf_info else { panic!("expected KeyVal"); };
    assert_eq!(r_kv.get("is_expired"), Some(&PyRep::Bool(true)));

    assert_eq!(balance(&ctx, P2), p2_before, "Buyer refunded upon raffle expiry");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].flag), InventoryFlag::Hangar);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), P1);
}

#[tokio::test]
async fn test_pos_towers_and_starbases_removed() {
    // parity: feat:removed.pos-towers
    eve_testkit::attest(&["feat:removed.pos-towers"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 887, "demo", "demo-secret", P1).await;

    let towers = rpc(&ctx, &mut s1, "posMgr", "GetControlTowers", &[]).await.expect("GetControlTowers");
    assert_eq!(towers, PyRep::List(Vec::new()));

    let moon = rpc(&ctx, &mut s1, "posMgr", "GetMoonForTower", &[PyRep::Integer(1)]).await.expect("GetMoonForTower");
    assert_eq!(moon, PyRep::None);

    let anchor_err = rpc(&ctx, &mut s1, "posMgr", "AnchorOrbital", &[PyRep::Integer(1), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(anchor_err.code, "StarbaseRemoved");
}

#[tokio::test]
async fn test_downtime_contract_expiry_settlement_and_corp_bill_division_isolation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 888, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 889, "other", "other-secret", P2).await;
    let now = ctx.now_unix();

    let exch_item_id = 995_001_i64;
    let auc_item_id = 995_002_i64;
    let exch_cid = 88_001_i64;
    let auc_cid = 88_002_i64;
    let p1_init = balance(&ctx, P1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            exch_item_id,
            InventoryItemRecord {
                item_id: exch_item_id,
                type_id: 34,
                owner_id: P1,
                location_id: exch_cid,
                flag: InventoryFlag::Locked,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            exch_cid,
            eve_db::ContractRecord {
                contract_id: exch_cid,
                issuer_id: P1,
                issuer_corp_id: 98_000_001,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                reward: IskCents::from_isk(25_000),
                expiration_unix: now - 10,
                item_ids: vec![exch_item_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            auc_item_id,
            InventoryItemRecord {
                item_id: auc_item_id,
                type_id: 587,
                owner_id: P1,
                location_id: auc_cid,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            auc_cid,
            eve_db::ContractRecord {
                contract_id: auc_cid,
                issuer_id: P1,
                issuer_corp_id: 98_000_001,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                current_bid: IskCents::from_isk(100_000),
                current_bidder_id: Some(P2),
                expiration_unix: now - 10,
                item_ids: vec![auc_item_id],
                ..Default::default()
            },
        );
    });

    let report = run_daily_downtime_and_jobs(&ctx);
    assert_eq!(report.get("expiredContracts"), Some(&2));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&exch_cid].status, "Expired");
        assert_eq!(snap.items[&exch_item_id].owner_id, P1);
        assert_eq!(snap.items[&exch_item_id].location_id, JITA_STATION_ID);
        assert_eq!(snap.items[&exch_item_id].flag, InventoryFlag::Hangar);

        assert_eq!(snap.contracts[&auc_cid].status, "Completed");
        assert_eq!(snap.contracts[&auc_cid].acceptor_id, Some(P2));
        assert_eq!(snap.items[&auc_item_id].owner_id, P2);
        assert_eq!(snap.items[&auc_item_id].location_id, JITA_STATION_ID);
        assert_eq!(snap.items[&auc_item_id].flag, InventoryFlag::Hangar);
    });
    assert_eq!(
        balance(&ctx, P1).0 - p1_init.0,
        IskCents::from_isk(125_000).0,
        "Issuer must receive 25k reward refund + 100k winning auction bid"
    );

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&P2) {
            ch2.corporation_id = corp_id;
        }
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(50_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.member_roles.insert(P2, 0);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_isk(50_000);
        for d in &mut corp.wallet_divisions {
            if d.division_id == 1 {
                d.balance = IskCents::from_isk(50_000);
            } else if d.division_id == 2 {
                d.balance = IskCents::from_isk(200_000);
            }
        }
    });
    s2.corporation_id = Some(corp_id);
    s2.corp_role = 0;

    let unauth_ap = rpc(
        &ctx,
        &mut s2,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![(
            PyRep::String("divisionID".into()),
            PyRep::Integer(1001),
        )])],
    )
    .await
    .expect_err("Non-accountant must not configure automatic bill payment");
    assert_eq!(unauth_ap.code, "CorpRoleRequired");

    let bill_id = create_bill(&ctx, 2, corp_id, 1_000_035, 120_000.0, now + 86_400, 0.0, JITA_STATION_ID, 0);
    rpc(
        &ctx,
        &mut s1,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(bill_id), PyRep::Integer(1001)],
    )
    .await
    .expect("PayCorporationBill from division 2 (1001) succeeds even though division 1 only has 50k");

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        let div1 = corp.wallet_divisions.iter().find(|d| d.division_id == 1).unwrap().balance;
        let div2 = corp.wallet_divisions.iter().find(|d| d.division_id == 2).unwrap().balance;
        assert_eq!(corp.master_wallet, IskCents::from_isk(50_000), "master_wallet untouched");
        assert_eq!(div1, IskCents::from_isk(50_000), "Division 1 untouched");
        assert_eq!(div2, IskCents::from_isk(80_000), "Division 2 debited by 120k");
    });
}
