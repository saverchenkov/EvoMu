//! Deep branch-coverage and mutation-killing unit tests for `market_price_history`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::market_price_history::{
    build_own_order_changed_payload, build_price_history_rows, date_str_to_filetime,
    date_str_to_unix_secs, extract_type_ids_arg,
    handle_market_transactions_rpc, resolve_station_solar_and_constellation,
};
use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
use eve_db::ledger::MarketTransactionRecord;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

pub(super) fn make_meta(range: i64, duration: i64, issue_unix: u64, account_key: Option<u16>, order_state: Option<u8>) -> MarketOrderMeta {
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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "TxHistCorp".into();
                __corp.ticker = "TXHC".into();
                __corp.ceo_id = ceo_id;
                __corp.members = HashSet::from([ceo_id, unauth_member, accountant_member]);
                __corp.member_roles = roles;
                __corp },
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
fn mutant_kill_matched_orders_npc_filter_history_and_ceo_char_tx_rpcs() {
    use crate::services::market_price_history::execute_crossed_order_fills_on_modify;
    use eve_db::MarketHistoryDayRecord;

    let ctx = test_ctx();
    let ceo_id = 90_000_001_i64;
    let player_seller = 95_700_020_i64;
    let player_buyer = 95_700_021_i64;
    let unauth_member = 95_700_022_i64;
    let corp_id = 98_700_020_i64;
    let region_id = 10_000_002_u32;
    let station_id = 60_003_760_i64;
    let test_tid = 99_350_u32;
    let other_tid = 99_351_u32;
    let now_unix = 1_750_100_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&ceo_id].clone();
        for (cid, bal) in [
            (ceo_id, IskCents(1_000_000)),
            (player_seller, IskCents(500_000)),
            (player_buyer, IskCents(500_000)),
            (unauth_member, IskCents(500_000)),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = if cid == unauth_member || cid == ceo_id { corp_id } else { 1_000_035 };
            ch.balance = bal;
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "CeoTxCorp".into();
        corp.ticker = "CTC".into();
        corp.ceo_id = ceo_id;
        corp.members = HashSet::from([ceo_id, unauth_member]);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        // 1. Buy cross against 1 NPC ask (excluded from matched_orders) and 1 player ask (included)
        snap.market_orders.insert(
            9_101,
            MarketOrderRecord {
                order_id: 9_101,
                owner_id: ceo_id,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: true,
                price: IskCents(1_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(2_000),
            },
        );
        snap.market_orders.insert(
            9_102,
            MarketOrderRecord {
                order_id: 9_102,
                owner_id: 1_000_035,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: false,
                price: IskCents(800),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            9_103,
            MarketOrderRecord {
                order_id: 9_103,
                owner_id: player_seller,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: false,
                price: IskCents(900),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        let mut bid_meta = make_meta(32767, 90, now_unix, None, None);
        let buy_cross = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            9_101,
            IskCents(1_000),
            1.0,
            false,
            0,
            ceo_id,
            now_unix,
            &mut bid_meta,
        )
        .unwrap();
        assert_eq!(buy_cross.matched_orders, vec![(9_103, player_seller)]);

        // 2. Sell cross against 1 NPC bid (excluded from matched_orders) and 1 player bid (included)
        snap.market_orders.insert(
            9_201,
            MarketOrderRecord {
                order_id: 9_201,
                owner_id: player_seller,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            9_202,
            MarketOrderRecord {
                order_id: 9_202,
                owner_id: 1_000_035,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: true,
                price: IskCents(700),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(700),
            },
        );
        snap.market_orders.insert(
            9_203,
            MarketOrderRecord {
                order_id: 9_203,
                owner_id: player_buyer,
                station_id,
                region_id,
                type_id: test_tid,
                is_bid: true,
                price: IskCents(600),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(600),
            },
        );
        let mut ask_meta = make_meta(32767, 90, now_unix, None, None);
        let sell_cross = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            9_201,
            IskCents(500),
            1.0,
            false,
            0,
            player_seller,
            now_unix,
            &mut ask_meta,
        )
        .unwrap();
        assert_eq!(sell_cross.matched_orders, vec![(9_203, player_buyer)]);

        // 3. build_price_history_rows: persisted market_history + mixed-type tx fallback filtering
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 8_101,
            date_unix: now_unix,
            type_id: other_tid,
            quantity: 5,
            price: IskCents(2_000),
            buyer_id: corp_id,
            seller_id: player_seller,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            location_id: station_id,
            journal_ref_id: 8_101,
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 8_102,
            date_unix: now_unix,
            type_id: test_tid,
            quantity: 3,
            price: IskCents(1_200),
            buyer_id: player_buyer,
            seller_id: corp_id,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            location_id: station_id,
            journal_ref_id: 8_102,
        });

        let tx_rows = build_price_history_rows(snap, other_tid, region_id);
        assert_eq!(tx_rows.len(), 1);
        assert_eq!(tx_rows[0][1], PyRep::Real(20.0));
        assert_eq!(tx_rows[0][4], PyRep::Integer(5));
        assert_eq!(tx_rows[0][5], PyRep::Integer(1));

        snap.market_history.insert(
            (region_id, other_tid),
            vec![
                MarketHistoryDayRecord {
                    date: "2024-03-02".into(),
                    average: 25.0,
                    highest: 30.0,
                    lowest: 20.0,
                    order_count: 4,
                    volume: 100,
                    open_price: Some(20.0),
                    close_price: Some(30.0),
                },
                MarketHistoryDayRecord {
                    date: "2024-03-01".into(),
                    average: 15.0,
                    highest: 18.0,
                    lowest: 12.0,
                    order_count: 2,
                    volume: 40,
                    open_price: Some(12.0),
                    close_price: Some(18.0),
                },
            ],
        );
        let hist_rows = build_price_history_rows(snap, other_tid, region_id);
        assert_eq!(hist_rows.len(), 2);
        assert_eq!(hist_rows[0][0], PyRep::Integer(date_str_to_filetime("2024-03-01").unwrap()));
        assert_eq!(hist_rows[0][1], PyRep::Real(12.0));
        assert_eq!(hist_rows[1][4], PyRep::Integer(100));
    });

    // 4. handle_market_transactions_rpc: unknown -> None, CharGetTransactions, CEO (no explicit roles) -> Ok, non-CEO -> CrpAccessDenied
    let sess_ceo = auth_session(1, ceo_id, corp_id, 30_000_142);
    let sess_unauth = auth_session(2, unauth_member, corp_id, 30_000_142);

    assert!(handle_market_transactions_rpc(&ctx, &sess_ceo, "UnknownMarketTxRpc", &[]).is_none());

    let char_res = handle_market_transactions_rpc(&ctx, &sess_ceo, "CharGetTransactions", &[])
        .unwrap()
        .unwrap();
    let PyRep::ObjectEx { list: c_rows, .. } = char_res else {
        panic!("expected CRowset");
    };
    assert_eq!(c_rows.len(), 2);

    let corp_ceo_res = handle_market_transactions_rpc(&ctx, &sess_ceo, "CorpGetTransactions", &[])
        .unwrap()
        .unwrap();
    let PyRep::ObjectEx { list: co_rows, .. } = corp_ceo_res else {
        panic!("expected CRowset");
    };
    assert_eq!(co_rows.len(), 2);

    assert_eq!(
        handle_market_transactions_rpc(&ctx, &sess_unauth, "CorpGetTransactions", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
}
