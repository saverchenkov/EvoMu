//! Unit tests for `market_contracts` (PLEX market, order expiry/modification, fee math, contract helpers).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9101);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn market_price_tick_remote_jumps_sales_tax_and_relist_fee_math() {
    assert!(is_valid_4_sig_fig_price(100.0));
    assert!(is_valid_4_sig_fig_price(1234.0));
    assert!(is_valid_4_sig_fig_price(12340.0));
    assert!(is_valid_4_sig_fig_price(123_400_000.0));
    assert!(is_valid_4_sig_fig_price(0.12));
    assert!(is_valid_4_sig_fig_price(99.99));
    assert!(!is_valid_4_sig_fig_price(12345.0));
    assert!(!is_valid_4_sig_fig_price(1.2345));
    assert!(!is_valid_4_sig_fig_price(100.01));
    assert!(!is_valid_4_sig_fig_price(0.0));
    assert!(!is_valid_4_sig_fig_price(-10.0));
    assert!(!is_valid_4_sig_fig_price(f64::NAN));
    assert!(!is_valid_4_sig_fig_price(f64::INFINITY));
    assert!(!is_valid_4_sig_fig_price(0.001));

    assert_eq!(max_remote_jumps_for_skill_level(0), -1);
    assert_eq!(max_remote_jumps_for_skill_level(1), 0);
    assert_eq!(max_remote_jumps_for_skill_level(2), 5);
    assert_eq!(max_remote_jumps_for_skill_level(3), 10);
    assert_eq!(max_remote_jumps_for_skill_level(4), 20);
    assert_eq!(max_remote_jumps_for_skill_level(5), 32_767);
    assert_eq!(max_remote_jumps_for_skill_level(9), 32_767);

    let gross = IskCents::from_isk(100_000_000);
    let tax0 = compute_sales_tax_cents(gross, 0);
    let tax5 = compute_sales_tax_cents(gross, 5);
    assert_eq!(tax0.0, 7_500_000_00);
    assert!(tax5.0 < tax0.0);
    assert_eq!(tax5.0, 3_375_000_00);

    // Minimum floor of 100 ISK (10_000 cents) on tiny modification
    let min_fee = compute_relist_fee_cents(
        IskCents::from_isk(100),
        IskCents::from_isk(90),
        1,
        0.03,
        5,
    );
    assert_eq!(min_fee, IskCents(10_000));

    // Price increase charges both base relist discount and 100% of incremental delta * broker_rate
    let inc_fee = compute_relist_fee_cents(
        IskCents::from_isk(10_000),
        IskCents::from_isk(20_000),
        100,
        0.02,
        0,
    );
    // new_total = 2_000_000 ISK; base = 2_000_000 * 0.02 * 0.50 = 20_000 ISK
    // inc = 1_000_000 ISK; delta = 1_000_000 * 0.02 = 20_000 ISK => total = 40_000 ISK
    assert_eq!(inc_fee, IskCents::from_isk(40_000));

    // Price decrease with Advanced Broker Relations V (discount_factor = 0.20)
    let dec_fee = compute_relist_fee_cents(
        IskCents::from_isk(20_000),
        IskCents::from_isk(10_000),
        100,
        0.02,
        5,
    );
    // new_total = 1_000_000 ISK; base = 1_000_000 * 0.02 * 0.20 = 4_000 ISK
    assert_eq!(dec_fee, IskCents::from_isk(4_000));
}

#[test]
fn sweep_expired_orders_refunds_buy_escrow_and_returns_sell_items() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let now = 1_750_000_000_u64;
    let expired_issue = now - 2 * 86_400;

    let (corp_id, initial_char_bal, initial_corp_bal, initial_plex) = ctx.db.with_mut_snapshot(|s| {
        let cid = s.characters[&char_id].corporation_id;
        s.corporations.entry(cid).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: cid,
            ceo_id: char_id,
            name: "SweepTestCorp".into(),
            ticker: "SWTC".into(),
            master_wallet: IskCents::from_isk(1_000_000),
            ..Default::default()
        });
        let cbal = s.characters[&char_id].balance;
        let corp_bal = s.corporations[&cid].master_wallet;
        let plex_before = s.plex_vault_balance(char_id);

        // 1. Expired personal buy order
        s.market_orders.insert(
            700_001,
            MarketOrderRecord {
                order_id: 700_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 400,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2000),
            },
        );
        save_market_order_meta(
            s,
            700_001,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 2. Expired corporate buy order (division 2 -> accountKey 1001)
        s.market_orders.insert(
            700_002,
            MarketOrderRecord {
                order_id: 700_002,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2500),
            },
        );
        save_market_order_meta(
            s,
            700_002,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1001),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 3. Expired personal PLEX sell order
        s.market_orders.insert(
            700_003,
            MarketOrderRecord {
                order_id: 700_003,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 10,
                vol_remaining: 6,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_003,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 4. Expired corporate regular item sell order
        s.market_orders.insert(
            700_004,
            MarketOrderRecord {
                order_id: 700_004,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(20),
                vol_entered: 300,
                vol_remaining: 150,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_004,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 5. Active non-expired personal buy order (must remain active)
        s.market_orders.insert(
            700_005,
            MarketOrderRecord {
                order_id: 700_005,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(6),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(600),
            },
        );
        save_market_order_meta(
            s,
            700_005,
            MarketOrderMeta {
                range: -1,
                duration: 7,
                issue_unix: now - 86_400,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        (cid, cbal, corp_bal, plex_before)
    });

    let swept = ctx.db.with_mut_snapshot(|s| sweep_expired_orders(s, now));
    assert_eq!(swept.len(), 4);

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, IskCents(initial_char_bal.0 + 2000_00));
        assert_eq!(s.corporations[&corp_id].master_wallet, initial_corp_bal);
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents(2500_00));
        assert_eq!(s.plex_vault_balance(char_id), initial_plex + 6);
        assert_eq!(s.market_orders[&700_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&700_001].escrow, IskCents(0));
        assert_eq!(s.market_orders[&700_005].vol_remaining, 100);

        let corp_deliv = s
            .items
            .values()
            .find(|i| {
                i.owner_id == corp_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Deliveries
                    && i.type_id == 35
            })
            .expect("expired corp sell items returned to Deliveries");
        assert_eq!(corp_deliv.quantity, 150);

        let meta1 = load_market_order_meta(s, &s.market_orders[&700_001], now);
        assert_eq!(meta1.order_state, Some(4));
        assert_eq!(meta1.final_vol_remaining, Some(400));
        assert_eq!(meta1.closed_unix, Some(now));
    });
}

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
        s.service_state_blobs.insert(format!("contract_deposit:{cid}"), "1000000".to_string());
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
