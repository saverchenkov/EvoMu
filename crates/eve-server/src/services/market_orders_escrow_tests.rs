//! Unit tests for `market_orders_escrow` (multi-sell, order cancellation, multi-buy, price history).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
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
    let mut s = SessionState::new(9301);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn place_multi_sell_order_validation_and_resting_placement() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // Non-marketProxy returns None
    assert!(handle_orders_escrow(&ctx, &mut sess, "", "", &[], &kw).is_none());

    // Undocked fails
    sess.station_id = None;
    let undocked = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(undocked.code, "NotDockedAtStation");
    sess.station_id = Some(JITA_STATION_ID);

    // Empty itemList fails
    let empty_err = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(empty_err.code, "InvalidMultiSellOrder");

    // Seed two stacks in Jita hangar: 500x type 34 and 1 singleton ship (type 587)
    let (stack_id, singleton_id) = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        s.next_entity_id += 1;
        let sid = s.next_entity_id;
        s.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 34,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let sgid = s.next_entity_id;
        s.items.insert(
            sgid,
            InventoryItemRecord {
                item_id: sgid,
                type_id: 587,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (sid, sgid)
    });

    // Assembled singleton item rejected
    let singleton_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("typeID", PyRep::Integer(587)),
        ("itemID", PyRep::Integer(singleton_id)),
        ("price", PyRep::Real(500_000.0)),
        ("quantity", PyRep::Integer(1)),
    ]);
    let sg_err = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![singleton_entry]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(sg_err.code, "CannotSellAssembledItem");

    // Partial stack sell order: sell 200 of 500 units @ 10 ISK, duration 90 days
    let valid_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(stack_id)),
        ("price", PyRep::Real(10.0)),
        ("quantity", PyRep::Integer(200)),
    ]);
    let res = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![valid_entry]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    let oid = res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        res.dict_get("filledImmediately").and_then(PyRep::as_bool),
        Some(false)
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&stack_id].quantity, 300);
        assert_eq!(s.market_orders[&oid].vol_remaining, 200);
        assert_eq!(s.market_orders[&oid].price, IskCents::from_isk(10));
    });
}

#[test]
fn place_multi_sell_immediate_match_with_margin_trading_cover() {
    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&seller_id].corporation_id);
    let mut sess = jita_session(seller_id, corp_id);
    let kw = HashMap::new();

    let (stack_id, buyer_bal_before) = ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&seller_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        let mut buyer = s.characters[&seller_id].clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 2;
        buyer.balance = IskCents::from_isk(10_000_000);
        s.characters.insert(buyer_id, buyer);

        // Resting buy order for 100 units @ 20 ISK with partial Margin Trading escrow (500 ISK instead of 2000 ISK)
        s.market_orders.insert(
            830_001,
            MarketOrderRecord {
                order_id: 830_001,
                owner_id: buyer_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(20),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500),
            },
        );
        save_market_order_meta(
            s,
            830_001,
            MarketOrderMeta {
                range: -1,
                duration: 90,
                issue_unix: ctx.now_unix().saturating_sub(100),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        s.next_entity_id += 1;
        let sid = s.next_entity_id;
        s.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 34,
                owner_id: seller_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (sid, IskCents::from_isk(10_000_000))
    });

    let entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(stack_id)),
        ("price", PyRep::Real(18.0)),
        ("quantity", PyRep::Integer(100)),
    ]);
    let res = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![entry]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    if let PyRep::List(orders) = res {
        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].dict_get("filledImmediately").and_then(PyRep::as_bool),
            Some(true)
        );
    }

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&830_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&830_001].escrow, IskCents(0));
        // Buyer covered uncovered 1500 ISK from wallet
        assert_eq!(
            s.characters[&buyer_id].balance,
            IskCents(buyer_bal_before.0 - 1500_00)
        );
        assert!(!s.items.contains_key(&stack_id));
    });
}

#[test]
fn cancel_char_order_buy_sell_plex_and_corp_authorization() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    let (bal_before, plex_before) = ctx.db.with_mut_snapshot(|s| {
        let b = s.characters[&char_id].balance;
        let p = s.plex_vault_balance(char_id);
        // 1. Personal buy order with 1200 ISK escrow
        s.market_orders.insert(
            840_001,
            MarketOrderRecord {
                order_id: 840_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(6),
                vol_entered: 200,
                vol_remaining: 200,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(1200),
            },
        );
        // 2. Personal PLEX sell order (5 PLEX)
        s.market_orders.insert(
            840_002,
            MarketOrderRecord {
                order_id: 840_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // 3. Personal regular item sell order (75x type 35)
        s.market_orders.insert(
            840_003,
            MarketOrderRecord {
                order_id: 840_003,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(25),
                vol_entered: 75,
                vol_remaining: 75,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // 4. Foreign order
        s.market_orders.insert(
            840_004,
            MarketOrderRecord {
                order_id: 840_004,
                owner_id: other_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(200),
            },
        );
        (b, p)
    });

    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(840_001)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(840_002)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(840_003)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Cancelling already-cancelled order fails
    let already_err = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(840_001)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(already_err.code, "OrderAlreadyFilled");

    // Cancelling foreign order fails
    let foreign_err = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(840_004)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(foreign_err.code, "OrderOwnershipViolation");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, IskCents(bal_before.0 + 1200_00));
        assert_eq!(s.plex_vault_balance(char_id), plex_before + 5);
        let returned_item = s
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == 35
            })
            .expect("cancelled sell order item returned to hangar");
        assert!(returned_item.quantity >= 75);
    });
}

#[test]
fn buy_multiple_items_and_price_history_queries() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&buyer_id].corporation_id);
    let mut sess = jita_session(buyer_id, corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(100_000);
        }
        let mut seller = s.characters[&buyer_id].clone();
        seller.character_id = seller_id;
        seller.user_id = 2;
        s.characters.insert(seller_id, seller);

        // Two sell orders for type 34: 30 @ 10 ISK and 40 @ 15 ISK
        s.market_orders.insert(
            850_001,
            MarketOrderRecord {
                order_id: 850_001,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 30,
                vol_remaining: 30,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            850_002,
            MarketOrderRecord {
                order_id: 850_002,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(15),
                vol_entered: 40,
                vol_remaining: 40,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    // Request 50 of type 34 (max 20 ISK -> fills 30@10 + 20@15 = 600 ISK) and 10 of type 36 (none available -> Unfilled)
    let reqs = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(50), PyRep::Real(20.0)]),
        PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(50.0)]),
    ]);
    let res = handle_orders_escrow(&ctx, &mut sess, "marketProxy", "BuyMultipleItems", &[reqs], &kw)
        .unwrap()
        .unwrap();

    assert_eq!(res.dict_get("totalQuantity").and_then(PyRep::as_i64), Some(50));
    assert_eq!(res.dict_get("totalCostISK").and_then(PyRep::as_f64), Some(600.0));
    if let Some(PyRep::List(statuses)) = res.dict_get("itemStatuses") {
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].dict_get("status").and_then(PyRep::as_str), Some("Filled"));
        assert_eq!(statuses[1].dict_get("status").and_then(PyRep::as_str), Some("Unfilled"));
    } else {
        panic!("expected itemStatuses list");
    }

    let old_hist = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOldPriceHistory",
        &[PyRep::Integer(34)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(old_hist.is_crowset());
    assert_eq!(old_hist.row_count(), Some(1));

    let new_hist = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetNewPriceHistory",
        &[PyRep::keyval([("typeID", PyRep::Integer(34))])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(new_hist.is_crowset());
}
