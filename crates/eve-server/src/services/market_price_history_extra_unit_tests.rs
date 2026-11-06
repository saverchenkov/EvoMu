//! Deep branch-coverage and mutation-killing unit tests for `market_price_history`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::market_contracts::PLEX_TYPE_ID;
use crate::services::market_price_history::{
    build_own_order_changed_payload, build_price_history_rows, date_str_to_filetime,
    date_str_to_unix_secs, execute_crossed_order_fills_on_modify, extract_type_ids_arg,
    handle_market_transactions_rpc, resolve_station_solar_and_constellation,
};
use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
use eve_db::ledger::MarketTransactionRecord;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

fn make_meta(range: i64, duration: i64, issue_unix: u64, account_key: Option<u16>, order_state: Option<u8>) -> MarketOrderMeta {
    MarketOrderMeta {
        range,
        duration,
        issue_unix,
        order_state,
        account_key,
        final_vol_remaining: None,
        closed_unix: None,
    }
}

#[test]
fn date_conversions_extract_type_ids_and_station_resolution_branches() {
    // 1. date_str_to_unix_secs & date_str_to_filetime: Jan/Feb vs Mar-Dec, pre-1970, and invalid inputs
    assert_eq!(date_str_to_unix_secs("1969-12-31"), None);
    assert_eq!(date_str_to_unix_secs("2024-00-15"), None);
    assert_eq!(date_str_to_unix_secs("2024-02-00"), None);
    assert_eq!(date_str_to_unix_secs("2024-02-29"), Some(1_709_164_800));
    assert_eq!(date_str_to_unix_secs("2024-03-01"), Some(1_709_251_200));
    assert_eq!(
        date_str_to_filetime("2024-03-01").unwrap() - date_str_to_filetime("2024-02-29").unwrap(),
        86_400 * 10_000_000
    );

    // 2. extract_type_ids_arg: Tuple, Object, ObjectEx (with list, with header ctor_args, and empty fallback)
    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Integer(34),
        ]))),
        Some((vec![34], false))
    );

    let obj_valid = PyRep::Object {
        type_name: "__builtin__.set".into(),
        args: Box::new(PyRep::Tuple(vec![PyRep::List(vec![
            PyRep::Integer(34),
            PyRep::Integer(35),
        ])])),
    };
    assert_eq!(
        extract_type_ids_arg(Some(&obj_valid)),
        Some((vec![34, 35], true))
    );

    let obj_invalid = PyRep::Object {
        type_name: "__builtin__.set".into(),
        args: Box::new(PyRep::Integer(34)),
    };
    assert_eq!(extract_type_ids_arg(Some(&obj_invalid)), None);

    let objex_with_list = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![])),
        list: vec![PyRep::Integer(36), PyRep::Integer(-5)],
        dict: vec![],
    };
    assert_eq!(
        extract_type_ids_arg(Some(&objex_with_list)),
        Some((vec![36], true))
    );

    let objex_with_hdr = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Integer(37)])]),
        ])),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(
        extract_type_ids_arg(Some(&objex_with_hdr)),
        Some((vec![37], true))
    );

    let objex_empty = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![])),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(extract_type_ids_arg(Some(&objex_empty)), Some((vec![], true)));

    // 3. resolve_station_solar_and_constellation & build_own_order_changed_payload branches
    let ctx = test_ctx();
    let item_office_id = 990_100_001i64;
    let open_ord_id = 990_100_002i64;
    let closed_ord_id = 990_100_003i64;
    let meta_only_ord_id = 990_100_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_office_id,
            InventoryItemRecord {
                item_id: item_office_id,
                type_id: 27,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            open_ord_id,
            MarketOrderRecord {
                order_id: open_ord_id,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(2500),
            },
        );
        snap.market_orders.insert(
            closed_ord_id,
            MarketOrderRecord {
                order_id: closed_ord_id,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            snap,
            meta_only_ord_id,
            MarketOrderMeta {
                range: -1,
                duration: 90,
                issue_unix: 1_700_000_000,
                order_state: Some(2),
                account_key: Some(1000),
                final_vol_remaining: Some(0),
                closed_unix: Some(1_700_000_100),
            },
        );
        snap.market_order_meta
            .get_mut(&meta_only_ord_id)
            .unwrap()
            .issued_by = Some(90_000_001);
    });

    ctx.db.with_snapshot(|snap| {
        // Direct solar system ID in station_id
        let (sys, con) = resolve_station_solar_and_constellation(&ctx, snap, 30_000_142);
        assert_eq!(sys, 30_000_142);
        assert!(con > 0);
        // Item location fallback
        let (sys_item, _) = resolve_station_solar_and_constellation(&ctx, snap, item_office_id);
        assert_eq!(sys_item, 30_000_142);
        // Unknown ID fallback
        assert_eq!(
            resolve_station_solar_and_constellation(&ctx, snap, 888_777_666),
            (0, 0)
        );
    });

    // build_own_order_changed_payload for open order (state=1), closed order (state=2), and meta-only order
    let p_open = build_own_order_changed_payload(&ctx, open_ord_id, "Modify", false);
    if let PyRep::List(v) = &p_open[0] {
        assert_eq!(v[0].dict_get("orderState").and_then(|x| x.as_i64()), Some(1));
    }
    let p_closed = build_own_order_changed_payload(&ctx, closed_ord_id, "Expiry", true);
    if let PyRep::List(v) = &p_closed[0] {
        assert_eq!(v[0].dict_get("orderState").and_then(|x| x.as_i64()), Some(2));
    }
    let p_meta = build_own_order_changed_payload(&ctx, meta_only_ord_id, "Expiry", true);
    if let PyRep::List(v) = &p_meta[0] {
        assert_eq!(v[0].dict_get("orderID").and_then(|x| x.as_i64()), Some(meta_only_ord_id));
        assert!(v[0].dict_get("solarSystemID").and_then(|x| x.as_i64()).unwrap_or(0) > 0);
    }
}

#[test]
fn build_price_history_fallback_from_transactions_and_corp_transactions_rpc() {
    let ctx = test_ctx();
    let ceo_id = 90_000_001i64;
    let unauth_member = 95_700_001i64;
    let accountant_member = 95_700_002i64;
    let corp_id = 98_700_001i64;
    let region_id = 10_000_002u32;
    let station_id = 60_003_760i64;
    let custom_type_id = 99_345u32;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&ceo_id].clone();
        for (cid, role) in [
            (unauth_member, 0u64),
            (accountant_member, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
            let _ = role;
        }
        let mut roles = HashMap::new();
        roles.insert(accountant_member, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "TxHistCorp".into(),
                ticker: "TXHC".into(),
                ceo_id,
                members: HashSet::from([ceo_id, unauth_member, accountant_member]),
                member_roles: roles,
                ..Default::default()
            },
        );
        // Anchor order at station_id so tx_region resolves to region_id
        snap.market_orders.insert(
            991_001,
            MarketOrderRecord {
                order_id: 991_001,
                owner_id: ceo_id,
                station_id,
                region_id,
                type_id: custom_type_id,
                is_bid: false,
                price: IskCents(1_000),
                vol_entered: 100,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Two transactions on the same day (1_700_000_000):
        // Fill 1: qty 2 @ 10.0 ISK (1_000 cents), corp is buyer on account 1002
        // Fill 2: qty 6 @ 30.0 ISK (3_000 cents), corp is seller on account 1003
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 7001,
            date_unix: 1_700_000_000,
            type_id: custom_type_id,
            quantity: 2,
            price: IskCents(1_000),
            buyer_id: corp_id,
            seller_id: ceo_id,
            buyer_account_key: 1002,
            seller_account_key: 1000,
            location_id: station_id,
            journal_ref_id: 5001,
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 7002,
            date_unix: 1_700_000_010,
            type_id: custom_type_id,
            quantity: 6,
            price: IskCents(3_000),
            buyer_id: ceo_id,
            seller_id: corp_id,
            buyer_account_key: 1000,
            seller_account_key: 1003,
            location_id: station_id,
            journal_ref_id: 5002,
        });
    });

    // 1. Fallback OHLC aggregation from market_transactions:
    // low = 10.0, high = 30.0, vol = 8, orders = 2, avg = (2*10 + 6*30)/8 = 200/8 = 25.0
    ctx.db.with_snapshot(|snap| {
        let rows = build_price_history_rows(snap, custom_type_id, region_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1], PyRep::Real(10.0));
        assert_eq!(rows[0][2], PyRep::Real(30.0));
        assert_eq!(rows[0][3], PyRep::Real(25.0));
        assert_eq!(rows[0][4], PyRep::Integer(8));
        assert_eq!(rows[0][5], PyRep::Integer(2));
    });

    // 2. handle_market_transactions_rpc:
    // a) Unauthenticated member without Accountant role denied on CorpGetTransactions
    let sess_denied = auth_session(1, unauth_member, corp_id, 30_000_142);
    let err = handle_market_transactions_rpc(&ctx, &sess_denied, "CorpGetTransactions", &[])
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // b) Junior Accountant with min_unix as Windows FileTime and accountKey=1002 filter
    let sess_acct = auth_session(1, accountant_member, corp_id, 30_000_142);
    let min_ft = (1_700_000_000i64 + 11_644_473_600) * 10_000_000;
    let res_1002 = handle_market_transactions_rpc(
        &ctx,
        &sess_acct,
        "CorpGetTransactions",
        &[PyRep::Integer(min_ft), PyRep::Integer(1002)],
    )
    .unwrap()
    .unwrap();
    // Only tx 7001 matches account 1002 (buyer, negative credit = -20.0)
    if let PyRep::Object { args, .. } = res_1002 {
        if let PyRep::Tuple(t) = args.as_ref() {
            if let Some(PyRep::List(rows)) = t.get(1) {
                assert_eq!(rows.len(), 1);
                if let PyRep::List(cells) = &rows[0] {
                    assert_eq!(cells[0], PyRep::Integer(7001));
                    assert_eq!(cells[12], PyRep::Real(-20.0));
                }
            }
        }
    }
}

#[test]
fn execute_crossed_order_fills_on_modify_buy_and_sell_margin_and_plex_branches() {
    let ctx = test_ctx();
    let buyer_char = 95_700_010i64;
    let seller_char = 95_700_011i64;
    let broke_buyer = 95_700_012i64;
    let corp_id = 98_700_010i64;
    let station_id = 60_003_760i64;
    let region_id = 10_000_002u32;
    let now_unix = 1_750_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        // Missing order returns zero ModifyCrossResult
        let mut dummy_meta = make_meta(32767, 90, now_unix, None, None);
        let none_res = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            999_999_999,
            IskCents(1_000),
            1.0,
            false,
            0,
            buyer_char,
            now_unix,
            &mut dummy_meta,
        )
        .unwrap();
        assert_eq!(none_res.delivered_to_corp_deliveries, 0);
        assert_eq!(none_res.final_escrow, IskCents(0));

        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, bal) in [
            (buyer_char, IskCents(100_000)),
            (seller_char, IskCents(50_000)),
            (broke_buyer, IskCents(0)),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.balance = bal;
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "CrossFillCorp".into(),
            ticker: "CFCP".into(),
            ceo_id: buyer_char,
            master_wallet: IskCents(200_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents(200_000);
        }
        snap.corporations.insert(corp_id, corp);

        // Scenario 1: Corp Buy Order (8001) modified upward to 1_000 cents/unit with 100% escrow (5_000 cents for 5 units),
        // crossing resting Ask (8002) at 800 cents/unit for 5 units -> price improvement refund of 1_000 cents to corp,
        // delivers 5 units to Corp Deliveries!
        snap.market_orders.insert(
            8001,
            MarketOrderRecord {
                order_id: 8001,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: 99_346,
                is_bid: true,
                price: IskCents(1_000),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(5_000),
            },
        );
        snap.market_orders.insert(
            8002,
            MarketOrderRecord {
                order_id: 8002,
                owner_id: seller_char,
                station_id,
                region_id,
                type_id: 99_346,
                is_bid: false,
                price: IskCents(800),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        let mut meta_8001 = make_meta(32767, 90, now_unix, Some(1000), None);
        let res1 = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            8001,
            IskCents(1_000),
            1.0,
            true,
            corp_id,
            buyer_char,
            now_unix,
            &mut meta_8001,
        )
        .unwrap();
        assert_eq!(res1.delivered_to_corp_deliveries, 5);
        assert_eq!(res1.final_escrow, IskCents(0));
        assert_eq!(snap.corporations[&corp_id].master_wallet, IskCents(201_000));
        assert_eq!(meta_8001.order_state, Some(2));

        // Scenario 2: Character Buy Order (8003) with partial margin escrow (escrow_fraction = 0.25, escrow = 250)
        // crossing Ask (8004) at 1_000 cents:
        // First test InsufficientFunds when broke_buyer cannot cover the 750 cents shortfall;
        // then test buyer_char covering the 750 cents shortfall!
        snap.market_orders.insert(
            8003,
            MarketOrderRecord {
                order_id: 8003,
                owner_id: broke_buyer,
                station_id,
                region_id,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents(1_000),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(250),
            },
        );
        snap.market_orders.insert(
            8004,
            MarketOrderRecord {
                order_id: 8004,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents(1_000),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        let mut meta_8003 = make_meta(32767, 90, now_unix, None, None);
        let Err(err_shortfall) = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            8003,
            IskCents(1_000),
            0.25,
            false,
            0,
            broke_buyer,
            now_unix,
            &mut meta_8003,
        ) else {
            panic!("expected InsufficientFunds error");
        };
        assert_eq!(err_shortfall.code, "InsufficientFunds");

        // Switch owner to buyer_char (who has 100_000 cents) -> covers 750 shortfall and settles PLEX to vault!
        snap.market_orders.get_mut(&8003).unwrap().owner_id = buyer_char;
        let _ = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            8003,
            IskCents(1_000),
            0.25,
            false,
            0,
            buyer_char,
            now_unix,
            &mut meta_8003,
        )
        .unwrap();
        assert_eq!(snap.characters[&buyer_char].balance, IskCents(99_250));

        // Scenario 3: Sell Order (8005) modified downward to 500 cents, crossing:
        // - Bid 8006 (broke_buyer, margin escrow = 100, price = 600): fails shortfall cover -> Bid 8006 canceled and 100 escrow refunded to broke_buyer!
        // - Bid 8007 (buyer_char, margin escrow = 100, price = 550): covers 450 shortfall from buyer_char, fills Sell Order 8005!
        snap.market_orders.insert(
            8005,
            MarketOrderRecord {
                order_id: 8005,
                owner_id: seller_char,
                station_id,
                region_id,
                type_id: 99_346,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            8006,
            MarketOrderRecord {
                order_id: 8006,
                owner_id: broke_buyer,
                station_id,
                region_id,
                type_id: 99_346,
                is_bid: true,
                price: IskCents(600),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(100),
            },
        );
        snap.market_orders.insert(
            8007,
            MarketOrderRecord {
                order_id: 8007,
                owner_id: buyer_char,
                station_id,
                region_id,
                type_id: 99_346,
                is_bid: true,
                price: IskCents(550),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(100),
            },
        );
        let mut meta_8005 = make_meta(32767, 90, now_unix, None, None);
        let _ = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            8005,
            IskCents(500),
            0.0,
            false,
            0,
            seller_char,
            now_unix,
            &mut meta_8005,
        )
        .unwrap();
        // Bid 8006 was canceled and refunded 100 cents to broke_buyer
        assert_eq!(snap.market_orders[&8006].vol_remaining, 0);
        assert_eq!(snap.characters[&broke_buyer].balance, IskCents(100));
        // Bid 8007 and Sell 8005 both filled
        assert_eq!(snap.market_orders[&8007].vol_remaining, 0);
        assert_eq!(snap.market_orders[&8005].vol_remaining, 0);
    });
}
