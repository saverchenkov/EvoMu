//! Deep branch-coverage and mutation-killing unit tests for `market_orders_escrow` and order lifecycle.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
use crate::services::market_trading::{
    handle_orders_escrow, load_market_order_meta, save_market_order_meta, MarketOrderMeta,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
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
    let mut s = SessionState::new(9401);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

fn sell_kv(station_id: i64, type_id: u32, item_id: i64, price: f64, qty: i64) -> PyRep {
    PyRep::keyval([
        ("stationID", PyRep::Integer(station_id)),
        ("typeID", PyRep::Integer(type_id as i64)),
        ("itemID", PyRep::Integer(item_id)),
        ("price", PyRep::Real(price)),
        ("quantity", PyRep::Integer(qty)),
    ])
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, loc_id: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id: loc_id, flag, quantity: qty, singleton, version: 1, ..Default::default() }
}

fn mk_order(order_id: i64, owner_id: i64, type_id: u32, is_bid: bool, price_isk: i64, qty: i32, is_npc: bool, escrow_isk: i64) -> MarketOrderRecord {
    MarketOrderRecord {
        order_id, owner_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID, type_id,
        is_bid, price: IskCents::from_isk(price_isk), vol_entered: qty, vol_remaining: qty,
        min_volume: 1, is_npc, escrow: IskCents::from_isk(escrow_isk),
    }
}

#[test]
fn place_multi_sell_order_validation_item_discovery_and_fee_guards() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();

    // Unauthenticated session
    let mut anon = SessionState::new(9402);
    assert!(handle_orders_escrow(&ctx, &mut anon, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![])], &kw).unwrap().is_err());

    // Missing region_id -> NoRegion
    sess.region_id = None;
    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![])], &kw).unwrap().unwrap_err().code,
        "NoRegion"
    );
    sess.region_id = Some(THE_FORGE_REGION_ID);

    // Non-list first argument, invalid duration, negative expectedBrokersFee, non-KeyVal/Dict entry, station mismatch, zero qty, invalid tick
    for (args, expected_code) in [
        (vec![PyRep::Integer(1)], "InvalidMultiSellOrder"),
        (vec![PyRep::List(vec![sell_kv(JITA_STATION_ID, 34, 1, 10.0, 1)]), PyRep::Bool(false), PyRep::Integer(5)], "InvalidMarketOrder"),
        (vec![PyRep::List(vec![sell_kv(JITA_STATION_ID, 34, 1, 10.0, 1)]), PyRep::Bool(false), PyRep::Integer(90), PyRep::Real(-0.01)], "InvalidMultiSellOrder"),
        (vec![PyRep::List(vec![PyRep::Integer(99)])], "InvalidMultiSellOrder"),
        (vec![PyRep::List(vec![sell_kv(JITA_STATION_ID + 1, 34, 1, 10.0, 1)])], "StationMismatch"),
        (vec![PyRep::List(vec![sell_kv(JITA_STATION_ID, 34, 1, 10.0, 0)])], "InvalidMultiSellOrder"),
        (vec![PyRep::List(vec![sell_kv(JITA_STATION_ID, 34, 1, 12345.0, 1)])], "InvalidPriceTickSize"),
    ] {
        let err = handle_orders_escrow(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &args, &kw).unwrap().unwrap_err();
        assert_eq!(err.code, expected_code);
    }

    // Type without marketGroupID -> ItemNotMarketEligible
    let non_mkt_tid = ctx.sde.types.values().find(|t| t.market_group_id.is_none()).map(|t| t.type_id).unwrap_or(1);
    let non_mkt_err = handle_orders_escrow(
        &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_kv(JITA_STATION_ID, non_mkt_tid, 1, 10.0, 1)])], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(non_mkt_err.code, "ItemNotMarketEligible");

    // Seed courier item, non-empty ship/container, item inside container, and valid stack
    let (courier_id, ship_with_child_id, cont_item_id, valid_stack_id) = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.items.retain(|_, i| i.owner_id != char_id);
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let (cid, sh_id, box_id, in_box_id, vs_id) = (910_001, 910_002, 910_004, 910_005, 910_006);
        s.items.insert(cid, mk_item(cid, 34, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 10, false));
        s.item_custom_names.insert(cid, "courier_cargo:777".into());
        s.items.insert(sh_id, mk_item(sh_id, 35, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, false));
        s.items.insert(910_003, mk_item(910_003, 34, char_id, sh_id, InventoryFlag::Cargo, 5, false));
        s.items.insert(box_id, mk_item(box_id, 23, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        s.items.insert(in_box_id, mk_item(in_box_id, 36, char_id, box_id, InventoryFlag::Hangar, 20, false));
        s.items.insert(vs_id, mk_item(vs_id, 37, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 50, false));
        (cid, sh_id, in_box_id, vs_id)
    });

    for (entry, expected_code) in [
        (sell_kv(JITA_STATION_ID, 34, 0, 10.0, 5), "ItemCannotBeSold"),
        (sell_kv(JITA_STATION_ID, 34, courier_id, 10.0, 5), "ItemCannotBeSold"),
        (sell_kv(JITA_STATION_ID, 35, 0, 10.0, 1), "CannotSellItemWithContents"),
        (sell_kv(JITA_STATION_ID, 35, ship_with_child_id, 10.0, 1), "CannotSellItemWithContents"),
        (sell_kv(JITA_STATION_ID, 36, 0, 10.0, 5), "ItemInsideContainer"),
        (sell_kv(JITA_STATION_ID, 36, cont_item_id, 10.0, 5), "ItemInsideContainer"),
        (sell_kv(JITA_STATION_ID, 37, valid_stack_id, 10.0, 999), "InsufficientInventoryForSellOrder"),
        (sell_kv(JITA_STATION_ID, 37, 999_999_999, 10.0, 1), "InsufficientInventoryForSellOrder"),
    ] {
        let err = handle_orders_escrow(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![entry])], &kw).unwrap().unwrap_err();
        assert_eq!(err.code, expected_code);
    }

    // Immediate duration=0 order with no matching bids -> ImmediateOrderUnfilled
    let imm_err = handle_orders_escrow(
        &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_kv(JITA_STATION_ID, 37, valid_stack_id, 10.0, 10)]), PyRep::Bool(false), PyRep::Integer(0)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(imm_err.code, "ImmediateOrderUnfilled");

    // expectedBrokersFee rate guard (0.001 < actual 0.03) and ISK guard (50.0 < 100.0 floor)
    for fee_arg in [PyRep::Real(0.001), PyRep::Real(50.0)] {
        let fee_err = handle_orders_escrow(
            &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
            &[PyRep::List(vec![sell_kv(JITA_STATION_ID, 37, valid_stack_id, 10.0, 10)]), PyRep::Bool(false), PyRep::Integer(90), fee_arg], &kw,
        ).unwrap().unwrap_err();
        assert_eq!(fee_err.code, "BrokerFeeExceeded");
    }
}

#[test]
fn place_multi_sell_corporate_roles_insolvent_margin_bid_and_corp_bid_cover() {
    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let insolv_buyer_id = 90_000_002_i64;
    let buyer_corp_id = 98_400_001_i64;
    let seller_corp_id = 98_400_002_i64;
    let mut sess = jita_session(seller_id, seller_corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&seller_id) {
            ch.corporation_id = seller_corp_id;
            ch.balance = IskCents::from_isk(1_000_000);
        }
        let mut insolv = s.characters[&seller_id].clone();
        insolv.character_id = insolv_buyer_id;
        insolv.user_id = 2;
        insolv.balance = IskCents(0);
        s.characters.insert(insolv_buyer_id, insolv);

        let mut s_corp = CorporationRecord {
            corporation_id: seller_corp_id, name: "SellerCorp".into(), ticker: "SLCP".into(), ceo_id: 90_000_099,
            master_wallet: IskCents::from_isk(500_000), members: HashSet::from([seller_id]), ..Default::default()
        };
        s_corp.ensure_wallet_divisions();
        s_corp.wallet_divisions[0].balance = IskCents::from_isk(500_000);
        s.corporations.insert(seller_corp_id, s_corp);

        let mut b_corp = CorporationRecord {
            corporation_id: buyer_corp_id, name: "BuyerCorp".into(), ticker: "BYCP".into(), ceo_id: insolv_buyer_id,
            master_wallet: IskCents::from_isk(100_000), ..Default::default()
        };
        b_corp.ensure_wallet_divisions();
        b_corp.wallet_divisions[0].balance = IskCents::from_isk(100_000);
        b_corp.wallet_divisions[1].balance = IskCents::from_isk(100_000);
        s.corporations.insert(buyer_corp_id, b_corp);
        s.items.insert(920_001, mk_item(920_001, 34, seller_corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 40, false));
        // Bid 1 (30 ISK): insolvent personal Margin Trading bid (escrow 100 ISK, needs 1200 ISK)
        s.market_orders.insert(920_101, mk_order(920_101, insolv_buyer_id, 34, true, 30, 40, false, 100));
        // Bid 2 (25 ISK): solvent corporate Margin Trading bid (escrow 200 ISK, needs 500 ISK for 20 units)
        s.market_orders.insert(920_102, mk_order(920_102, buyer_corp_id, 34, true, 25, 20, false, 200));
        save_market_order_meta(
            s, 920_102,
            MarketOrderMeta { range: -1, duration: 90, issue_unix: ctx.now_unix().saturating_sub(100), order_state: None, account_key: Some(1001), final_vol_remaining: None, closed_unix: None },
        );
    });

    let dict_entry = PyRep::Dict(vec![
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("itemID".into()), PyRep::Integer(920_001)),
        (PyRep::String("price".into()), PyRep::Real(20.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(40)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ]);

    // Without accountant/trader role -> CorpRoleRequired
    let role_err = handle_orders_escrow(
        &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![dict_entry.clone()]), PyRep::Bool(true), PyRep::Integer(90), PyRep::Real(0.0), PyRep::Integer(1000)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(role_err.code, "CorpRoleRequired");

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&seller_corp_id).unwrap().member_roles.insert(
            seller_id,
            corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_JUNIOR_ACCOUNTANT | (1u64 << 20),
        );
    });

    let ok = handle_orders_escrow(
        &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![dict_entry]), PyRep::Bool(true), PyRep::Integer(90), PyRep::Real(0.0), PyRep::Integer(1000)], &kw,
    ).unwrap().unwrap();
    assert_eq!(ok.dict_get("filledImmediately").and_then(PyRep::as_bool), Some(false));
    let resting_oid = ok.dict_get("orderID").and_then(PyRep::as_i64).unwrap();

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&920_101].vol_remaining, 0);
        assert_eq!(s.characters[&insolv_buyer_id].balance, IskCents::from_isk(100));
        assert_eq!(s.market_orders[&920_102].vol_remaining, 0);
        assert_eq!(s.corporations[&buyer_corp_id].master_wallet, IskCents::from_isk(100_000));
        assert_eq!(s.corporations[&buyer_corp_id].division_balance(2), IskCents::from_isk(99_700));
        assert_eq!(s.market_orders[&resting_oid].vol_remaining, 20);
        assert_eq!(s.market_orders[&resting_oid].owner_id, seller_corp_id);
    });
}

#[test]
fn cancel_char_order_corporate_roles_and_deliveries_stack_merge() {
    let ctx = test_ctx();
    let member_id = 90_000_001_i64;
    let corp_id = 98_400_010_i64;
    let mut sess = jita_session(member_id, corp_id);
    let kw = HashMap::new();

    assert_eq!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[], &kw).unwrap().unwrap_err().code, "InvalidMarketOrder");
    assert_eq!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(999_888_777)], &kw).unwrap().unwrap_err().code, "OrderNotFound");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
        }
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id, name: "CancelCorp".into(), ticker: "CNCL".into(), ceo_id: 90_000_099,
                master_wallet: IskCents::from_isk(10_000), members: HashSet::from([member_id]), ..Default::default()
            },
        );
        s.items.insert(930_001, mk_item(930_001, 35, corp_id, JITA_STATION_ID, InventoryFlag::Deliveries, 30, false));
        s.market_orders.insert(930_101, mk_order(930_101, corp_id, 34, true, 10, 50, false, 500));
        save_market_order_meta(
            s, 930_101,
            MarketOrderMeta { range: -1, duration: 90, issue_unix: ctx.now_unix(), order_state: None, account_key: Some(1001), final_vol_remaining: None, closed_unix: None },
        );
        s.market_orders.insert(930_102, mk_order(930_102, corp_id, 35, false, 20, 45, false, 0));
        save_market_order_meta(
            s, 930_102,
            MarketOrderMeta { range: 32767, duration: 90, issue_unix: ctx.now_unix(), order_state: None, account_key: Some(1000), final_vol_remaining: None, closed_unix: None },
        );
        s.market_order_meta.get_mut(&930_102).unwrap().issued_by = Some(member_id);
    });

    assert_eq!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(930_101)], &kw).unwrap().unwrap_err().code, "CorpRoleRequired");
    assert_eq!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(930_102)], &kw).unwrap().unwrap(), PyRep::Bool(true));

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(member_id, corp_roles::ROLE_TRADER);
    });
    assert_eq!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(930_101)], &kw).unwrap().unwrap(), PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&930_001].quantity, 75);
        assert_eq!(s.items[&930_001].version, 2);
        assert_eq!(s.corporations[&corp_id].master_wallet, IskCents::from_isk(10_000));
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents::from_isk(500));
        let m = load_market_order_meta(s, &s.market_orders[&930_101], 0);
        assert_eq!(m.order_state, Some(3));
        assert_eq!(m.final_vol_remaining, Some(50));
    });
}

#[test]
fn buy_multiple_items_entry_formats_visibility_partial_isk_npc_skill_and_plex() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_corp_id = 98_400_020_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&buyer_id].corporation_id);
    let mut sess = jita_session(buyer_id, corp_id);
    let kw = HashMap::new();

    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "BuyMultipleItems", &[PyRep::Integer(JITA_STATION_ID + 5), PyRep::List(vec![])], &kw).unwrap().unwrap_err().code,
        "StationMismatch"
    );

    let vis_req = PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(1)), ("orderRange", PyRep::Integer(5))])]);
    assert_eq!(
        handle_orders_escrow(&ctx, &mut sess, "marketProxy", "BuyMultipleItems", &[vis_req], &kw).unwrap().unwrap_err().code,
        "InsufficientVisibilitySkill"
    );

    let skill_tid = ctx.sde.types.values().find(|t| t.published && t.category_id == 16).map(|t| t.type_id).unwrap_or(3443);

    let plex_before = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(150);
        }
        s.skills.entry(buyer_id).or_default().insert(3447, TrainedSkillRecord { type_id: 3447, level: 5, skill_points: 256_000.0 });
        s.corporations.insert(
            seller_corp_id,
            CorporationRecord { corporation_id: seller_corp_id, name: "AskCorp".into(), ticker: "ASKC".into(), ceo_id: 90_000_099, master_wallet: IskCents(0), ..Default::default() },
        );
        s.market_orders.insert(940_001, mk_order(940_001, seller_corp_id, PLEX_TYPE_ID, false, 50, 1, false, 0));
        s.market_orders.insert(940_002, mk_order(940_002, 1_000_035, skill_tid, false, 50, 100, true, 0));
        s.market_orders.insert(940_003, mk_order(940_003, seller_corp_id, 34, false, 10, 10, false, 0));
        s.market_orders.insert(940_004, mk_order(940_004, seller_corp_id, 35, false, 10, 5, false, 0));
        s.plex_vault_balance(buyer_id)
    });

    let entries = PyRep::List(vec![
        PyRep::keyval([("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)), ("qty", PyRep::Integer(1)), ("price", PyRep::Real(60.0)), ("range", PyRep::Integer(32767))]),
        PyRep::Dict(vec![
            (PyRep::String("typeID".into()), PyRep::Integer(skill_tid as i64)),
            (PyRep::String("qty".into()), PyRep::Integer(1)),
            (PyRep::String("maxPrice".into()), PyRep::Real(60.0)),
            (PyRep::String("orderRange".into()), PyRep::Integer(0)),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(10)]),
        PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(5)]),
    ]);

    let res = handle_orders_escrow(&ctx, &mut sess, "marketProxy", "BuyMultipleItems", &[PyRep::Integer(JITA_STATION_ID), entries], &kw).unwrap().unwrap();
    if let Some(PyRep::List(statuses)) = res.dict_get("itemStatuses") {
        assert_eq!(statuses[0].dict_get("status").and_then(PyRep::as_str), Some("Filled"));
        assert_eq!(statuses[1].dict_get("status").and_then(PyRep::as_str), Some("Filled"));
        assert_eq!(statuses[2].dict_get("status").and_then(PyRep::as_str), Some("Partial"));
        assert_eq!(statuses[3].dict_get("status").and_then(PyRep::as_str), Some("InsufficientISK"));
    } else {
        panic!("expected itemStatuses");
    }

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.plex_vault_balance(buyer_id), plex_before + 1);
        assert_eq!(s.market_orders[&940_002].vol_remaining, 100);
        assert!(s.corporations[&seller_corp_id].master_wallet.0 > 0);
    });
}
