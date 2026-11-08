//! Semantic unit tests split from `market_orders_escrow_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
use crate::services::market_trading::{
    handle_orders_escrow, save_market_order_meta, MarketOrderMeta,
};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    UpwellStructureRecord,
};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[test]
fn corporate_sell_and_cancel_orders_and_margin_trading_insolvency() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let mut sess = jita_session(char_id, corp_id);

    let stack_id = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch2) = s.characters.get_mut(&other_char) {
            ch2.corporation_id = corp_id;
        }
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.ceo_id = other_char;
            corp.member_roles.insert(char_id, 0);
            corp.ensure_wallet_divisions();
            corp.set_division_balance(1, IskCents::from_isk(1_000_000));
        }
        insert_item(s, 34, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 500, false)
    });

    let entry = PyRep::keyval([
        ("itemID", PyRep::Integer(stack_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(100.0)),
        ("quantity", PyRep::Integer(100)),
    ]);
    assert_eq!(
        call_escrow(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry.clone()]), PyRep::Bool(true), PyRep::Integer(14)]
        )
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );

    // Grant accountant + trader roles and place corporate resting sell order using division 1001 (div 2)
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.member_roles.insert(
            char_id,
            corp_roles::ROLE_ACCOUNTANT
                | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | (1u64 << 20)
                | corp_roles::ROLE_TRADER,
        );
        corp.ensure_wallet_divisions();
        if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 2) {
            d.balance = IskCents::from_isk(500_000);
        }
    });
    sess.corp_account_key = 1001;
    let res = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![entry]), PyRep::Bool(true), PyRep::Integer(14)],
    )
    .unwrap();
    let corp_sell_oid = res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();

    // Strip trader role: issuer can still cancel their own order, while unprivileged other_char gets CorpRoleRequired
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.member_roles.insert(char_id, 0);
        corp.ceo_id = 90_000_099;
    });
    let mut other_sess = jita_session(other_char, corp_id);
    assert_eq!(
        call_escrow(&ctx, &mut other_sess, "CancelCharOrder", &[PyRep::Integer(corp_sell_oid)])
            .unwrap_err()
            .code,
        "CorpRoleRequired"
    );

    call_escrow(&ctx, &mut sess, "CancelCharOrder", &[PyRep::Integer(corp_sell_oid)]).unwrap();
    let in_deliveries = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .any(|it| it.location_id == JITA_STATION_ID && it.flag == InventoryFlag::Deliveries && it.quantity == 100)
    });
    assert!(in_deliveries);

    // Seed a corporate buy order with escrow and cancel it as CEO -> refunds escrow to corp division 2
    let corp_buy_oid = 888_101_i64;
    let insolvent_buy_oid = 888_102_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.ceo_id = char_id;
        corp.ensure_wallet_divisions();
        if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 2) {
            d.balance = IskCents::from_isk(1_000);
        }
        s.market_orders.insert(
            corp_buy_oid,
            MarketOrderRecord {
                order_id: corp_buy_oid,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(50),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500),
            },
        );
        save_market_order_meta(
            s,
            corp_buy_oid,
            MarketOrderMeta {
                range: -1,
                duration: 14,
                issue_unix: now_u,
                order_state: None,
                account_key: Some(1001),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        if let Some(ch) = s.characters.get_mut(&other_char) {
            ch.balance = IskCents::ZERO;
        }
        s.market_orders.insert(
            insolvent_buy_oid,
            MarketOrderRecord {
                order_id: insolvent_buy_oid,
                owner_id: other_char,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(100),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(1_200),
            },
        );
    });
    call_escrow(&ctx, &mut sess, "CancelCharOrder", &[PyRep::Integer(corp_buy_oid)]).unwrap();
    let div2_bal = ctx.db.with_snapshot(|s| {
        s.corporations[&corp_id]
            .wallet_divisions
            .iter()
            .find(|d| d.division_id == 2)
            .unwrap()
            .balance
            .to_f64_isk()
    });
    assert!((div2_bal - 1_500.0).abs() < 1e-2);

    // Selling with duration=0 against insolvent buy order refunds buyer's escrow, zeroes buy order, and returns ImmediateOrderUnfilled
    let immediate_entry = PyRep::keyval([
        ("itemID", PyRep::Integer(stack_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(90.0)),
        ("quantity", PyRep::Integer(50)),
    ]);
    assert_eq!(
        call_escrow(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![immediate_entry]), PyRep::Bool(false), PyRep::Integer(0)]
        )
        .unwrap_err()
        .code,
        "ImmediateOrderUnfilled"
    );
}

#[test]
fn buy_multiple_items_plex_npc_infinite_supply_visibility_and_history_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10_000_000);
        }
        s.market_orders.insert(
            901_001,
            MarketOrderRecord {
                order_id: 901_001,
                owner_id: 1_000_035,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 3300,
                is_bid: false,
                price: IskCents::from_isk(20_000),
                vol_entered: 100,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents::ZERO,
            },
        );
        s.market_orders.insert(
            901_002,
            MarketOrderRecord {
                order_id: 901_002,
                owner_id: 90_000_002,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(100_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });

    assert_eq!(
        call_escrow(
            &ctx,
            &mut sess,
            "BuyMultipleItems",
            &[PyRep::Integer(JITA_STATION_ID + 1), PyRep::List(vec![])]
        )
        .unwrap_err()
        .code,
        "StationMismatch"
    );

    let entries = PyRep::List(vec![
        PyRep::keyval([
            ("typeID", PyRep::Integer(3300)),
            ("qty", PyRep::Integer(2)),
            ("price", PyRep::Real(25_000.0)),
        ]),
        PyRep::Dict(vec![
            (PyRep::String("typeID".into()), PyRep::Integer(PLEX_TYPE_ID as i64)),
            (PyRep::String("quantity".into()), PyRep::Integer(3)),
            (PyRep::String("maxPrice".into()), PyRep::Real(150_000.0)),
        ]),
    ]);
    let res = call_escrow(
        &ctx,
        &mut sess,
        "BuyMultipleItems",
        &[PyRep::Integer(JITA_STATION_ID), entries, PyRep::Bool(true)],
    )
    .unwrap();
    let PyRep::List(orders_created) = res else {
        panic!("expected List from 3-arg BuyMultipleItems");
    };
    assert_eq!(orders_created.len(), 2);
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&901_001].vol_remaining), 100);
    assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0)), 503);

    // Order range > max_vis_jumps -> InsufficientVisibilitySkill
    let far_entry = PyRep::List(vec![PyRep::keyval([
        ("typeID", PyRep::Integer(34)),
        ("quantity", PyRep::Integer(1)),
        ("maxPrice", PyRep::Real(10.0)),
        ("orderRange", PyRep::Integer(20)),
    ])]);
    assert_eq!(
        call_escrow(
            &ctx,
            &mut sess,
            "BuyMultipleItems",
            &[PyRep::Integer(JITA_STATION_ID), far_entry]
        )
        .unwrap_err()
        .code,
        "InsufficientVisibilitySkill"
    );

    // Price history NoRegion and InvalidMarketOrder errors + kwargs typeID
    sess.region_id = None;
    assert_eq!(
        call_escrow(&ctx, &mut sess, "GetOldPriceHistory", &[]).unwrap_err().code,
        "NoRegion"
    );
    sess.region_id = Some(THE_FORGE_REGION_ID);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "GetNewPriceHistory", &[]).unwrap_err().code,
        "InvalidMarketOrder"
    );
    let mut hist_kw = HashMap::new();
    hist_kw.insert("typeID".to_string(), PyRep::Integer(34));
    assert!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "GetNewPriceHistory", &[], &hist_kw)
        .unwrap()
        .is_ok());

    // Upwell structure market access check
    let struct_id = 1_000_000_999_001_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: 30_000_142,
                owner_corp_id: corp_id,
                type_id: 35832,
                name: "Test Azbel".into(),
                state: "HullVulnerable".into(),
                ..Default::default()
            },
        );
    });
    sess.station_id = Some(struct_id);
    let struct_err = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(0)),
            ("typeID", PyRep::Integer(34)),
            ("price", PyRep::Real(10.0)),
            ("quantity", PyRep::Integer(1)),
        ])])],
    )
    .unwrap_err();
    assert!(!struct_err.code.is_empty());
}
