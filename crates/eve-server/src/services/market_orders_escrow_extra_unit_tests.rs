#![allow(unused_imports)]
//! Additional unit and mutation-killing tests for `market_orders_escrow`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
use crate::services::market_trading::{
    handle_orders_escrow, save_market_order_meta, MarketOrderMeta,
};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "market_corp_orders_and_plex_buy_unit_tests.rs"]
mod market_corp_orders_and_plex_buy_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9401);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

pub(super) fn call_escrow(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle_orders_escrow(ctx, sess, "marketProxy", method, args, &kw).unwrap()
}

pub(super) fn insert_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
    snap.next_entity_id += 1;
    let id = snap.next_entity_id;
    snap.items.insert(
        id,
        InventoryItemRecord {
            item_id: id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
    id
}

#[test]
fn place_multi_sell_order_validation_broker_fee_and_inventory_error_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);

    // NoRegion check
    sess.region_id = None;
    let err = call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![])]).unwrap_err();
    assert_eq!(err.code, "NoRegion");
    sess.region_id = Some(THE_FORGE_REGION_ID);

    let stack_id = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        insert_item(s, 34, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1000, false)
    });

    // Dict entry format
    let dict_entry = PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(stack_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("price".into()), PyRep::Real(10.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(100)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
    ]);

    // Invalid duration (5 days is not in [0,1,3,7,14,30,90]) -> InvalidMarketOrder
    let err = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![dict_entry.clone()]), PyRep::Bool(false), PyRep::Integer(5)],
    )
    .unwrap_err();
    assert_eq!(err.code, "InvalidMarketOrder");

    // Negative expectedBrokersFee -> InvalidMultiSellOrder
    let err = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![dict_entry.clone()]),
            PyRep::Bool(false),
            PyRep::Integer(14),
            PyRep::Real(-0.01),
        ],
    )
    .unwrap_err();
    assert_eq!(err.code, "InvalidMultiSellOrder");

    // Rate expectedBrokersFee <= 1.0 (0.0001 < actual ~0.03) -> BrokerFeeExceeded
    let err = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![dict_entry.clone()]),
            PyRep::Bool(false),
            PyRep::Integer(14),
            PyRep::Real(0.0001),
        ],
    )
    .unwrap_err();
    assert_eq!(err.code, "BrokerFeeExceeded");

    // Absolute expectedBrokersFee > 1.0 (2.0 ISK < min 100 ISK broker fee) -> BrokerFeeExceeded
    let err = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![dict_entry.clone()]),
            PyRep::Bool(false),
            PyRep::Integer(14),
            PyRep::Real(2.0),
        ],
    )
    .unwrap_err();
    assert_eq!(err.code, "BrokerFeeExceeded");

    // expectedBrokersFee == 0.0 is ignored and succeeds
    let ok = call_escrow(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![dict_entry]), PyRep::Bool(false), PyRep::Integer(14), PyRep::Real(0.0)],
    )
    .unwrap();
    assert_eq!(ok.dict_get("placedCount").and_then(PyRep::as_i64), Some(1));

    // StationMismatch, InvalidPriceTickSize (>4 sig digits), and ItemNotMarketEligible (type 3468)
    let bad_station = PyRep::keyval([
        ("itemID", PyRep::Integer(stack_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(10.0)),
        ("quantity", PyRep::Integer(10)),
        ("stationID", PyRep::Integer(JITA_STATION_ID + 1)),
    ]);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![bad_station])])
            .unwrap_err()
            .code,
        "StationMismatch"
    );

    let bad_tick = PyRep::keyval([
        ("itemID", PyRep::Integer(stack_id)),
        ("typeID", PyRep::Integer(34)),
        ("price", PyRep::Real(123.456)),
        ("quantity", PyRep::Integer(10)),
    ]);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![bad_tick])])
            .unwrap_err()
            .code,
        "InvalidPriceTickSize"
    );

    let wrap_entry = PyRep::keyval([
        ("itemID", PyRep::Integer(0)),
        ("typeID", PyRep::Integer(3468)),
        ("price", PyRep::Real(10.0)),
        ("quantity", PyRep::Integer(1)),
    ]);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![wrap_entry])])
            .unwrap_err()
            .code,
        "ItemNotMarketEligible"
    );
}

#[test]
fn place_multi_sell_order_damaged_ship_and_container_auto_discovery_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);

    let (ship_id, box_id, wrapped_id, in_box_id) = ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, it| it.location_id != JITA_STATION_ID);
        let ship_id = insert_item(s, 587, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
        s.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 5.0);

        let wrapped_id = insert_item(s, 35, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 50, false);
        s.item_custom_names
            .insert(wrapped_id, "courier_cargo:777".into());

        let box_id = insert_item(s, 3293, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, false);
        let in_box_id = insert_item(s, 37, char_id, box_id, InventoryFlag::Cargo, 10, false);
        (ship_id, box_id, wrapped_id, in_box_id)
    });

    // 1. Damaged ship hull -> CantRepackageDamagedItem; then clear hull damage and set rack heat -> CantRepackageDamagedItem
    let ship_entry = PyRep::keyval([
        ("itemID", PyRep::Integer(ship_id)),
        ("typeID", PyRep::Integer(587)),
        ("price", PyRep::Real(100_000.0)),
        ("quantity", PyRep::Integer(1)),
    ]);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![ship_entry.clone()])])
            .unwrap_err()
            .code,
        "CantRepackageDamagedItem"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship_id).unwrap().dynamic_attributes.clear();
    });
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit.rack_heat = [10.0, 0.0, 0.0];
    ctx.store_ship_fitting(ship_id, fit);
    assert_eq!(
        call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![ship_entry])])
            .unwrap_err()
            .code,
        "CantRepackageDamagedItem"
    );

    // 2. Courier cargo item (35), container with contents (3293), and item inside container (37)
    for (item_id, tid, qty, expected) in [
        (wrapped_id, 35, 10, "ItemCannotBeSold"),
        (0, 35, 10, "ItemCannotBeSold"),
        (box_id, 3293, 1, "CannotSellItemWithContents"),
        (0, 3293, 1, "CannotSellItemWithContents"),
        (in_box_id, 37, 5, "ItemInsideContainer"),
        (0, 37, 5, "ItemInsideContainer"),
    ] {
        let entry = PyRep::keyval([
            ("itemID", PyRep::Integer(item_id)),
            ("typeID", PyRep::Integer(tid)),
            ("price", PyRep::Real(10.0)),
            ("quantity", PyRep::Integer(qty)),
        ]);
        assert_eq!(
            call_escrow(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![entry])])
                .unwrap_err()
                .code,
            expected
        );
    }
}

#[test]
fn issue_789_buy_multiple_items_multi_fill_emits_final_deduped_account_change() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&buyer_id].corporation_id);
    let mut sess = jita_session(buyer_id, corp_id);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.characters.get_mut(&buyer_id).unwrap().balance = IskCents::from_isk(1_000_000);
        if let Some(ch2) = s.characters.get_mut(&seller_id) {
            ch2.balance = IskCents::from_isk(50_000);
        } else {
            let mut ch2 = s.characters[&buyer_id].clone();
            ch2.character_id = seller_id;
            ch2.user_id = 2;
            ch2.name = "ThirdParty Seller".to_string();
            ch2.balance = IskCents::from_isk(50_000);
            s.characters.insert(seller_id, ch2);
        }
        s.next_entity_id += 1;
        let oid_self = s.next_entity_id;
        s.market_orders.insert(
            oid_self,
            MarketOrderRecord {
                order_id: oid_self,
                owner_id: buyer_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.next_entity_id += 1;
        let oid_third_a = s.next_entity_id;
        s.market_orders.insert(
            oid_third_a,
            MarketOrderRecord {
                order_id: oid_third_a,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(200),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.next_entity_id += 1;
        let oid_third_b = s.next_entity_id;
        s.market_orders.insert(
            oid_third_b,
            MarketOrderRecord {
                order_id: oid_third_b,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: false,
                price: IskCents::from_isk(300),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    ctx.db
        .acquire_online_lock(1, 9401, Some(buyer_id), true, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(2, 9402, Some(seller_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(buyer_id);
    let _ = ctx.drain_notifications_for_char(seller_id);

    // Self-fill on type 34, followed by two fills from seller_id on types 35 and 36.
    let reqs = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(10), PyRep::Real(100.0)]),
        PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(10), PyRep::Real(200.0)]),
        PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(5), PyRep::Real(300.0)]),
    ]);
    call_escrow(&ctx, &mut sess, "BuyMultipleItems", &[reqs]).unwrap();

    let buyer_final = ctx.db.get_character(buyer_id).unwrap().balance.to_wire_f64();
    let seller_final = ctx.db.get_character(seller_id).unwrap().balance.to_wire_f64();

    let buyer_bals: Vec<f64> = ctx
        .drain_notifications_for_char(buyer_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert_eq!(
        buyer_bals.len(),
        1,
        "Mixed self-fill + third-party BuyMultipleItems must emit only the true final buyer balance: {buyer_bals:?}"
    );
    assert!(
        (buyer_bals[0] - buyer_final).abs() < 1e-2,
        "Buyer OnAccountChange ({:?}) must match final balance ({buyer_final})",
        buyer_bals[0]
    );

    let seller_bals: Vec<f64> = ctx
        .drain_notifications_for_char(seller_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert_eq!(
        seller_bals.len(),
        1,
        "Third-party seller appearing across multiple BuyMultipleItems lines must get one deduplicated final OnAccountChange: {seller_bals:?}"
    );
    assert!(
        (seller_bals[0] - seller_final).abs() < 1e-2,
        "Seller OnAccountChange ({:?}) must match final balance ({seller_final})",
        seller_bals[0]
    );
}
